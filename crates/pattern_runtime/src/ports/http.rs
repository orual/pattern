//! `HttpPort` — runtime-provided HTTP/HTTPS request port.
//!
//! Wraps a `reqwest::Client` and exposes HTTP verbs to agents via the `Port`
//! trait. Configuration (base URL, default headers, timeout) is held in a
//! `Mutex<HttpConfig>` because `Port::call` takes `&self` and `reqwest` does
//! not allow header mutation on a constructed client without rebuilding.
//!
//! # Method dispatch
//!
//! | Method      | Payload fields                                         |
//! |-------------|--------------------------------------------------------|
//! | `configure` | `base_url?`, `default_headers?`, `timeout_secs?`       |
//! | `get`       | `url`, `headers?`, `query?`                            |
//! | `post`      | `url`, `headers?`, `body?`, `query?`                   |
//! | `put`       | `url`, `headers?`, `body?`, `query?`                   |
//! | `delete`    | `url`, `headers?`, `query?`                            |
//! | `head`      | `url`, `headers?`, `query?`                            |
//!
//! All methods return the response as JSON: `{ status: u16, headers: {}, body: String }`.
//!
//! # Content-type allowlist
//!
//! HttpPort only returns text-compatible bodies. Binary responses (images,
//! archives, etc.) produce `PortError::CallFailed` with a message directing
//! the agent to use `Shell.Execute` with `curl` instead. The allowlist is
//! conservative; extend only with concrete need.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use pattern_core::traits::port::Port;
use pattern_core::types::port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};
use serde::{Deserialize, Serialize};

/// Mutable configuration for `HttpPort`. Held behind a `Mutex` so that
/// `call("configure", ...)` can update it without `&mut self`.
///
/// Both `Serialize` and `Deserialize` are derived so tests (and any
/// future tooling that wants to snapshot the config) can round-trip
/// the struct through JSON; the asymmetric Deserialize-only shape
/// from earlier phases prevented round-trip assertions in tests.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct HttpConfig {
    /// Base URL prepended to all request URLs that don't start with a scheme.
    /// Trailing slashes are normalised to exactly one separator at join time.
    #[serde(default)]
    base_url: Option<String>,
    /// Default headers applied to every request. Per-request headers from the
    /// call payload override these.
    #[serde(default)]
    default_headers: BTreeMap<String, String>,
    /// Per-request timeout in seconds. `None` means no explicit timeout
    /// (reqwest's default applies).
    ///
    /// The JSON field is `timeout_secs` (not `timeout`) because `Duration`
    /// doesn't have a standard JSON representation.
    #[serde(default, rename = "timeout_secs")]
    timeout: Option<u64>,
}

/// Payload for HTTP verb calls.
#[derive(Debug, Deserialize)]
struct RequestPayload {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    query: BTreeMap<String, String>,
}

/// Response shape returned by all HTTP verb methods.
#[derive(Debug, Serialize)]
struct ResponsePayload {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

/// Runtime-provided HTTP/HTTPS request port.
///
/// Registered at `TidepoolRuntime` startup by Phase 5 Task 2. Agents that
/// have HTTP in their `CapabilitySet` can call it via `Port.Call("http", ...)`.
#[derive(Debug)]
pub struct HttpPort {
    id: PortId,
    client: reqwest::Client,
    /// Mutable configuration (base URL, headers, timeout). Behind a `Mutex`
    /// because `Port::call` takes `&self`.
    config: Mutex<HttpConfig>,
}

impl Default for HttpPort {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpPort {
    /// Construct a new `HttpPort` with a default `reqwest::Client`.
    ///
    /// The default config builder cannot fail: it only sets compression flags
    /// and uses the default redirect / TLS policy. The `expect` here is a
    /// build-time invariant, not a runtime concern.
    pub fn new() -> Self {
        Self {
            id: PortId::new("http"),
            client: reqwest::Client::builder()
                .gzip(true)
                .brotli(true)
                .build()
                .expect("HTTP client builder cannot fail with default config"),
            config: Mutex::new(HttpConfig::default()),
        }
    }

