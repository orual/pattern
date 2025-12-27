//! Web tool for fetching and searching web content

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::runtime::ToolContext;
use crate::{CoreError, PatternHttpClient, Result, tool::AiTool};

/// Operation types for web interactions
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebOperation {
    Fetch,
    Search,
}

/// Format for web content rendering
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebFormat {
    Html,
    #[default]
    Markdown,
}

/// Input for web interactions
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebInput {
    /// The operation to perform
    pub operation: WebOperation,

    /// For fetch: URL to retrieve
    /// For search: search query
    pub query: String,

    /// For fetch: output format (default: markdown)
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<WebFormat>,

    /// For search: maximum results (1-20, default: 10)
    /// For fetch: max characters per page (default: 10000)
    #[schemars(default, with = "i64")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,

    /// For fetch: continue reading from this character offset
    #[schemars(default, with = "i64")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_from: Option<usize>,
    // request_heartbeat handled via ExecutionMeta injection; field removed
}

/// Result from a web search
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(inline)]
pub struct SearchResult {
    /// Result title
    pub title: String,
    /// Result URL
    pub url: String,
    /// Result snippet/description
    pub snippet: String,
}

/// Metadata about fetched content
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchMetadata {
    /// Final URL after redirects
    pub url: String,
    /// Content type from server
    pub content_type: String,
    /// Format used for conversion
    pub format: WebFormat,
    /// Total content length in characters
    pub total_length: usize,
    /// Current offset in content
    pub offset: usize,
    /// Whether more content is available
    pub has_more: bool,
}

/// Output from web operations
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WebOutput {
    /// The main content or results
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Search results (when operation is search)
    #[schemars(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<SearchResult>>,

    /// Metadata about the operation
    #[schemars(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FetchMetadata>,

    /// For pagination: offset to continue from
    #[schemars(default, with = "i64")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

/// Cached fetch content
#[derive(Debug, Clone)]
struct CachedContent {
    content: String,
    timestamp: Instant,
}

/// Web interaction tool
#[derive(Clone)]
pub struct WebTool {
    #[allow(dead_code)]
    pub(crate) ctx: Arc<dyn ToolContext>,
    client: PatternHttpClient,
    /// Cache URL -> (content, timestamp)
    fetch_cache: Arc<DashMap<String, CachedContent>>,
    /// Most recently fetched URL for continuation
    last_fetch_url: Arc<std::sync::Mutex<Option<String>>>,
}

impl std::fmt::Debug for WebTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebTool")
            .field("ctx", &"Arc<dyn ToolContext>")
            .field("client", &self.client)
            .field("fetch_cache", &self.fetch_cache)
            .field("last_fetch_url", &self.last_fetch_url)
            .finish()
    }
}

impl WebTool {
    /// Create a new web tool
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        let client = PatternHttpClient::default();

