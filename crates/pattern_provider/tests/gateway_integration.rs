//! Gateway integration tests — end-to-end HTTP round trips via wiremock.
//!
//! Covers the variant matrix the gateway should handle:
//!
//! - **text streaming** (Anthropic + Gemini): canned provider-native
//!   responses parse through genai into `ChatStreamEvent::Chunk` + `End`.
//! - **tool-call streaming** (Anthropic): `ChatStreamEvent::ToolCallChunk`
//!   surfaces through the gateway.
//! - **thinking/reasoning streaming** (Gemini): reasoning content surfaces
//!   distinct from plain text.
//! - **OAuth Bearer auth** (Anthropic): subscription-oauth tier produces
//!   `Authorization: Bearer …`; the shaper owns `anthropic-beta` (single
//!   source of truth including the oauth marker), distinct from the API-key
//!   tier's `x-api-key` path.
//! - **429 retry-then-succeed**: first attempt 429 → exponential backoff →
//!   second attempt 200 → stream completes. Proves retry logic is active.
//! - **429 persistent**: all retries return 429 → error surfaces cleanly
//!   with no content chunks leaking through.
//! - **500 error**: surfaces as `ProviderError::RequestFailed` with the
//!   status code preserved.
//! - **missing credential**: `NoAuthAvailable` without hitting the wire.
//! - **provider isolation**: parallel Anthropic + Gemini requests target
//!   the right server with the right auth (AC5.6).
//!
//! Assertions use wiremock's `body_partial_json` for structural checks on
//! outbound bodies (the shaper's system-prompt injection, tool-call
//! payload shape, etc.) and `.expect(1)` on every mock so matcher misses
//! surface as `MockServer` drop panics rather than silent-pass tests.
//!
//! SSE + JSON fixtures live under `tests/data/`:
//! - `anthropic_text_stream.sse` — pattern-authored text-delta variant
//! - `anthropic_tool_stream.sse` — copied verbatim from rust-genai's yakbak
//!   fixture (`tests/data/yakbak/anthropic/tool_stream/response_000.txt`)
//! - `gemini_text_stream.json` — pattern-authored two-chunk text stream
//! - `gemini_thinking_stream.json` — copied verbatim from rust-genai's
//!   yakbak fixture (`tests/data/yakbak/gemini/thinking_stream/response_000.txt`)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream::StreamExt;
use jiff::Timestamp;
use pattern_core::error::ProviderError;
use pattern_core::traits::provider_client::ProviderClient;
use pattern_core::types::provider::{
    ChatMessage, ChatOptions, ChatStreamEvent, CompletionRequest, ProviderCredential,
};
use pattern_provider::auth::{AuthTier, CredentialChain, GeminiAuthChain, ResolvedCredential};
use pattern_provider::gateway::PatternGatewayClient;
use pattern_provider::ratelimit::ProviderRateLimiter;
use pattern_provider::shaper::{
    HonestPatternShaper, NoOpShaper, RequestShaper, ShaperCompatMode, ShaperConfig,
};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, header_exists, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ---- Custom stateful responder for retry tests ----

/// A responder that returns 429 on the first N calls, then 200 + SSE.
/// Uses an AtomicUsize to track calls safely across async boundaries.
struct RetryThenSucceed {
    fail_times: usize,
    calls: Arc<AtomicUsize>,
    success_body: &'static str,
    success_content_type: &'static str,
}

impl RetryThenSucceed {
    fn new(
        fail_times: usize,
        success_body: &'static str,
        success_content_type: &'static str,
    ) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let responder = Self {
            fail_times,
            calls: Arc::clone(&calls),
            success_body,
            success_content_type,
        };
        (responder, calls)
    }
}

impl Respond for RetryThenSucceed {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let call_n = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_n < self.fail_times {
            ResponseTemplate::new(429)
                .set_body_string("rate limit exceeded")
                .insert_header("retry-after", "0")
                // Force connection close so the retry uses a fresh TCP connection
                // rather than potentially reusing a keep-alive connection that's
                // in a post-429 state.
                .insert_header("connection", "close")
        } else {
            ResponseTemplate::new(200).set_body_raw(self.success_body, self.success_content_type)
        }
    }
}

