//! [`PatternGatewayClient`] — the `pattern_core::traits::ProviderClient` impl.
//!
//! One gateway instance dispatches per-call on the request's model string:
//!
//! - **credential resolution**: the matching [`CredentialChain`] (by
//!   `AdapterKind → provider name`) produces a [`ResolvedCredential`].
//! - **request shaping**: the matching [`RequestShaper`] mutates the
//!   [`ChatRequest`] (setting `system_blocks` etc.) and returns
//!   identification headers.
//! - **rate limiting**: the matching [`ProviderRateLimiter`] gates the
//!   call (per-provider buckets; independent across providers).
//! - **token counting**: the matching [`TokenCounter`], when present,
//!   services `count_tokens` via its own bucket.
//! - **session UUID**: cross-provider; rotates on caller signal.
//!
//! # 429 / subscription-tier handling
//!
//! On HTTP 429 or RateLimited errors the gateway retries with exponential
//! backoff + jitter, capped at a small number of attempts. The server-side
//! `Retry-After` header, when surfaced through genai's error shape, caps
//! the backoff waiting period.
//!
//! **Known gap** (tracked in Task 18 followup): Anthropic's subscription
//! tier sends a long-window reset header (e.g.
//! `anthropic-ratelimit-unified-5h-reset`) when the 5-hour subscription
//! budget is exhausted. The gateway currently treats these as normal 429s
//! with backoff; parsing the reset header and surfacing "wait until T"
//! semantics requires access to response headers that genai doesn't
//! currently expose through its error type. Follow-up task: parse via a
//! genai middleware or an internal reqwest call alongside the stream.
//!
//! # Streaming
//!
//! `complete` returns [`ChunkStream`] — genai's event stream mapped 1:1
//! to `Result<ChatStreamEvent, ProviderError>`. Pattern does not buffer
//! the stream.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::ChatRequest;
use genai::resolver::{AuthData, Endpoint};
use genai::{Headers, ModelIden, ServiceTarget};
use pattern_core::error::ProviderError;
use pattern_core::traits::provider_client::{ChunkStream, ProviderClient};
use pattern_core::types::provider::{ChatStreamEvent, CompletionRequest, TokenCount};
use secrecy::ExposeSecret;

use crate::auth::{AuthTier, CredentialChain, ResolvedCredential};
use crate::ratelimit::ProviderRateLimiter;
use crate::session_uuid::SessionUuidRotator;
use crate::shaper::{RequestShaper, ShapeContext};
use crate::token_count::{CountTokensRequest, TokenCounter};

/// Gateway construction. Composed via [`PatternGatewayClientBuilder`].
///
/// `Debug` is implemented manually to log only the provider-name set — the
/// internal `dyn CredentialChain` / `dyn RequestShaper` trait objects don't
/// implement `Debug` and shouldn't leak into log output regardless.
pub struct PatternGatewayClient {
    /// Shared genai::Client. We dispatch around it rather than relying on
    /// its internal resolvers — each call builds a `ServiceTarget` that
    /// includes our pre-composed `AuthData::RequestOverride`.
    genai: genai::Client,

    /// Per-provider credential resolution chains, keyed by provider name
    /// (e.g. `"anthropic"`, `"gemini"`).
    chains: HashMap<String, Arc<dyn CredentialChain>>,

    /// Per-provider request shapers.
    shapers: HashMap<String, Arc<dyn RequestShaper>>,

    /// Per-provider rate limiters.
    limiters: HashMap<String, Arc<ProviderRateLimiter>>,

    /// Optional per-provider token counter. Present for providers whose
    /// `count_tokens` endpoint pattern knows how to call (currently just
    /// Anthropic).
    token_counters: HashMap<String, Arc<TokenCounter>>,

    /// Shared session UUID rotator. Cross-provider.
    session_uuid: Arc<SessionUuidRotator>,

    /// Default persona rendered into the shaper's slot-[2] block. Callers
    /// that want per-request persona override can extend `CompletionRequest`
    /// with a metadata field in a follow-up; for now the gateway carries a
    /// single persona per instance.
    default_persona: String,

    /// Per-provider base-URL overrides, keyed by provider name. When present,
    /// replaces the hardcoded canonical URL in [`chat_url_for`]. Primarily
    /// used by integration tests to point at wiremock servers, but also
    /// surface for self-hosted proxies and corporate routing.
    ///
    /// The override replaces the *base* URL (scheme+host+port); the
    /// adapter-specific path suffix (`/v1/messages` for Anthropic,
    /// `/v1beta/models/{model}:streamGenerateContent` for Gemini) still
    /// appends.
    base_url_overrides: HashMap<String, String>,
}