    /// Execute an HTTP request using the configured client.
    ///
    /// Applies base URL, default headers, per-request headers, query params,
    /// body, and timeout from the current `HttpConfig`.
    async fn do_request(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, PortError> {
        let req: RequestPayload =
            serde_json::from_value(payload).map_err(|e| PortError::BadPayload {
                port: self.id.clone(),
                method: method.to_string(),
                message: e.to_string(),
            })?;

        // Clone config under the lock so we hold it as briefly as possible.
        let cfg = self
            .config
            .lock()
            .expect("HttpConfig mutex poisoned")
            .clone();

        // Resolve final URL: if a base_url is configured and the request URL
        // doesn't start with a scheme, prepend the base.
        let url = match &cfg.base_url {
            Some(base) if !req.url.starts_with("http://") && !req.url.starts_with("https://") => {
                format!(
                    "{}/{}",
                    base.trim_end_matches('/'),
                    req.url.trim_start_matches('/')
                )
            }
            _ => req.url.clone(),
        };

        let verb = match method {
            "get" => reqwest::Method::GET,
            "post" => reqwest::Method::POST,
            "put" => reqwest::Method::PUT,
            "delete" => reqwest::Method::DELETE,
            "head" => reqwest::Method::HEAD,
            // Caller already validated the method — this is unreachable.
            _ => unreachable!("do_request called with unsupported method: {method}"),
        };

        let mut builder = self.client.request(verb, &url);

        // Default headers first; per-request headers override them.
        for (k, v) in &cfg.default_headers {
            builder = builder.header(k, v);
        }
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }

        if !req.query.is_empty() {
            builder = builder.query(&req.query.iter().collect::<Vec<_>>());
        }

        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }

        if let Some(secs) = cfg.timeout {
            builder = builder.timeout(Duration::from_secs(secs));
        }

        let response = builder
            .send()
            .await
            .map_err(|e| PortError::CallFailed(self.id.clone(), e.to_string()))?;

        let status = response.status().as_u16();
        let headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
            .collect();

        // Reject binary Content-Types. Sandboxed agents shouldn't pull arbitrary
        // binaries into the conversation loop. For binary content the agent
        // should use Shell.Execute with curl after appropriate permission.
        let content_type = headers
            .get("content-type")
            .map(|s| s.as_str())
            .unwrap_or("");
        if !is_text_content_type(content_type) {
            return Err(PortError::CallFailed(
                self.id.clone(),
                format!(
                    "non-text response Content-Type: {content_type}. \
                     HttpPort returns text-only bodies; for binary content \
                     use Shell.Execute with curl after appropriate permission."
                ),
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|e| PortError::CallFailed(self.id.clone(), e.to_string()))?;

        let resp = ResponsePayload {
            status,
            headers,
            body,
        };
        serde_json::to_value(&resp)
            .map_err(|e| PortError::CallFailed(self.id.clone(), e.to_string()))
    }
}

#[async_trait]
impl Port for HttpPort {
    fn id(&self) -> &PortId {
        &self.id
    }

    fn metadata(&self) -> PortMetadata {
        PortMetadata::new(
            self.id.clone(),
            "HTTP/HTTPS request port (text responses only)",
        )
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_methods(["configure", "get", "post", "put", "delete", "head"])
    }

    fn capabilities(&self) -> PortCapabilities {
        // Not subscribable — HTTP is request/response, not event-stream.
        PortCapabilities::default().with_callable(true)
    }

    async fn subscribe(
        &self,
        _config: serde_json::Value,
    ) -> Result<BoxStream<'static, PortEvent>, PortError> {
        Err(PortError::NotSubscribable(self.id.clone()))
    }

    async fn call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, PortError> {
        match method {
            "configure" => {
                let cfg: HttpConfig =
                    serde_json::from_value(payload).map_err(|e| PortError::BadPayload {
                        port: self.id.clone(),
                        method: method.to_string(),
                        message: e.to_string(),
                    })?;
                *self.config.lock().expect("HttpConfig mutex poisoned") = cfg;
                Ok(serde_json::json!({}))
            }
            "get" | "post" | "put" | "delete" | "head" => self.do_request(method, payload).await,
            other => Err(PortError::UnsupportedMethod {
                port: self.id.clone(),
                method: other.to_string(),
            }),
        }
    }

