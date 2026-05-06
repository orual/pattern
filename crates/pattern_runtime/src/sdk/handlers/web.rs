//! Handler for `Pattern.Web` — web search and content fetching.
//!
//! Provides structured web search (Brave → DuckDuckGo cascade) and
//! URL content fetching with readable-text extraction.

use std::sync::Arc;
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
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0")
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
        let request_repr = format!("{req:?}");

        let result = match req {
            WebReq::WebSearch(query, limit) => {
                let limit = limit.unwrap_or(10).min(20) as usize;
                search_brave(&client, &query, limit).or_else(|e| {
                    tracing::warn!("Brave search failed: {e}, falling back to DuckDuckGo");
                    search_ddg(&client, &query, limit)
                })
            }
            WebReq::WebFetch(url, format) => {
                let readable = format.as_deref() != Some("raw");
                fetch_page(&client, &url, readable, 0, None)
            }
            WebReq::WebFetchContinue(url, offset, limit) => {
                let offset = offset as usize;
                let limit = limit.map(|l| l as usize);
                fetch_page(&client, &url, true, offset, limit)
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

fn search_brave(client: &reqwest::Client, query: &str, limit: usize) -> Result<String, String> {
    let rt = tokio::runtime::Handle::try_current().map_err(|e| format!("no tokio runtime: {e}"))?;

    let response = std::thread::scope(|s| {
        let client = client.clone();
        let query = query.to_string();
        s.spawn(move || {
            rt.block_on(async {
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
    let result_sel = Selector::parse(".snippet").unwrap();
    let title_sel = Selector::parse("a.heading-serpresult, .snippet-title a, h3 a").unwrap();
    let desc_sel = Selector::parse(".snippet-description, .snippet-content p").unwrap();
    let url_sel = Selector::parse(".snippet-url cite, cite").unwrap();

    let mut results = Vec::new();

    for elem in document.select(&result_sel).take(limit) {
        let title = elem
            .select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let url = elem
            .select(&title_sel)
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

fn search_ddg(client: &reqwest::Client, query: &str, limit: usize) -> Result<String, String> {
    let rt = tokio::runtime::Handle::try_current().map_err(|e| format!("no tokio runtime: {e}"))?;

    let response = std::thread::scope(|s| {
        let client = client.clone();
        let query = query.to_string();
        s.spawn(move || {
            rt.block_on(async {
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

fn fetch_page(
    client: &reqwest::Client,
    url: &str,
    readable: bool,
    offset: usize,
    limit: Option<usize>,
) -> Result<String, String> {
    let rt = tokio::runtime::Handle::try_current().map_err(|e| format!("no tokio runtime: {e}"))?;

    let html = std::thread::scope(|s| {
        let client = client.clone();
        let url = url.to_string();
        s.spawn(move || {
            rt.block_on(async {
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
        html_to_markdown(&html)
    } else {
        html
    };

    let total_len = content.len();
    let max_chars = limit.unwrap_or(10_000);
    let start = offset.min(total_len);
    let end = (start + max_chars).min(total_len);
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

/// Convert HTML to readable markdown.
/// Preprocesses to strip scripts/styles, then uses html2md for conversion.
fn html_to_markdown(html: &str) -> String {
    let cleaned = preprocess_html(html);
    html2md::parse_html(&cleaned)
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