impl std::fmt::Debug for PatternGatewayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Trait objects inside (CredentialChain, RequestShaper) don't
        // implement Debug — surface only the identifying metadata.
        f.debug_struct("PatternGatewayClient")
            .field("providers", &self.chains.keys().collect::<Vec<_>>())
            .field(
                "token_counters",
                &self.token_counters.keys().collect::<Vec<_>>(),
            )
            .field("default_persona_len", &self.default_persona.len())
            .finish_non_exhaustive()
    }
}

impl PatternGatewayClient {
    /// Start composing a gateway. See [`PatternGatewayClientBuilder`].
    pub fn builder() -> PatternGatewayClientBuilder {
        PatternGatewayClientBuilder::default()
    }

    /// Introspection: which providers are wired into this gateway.
    pub fn provider_names(&self) -> Vec<&str> {
        self.chains.keys().map(String::as_str).collect()
    }

    fn provider_for_model(&self, model: &str) -> Result<(String, AdapterKind), ProviderError> {
        let adapter = AdapterKind::from_model(model).map_err(|e| ProviderError::RequestFailed {
            status: 0,
            body: Some(format!("unknown model '{model}': {e}")),
        })?;
        let name = adapter_kind_to_provider_name(adapter).to_string();
        Ok((name, adapter))
    }

    fn shape_context<'a>(
        &'a self,
        session: &'a crate::session_uuid::PatternSessionUuid,
        model: &'a str,
        auth_tier: AuthTier,
    ) -> ShapeContext<'a> {
        ShapeContext {
            session_uuid: session,
            model,
            auth_tier,
            persona: &self.default_persona,
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        }
    }
}

#[async_trait]
impl ProviderClient for PatternGatewayClient {
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream, ProviderError> {
        let CompletionRequest {
            model,
            chat,
            options,
        } = request;
        let mut chat = chat;
        let (provider, adapter) = self.provider_for_model(&model)?;

        let chain = self
            .chains
            .get(&provider)
            .ok_or_else(|| ProviderError::NoAuthAvailable {
                provider: provider.clone(),
            })?;
        let resolved = chain.resolve().await?;

        let shaper = self
            .shapers
            .get(&provider)
            .ok_or_else(|| ProviderError::NoAuthAvailable {
                provider: provider.clone(),
            })?;
        let session = self.session_uuid.current();
        let ctx = self.shape_context(&session, &model, resolved.source);
        let ident_headers = shaper.shape(&mut chat, &ctx)?;

        // Compose the full outbound header set: shaper identification +
        // per-tier auth headers. These two sets deliberately do NOT overlap
        // after the fix in commit 1 of the Phase 4 code review: the shaper
        // owns `anthropic-beta` (single source of truth, including the OAuth
        // marker), and `auth_headers_for_tier` owns `authorization` /
        // `x-api-key` / `anthropic-version`. BTreeMap::extend is still
        // last-insert-wins, but a collision here would now be a bug.
        let mut outbound_headers = ident_headers;
        outbound_headers.extend(auth_headers_for_tier(&resolved, adapter));

        let target = service_target(
            adapter,
            &model,
            outbound_headers,
            self.base_url_overrides.get(&provider).map(String::as_str),
        );

        let limiter =
            self.limiters
                .get(&provider)
                .ok_or_else(|| ProviderError::NoAuthAvailable {
                    provider: provider.clone(),
                })?;
        limiter.acquire_completion().await;

        // Open the stream with transparent retry on pre-stream failures
        // AND on first-event tunneled HTTP errors (429 / 5xx). Once the
        // first successful event arrives, subsequent errors flow through
        // to the caller — retrying after content emission would duplicate
        // output.
        open_stream_with_retry(&self.genai, target, chat, options, RetryPolicy::default()).await
    }