// ---- Fixtures ----

const ANTHROPIC_TEXT_STREAM: &str = include_str!("data/anthropic_text_stream.sse");
const ANTHROPIC_TOOL_STREAM: &str = include_str!("data/anthropic_tool_stream.sse");
const GEMINI_TEXT_STREAM: &str = include_str!("data/gemini_text_stream.json");
const GEMINI_THINKING_STREAM: &str = include_str!("data/gemini_thinking_stream.json");

// ---- Test helpers ----

struct StaticApiKeyChain {
    provider: &'static str,
    token: ProviderCredential,
}

#[async_trait]
impl CredentialChain for StaticApiKeyChain {
    fn provider(&self) -> &str {
        self.provider
    }

    async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
        Ok(ResolvedCredential {
            source: AuthTier::ApiKey,
            token: self.token.clone(),
        })
    }
}

#[cfg(feature = "subscription-oauth")]
struct StaticOAuthChain {
    token: ProviderCredential,
}

#[cfg(feature = "subscription-oauth")]
#[async_trait]
impl CredentialChain for StaticOAuthChain {
    fn provider(&self) -> &str {
        "anthropic"
    }

    async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
        Ok(ResolvedCredential {
            source: AuthTier::StoredOauth,
            token: self.token.clone(),
        })
    }
}

