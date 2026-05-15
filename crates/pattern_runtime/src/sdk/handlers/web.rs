//! Handler for `Pattern.Web` — web search and content fetching.
//!
//! Provides structured web search (Brave → DuckDuckGo cascade) and
//! URL content fetching with readable-text extraction.

use std::sync::atomic::Ordering;

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::WebReq;
use crate::session::{SessionContext, record_exchange};
use crate::timeout::{CANCELLED_SENTINEL, HandlerGuard};

/// Handler position in the canonical [`crate::sdk::bundle::SdkBundle`] HList.
const WEB_HANDLER_TAG: u32 = 18;

/// Handler for `Pattern.Web`.
#[derive(Clone, Debug)]
pub struct WebHandler {
    client: reqwest::Client,
}

impl WebHandler {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:149.0) Gecko/20100101 Firefox/149.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

impl Default for WebHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl DescribeEffect for WebHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Web",
            description: "Web search and content fetching (WebSearch/WebFetch/WebFetchContinue)",
            constructors: std::borrow::Cow::Borrowed(&[
                "WebSearch         :: Text -> Maybe Int -> Web Text",
                "WebFetch          :: Text -> Maybe Text -> Web Text",
                "WebFetchContinue  :: Text -> Int -> Maybe Int -> Web Text",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[]),
            helpers: std::borrow::Cow::Borrowed(&[
                "search :: Member Web effs => Text -> Eff effs Text\nsearch q = send (WebSearch q Nothing)",
                "fetch :: Member Web effs => Text -> Eff effs Text\nfetch url = send (WebFetch url Nothing)",
                "fetchReadable :: Member Web effs => Text -> Eff effs Text\nfetchReadable url = send (WebFetch url (Just \"readable\"))",
                "fetchRaw :: Member Web effs => Text -> Eff effs Text\nfetchRaw url = send (WebFetch url (Just \"raw\"))",
                "fetchContinue :: Member Web effs => Text -> Int -> Eff effs Text\nfetchContinue url offset = send (WebFetchContinue url offset Nothing)",
            ]),
        }
    }
}

impl EffectHandler<SessionContext> for WebHandler {
    type Request = WebReq;