    async fn count_tokens(&self, request: &CompletionRequest) -> Result<TokenCount, ProviderError> {
        let (provider, _adapter) = self.provider_for_model(&request.model)?;

        let counter =
            self.token_counters
                .get(&provider)
                .ok_or_else(|| ProviderError::TokenCountFailed {
                    reason: format!(
                        "no token counter configured for provider '{provider}' — \
                     pattern currently supports count_tokens for Anthropic only"
                    ),
                })?;

        let chain = self
            .chains
            .get(&provider)
            .ok_or_else(|| ProviderError::NoAuthAvailable {
                provider: provider.clone(),
            })?;
        let resolved = chain.resolve().await?;

        let shaper = self
            .shapers
            .get(&provider)
            .ok_or_else(|| ProviderError::NoAuthAvailable {
                provider: provider.clone(),
            })?;
        let session = self.session_uuid.current();
        let ctx = self.shape_context(&session, &request.model, resolved.source);

        let ct_req = CountTokensRequest {
            model: request.model.clone(),
            system: request.chat.system.clone(),
            system_blocks: request.chat.system_blocks.clone(),
            messages: request.chat.messages.clone(),
            tools: request.chat.tools.clone(),
        };

        let details = counter
            .count(&resolved, shaper.as_ref(), &ctx, &ct_req)
            .await?;
        Ok(details.into())
    }
}

// ---- Builder ----

/// Fluent builder for [`PatternGatewayClient`].
#[derive(Default)]
pub struct PatternGatewayClientBuilder {
    chains: HashMap<String, Arc<dyn CredentialChain>>,
    shapers: HashMap<String, Arc<dyn RequestShaper>>,
    limiters: HashMap<String, Arc<ProviderRateLimiter>>,
    token_counters: HashMap<String, Arc<TokenCounter>>,
    session_uuid: Option<Arc<SessionUuidRotator>>,
    default_persona: Option<String>,
    genai: Option<genai::Client>,
    base_url_overrides: HashMap<String, String>,
}

impl PatternGatewayClientBuilder {
    /// Register a provider's full pipeline — credential chain + shaper +
    /// rate limiter. Must be called at least once per provider the
    /// gateway should serve.
    pub fn with_provider(
        mut self,
        name: impl Into<String>,
        chain: Arc<dyn CredentialChain>,
        shaper: Arc<dyn RequestShaper>,
        limiter: Arc<ProviderRateLimiter>,
    ) -> Self {
        let name = name.into();
        self.chains.insert(name.clone(), chain);
        self.shapers.insert(name.clone(), shaper);
        self.limiters.insert(name, limiter);
        self
    }

    /// Attach a token counter for a provider. Typically only Anthropic
    /// gets one in Phase 4 (it's the only provider with a
    /// `/v1/messages/count_tokens` endpoint we've wired).
    pub fn with_token_counter(
        mut self,
        name: impl Into<String>,
        counter: Arc<TokenCounter>,
    ) -> Self {
        self.token_counters.insert(name.into(), counter);
        self
    }

    /// Override the default session-UUID rotator. Useful for tests that
    /// want deterministic UUIDs.
    pub fn with_session_uuid(mut self, rotator: Arc<SessionUuidRotator>) -> Self {
        self.session_uuid = Some(rotator);
        self
    }

    /// Set the default persona rendered into the shaper's persona slot.
    pub fn with_persona(mut self, persona: impl Into<String>) -> Self {
        self.default_persona = Some(persona.into());
        self
    }

    /// Override the underlying `genai::Client` (e.g. to inject a
    /// custom-configured `reqwest::Client`).
    pub fn with_genai_client(mut self, client: genai::Client) -> Self {
        self.genai = Some(client);
        self
    }

    /// Override the base URL for a provider. Primarily used by integration
    /// tests to point the gateway at a wiremock server; also supports
    /// self-hosted proxies and corporate routing.
    ///
    /// `base_url` should be scheme+host+port only, no trailing slash. The
    /// adapter-specific path (`/v1/messages`, etc.) still appends.
    pub fn with_provider_base_url(
        mut self,
        name: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        self.base_url_overrides.insert(name.into(), base_url.into());
        self
    }

    pub fn build(self) -> Result<PatternGatewayClient, ProviderError> {
        if self.chains.is_empty() {
            return Err(ProviderError::ShaperMisconfigured {
                reason: "gateway needs at least one provider registered".into(),
            });
        }
        Ok(PatternGatewayClient {
            genai: self.genai.unwrap_or_default(),
            chains: self.chains,
            shapers: self.shapers,
            limiters: self.limiters,
            token_counters: self.token_counters,
            session_uuid: self
                .session_uuid
                .unwrap_or_else(|| Arc::new(SessionUuidRotator::new())),
            default_persona: self.default_persona.unwrap_or_default(),
            base_url_overrides: self.base_url_overrides,
        })
    }
}

// ---- Retry policy ----