        Self {
            ctx,
            client,
            fetch_cache: Arc::new(DashMap::new()),
            last_fetch_url: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Search using Kagi with session cookies and auth header
    async fn search_kagi(&self, query: &str, limit: usize) -> Result<WebOutput> {
        // Get auth credentials from environment
        let kagi_session = std::env::var("KAGI_SESSION").map_err(|e| {
            CoreError::tool_exec_msg(
                "web",
                serde_json::json!({ "query": query }),
                format!("KAGI_SESSION environment variable not set: {}", e),
            )
        })?;

        let kagi_search = std::env::var("KAGI_SEARCH").unwrap_or_default(); // Optional, may not be needed

        let kagi_auth = std::env::var("KAGI_AUTH").unwrap_or_default(); // Optional auth header

        // Build cookie header
        let mut cookie = format!("kagi_session={}", kagi_session);
        if !kagi_search.is_empty() {
            cookie.push_str(&format!("; _kagi_search={}", kagi_search));
        }

        let mut request = self
            .client
            .client
            .get("https://kagi.com/search")
            .query(&[("q", query)])
            .header("Cookie", cookie)
            .header(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0",
            )
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            );

        // Add auth header if present
        if !kagi_auth.is_empty() {
            request = request.header("X-Kagi-Authorization", kagi_auth);
        }

        let response = request.send().await.map_err(|e| {
            CoreError::tool_exec_error("web", serde_json::json!({ "query": query }), e)
        })?;

        if !response.status().is_success() {
            return Err(CoreError::tool_exec_msg(
                "web",
                serde_json::json!({ "query": query }),
                format!("Kagi returned status: {}", response.status()),
            ));
        }

        let html = response.text().await.map_err(|e| {
            CoreError::tool_exec_error("web", serde_json::json!({ "query": query }), e)
        })?;

        // Parse Kagi HTML results with scraper
        let document = scraper::Html::parse_document(&html);

        // Kagi uses specific selectors for their search results
        let result_selector =
            scraper::Selector::parse(".search-result, ._0_result, .result").unwrap();
        let title_selector =
            scraper::Selector::parse("h3 a, .result-title a, ._0_title a, a._0_title_link")
                .unwrap();
        let url_selector = scraper::Selector::parse(".result-url, ._0_url, cite").unwrap();
        let desc_selector =
            scraper::Selector::parse(".result-desc, ._0_snippet, .search-result__snippet").unwrap();

        let mut results = Vec::new();

        for (i, result_elem) in document.select(&result_selector).enumerate() {
            if i >= limit {
                break;
            }

            // Try to extract title and URL from the link
            let title_elem = result_elem.select(&title_selector).next();
            let (title, url) = if let Some(elem) = title_elem {
                let title = elem.text().collect::<String>().trim().to_string();
                let url = elem
                    .value()
                    .attr("href")
                    .map(|u| {
                        // Kagi sometimes uses relative URLs
                        if u.starts_with("/url?") {
                            // Extract actual URL from redirect
                            u.split("url=")
                                .nth(1)
                                .and_then(|s| s.split('&').next())
                                .and_then(|s| urlencoding::decode(s).ok())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| u.to_string())
                        } else if u.starts_with("http") {
                            u.to_string()
                        } else {
                            format!("https://kagi.com{}", u)
                        }
                    })
                    .unwrap_or_default();
                (title, url)
            } else {
                // Fallback: try to find any link in the result
                let link = result_elem
                    .select(&scraper::Selector::parse("a[href]").unwrap())
                    .next();
                if let Some(link_elem) = link {
                    let title = link_elem.text().collect::<String>().trim().to_string();
                    let url = link_elem.value().attr("href").unwrap_or("").to_string();
                    (title, url)
                } else {
                    continue;
                }
            };

            // Try to extract URL from cite if not found
            let url = if url.is_empty() {
                result_elem
                    .select(&url_selector)
                    .next()
                    .map(|e| e.text().collect::<String>().trim().to_string())
                    .unwrap_or(url)
            } else {
                url
            };

            // Extract snippet
            let snippet = result_elem
                .select(&desc_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_else(|| {
                    // Fallback: get text content of result, excluding title
                    result_elem
                        .text()
                        .collect::<String>()
                        .lines()
                        .filter(|line| !line.trim().is_empty() && !line.contains(&title))
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .trim()
                        .to_string()
                });

            if !url.is_empty() && !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }

        Ok(WebOutput {
            content: None,
            results: Some(results),
            metadata: None,
            next_offset: None,
        })
    }