    fn handle(
        &mut self,
        req: WebReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        if state.cancellation.load(Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{CANCELLED_SENTINEL}: web handler cancelled at entry"
            )));
        }

        let _guard = HandlerGuard::enter(&state.gate);

        let constructor_name = match &req {
            WebReq::WebSearch(_, _) => "WebSearch",
            WebReq::WebFetch(_, _) => "WebFetch",
            WebReq::WebFetchContinue(_, _, _) => "WebFetchContinue",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Web",
            constructor_name,
        )?;

        let client = self.client.clone();
        let handle = cx.user().tokio_handle().clone();
        let request_repr = format!("{req:?}");

        let result = match req {
            WebReq::WebSearch(query, limit) => {
                let limit = limit.unwrap_or(10).min(20) as usize;
                search_brave(&handle, &client, &query, limit).or_else(|e| {
                    tracing::warn!("Brave search failed: {e}, falling back to DuckDuckGo");
                    search_ddg(&handle, &client, &query, limit)
                })
            }
            WebReq::WebFetch(url, format) => {
                let readable = format.as_deref() != Some("raw");
                match fetch_page_typed(&handle, &client, &url) {
                    Ok(FetchOutcome::Text(html)) => {
                        // Existing text path: html->markdown if readable, then paginate.
                        let content = if readable {
                            html_to_markdown(&html, Some(&url))
                        } else {
                            html
                        };
                        let total_len = content.len();
                        let max_chars = 10_000usize;
                        let start = 0usize;
                        let end = floor_char_boundary(&content, max_chars.min(total_len));
                        let slice = &content[start..end];
                        let has_more = end < total_len;
                        let result = serde_json::json!({
                            "content": slice,
                            "offset": start,
                            "total_length": total_len,
                            "has_more": has_more,
                            "next_offset": if has_more { Some(end) } else { None::<usize> },
                        });
                        serde_json::to_string(&result)
                            .map_err(|e| format!("failed to serialize: {e}"))
                    }
                    Ok(FetchOutcome::Binary {
                        bytes,
                        content_type,
                        display_name,
                    }) => {
                        // Binary path: build a ContentPart::Binary via the multimodal helper,
                        // push to the side-channel, return marker text to the agent's eval.
                        match pattern_core::multimodal::bytes_to_binary_part(
                            bytes,
                            &content_type,
                            display_name,
                            &pattern_core::multimodal::BinaryConvertOpts::default(),
                        ) {
                            Ok((part, meta)) => {
                                let marker = pattern_core::multimodal::marker_text_for(&meta);
                                cx.user().push_pending_tool_attachment(part);
                                Ok(marker)
                            }
                            Err(e) => Err(format!("multi-modal binary build failed: {e}")),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            WebReq::WebFetchContinue(url, offset, limit) => {
                let offset = offset as usize;
                let limit = limit.map(|l| l as usize);
                fetch_page(&handle, &client, &url, true, offset, limit)
            }
        };

        let value = result.map_err(|e| EffectError::Handler(format!("Pattern.Web: {e}")))?;

        let response = cx.respond(value);

        if let Ok(ref val) = response {
            let log = cx.user().checkpoint_log();
            let turn = cx.user().current_turn();
            record_exchange(&log, WEB_HANDLER_TAG, request_repr, val, turn);
        }

        response
    }
}

// ---- Search implementations ----

fn search_brave(
    handle: &tokio::runtime::Handle,
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<String, String> {
    let response = std::thread::scope(|s| {
        let client = client.clone();
        let query = query.to_string();
        s.spawn(move || {
            handle.block_on(async {
                client
                    .get("https://search.brave.com/search")
                    .query(&[("q", &query)])
                    .header("Accept", "text/html,application/xhtml+xml")
                    .send()
                    .await
                    .map_err(|e| format!("brave request failed: {e}"))?
                    .text()
                    .await
                    .map_err(|e| format!("brave body read failed: {e}"))
            })
        })
        .join()
        .map_err(|_| "brave search thread panicked".to_string())?
    })?;

    parse_brave_results(&response, limit)
}

fn parse_brave_results(html: &str, limit: usize) -> Result<String, String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);

    // Brave uses various selectors for results. Try multiple patterns.
    let result_sel = Selector::parse("div.snippet[data-type=\"web\"]").unwrap();
    let title_sel = Selector::parse(".search-snippet-title").unwrap();
    let link_sel = Selector::parse("a[href]").unwrap();
    // Brave description: the text content is in a .content div inside .generic-snippet
    // The class may include svelte hashes: "content desktop-default-regular t-primary..."
    let desc_sel = Selector::parse(".content").unwrap();

    let mut results = Vec::new();
    let total_snippets = document.select(&result_sel).count();
    tracing::debug!(total_snippets, "brave: found snippet elements");

    for elem in document.select(&result_sel).take(limit) {
        let desc_count = elem.select(&desc_sel).count();
        tracing::debug!(desc_count, "brave: desc matches in snippet");
        let title = elem
            .select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let url = elem
            .select(&link_sel)
            .next()
            .and_then(|e| e.value().attr("href"))
            .unwrap_or_default()
            .to_string();

        let snippet = elem
            .select(&desc_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        if !title.is_empty() || !url.is_empty() {
            results.push(serde_json::json!({
                "title": title,
                "url": url,
                "snippet": snippet,
            }));
        }
    }

    // Fallback: if no structured results found, try simpler link extraction
    if results.is_empty() {
        let link_sel = Selector::parse("a[href]").unwrap();
        for elem in document.select(&link_sel).take(limit * 2) {
            let href = elem.value().attr("href").unwrap_or_default();
            if href.starts_with("http") && !href.contains("brave.com") {
                let text = elem.text().collect::<String>().trim().to_string();
                if !text.is_empty() {
                    results.push(serde_json::json!({
                        "title": text,
                        "url": href,
                        "snippet": "",
                    }));
                }
            }
            if results.len() >= limit {
                break;
            }
        }
    }

    serde_json::to_string(&results).map_err(|e| format!("failed to serialize results: {e}"))
}

fn search_ddg(
    handle: &tokio::runtime::Handle,
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<String, String> {
    let response = std::thread::scope(|s| {
        let client = client.clone();
        let query = query.to_string();
        s.spawn(move || {
            handle.block_on(async {
                client
                    .get("https://html.duckduckgo.com/html/")
                    .query(&[("q", &query)])
                    .send()
                    .await
                    .map_err(|e| format!("ddg request failed: {e}"))?
                    .text()
                    .await
                    .map_err(|e| format!("ddg body read failed: {e}"))
            })
        })
        .join()
        .map_err(|_| "ddg search thread panicked".to_string())?
    })?;

    parse_ddg_results(&response, limit)
}

fn parse_ddg_results(html: &str, limit: usize) -> Result<String, String> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let result_sel = Selector::parse(".result").unwrap();
    let title_sel = Selector::parse(".result__a").unwrap();
    let snippet_sel = Selector::parse(".result__snippet").unwrap();

    let mut results = Vec::new();

    for elem in document.select(&result_sel).take(limit) {
        let title_elem = elem.select(&title_sel).next();
        let (title, url) = if let Some(te) = title_elem {
            (
                te.text().collect::<String>().trim().to_string(),
                te.value().attr("href").unwrap_or_default().to_string(),
            )
        } else {
            continue;
        };

        let snippet = elem
            .select(&snippet_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        if !url.is_empty() {
            results.push(serde_json::json!({
                "title": title,
                "url": url,
                "snippet": snippet,
            }));
        }
    }

    serde_json::to_string(&results).map_err(|e| format!("failed to serialize results: {e}"))
}

// ---- Fetch implementation ----

/// Result of a single fetch — either text or binary.
#[allow(clippy::large_enum_variant)]
enum FetchOutcome {
    Text(String),
    Binary {
        bytes: Vec<u8>,
        content_type: String,
        display_name: Option<String>,
    },
}

/// Content-type-aware fetch. Inspects HTTP Content-Type before decoding body.
fn fetch_page_typed(
    handle: &tokio::runtime::Handle,
    client: &reqwest::Client,
    url: &str,
) -> Result<FetchOutcome, String> {
    std::thread::scope(|s| {
        let client = client.clone();
        let handle = handle.clone();
        let url = url.to_string();
        s.spawn(move || {
            handle.block_on(async {
                let resp = client
                    .get(&url)
                    .header(
                        "Accept",
                        "text/html,application/xhtml+xml,image/*,application/pdf,*/*",
                    )
                    .send()
                    .await
                    .map_err(|e| format!("fetch failed: {e}"))?;
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                if pattern_core::multimodal::is_binary_mime(&content_type) {
                    let bytes = resp
                        .bytes()
                        .await
                        .map_err(|e| format!("binary body read failed: {e}"))?
                        .to_vec();
                    let display_name = url
                        .rsplit('/')
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.split('?').next().unwrap_or(s).to_string());
                    Ok(FetchOutcome::Binary {
                        bytes,
                        content_type,
                        display_name,
                    })
                } else {
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| format!("text body read failed: {e}"))?;
                    Ok(FetchOutcome::Text(text))
                }
            })
        })
        .join()
        .map_err(|_| "fetch thread panicked".to_string())?
    })
}

fn fetch_page(
    handle: &tokio::runtime::Handle,
    client: &reqwest::Client,
    url: &str,
    readable: bool,
    offset: usize,
    limit: Option<usize>,
) -> Result<String, String> {
    let html = std::thread::scope(|s| {
        let client = client.clone();
        let handle = handle.clone();
        let url = url.to_string();
        s.spawn(move || {
            handle.block_on(async {
                client
                    .get(&url)
                    .header("Accept", "text/html,application/xhtml+xml,*/*")
                    .send()
                    .await
                    .map_err(|e| format!("fetch failed: {e}"))?
                    .text()
                    .await
                    .map_err(|e| format!("body read failed: {e}"))
            })
        })
        .join()
        .map_err(|_| "fetch thread panicked".to_string())?
    })?;

    let content = if readable {
        html_to_markdown(&html, Some(url))
    } else {
        html
    };

    let total_len = content.len();
    let max_chars = limit.unwrap_or(10_000);
    // Agent-supplied byte offsets may land mid-character (e.g. inside a
    // multi-byte UTF-8 sequence). Round both ends down to the nearest
    // char boundary before slicing — naked `&content[start..end]` panics
    // and crashes the eval worker. The reported `next_offset` reflects
    // the rounded `end` so the agent's next call lands on a valid boundary.
    let start = floor_char_boundary(&content, offset.min(total_len));
    let end = floor_char_boundary(&content, (start + max_chars).min(total_len));
    let slice = &content[start..end];
    let has_more = end < total_len;

    let result = serde_json::json!({
        "content": slice,
        "offset": start,
        "total_length": total_len,
        "has_more": has_more,
        "next_offset": if has_more { Some(end) } else { None },
    });

    serde_json::to_string(&result).map_err(|e| format!("failed to serialize: {e}"))
}

/// Round a byte index down to the nearest UTF-8 character boundary.
///
/// Used by the fetch-pagination path so agent-supplied offsets that land
/// mid-multi-byte-sequence don't panic the slice operation. Walking back
/// at most 3 bytes is sufficient because UTF-8 sequences are at most 4
/// bytes; we land on the start byte (high bits 0xxxxxxx or 11xxxxxx).
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Convert HTML to readable markdown.

/// Preprocesses to strip scripts/styles, then uses html2md for conversion.
fn html_to_markdown(html: &str, base_url: Option<&str>) -> String {
    let cleaned = preprocess_html(html);
    let parsed_base = base_url.and_then(|u| url::Url::parse(u).ok());
    html2md::rewrite_html_custom_with_url(&cleaned, &None, false, &parsed_base)
}

/// Strip script, style, SVG, noscript, comments and JS event handlers
/// from HTML before markdown conversion.
fn preprocess_html(html: &str) -> String {
    use regex::Regex;

    let script_re = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let style_re = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let comment_re = Regex::new(r"(?s)<!--.*?-->").unwrap();
    let svg_re = Regex::new(r"(?is)<svg[^>]*>.*?</svg>").unwrap();
    let noscript_re = Regex::new(r"(?is)<noscript[^>]*>.*?</noscript>").unwrap();

    let mut cleaned = script_re.replace_all(html, "").to_string();
    cleaned = style_re.replace_all(&cleaned, "").to_string();
    cleaned = comment_re.replace_all(&cleaned, "").to_string();
    cleaned = svg_re.replace_all(&cleaned, "").to_string();
    cleaned = noscript_re.replace_all(&cleaned, "").to_string();
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_brave_results_from_fixture() {
        let html = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/brave-search.html"
        ))
        .expect("fixture file");

        let results_json = parse_brave_results(&html, 10).expect("parse should succeed");
        let results: Vec<serde_json::Value> = serde_json::from_str(&results_json).unwrap();

        eprintln!("Total results: {}", results.len());
        for (i, r) in results.iter().enumerate() {
            let snippet_preview = &r["snippet"].as_str().unwrap_or("");
            let preview = &snippet_preview[..std::cmp::min(80, snippet_preview.len())];
            eprintln!(
                "Result {}: title={:?} snippet={:?}",
                i,
                r["title"].as_str().unwrap_or(""),
                preview
            );
        }

        // Diagnose selectors
        use scraper::{Html, Selector};
        let document = Html::parse_document(&html);

        let result_sel = Selector::parse("div.snippet[data-type=\"web\"]").unwrap();
        let snippet_count = document.select(&result_sel).count();
        eprintln!("div.snippet[data-type=web] matches: {}", snippet_count);

        let selectors = [
            ".content",
            ".generic-snippet",
            ".generic-snippet .content",
            ".generic-snippet > .content",
            "div.content",
        ];
        for sel_str in &selectors {
            let sel = Selector::parse(sel_str).unwrap();
            let top = document.select(&sel).count();
            let inner = document
                .select(&result_sel)
                .next()
                .map(|e| e.select(&sel).count())
                .unwrap_or(0);
            eprintln!(
                "Selector {:?}: top-level={}, in-first-snippet={}",
                sel_str, top, inner
            );
        }

        // Detailed look at first snippet's desc element
        let desc_sel = Selector::parse(".content").unwrap();
        if let Some(first_snippet) = document.select(&result_sel).next() {
            if let Some(desc_elem) = first_snippet.select(&desc_sel).next() {
                let inner = desc_elem.inner_html();
                let text_pieces: Vec<&str> = desc_elem.text().collect();
                eprintln!(
                    "DESC inner_html (first 200): {:?}",
                    &inner[..std::cmp::min(200, inner.len())]
                );
                eprintln!("DESC text pieces: {:?}", text_pieces);
                eprintln!("DESC text joined: {:?}", text_pieces.join("").trim());
            } else {
                eprintln!("NO desc element found in first snippet!");
            }
        }

        assert!(snippet_count > 0, "should find snippet elements");
    }
}