/// Retry policy for transient outbound failures (429, network flakes).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// Open a chat stream, retrying pre-stream and first-event failures with
/// exponential backoff.
///
/// Two retry points:
///
/// 1. **Pre-stream**: `exec_chat_stream` returns `Err` (auth resolution,
///    bucket acquire, reqwest-level transport failure). Retry if
///    [`is_retryable`] classifies the error as transient.
/// 2. **First-event**: genai tunnels HTTP errors (429, 5xx) into the
///    stream as an initial error event. We peek the first event before
///    handing the stream to the caller — if it's retryable and no
///    content has yet been observed, we drop the stream and re-open.
///
/// Once the first event is Ok (i.e. `message_start` has arrived),
/// subsequent failures stream through to the caller verbatim. Retrying
/// after content has been emitted would duplicate output.
async fn open_stream_with_retry(
    client: &genai::Client,
    target: ServiceTarget,
    chat: ChatRequest,
    options: genai::chat::ChatOptions,
    policy: RetryPolicy,
) -> Result<ChunkStream, ProviderError> {
    use futures::stream::StreamExt;

    let mut attempt: u32 = 0;
    loop {
        let open_result = client
            .exec_chat_stream(target.clone(), chat.clone(), Some(&options))
            .await;

        let mut stream = match open_result {
            Ok(resp) => resp.stream,
            Err(e) => {
                attempt += 1;
                if !is_retryable(&e) || attempt >= policy.max_attempts {
                    return Err(map_genai_error(e));
                }
                let delay = exponential_backoff(attempt, policy.base_delay, policy.max_delay);
                tracing::warn!(
                    attempt,
                    max = policy.max_attempts,
                    wait_ms = delay.as_millis(),
                    error = %e,
                    "pre-stream error; retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        // Peek the stream to detect early errors before committing to the
        // caller. genai always emits `ChatStreamEvent::Start` as event 0
        // (from the SSE "Open" pseudo-event), so we peek TWO events:
        //
        //   event 0: Start  → transport open, not yet proof of success
        //   event 1: Chunk/End/ToolCallChunk (success) OR Err (failure)
        //
        // Only after seeing a non-Start Ok event do we commit. If event 1 is
        // a retryable error (429, 5xx, transport), we drop the stream and
        // re-open. This is why we must peek past Start — committing on Start
        // alone would prevent retry for any HTTP-level error (they all start
        // with a transport-open Start before the status propagates).
        let Some(event_0) = stream.next().await else {
            // Empty stream (no events at all). Unusual and not retryable.
            tracing::warn!("genai stream closed with zero events");
            let empty: futures::stream::Empty<Result<ChatStreamEvent, ProviderError>> =
                futures::stream::empty();
            return Ok(Box::pin(empty));
        };

        // If event 0 is not the expected Start, treat it like any other event.
        let is_start = matches!(event_0, Ok(ChatStreamEvent::Start));
        if !is_start {
            match event_0 {
                Ok(evt) => {
                    let head = futures::stream::once(async move { Ok(evt) });
                    let tail = stream.map(|r| r.map_err(map_genai_error));
                    return Ok(Box::pin(head.chain(tail)));
                }
                Err(e) => {
                    attempt += 1;
                    if !is_first_event_retryable(&e) || attempt >= policy.max_attempts {
                        return Err(map_genai_error(e));
                    }
                    let delay = exponential_backoff(attempt, policy.base_delay, policy.max_delay);
                    let server_hint = server_rate_limit_hint(&e);
                    let wait = server_hint
                        .map(|h| h.min(policy.max_delay))
                        .unwrap_or(delay);
                    tracing::warn!(
                        attempt,
                        max = policy.max_attempts,
                        wait_ms = wait.as_millis(),
                        error = %e,
                        "event-0 error; retrying"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
        }

        // event 0 is Start. Peek event 1 to see if the request actually
        // succeeded — errors tunnel through event 1 for HTTP-level failures.
        let start_evt = event_0; // Ok(Start)
        let Some(event_1) = stream.next().await else {
            // Start with no follow-up — unusual, treat as empty stream.
            tracing::warn!("genai stream closed after Start with no content");
            let empty: futures::stream::Empty<Result<ChatStreamEvent, ProviderError>> =
                futures::stream::empty();
            return Ok(Box::pin(empty));
        };

        match event_1 {
            Ok(evt) => {
                // event 1 is Ok → request accepted, content is flowing.
                // Stitch Start + event 1 back at the front, then the tail.
                // All items are mapped through the ProviderError converter so
                // the combined stream has a uniform item type.
                let head = futures::stream::iter([start_evt.map_err(map_genai_error), Ok(evt)]);
                let tail = stream.map(|r| r.map_err(map_genai_error));
                return Ok(Box::pin(head.chain(tail)));
            }
            Err(e) => {
                attempt += 1;
                if !is_first_event_retryable(&e) || attempt >= policy.max_attempts {
                    return Err(map_genai_error(e));
                }
                let delay = exponential_backoff(attempt, policy.base_delay, policy.max_delay);
                // Parse the server-provided rate-limit hint from the error.
                // The hint lives in HttpError's headers when available.
                let server_hint = server_rate_limit_hint(&e);
                let wait = server_hint
                    .map(|h| h.min(policy.max_delay))
                    .unwrap_or(delay);
                tracing::warn!(
                    attempt,
                    max = policy.max_attempts,
                    wait_ms = wait.as_millis(),
                    error = %e,
                    "first-event error after Start; retrying"
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Classify the first-poll stream error: is it worth re-opening?
///
/// genai's tunneled status errors land as `Error::HttpError { status, ... }`.
/// 429 and 5xx are transient; 4xx other than 429 are caller bugs.
fn is_first_event_retryable(err: &genai::Error) -> bool {
    use genai::Error as E;
    match err {
        E::HttpError { status, .. } => status.as_u16() == 429 || status.is_server_error(),
        E::WebStream { .. } => true,
        E::WebModelCall { webc_error, .. } => is_webc_retryable(webc_error),
        _ => false,
    }
}

/// Extract a server-provided rate-limit wait hint from an error, when
/// available. Preference order:
///
/// 1. Response headers via `parse_rate_limit_reset` (honours the
///    Anthropic 5-hour cap reset + RFC 7231 `Retry-After`).
/// 2. JSON body's `error.retry_after_ms` / `error.retry_after` (some
///    providers — Anthropic in particular — include retry hints here
///    instead of, or in addition to, headers).
///
/// Returns `None` when neither source yields a parseable hint; caller
/// falls back to the computed exponential backoff.
fn server_rate_limit_hint(err: &genai::Error) -> Option<Duration> {
    use genai::Error as E;
    match err {
        E::HttpError { headers, body, .. } => {
            if let Some(d) = parse_rate_limit_reset(headers) {
                return Some(d);
            }
            // Body-level hint as secondary source.
            let v: serde_json::Value = serde_json::from_str(body).ok()?;
            let err_obj = v.get("error")?;
            if let Some(ms) = err_obj.get("retry_after_ms").and_then(|x| x.as_u64()) {
                return Some(Duration::from_millis(ms));
            }
            if let Some(s) = err_obj.get("retry_after").and_then(|x| x.as_u64()) {
                return Some(Duration::from_secs(s));
            }
            None
        }
        // `WebStream` wraps the raw BoxError from the transport layer. When
        // the underlying error is a `genai::Error::HttpError` (the common
        // case for 429s surfaced through the SSE stream), try to downcast
        // and extract the rate-limit hint from there.
        E::WebStream { error, .. } => {
            let inner = error.downcast_ref::<E>()?;
            server_rate_limit_hint(inner)
        }
        _ => None,
    }
}

fn exponential_backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    use rand::Rng;
    // 2^(attempt-1) * base, capped at max.
    let factor = 1u64
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u64::MAX);
    let scaled = base.saturating_mul(factor.min(u32::MAX as u64) as u32);
    let capped = scaled.min(max);
    // Add up to 25% jitter on top.
    let jitter_ms = rand::thread_rng().gen_range(0..=(capped.as_millis() / 4).max(1) as u64);
    capped + Duration::from_millis(jitter_ms)
}

/// Is a genai error worth retrying?
///
/// Retry on: 429 (rate-limit), 5xx (transient server), reqwest transport
/// failures (connect/timeout). Do NOT retry on: 4xx other than 429
/// (auth / payload shape), stream-parse errors (our bug or a provider
/// protocol change).
fn is_retryable(err: &genai::Error) -> bool {
    use genai::Error as E;
    match err {
        E::WebModelCall { webc_error, .. } => is_webc_retryable(webc_error),
        E::WebStream { .. } => true, // stream-layer transport hiccups
        _ => false,
    }
}

fn is_webc_retryable(err: &genai::webc::Error) -> bool {
    use genai::webc::Error as W;
    match err {
        W::ResponseFailedStatus { status, .. } => {
            status.as_u16() == 429 || status.is_server_error()
        }
        W::Reqwest(e) => e.is_connect() || e.is_timeout() || e.is_request(),
        _ => false,
    }
}

// ---- genai error mapping ----

fn map_genai_error(err: genai::Error) -> ProviderError {
    use genai::Error as E;
    match err {
        E::WebModelCall { webc_error, .. } => map_webc_error(webc_error),
        E::HttpError {
            status,
            canonical_reason: _,
            body,
            headers,
        } => {
            if status.as_u16() == 429 {
                let retry_after =
                    parse_rate_limit_reset(&headers).unwrap_or_else(|| Duration::from_secs(60));
                ProviderError::RateLimited { retry_after }
            } else {
                ProviderError::RequestFailed {
                    status: status.as_u16(),
                    body: Some(body),
                }
            }
        }
        E::ChatResponseGeneration {
            response_body,
            cause,
            ..
        } => ProviderError::RequestFailed {
            status: 0,
            body: Some(format!(
                "chat response generation failed: {cause}; body: {response_body}"
            )),
        },
        E::ChatResponse { body, .. } => ProviderError::RequestFailed {
            status: 0,
            body: Some(body.to_string()),
        },
        E::StreamParse { serde_error, .. } => ProviderError::RequestFailed {
            status: 0,
            body: Some(format!("stream parse error: {serde_error}")),
        },
        E::WebStream { cause, error, .. } => {
            // `WebStream` wraps the raw BoxError from the SSE transport layer.
            // When the underlying error is a `genai::Error::HttpError` (the
            // common case for 429s and 5xx responses surfaced through the
            // stream), downcast and map it properly so callers see structured
            // `RateLimited` / `RequestFailed` rather than an opaque
            // `RequestFailed { status: 0 }`. `genai::Error` is not `Clone`,
            // so we downcast by reference and match the inner variant directly
            // rather than delegating to `map_genai_error`.
            if let Some(E::HttpError {
                status,
                body,
                headers,
                ..
            }) = error.downcast_ref::<E>()
            {
                return if status.as_u16() == 429 {
                    let retry_after =
                        parse_rate_limit_reset(headers).unwrap_or_else(|| Duration::from_secs(60));
                    ProviderError::RateLimited { retry_after }
                } else {
                    ProviderError::RequestFailed {
                        status: status.as_u16(),
                        body: Some(body.clone()),
                    }
                };
            }
            ProviderError::RequestFailed {
                status: 0,
                body: Some(format!("stream transport error: {cause}")),
            }
        }
        other => ProviderError::RequestFailed {
            status: 0,
            body: Some(other.to_string()),
        },
    }
}

/// Map a `genai::webc::Error` into the gateway's `ProviderError`, including
/// structured extraction of rate-limit reset info from response headers.
fn map_webc_error(err: genai::webc::Error) -> ProviderError {
    use genai::webc::Error as W;
    match err {
        W::ResponseFailedStatus {
            status,
            body,
            headers,
        } => {
            if status.as_u16() == 429 {
                let retry_after =
                    parse_rate_limit_reset(&headers).unwrap_or_else(|| Duration::from_secs(60));
                ProviderError::RateLimited { retry_after }
            } else {
                ProviderError::RequestFailed {
                    status: status.as_u16(),
                    body: Some(body),
                }
            }
        }
        other => ProviderError::RequestFailed {
            status: 0,
            body: Some(other.to_string()),
        },
    }
}

/// Parse a rate-limit reset hint from response headers.
///
/// Preference order:
/// 1. `anthropic-ratelimit-unified-5h-reset` (UNIX epoch seconds;
///    Anthropic subscription-tier 5-hour cap signal — when this is
///    present, the wait is hours, not seconds, and callers want to
///    surface that clearly).
/// 2. `Retry-After` (RFC 7231 — delta-seconds only; we don't parse the
///    HTTP-date variant yet).
///
/// Returns `None` if neither header parses cleanly; caller falls back to
/// a configured default.
fn parse_rate_limit_reset(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    // Anthropic subscription 5-hour cap: absolute UNIX epoch seconds.
    if let Some(v) = headers.get("anthropic-ratelimit-unified-5h-reset")
        && let Ok(s) = v.to_str()
        && let Ok(reset_epoch) = s.parse::<i64>()
    {
        let now = jiff::Timestamp::now().as_second();
        let delta = reset_epoch.saturating_sub(now).max(0) as u64;
        return Some(Duration::from_secs(delta));
    }
    // RFC 7231 Retry-After: delta-seconds variant only.
    if let Some(v) = headers.get(reqwest::header::RETRY_AFTER)
        && let Ok(s) = v.to_str()
        && let Ok(secs) = s.parse::<u64>()
    {
        return Some(Duration::from_secs(secs));
    }
    None
}

// ---- Per-tier auth header composition ----

/// Build the per-tier auth headers. Lowercased keys to match HTTP's
/// case-insensitive semantics — letting the gateway merge with the
/// shaper's identification headers via a plain `BTreeMap::extend` without
/// worrying about `Authorization` vs `authorization` dedup.
fn auth_headers_for_tier(
    resolved: &ResolvedCredential,
    adapter: AdapterKind,
) -> std::collections::BTreeMap<String, String> {
    let mut headers = std::collections::BTreeMap::new();
    // Common: every Anthropic request needs anthropic-version.
    if matches!(adapter, AdapterKind::Anthropic) {
        headers.insert("anthropic-version".into(), "2023-06-01".into());
    }

    let token = resolved.token.access_token.expose_secret().to_string();
    match resolved.source {
        AuthTier::ApiKey => match adapter {
            AdapterKind::Anthropic => {
                headers.insert("x-api-key".into(), token);
            }
            AdapterKind::Gemini => {
                headers.insert("x-goog-api-key".into(), token);
            }
            _ => {
                headers.insert("authorization".into(), format!("Bearer {token}"));
            }
        },
        #[cfg(feature = "subscription-oauth")]
        AuthTier::SessionPickup | AuthTier::Pkce => {
            headers.insert("authorization".into(), format!("Bearer {token}"));
            // NOTE: `anthropic-beta: oauth-2025-04-20` is intentionally NOT
            // inserted here. It lives in `shaper::anthropic::headers::build_beta_header_value`
            // alongside the other beta markers (prompt-caching-scope, etc.).
            // Emitting it here would cause `BTreeMap::extend` in the caller to
            // overwrite the shaper's `anthropic-beta` value (last-insert-wins),
            // silently dropping capability markers on every OAuth-tier call.
            // The shaper is the single source of truth for the full beta value.
        }
    }

    headers
}

// ---- ServiceTarget construction ----

fn service_target(
    adapter: AdapterKind,
    model: &str,
    headers: std::collections::BTreeMap<String, String>,
    base_url_override: Option<&str>,
) -> ServiceTarget {
    let url = chat_url_for(adapter, model, base_url_override);
    // Single conversion to Vec at the genai boundary.
    let headers_vec: Vec<(String, String)> = headers.into_iter().collect();
    ServiceTarget {
        model: ModelIden::new(adapter, model.to_string()),
        // Endpoint is irrelevant under RequestOverride but must be non-empty.
        endpoint: Endpoint::from_static("https://pattern-gateway-override.invalid"),
        auth: AuthData::RequestOverride {
            url,
            headers: Headers::from(headers_vec),
        },
    }
}

/// Build the chat URL for an adapter, honouring any base-URL override.
///
/// Pattern's gateway uses [`AuthData::RequestOverride`] which fully replaces
/// the URL genai would have computed, so we need to know the canonical
/// endpoint ourselves. The per-adapter path suffix is fixed; the base URL
/// (scheme + host + optional port) can be overridden via the gateway
/// builder's `with_provider_base_url` for tests and self-hosted proxies.
fn chat_url_for(adapter: AdapterKind, model: &str, base_url_override: Option<&str>) -> String {
    match adapter {
        AdapterKind::Anthropic => {
            let base = base_url_override.unwrap_or("https://api.anthropic.com");
            format!("{base}/v1/messages")
        }
        AdapterKind::Gemini => {
            // Gemini's endpoint embeds the model name and the service verb.
            let base = base_url_override.unwrap_or("https://generativelanguage.googleapis.com");
            format!("{base}/v1beta/models/{model}:streamGenerateContent")
        }
        _ => {
            // Surface a clearly-invalid URL so mis-routed calls fail loudly
            // rather than silently hitting some other service.
            let base =
                base_url_override.unwrap_or("https://pattern-gateway-unsupported-adapter.invalid");
            format!("{base}/v1/messages")
        }
    }
}

/// Map [`AdapterKind`] to pattern's provider-name convention (used as the
/// key in credential-chain / shaper / rate-limiter maps).
fn adapter_kind_to_provider_name(adapter: AdapterKind) -> &'static str {
    match adapter {
        AdapterKind::Anthropic => "anthropic",
        AdapterKind::Gemini => "gemini",
        AdapterKind::OpenAI | AdapterKind::OpenAIResp => "openai",
        AdapterKind::Groq => "groq",
        AdapterKind::DeepSeek => "deepseek",
        AdapterKind::Cohere => "cohere",
        AdapterKind::Ollama | AdapterKind::OllamaCloud => "ollama",
        AdapterKind::Xai => "xai",
        AdapterKind::Fireworks => "fireworks",
        AdapterKind::Together => "together",
        AdapterKind::Mimo => "mimo",
        AdapterKind::Nebius => "nebius",
        AdapterKind::Zai => "zai",
        AdapterKind::BigModel => "bigmodel",
        AdapterKind::Aliyun => "aliyun",
        AdapterKind::Vertex => "vertex",
        AdapterKind::GithubCopilot => "github_copilot",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use pattern_core::types::provider::ProviderCredential;
    use secrecy::SecretString;

    #[test]
    fn adapter_to_provider_name_covers_known_adapters() {
        assert_eq!(
            adapter_kind_to_provider_name(AdapterKind::Anthropic),
            "anthropic"
        );
        assert_eq!(adapter_kind_to_provider_name(AdapterKind::Gemini), "gemini");
        assert_eq!(adapter_kind_to_provider_name(AdapterKind::OpenAI), "openai");
    }

    #[test]
    fn builder_requires_at_least_one_provider() {
        let result = PatternGatewayClient::builder().build();
        match result {
            Err(ProviderError::ShaperMisconfigured { .. }) => {}
            Err(other) => panic!("expected ShaperMisconfigured, got {other:?}"),
            Ok(_) => panic!("empty gateway should not build"),
        }
    }

    #[test]
    fn exponential_backoff_scales_and_caps() {
        let base = Duration::from_millis(100);
        let max = Duration::from_secs(5);
        for attempt in 1..=10 {
            let d = exponential_backoff(attempt, base, max);
            assert!(d >= base, "attempt {attempt}: {:?} < base {:?}", d, base);
            assert!(
                d <= max + Duration::from_millis((max.as_millis() / 4) as u64 + 1),
                "attempt {attempt}: {:?} > max {:?} + jitter",
                d,
                max
            );
        }
    }

    fn api_key_auth_token() -> ProviderCredential {
        let now = Timestamp::now();
        ProviderCredential {
            provider: "anthropic".into(),
            access_token: SecretString::from("sk-ant-test".to_string()),
            refresh_token: None,
            expires_at: None,
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn auth_headers_api_key_anthropic() {
        let resolved = ResolvedCredential {
            source: AuthTier::ApiKey,
            token: api_key_auth_token(),
        };
        let hdrs = auth_headers_for_tier(&resolved, AdapterKind::Anthropic);
        assert!(hdrs.contains_key("x-api-key"));
        assert!(hdrs.contains_key("anthropic-version"));
        assert!(!hdrs.contains_key("authorization"));
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn auth_headers_oauth_anthropic() {
        let resolved = ResolvedCredential {
            source: AuthTier::Pkce,
            token: api_key_auth_token(),
        };
        let hdrs = auth_headers_for_tier(&resolved, AdapterKind::Anthropic);
        // Keys are lowercased (HTTP case-insensitive + BTreeMap-friendly).
        assert!(hdrs.contains_key("authorization"));
        assert!(hdrs.contains_key("anthropic-version"));
        assert!(!hdrs.contains_key("x-api-key"));
        // `anthropic-beta` is NOT emitted here — it lives in the shaper's
        // `build_beta_header_value` as the single source of truth. Emitting
        // it here would overwrite the shaper's capability markers via
        // BTreeMap::extend (last-insert-wins). See shaper/anthropic/headers.rs.
        assert!(
            !hdrs.contains_key("anthropic-beta"),
            "auth_headers_for_tier must not emit anthropic-beta; \
             the shaper owns that header to prevent silent collision"
        );
    }

    #[test]
    fn auth_headers_api_key_gemini() {
        let mut tok = api_key_auth_token();
        tok.provider = "gemini".into();
        let resolved = ResolvedCredential {
            source: AuthTier::ApiKey,
            token: tok,
        };
        let hdrs = auth_headers_for_tier(&resolved, AdapterKind::Gemini);
        assert!(hdrs.contains_key("x-goog-api-key"));
        assert!(!hdrs.contains_key("anthropic-version"));
    }

    // End-to-end streaming round trip tests live in
    // `crates/pattern_provider/tests/gateway_integration.rs` — they
    // exercise the Anthropic and Gemini paths end-to-end via wiremock
    // and use the per-provider base-URL override to target a test
    // server. The tests that stay here are shape-only unit tests
    // (auth-header composition, adapter name mapping, backoff math).
}