    /// Search using Brave Search (no API key required for basic searches)
    async fn search_brave(&self, query: &str, limit: usize) -> Result<WebOutput> {
        let response = self
            .client
            .client
            .get("https://search.brave.com/search")
            .query(&[("q", query)])
            .header(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0",
            )
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .send()
            .await
            .map_err(|e| CoreError::ToolExecutionFailed {
                tool_name: "web".to_string(),
                cause: format!("Brave search request failed: {}", e),
                parameters: serde_json::json!({ "query": query }),
            })?;

        let html = response
            .text()
            .await
            .map_err(|e| CoreError::ToolExecutionFailed {
                tool_name: "web".to_string(),
                cause: format!("Failed to read Brave search results: {}", e),
                parameters: serde_json::json!({ "query": query }),
            })?;

        // Parse Brave search results with scraper
        let document = scraper::Html::parse_document(&html);

        // Brave uses data attributes for result types
        let result_selector = scraper::Selector::parse("[data-type='web']").unwrap();
        let title_selector = scraper::Selector::parse(".snippet-title, h3, .title").unwrap();
        let url_selector =
            scraper::Selector::parse(".snippet-url cite, cite, .result-url").unwrap();
        let desc_selector =
            scraper::Selector::parse(".snippet-description, .snippet-content").unwrap();

        let mut results = Vec::new();

        for (i, result_elem) in document.select(&result_selector).enumerate() {
            if i >= limit {
                break;
            }

            let title = result_elem
                .select(&title_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let url = result_elem
                .select(&url_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .or_else(|| {
                    // Try to find URL in href attributes
                    result_elem
                        .select(&scraper::Selector::parse("a[href]").unwrap())
                        .next()
                        .and_then(|a| a.value().attr("href"))
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();

            let snippet = result_elem
                .select(&desc_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_else(|| {
                    // Fallback: get text content skipping title
                    result_elem
                        .text()
                        .collect::<String>()
                        .lines()
                        .skip(1)
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .trim()
                        .to_string()
                });

            if !url.is_empty() && !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }

        Ok(WebOutput {
            content: None,
            results: Some(results),
            metadata: None,
            next_offset: None,
        })
    }

    /// Preprocess HTML to remove script and style tags for cleaner markdown conversion
    fn preprocess_html(html: &str) -> String {
        // Use regex for reliable removal of script/style content
        let script_regex = regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
        let style_regex = regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
        let comment_regex = regex::Regex::new(r"(?s)<!--.*?-->").unwrap();
        let svg_regex = regex::Regex::new(r"(?is)<svg[^>]*>.*?</svg>").unwrap();
        let noscript_regex = regex::Regex::new(r"(?is)<noscript[^>]*>.*?</noscript>").unwrap();

        // Remove inline event handlers and javascript: URLs
        let onclick_regex = regex::Regex::new(r#"\s*on\w+\s*=\s*["'][^"']*["']"#).unwrap();
        let js_url_regex = regex::Regex::new(r#"href\s*=\s*["']javascript:[^"']*["']"#).unwrap();

        let mut cleaned = script_regex.replace_all(html, "").to_string();
        cleaned = style_regex.replace_all(&cleaned, "").to_string();
        cleaned = comment_regex.replace_all(&cleaned, "").to_string();
        cleaned = svg_regex.replace_all(&cleaned, "").to_string();
        cleaned = noscript_regex.replace_all(&cleaned, "").to_string();
        cleaned = onclick_regex.replace_all(&cleaned, "").to_string();
        cleaned = js_url_regex.replace_all(&cleaned, "href=\"#\"").to_string();

        // Also remove common ad/tracking elements by id/class patterns
        let ad_regex = regex::Regex::new(r#"(?is)<div[^>]*(?:class|id)=["'][^"']*(?:ad[sv]?|banner|sponsor|promo|widget|sidebar|popup|overlay|modal|cookie|gdpr|newsletter|signup|subscribe)[^"']*["'][^>]*>.*?</div>"#).unwrap();
        cleaned = ad_regex.replace_all(&cleaned, "").to_string();

        cleaned
    }

    /// Fetch content from a URL with pagination support
    async fn fetch_url(
        &self,
        url: String,
        format: WebFormat,
        continue_from: Option<usize>,
    ) -> Result<WebOutput> {
        const CACHE_DURATION_SECS: u64 = 300; // 5 minutes
        const DEFAULT_PAGE_SIZE: usize = 10000; // 10k chars per page

        // Handle blank query with continue_from
        let url = if url.is_empty() && continue_from.is_some() {
            // Get the last fetched URL
            let last_url = self.last_fetch_url.lock().unwrap();
            match &*last_url {
                Some(url) => url.clone(),
                None => {
                    return Err(CoreError::ToolExecutionFailed {
                        tool_name: "web".to_string(),
                        cause: "No previous fetch to continue from".to_string(),
                        parameters: serde_json::json!({ "continue_from": continue_from }),
                    });
                }
            }
        } else {
            // Store this URL as the most recent
            if !url.is_empty() {
                *self.last_fetch_url.lock().unwrap() = Some(url.clone());
            }
            url
        };

        // Check cache first
        let full_content = if let Some(cached) = self.fetch_cache.get(&url) {
            if cached.timestamp.elapsed().as_secs() < CACHE_DURATION_SECS {
                cached.content.clone()
            } else {
                // Cache expired, remove and re-fetch
                drop(cached); // Release read lock before removing
                self.fetch_cache.remove(&url);
                self.fetch_and_cache(&url, format).await?
            }
        } else {
            self.fetch_and_cache(&url, format).await?
        };

        // Handle pagination
        let start = continue_from.unwrap_or(0);
        let page_size = DEFAULT_PAGE_SIZE;
        let total_length = full_content.chars().count();

        // Ensure start is within bounds
        if start >= total_length {
            return Ok(WebOutput {
                content: Some(String::new()),
                results: None,
                metadata: Some(FetchMetadata {
                    url: url.clone(),
                    content_type: "text/html".to_string(),
                    format,
                    total_length,
                    offset: start,
                    has_more: false,
                }),
                next_offset: None,
            });
        }

        // Calculate end position
        let end = (start + page_size).min(total_length);
        let has_more = end < total_length;

        // Extract the page content (handle char boundaries properly)
        let page_content: String = full_content.chars().skip(start).take(end - start).collect();

        Ok(WebOutput {
            content: Some(page_content),
            results: None,
            metadata: Some(FetchMetadata {
                url: url.clone(),
                content_type: "text/html".to_string(),
                format,
                total_length,
                offset: start,
                has_more,
            }),
            next_offset: if has_more { Some(end) } else { None },
        })
    }

    /// Fetch content and store in cache
    async fn fetch_and_cache(&self, url: &str, format: WebFormat) -> Result<String> {
        let parsed_domain = Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(|s| s.to_string()))
            .unwrap_or("".to_string());

        let response = self
            .client
            .client
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0",
            )
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
            )
            .header("Alt-Used", parsed_domain)
            .header("Accept-Language", "en-GB,en;q=0.5")
            .header("Accept-Encoding", "gzip, deflate, zstd")
            .header("Connection", "keep-alive")
            .header("Sec-GPC", "1")
            .header("Upgrade-Insecure-Requests", "1")
            .send()
            .await
            .map_err(|e| CoreError::ToolExecutionFailed {
                tool_name: "web".to_string(),
                cause: format!("Failed to fetch URL: {}", e),
                parameters: serde_json::json!({ "url": url }),
            })?;

        let text = response
            .text()
            .await
            .map_err(|e| CoreError::ToolExecutionFailed {
                tool_name: "web".to_string(),
                cause: format!("Failed to read response body: {}", e),
                parameters: serde_json::json!({ "url": url }),
            })?;

        let content = match format {
            WebFormat::Html => text,
            WebFormat::Markdown => {
                // Preprocess HTML to remove script and style tags for cleaner markdown
                let cleaned_html = Self::preprocess_html(&text);
                html2md::parse_html(&cleaned_html)
            }
        };

        // Store in cache
        self.fetch_cache.insert(
            url.to_string(),
            CachedContent {
                content: content.clone(),
                timestamp: Instant::now(),
            },
        );

        Ok(content)
    }

    /// Search the web using Kagi (if available) or fallback providers
    async fn search_web(&self, query: String, limit: usize) -> Result<WebOutput> {
        let limit = limit.max(1).min(20);

        // Try Kagi first if we have a session cookie
        if std::env::var("KAGI_SESSION").is_ok() {
            match self.search_kagi(&query, limit).await {
                Ok(output)
                    if output
                        .results
                        .as_ref()
                        .map(|r| !r.is_empty())
                        .unwrap_or(false) =>
                {
                    return Ok(output);
                }
                Err(e) => {
                    tracing::warn!("Kagi search failed, falling back: {}", e);
                }
                _ => {
                    tracing::debug!("Kagi returned no results, trying fallback");
                }
            }
        }

        // Try Brave Search as primary fallback
        match self.search_brave(&query, limit).await {
            Ok(output)
                if output
                    .results
                    .as_ref()
                    .map(|r| !r.is_empty())
                    .unwrap_or(false) =>
            {
                return Ok(output);
            }
            Err(e) => {
                tracing::warn!("Brave search failed, trying DuckDuckGo: {}", e);
            }
            _ => {
                tracing::debug!("Brave returned no results, trying DuckDuckGo");
            }
        }

        // Use DuckDuckGo HTML interface
        let search_url = "https://html.duckduckgo.com/html/";
        let response = self
            .client
            .client
            .get(search_url)
            .query(&[("q", &query)])
            .send()
            .await
            .map_err(|e| CoreError::ToolExecutionFailed {
                tool_name: "web".to_string(),
                cause: format!("Search request failed: {}", e),
                parameters: serde_json::json!({ "query": &query }),
            })?;

        let html = response
            .text()
            .await
            .map_err(|e| CoreError::ToolExecutionFailed {
                tool_name: "web".to_string(),
                cause: format!("Failed to read search results: {}", e),
                parameters: serde_json::json!({ "query": &query }),
            })?;

        // Parse search results using scraper
        let document = scraper::Html::parse_document(&html);
        let result_selector = scraper::Selector::parse(".result").unwrap();
        let title_selector = scraper::Selector::parse(".result__title a").unwrap();
        let snippet_selector = scraper::Selector::parse(".result__snippet").unwrap();

        let mut results = Vec::new();

        for (i, result) in document.select(&result_selector).enumerate() {
            if i >= limit {
                break;
            }

            // Extract title and URL
            let title_elem = result.select(&title_selector).next();
            let (title, url) = if let Some(elem) = title_elem {
                let title = elem.text().collect::<String>().trim().to_string();
                let url = elem.value().attr("href").unwrap_or("").to_string();
                (title, url)
            } else {
                continue;
            };

            // Extract snippet
            let snippet = result
                .select(&snippet_selector)
                .next()
                .map(|elem| elem.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            // Skip if no URL
            if url.is_empty() {
                continue;
            }

            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        Ok(WebOutput {
            content: None,
            results: Some(results),
            metadata: None,
            next_offset: None,
        })
    }
}

#[async_trait]
impl AiTool for WebTool {
    type Input = WebInput;
    type Output = WebOutput;

    fn name(&self) -> &str {
        "web"
    }

    fn description(&self) -> &str {
        r#"Interact with the web. Operations: 'fetch' to get content from a URL, 'search' to search the web using DuckDuckGo.

When using 'fetch' you can select format "html" or "md" (default: "md")
- Returns 10k characters at a time to avoid overwhelming context
- Check metadata.has_more and use continue_from with next_offset to read more
- Shortcut: Leave query blank with continue_from to continue previous URL

The "md" format converts HTML to readable markdown, which is usually better for understanding content.
The "html" format returns raw HTML, useful when you need to see exact formatting or extract specific elements

Important search operators:
- cats dogs: results about cats or dogs
- "cats and dogs": exact term (avoid unless necessary)
- ~"cats and dogs": semantically similar terms
- cats -dogs: reduce results about dogs
- cats +dogs: increase results about dogs
- cats filetype:pdf: search pdfs about cats (supports doc(x), xls(x), ppt(x), html)
- dogs site:example.com: search dogs on example.com
- cats -site:example.com: exclude example.com from results
- intitle:dogs: title contains "dogs"
- inurl:cats: URL contains "cats"

Use this whenever you need current information, facts, news, or anything beyond your training data."#
    }

    async fn execute(
        &self,
        params: Self::Input,
        _meta: &crate::tool::ExecutionMeta,
    ) -> Result<Self::Output> {
        match params.operation {
            WebOperation::Fetch => {
                let format = params.format.unwrap_or_default();
                self.fetch_url(params.query, format, params.continue_from)
                    .await
            }
            WebOperation::Search => {
                let limit = params.limit.unwrap_or(10).max(1).min(20);
                self.search_web(params.query, limit).await
            }
        }
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some(
            "Use 'fetch' to retrieve content from specific URLs. \
             Use 'search' to find information on the web. \
             Always check if information is available in memory before searching the web.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_input_serialization() {
        let fetch = WebInput {
            operation: WebOperation::Fetch,
            query: "https://example.com".to_string(),
            format: Some(WebFormat::Markdown),
            limit: None,
            continue_from: None,
        };
        let json = serde_json::to_string(&fetch).unwrap();
        assert!(json.contains("\"operation\":\"fetch\""));
        assert!(json.contains("\"query\":\"https://example.com\""));

        let search = WebInput {
            operation: WebOperation::Search,
            query: "rust programming".to_string(),
            format: None,
            limit: Some(5),
            continue_from: None,
        };
        let json = serde_json::to_string(&search).unwrap();
        assert!(json.contains("\"operation\":\"search\""));
        assert!(json.contains("\"query\":\"rust programming\""));
    }
}