fn token(provider: &str, key: &str) -> ProviderCredential {
    let now = Timestamp::now();
    ProviderCredential {
        provider: provider.into(),
        access_token: SecretString::from(key.to_string()),
        refresh_token: None,
        expires_at: None,
        scope: None,
        session_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn honest_shaper() -> Arc<dyn RequestShaper> {
    Arc::new(
        HonestPatternShaper::new(ShaperConfig {
            x_app: "pattern".into(),
            compat_mode: ShaperCompatMode::HonestPattern,
            target_is_first_party: false,
            enable_interleaved_thinking: false,
            enable_dev_full_thinking: false,
            enable_context_management: false,
            enable_extended_cache_ttl: false,
            enable_1m_context: false,
        })
        .expect("valid shaper config"),
    )
}

/// Drain a stream, collecting counts of each event variant. Capped at 200
/// iterations so a broken test can't hang the suite.
#[derive(Default)]
struct StreamObservation {
    chunk_count: usize,
    reasoning_count: usize,
    tool_call_count: usize,
    end_count: usize,
    error_count: usize,
    concatenated_text: String,
}

async fn drain_stream(
    stream: pattern_core::traits::provider_client::ChunkStream,
) -> StreamObservation {
    let mut stream = stream;
    let mut obs = StreamObservation::default();
    let mut guard = 0;

    while let Some(evt) = stream.next().await {
        guard += 1;
        if guard > 200 {
            break;
        }
        match evt {
            Ok(ChatStreamEvent::Chunk(c)) => {
                obs.chunk_count += 1;
                obs.concatenated_text.push_str(&c.content);
            }
            Ok(ChatStreamEvent::ReasoningChunk(_)) => {
                obs.reasoning_count += 1;
            }
            Ok(ChatStreamEvent::ToolCallChunk(_)) => {
                obs.tool_call_count += 1;
            }
            Ok(ChatStreamEvent::End(_)) => {
                obs.end_count += 1;
            }
            Ok(_) => {}
            Err(_) => obs.error_count += 1,
        }
    }

    obs
}

// ==== Anthropic: text streaming + API-key auth ====

/// Happy path: API-key tier → outbound request has `x-api-key` +
/// `anthropic-version` + `messages` array + `system` field (from the
/// HonestPattern shaper's single-block output). Server returns canned
/// SSE text stream; drain produces Chunk events whose concatenated
/// content spells out the fixture's text.
#[tokio::test]
async fn anthropic_text_stream_api_key() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("x-api-key", "sk-ant-text-test"))
        .and(header_exists("X-App"))
        .and(header_exists("X-Pattern-Session-Id"))
        // Body shape: the user's message must reach Anthropic verbatim.
        .and(body_partial_json(json!({
            "model": "claude-opus-4-7",
            "messages": [
                {"role": "user", "content": "hello world"}
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(ANTHROPIC_TEXT_STREAM, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticApiKeyChain {
        provider: "anthropic",
        token: token("anthropic", "sk-ant-text-test"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req =
        CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("hello world"));
    let stream = gateway.complete(req).await.expect("complete opens");

    let obs = drain_stream(stream).await;
    assert!(
        obs.chunk_count >= 3,
        "expected ≥3 Chunks, got {}",
        obs.chunk_count
    );
    assert_eq!(
        obs.concatenated_text, "Hello there!",
        "concatenated chunks must match fixture text exactly"
    );
    assert_eq!(
        obs.end_count, 1,
        "stream must terminate with exactly one End"
    );
    assert_eq!(obs.error_count, 0, "no stream-parse errors expected");
    // MockServer.drop verifies .expect(1) matched exactly once.
}

/// Anthropic tool-call streaming: using rust-genai's own yakbak fixture
/// verbatim. Drain should produce ToolCallChunk events (one per
/// input_json_delta) and terminate on `stop_reason: tool_use`.
#[tokio::test]
async fn anthropic_tool_stream_surfaces_tool_call_chunks() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-tool-test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(ANTHROPIC_TOOL_STREAM, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticApiKeyChain {
        provider: "anthropic",
        token: token("anthropic", "sk-ant-tool-test"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("claude-haiku-4-5-20251001")
        .append_message(ChatMessage::user("what's the weather in Paris?"));
    let stream = gateway.complete(req).await.expect("complete opens");

    let obs = drain_stream(stream).await;
    assert!(
        obs.tool_call_count >= 1,
        "tool_stream fixture must surface ≥1 ToolCallChunk, got {}",
        obs.tool_call_count
    );
    assert_eq!(obs.end_count, 1);
    assert_eq!(obs.error_count, 0);
}

// ==== Anthropic: OAuth Bearer auth ====

/// subscription-oauth tier → `Authorization: Bearer` NOT `x-api-key`.
/// The shaper owns the `Anthropic-Beta` header (single source of truth),
/// so we verify Bearer auth works; the header-composition test below
/// covers the beta-value shape.
#[cfg(feature = "subscription-oauth")]
#[tokio::test]
async fn anthropic_oauth_bearer_auth_round_trip() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("Authorization", "Bearer oauth-test-access-token"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(ANTHROPIC_TEXT_STREAM, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticOAuthChain {
        token: token("anthropic", "oauth-test-access-token"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("hi"));
    let stream = gateway.complete(req).await.expect("complete opens");
    let obs = drain_stream(stream).await;

    assert_eq!(obs.concatenated_text, "Hello there!");
    assert_eq!(obs.end_count, 1);
}

/// Regression test: OAuth + first-party target must include BOTH
/// `oauth-2025-04-20` AND `prompt-caching-scope-2026-01-05` in the
/// same `Anthropic-Beta` header value. Before the Phase 4 code-review
/// fix, `auth_headers_for_tier` emitted `oauth-2025-04-20` as a separate
/// header insertion that overwrote the shaper's capability markers via
/// `BTreeMap::extend` (last-insert-wins), silently dropping
/// `prompt-caching-scope-2026-01-05` on every subscription-tier call.
#[cfg(feature = "subscription-oauth")]
#[tokio::test]
async fn anthropic_oauth_first_party_beta_header_contains_both_markers() {
    let server = MockServer::start().await;

    // Mount a permissive mock — we'll extract the header from wiremock's
    // received requests after the fact.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("Authorization", "Bearer oauth-first-party-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(ANTHROPIC_TEXT_STREAM, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Build a shaper with `target_is_first_party: true` — this is the
    // production default for subscription-tier calls and is what caused the
    // silent drop before the fix.
    let first_party_shaper: Arc<dyn RequestShaper> = Arc::new(
        HonestPatternShaper::new(ShaperConfig {
            x_app: "pattern".into(),
            compat_mode: ShaperCompatMode::HonestPattern,
            target_is_first_party: true, // ← enables prompt-caching-scope
            enable_interleaved_thinking: false,
            enable_dev_full_thinking: false,
            enable_context_management: false,
            enable_extended_cache_ttl: false,
            enable_1m_context: false,
        })
        .expect("valid shaper config"),
    );

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticOAuthChain {
        token: token("anthropic", "oauth-first-party-token"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            first_party_shaper,
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("hi"));
    let stream = gateway.complete(req).await.expect("complete opens");
    let obs = drain_stream(stream).await;
    assert_eq!(obs.end_count, 1, "stream must complete");

    // Inspect what wiremock received. The received_requests() API returns
    // all matched requests, letting us inspect the actual outbound headers.
    let requests = server
        .received_requests()
        .await
        .expect("requests available");
    assert_eq!(requests.len(), 1, "exactly one request must have been sent");

    let beta_header = requests[0]
        .headers
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let beta = beta_header.expect("anthropic-beta header must be present on OAuth+1P call");
    assert!(
        beta.contains("oauth-2025-04-20"),
        "anthropic-beta must contain oauth-2025-04-20; got: {beta:?}"
    );
    assert!(
        beta.contains("prompt-caching-scope-2026-01-05"),
        "anthropic-beta must contain prompt-caching-scope-2026-01-05 \
         (was silently dropped before the header-collision fix); got: {beta:?}"
    );
}

// ==== 429 error surfacing and retry ====

/// Persistent 429: a 429 that fires on every attempt exhausts the retry
/// budget and surfaces as a stream error with NO content chunks. This
/// validates the error-surfacing contract for the exhausted-retry path.
///
/// NOTE: The gateway's `open_stream_with_retry` already implements
/// first-event 429 retry (exponential backoff, up to `RetryPolicy::max_attempts`).
/// This test verifies what happens when ALL retries fail — the error
/// surfaces cleanly. The `anthropic_429_retries_then_succeeds` test
/// verifies the retry-then-succeed path.
///
/// The mock is mounted without `.expect(N)` because the retry loop fires
/// several times; we assert on the outcome, not the hit count.
#[tokio::test]
async fn anthropic_429_surfaces_as_stream_error_without_content() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-429-test"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_string("rate limit exceeded")
                .insert_header("retry-after", "0"),
        )
        // No .expect(N) — retry fires multiple times; we care about the
        // final outcome, not the hit count.
        .mount(&server)
        .await;

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticApiKeyChain {
        provider: "anthropic",
        token: token("anthropic", "sk-ant-429-test"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("hi"));

    // A persistent 429 surfaces as either:
    // - Err from complete() when the retry budget is exhausted upfront, OR
    // - a stream error on the first event (genai tunnels non-2xx as stream errors).
    // What MUST NOT happen: content chunks arriving as if the request succeeded.
    match gateway.complete(req).await {
        Err(ProviderError::RateLimited { .. }) => {
            // Retries exhausted, error returned upfront. Correct.
        }
        Err(other) => panic!("persistent 429 must surface as RateLimited, got {other:?}"),
        Ok(stream) => {
            let obs = drain_stream(stream).await;
            assert_eq!(obs.chunk_count, 0, "429 must not produce content chunks");
            assert_eq!(obs.tool_call_count, 0);
            assert!(obs.error_count > 0, "429 must surface as a stream error");
            assert_eq!(obs.end_count, 0, "429 must not emit End");
        }
    }
}

/// 429 on the first attempt, 200 with a valid SSE stream on the second.
/// Proves that `open_stream_with_retry` actually retries and the stream
/// completes successfully — the gateway's retry budget isn't decorative.
///
/// Uses `RetryThenSucceed`, a custom stateful responder that returns 429 on
/// the first call and 200 + SSE on subsequent calls. `retry-after: 0` keeps
/// the test fast.
///
/// Retry verification: the shared call counter is asserted to be 2 after the
/// stream completes — proving the retry fired and hit the server twice.
#[tokio::test]
async fn anthropic_429_retries_then_succeeds() {
    let server = MockServer::start().await;

    let (responder, call_counter) =
        RetryThenSucceed::new(1, ANTHROPIC_TEXT_STREAM, "text/event-stream");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-retry-test"))
        .respond_with(responder)
        // No .expect() — we verify hit count via the call counter.
        .mount(&server)
        .await;

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticApiKeyChain {
        provider: "anthropic",
        token: token("anthropic", "sk-ant-retry-test"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("hi"));
    let stream = gateway
        .complete(req)
        .await
        .expect("complete must succeed after retry");
    let obs = drain_stream(stream).await;

    // Verify retry fired: the responder was called exactly twice (429 + retry).
    // This is the key assertion proving the retry loop actually runs.
    let calls = call_counter.load(Ordering::SeqCst);
    assert_eq!(
        calls, 2,
        "gateway must retry once: expected 2 calls (429 + retry), got {calls}"
    );

    // The retry succeeded — we get content, not errors.
    assert_eq!(
        obs.concatenated_text, "Hello there!",
        "retry path must produce correct content (chunk_count={}, error_count={}, end_count={})",
        obs.chunk_count, obs.error_count, obs.end_count
    );
    assert_eq!(
        obs.end_count, 1,
        "stream must terminate cleanly after retry"
    );
    assert_eq!(
        obs.error_count, 0,
        "no stream errors after successful retry"
    );
}

// ==== 500 error path ====

/// A 500 that persists across all retry attempts → gateway exhausts
/// retries and surfaces `ProviderError::RequestFailed`. Stream opening
/// may fail upfront OR mid-stream; either way the ultimate error should
/// propagate.
#[tokio::test]
async fn anthropic_500_propagates_as_request_failed() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
        // Mount without expect() — retry policy may hit this 1 or N times;
        // we assert on the ultimate outcome not the hit count.
        .mount(&server)
        .await;

    let chain: Arc<dyn CredentialChain> = Arc::new(StaticApiKeyChain {
        provider: "anthropic",
        token: token("anthropic", "sk-ant-500-test"),
    });
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider_base_url("anthropic", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("hi"));

    // A 500 must surface as an error — either upfront (before the stream
    // opens) or mid-stream. What MUST NOT happen: content chunks getting
    // through as if the request succeeded.
    match gateway.complete(req).await {
        Err(ProviderError::RequestFailed { status, .. }) => {
            assert_eq!(status, 500, "RequestFailed must preserve the HTTP status");
        }
        Err(ProviderError::RateLimited { .. }) => {
            panic!("500 must not be classified as a rate limit");
        }
        Err(other) => panic!("expected RequestFailed with status 500, got {other:?}"),
        Ok(stream) => {
            let obs = drain_stream(stream).await;
            assert_eq!(
                obs.chunk_count, 0,
                "500 must not produce content chunks (got {} chunks, text={:?})",
                obs.chunk_count, obs.concatenated_text
            );
            assert_eq!(
                obs.tool_call_count, 0,
                "500 must not produce tool-call chunks"
            );
            assert!(
                obs.error_count > 0,
                "if the stream opens on a 500, at least one error must propagate \
                 (chunks={}, errors={}, end={})",
                obs.chunk_count,
                obs.error_count,
                obs.end_count
            );
        }
    }
}

// ==== Gemini: text streaming ====

/// Gemini happy path: credential via GeminiAuthChain env lookup → NoOpShaper
/// → `x-goog-api-key` header → gateway dispatches to the Gemini-shaped URL
/// path. Response body parses through genai's Gemini streamer.
#[tokio::test]
async fn gemini_text_stream_api_key() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/v1beta/models/.+:streamGenerateContent"))
        .and(header("x-goog-api-key", "gem-text-test"))
        .and(header_exists("User-Agent"))
        .and(body_partial_json(json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{"text": "hello gemini"}]
                }
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(GEMINI_TEXT_STREAM, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let prior = std::env::var("GEMINI_API_KEY").ok();
    // SAFETY: nextest default isolation = one test per process; env writes are safe.
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "gem-text-test");
    }

    let chain: Arc<dyn CredentialChain> = Arc::new(GeminiAuthChain::new());
    let shaper: Arc<dyn RequestShaper> = Arc::new(NoOpShaper);
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "gemini",
            chain,
            shaper,
            Arc::new(ProviderRateLimiter::gemini_default()),
        )
        .with_provider_base_url("gemini", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("gemini-2.5-flash")
        .append_message(ChatMessage::user("hello gemini"));
    let stream = gateway.complete(req).await.expect("complete opens");
    let obs = drain_stream(stream).await;

    // Restore env BEFORE assertions so a panic doesn't leak env state.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("GEMINI_API_KEY", v),
            None => std::env::remove_var("GEMINI_API_KEY"),
        }
    }

    assert!(
        obs.chunk_count >= 1,
        "gemini text stream should surface ≥1 Chunk, got chunk_count={} (errors={})",
        obs.chunk_count,
        obs.error_count
    );
    assert!(
        obs.concatenated_text.contains("Hello"),
        "concatenated text should contain 'Hello'; got {:?}",
        obs.concatenated_text
    );
    assert_eq!(obs.end_count, 1);
}

/// Gemini thinking stream: the yakbak `thinking_stream` fixture produces
/// `ReasoningChunk` events for `thought: true` parts plus `Chunk` events
/// for final answer parts. Verifies the gateway passes both through.
#[tokio::test]
async fn gemini_thinking_stream_surfaces_reasoning_and_text() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/v1beta/models/.+:streamGenerateContent"))
        .and(header("x-goog-api-key", "gem-thinking-test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(GEMINI_THINKING_STREAM, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let prior = std::env::var("GEMINI_API_KEY").ok();
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "gem-thinking-test");
    }

    let chain: Arc<dyn CredentialChain> = Arc::new(GeminiAuthChain::new());
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "gemini",
            chain,
            Arc::new(NoOpShaper),
            Arc::new(ProviderRateLimiter::gemini_default()),
        )
        .with_provider_base_url("gemini", server.uri())
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("gemini-2.5-flash")
        .append_message(ChatMessage::user("why is the sky blue?"));
    let stream = gateway.complete(req).await.expect("complete opens");
    let obs = drain_stream(stream).await;

    unsafe {
        match prior {
            Some(v) => std::env::set_var("GEMINI_API_KEY", v),
            None => std::env::remove_var("GEMINI_API_KEY"),
        }
    }

    assert!(
        obs.reasoning_count >= 1,
        "thinking fixture should surface ≥1 ReasoningChunk, got {}",
        obs.reasoning_count
    );
    assert!(
        obs.chunk_count >= 1,
        "thinking fixture should surface ≥1 text Chunk (the actual answer), got {}",
        obs.chunk_count
    );
    assert!(
        obs.concatenated_text.to_lowercase().contains("blue"),
        "final answer should reference 'blue'; got {:?}",
        obs.concatenated_text
    );
    assert_eq!(obs.end_count, 1);
}

// ==== Error: no credential ====

/// Without GEMINI_API_KEY or GOOGLE_API_KEY set, the chain returns
/// NoAuthAvailable and the gateway never makes an HTTP call.
#[tokio::test]
async fn gemini_without_credential_surfaces_no_auth_available() {
    let prior_gemini = std::env::var("GEMINI_API_KEY").ok();
    let prior_google = std::env::var("GOOGLE_API_KEY").ok();
    unsafe {
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("GOOGLE_API_KEY");
    }

    let chain: Arc<dyn CredentialChain> = Arc::new(GeminiAuthChain::new());
    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "gemini",
            chain,
            Arc::new(NoOpShaper),
            Arc::new(ProviderRateLimiter::gemini_default()),
        )
        // Point at an unreachable URL — if the gateway tries to make a
        // request anyway the test fails loudly via connection error.
        .with_provider_base_url("gemini", "https://pattern-test-unreachable.invalid")
        .build()
        .expect("gateway builds");

    let req = CompletionRequest::new("gemini-2.5-flash").append_message(ChatMessage::user("hi"));
    let result = gateway.complete(req).await;

    unsafe {
        match prior_gemini {
            Some(v) => std::env::set_var("GEMINI_API_KEY", v),
            None => std::env::remove_var("GEMINI_API_KEY"),
        }
        match prior_google {
            Some(v) => std::env::set_var("GOOGLE_API_KEY", v),
            None => std::env::remove_var("GOOGLE_API_KEY"),
        }
    }

    match result {
        Err(ProviderError::NoAuthAvailable { provider }) => {
            assert_eq!(provider, "gemini");
        }
        Err(other) => panic!("expected NoAuthAvailable{{gemini}}, got {other:?}"),
        Ok(_) => panic!("must not open a stream without credentials"),
    }
}

// ==== Provider isolation (AC5.6) ====

/// Two providers registered on the same gateway resolve independently:
/// Anthropic call hits the Anthropic server with `x-api-key`, Gemini call
/// hits the Gemini server with `x-goog-api-key`. Cross-contamination would
/// manifest as wiremock 404s (no matcher match).
#[tokio::test]
async fn provider_dispatch_routes_per_model() {
    let anth_server = MockServer::start().await;
    let gem_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-iso"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(ANTHROPIC_TEXT_STREAM, "text/event-stream"),
        )
        .expect(1)
        .mount(&anth_server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"/v1beta/models/.+:streamGenerateContent"))
        .and(header("x-goog-api-key", "gem-iso"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(GEMINI_TEXT_STREAM, "application/json"),
        )
        .expect(1)
        .mount(&gem_server)
        .await;

    let prior_gemini = std::env::var("GEMINI_API_KEY").ok();
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "gem-iso");
    }

    let anth_chain: Arc<dyn CredentialChain> = Arc::new(StaticApiKeyChain {
        provider: "anthropic",
        token: token("anthropic", "sk-ant-iso"),
    });
    let gem_chain: Arc<dyn CredentialChain> = Arc::new(GeminiAuthChain::new());

    let gateway = PatternGatewayClient::builder()
        .with_provider(
            "anthropic",
            anth_chain,
            honest_shaper(),
            Arc::new(ProviderRateLimiter::anthropic_default()),
        )
        .with_provider(
            "gemini",
            gem_chain,
            Arc::new(NoOpShaper),
            Arc::new(ProviderRateLimiter::gemini_default()),
        )
        .with_provider_base_url("anthropic", anth_server.uri())
        .with_provider_base_url("gemini", gem_server.uri())
        .build()
        .expect("gateway builds");

    let anth_stream = gateway
        .complete(
            CompletionRequest::new("claude-opus-4-7").append_message(ChatMessage::user("anth")),
        )
        .await
        .expect("anthropic completes");
    let anth_obs = drain_stream(anth_stream).await;

    let gem_stream = gateway
        .complete(
            CompletionRequest::new("gemini-2.5-flash").append_message(ChatMessage::user("gem")),
        )
        .await
        .expect("gemini completes");
    let gem_obs = drain_stream(gem_stream).await;

    unsafe {
        match prior_gemini {
            Some(v) => std::env::set_var("GEMINI_API_KEY", v),
            None => std::env::remove_var("GEMINI_API_KEY"),
        }
    }

    assert_eq!(anth_obs.concatenated_text, "Hello there!");
    assert_eq!(anth_obs.end_count, 1);
    assert!(gem_obs.chunk_count >= 1);
    assert_eq!(gem_obs.end_count, 1);
    // Both mock servers assert `.expect(1)` on drop — if one got 0 or 2
    // hits the test fails.
}

/// Unused helpers: kept here so ChatOptions is still referenced when we
/// wire it into future tests (temperature, reasoning_effort, etc.).
#[allow(dead_code)]
fn _unused_chat_options_sentinel() -> ChatOptions {
    ChatOptions::default()
}