    /// Returns the Haskell `Pattern.Http` library module.
    ///
    /// The source lives at `crates/pattern_runtime/haskell/ports/Http.hs`
    /// — outside the SDK include tree. It is not auto-resolved by the
    /// GHC harness; instead, [`TidepoolSession::open_with_agent_loop`]
    /// materializes every registered port's `library()` output into a
    /// per-session temp directory at the path implied by the module
    /// declaration (`module Pattern.Http where` → `Pattern/Http.hs`)
    /// and adds the temp dir to the include path. Plugins that ship
    /// non-SDK port libraries follow the same delivery path.
    fn library(&self) -> Option<smol_str::SmolStr> {
        Some(smol_str::SmolStr::new_static(include_str!(
            "../../haskell/ports/Http.hs"
        )))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Allowlist of Content-Type values that `HttpPort` will return as body text.
///
/// Conservative by design — extend only when there's a concrete need.
/// The empty-string fallback (servers that omit `Content-Type`) is allowed:
/// `reqwest` decodes the bytes as UTF-8 (or latin-1 fallback), and the agent
/// can decide what to do with an untyped response.
fn is_text_content_type(ct: &str) -> bool {
    let main = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    main.starts_with("text/")
        || main == "application/json"
        || main == "application/xml"
        || main == "application/x-www-form-urlencoded"
        || main == "application/javascript"
        || main == "application/x-yaml"
        || main == "application/yaml"
        // Servers that omit Content-Type — allow and let the agent decide.
        || main.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn port() -> HttpPort {
        HttpPort::new()
    }

    /// Metadata must advertise all six method names.
    #[test]
    fn metadata_advertises_methods() {
        let p = port();
        let meta = p.metadata();
        let expected: Vec<&str> = vec!["configure", "get", "post", "put", "delete", "head"];
        for name in expected {
            assert!(
                meta.methods.iter().any(|m| m == name),
                "metadata.methods missing: {name}"
            );
        }
        assert_eq!(
            meta.methods.len(),
            6,
            "unexpected extra methods: {:?}",
            meta.methods
        );
    }

    /// `subscribe` must return `NotSubscribable` — HttpPort is call-only.
    ///
    /// `BoxStream<'static, PortEvent>` does not implement `Debug`, so we
    /// cannot use `expect_err` / `unwrap_err` (both require `T: Debug`).
    /// Use `match` to extract the error without the Debug bound.
    #[tokio::test]
    async fn subscribe_returns_not_subscribable() {
        let p = port();
        match p.subscribe(json!({})).await {
            Ok(_) => panic!("subscribe must fail with NotSubscribable"),
            Err(e) => assert!(
                matches!(e, PortError::NotSubscribable(ref id) if id.as_str() == "http"),
                "expected NotSubscribable(http), got: {e:?}"
            ),
        }
    }

    /// An unrecognised method name must return `UnsupportedMethod`.
    #[tokio::test]
    async fn unknown_method_returns_unsupported() {
        let p = port();
        let err = p
            .call("invalid", json!({}))
            .await
            .expect_err("unknown method must fail");
        assert!(
            matches!(err, PortError::UnsupportedMethod { ref method, .. } if method == "invalid"),
            "expected UnsupportedMethod, got: {err:?}"
        );
    }

    /// `configure` must accept a payload and persist base_url so that a
    /// subsequent `library()` call doesn't break anything — we verify the
    /// state is updated by reading the configured value back via the
    /// internal lock.
    #[tokio::test]
    async fn configure_persists_base_url() {
        let p = port();
        let result = p
            .call("configure", json!({"base_url": "https://example.com"}))
            .await
            .expect("configure must succeed");
        // Returns empty JSON object on success.
        assert_eq!(result, json!({}), "configure must return {{}}");

        // Read back via internal lock.
        let cfg = p.config.lock().unwrap();
        assert_eq!(
            cfg.base_url.as_deref(),
            Some("https://example.com"),
            "base_url not persisted"
        );
    }

    /// `library()` must return the Haskell `Pattern.Http` source.
    #[test]
    fn library_returns_haskell_module() {
        let p = port();
        let src = p.library().expect("library must return Some");
        assert!(
            src.contains("module Pattern.Http"),
            "library source missing module declaration"
        );
        assert!(
            src.contains("httpGet"),
            "library source missing httpGet helper"
        );
    }

    /// Content-type allowlist: text types are accepted.
    #[test]
    fn is_text_content_type_accepts_text_and_json() {
        for ct in &[
            "text/html",
            "text/plain; charset=utf-8",
            "application/json",
            "application/xml",
            "application/javascript",
            "application/x-yaml",
            "application/yaml",
            "application/x-www-form-urlencoded",
            "", // omitted header
        ] {
            assert!(is_text_content_type(ct), "should be accepted: {ct:?}");
        }
    }

    /// Content-type allowlist: binary types are rejected.
    #[test]
    fn is_text_content_type_rejects_binary() {
        for ct in &[
            "image/png",
            "image/jpeg",
            "application/octet-stream",
            "application/zip",
            "application/pdf",
            "audio/mpeg",
            "video/mp4",
        ] {
            assert!(!is_text_content_type(ct), "should be rejected: {ct:?}");
        }
    }

    /// Wiremock integration test: GET /hello → 200 "world".
    #[tokio::test]
    async fn get_against_wiremock_returns_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hello"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/plain")
                    .set_body_string("world"),
            )
            .mount(&server)
            .await;

        let p = port();
        // Configure base_url to the mock server.
        p.call("configure", json!({"base_url": server.uri()}))
            .await
            .expect("configure must succeed");

        let resp = p
            .call("get", json!({"url": "/hello"}))
            .await
            .expect("GET must succeed");

        assert_eq!(resp["status"], 200, "status must be 200");
        assert_eq!(resp["body"], "world", "body must be 'world'");
    }
}
