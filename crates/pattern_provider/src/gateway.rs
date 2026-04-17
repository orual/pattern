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
use futures::stream::TryStreamExt;
use genai::adapter::AdapterKind;
use genai::chat::ChatRequest;
use genai::resolver::{AuthData, Endpoint};
use genai::{Headers, ModelIden, ServiceTarget};
use pattern_core::error::ProviderError;
use pattern_core::traits::provider_client::{ChunkStream, ProviderClient};
use pattern_core::types::provider::{CompletionRequest, TokenCount};
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
        // per-tier auth headers. For RequestOverride this fully replaces
        // what genai would have built from `AuthData::Key`.
        let mut outbound_headers = ident_headers;
        outbound_headers.extend(auth_headers_for_tier(&resolved, adapter));

        let target = service_target(adapter, &model, outbound_headers);

        let limiter =
            self.limiters
                .get(&provider)
                .ok_or_else(|| ProviderError::NoAuthAvailable {
                    provider: provider.clone(),
                })?;
        limiter.acquire_completion().await;

        // Exec + retry on transient failures (429, transient network
        // errors). Streaming errors mid-stream propagate to the caller.
        let stream_resp =
            exec_chat_stream_with_retry(&self.genai, target, chat, options, RetryPolicy::default())
                .await?;

        // Map genai's stream events + errors into pattern's Result<ChatStreamEvent, ProviderError>.
        let mapped = stream_resp.stream.map_err(map_genai_error);

        Ok(Box::pin(mapped))
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

    pub fn build(self) -> Result<PatternGatewayClient, ProviderError> {
        if self.chains.is_empty() {
            return Err(ProviderError::ShaperMisconfigured {
                reason: "gateway needs at least one provider registered".into(),
            });
        }
        Ok(PatternGatewayClient {
            genai: self.genai.unwrap_or_else(genai::Client::default),
            chains: self.chains,
            shapers: self.shapers,
            limiters: self.limiters,
            token_counters: self.token_counters,
            session_uuid: self
                .session_uuid
                .unwrap_or_else(|| Arc::new(SessionUuidRotator::new())),
            default_persona: self.default_persona.unwrap_or_default(),
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

async fn exec_chat_stream_with_retry(
    client: &genai::Client,
    target: ServiceTarget,
    chat: ChatRequest,
    options: genai::chat::ChatOptions,
    policy: RetryPolicy,
) -> Result<genai::chat::ChatStreamResponse, ProviderError> {
    let mut attempt: u32 = 0;
    loop {
        let target_clone = target.clone();
        let chat_clone = chat.clone();
        let result = client
            .exec_chat_stream(target_clone, chat_clone, Some(&options))
            .await;

        match result {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                attempt += 1;
                if !is_retryable(&e) || attempt >= policy.max_attempts {
                    return Err(map_genai_error(e));
                }
                // Exponential backoff with jitter.
                let delay = exponential_backoff(attempt, policy.base_delay, policy.max_delay);
                tracing::warn!(
                    attempt,
                    max = policy.max_attempts,
                    wait_ms = delay.as_millis(),
                    error = %e,
                    "transient gateway error; retrying"
                );
                tokio::time::sleep(delay).await;
            }
        }
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
/// Retry on: 429 (explicit rate-limit), network-transport hiccups.
/// Do NOT retry on: 4xx other than 429 (auth / payload), 5xx until we have
/// explicit classification (a hard 500 loop is bad; adjust if needed in a
/// follow-up).
fn is_retryable(err: &genai::Error) -> bool {
    use genai::Error as E;
    match err {
        E::WebModelCall { webc_error, .. } => is_webc_retryable(webc_error),
        E::WebStream { .. } => true, // stream-layer transport issues can be transient
        _ => false,
    }
}

fn is_webc_retryable(err: &genai::webc::Error) -> bool {
    let msg = err.to_string();
    // Best-effort substring check. genai's webc::Error doesn't currently
    // expose HTTP status in a structured way we can rely on; when it does,
    // switch to typed matching.
    msg.contains("429")
        || msg.contains("rate")
        || msg.contains("timeout")
        || msg.contains("connect")
}

// ---- genai error mapping ----

fn map_genai_error(err: genai::Error) -> ProviderError {
    use genai::Error as E;
    match err {
        E::WebModelCall { webc_error, .. } => {
            let msg = webc_error.to_string();
            if msg.contains("429") || msg.contains("rate") {
                // TODO: parse Retry-After and anthropic-ratelimit-unified-5h-reset
                // headers when the webc error shape exposes them. For now the
                // retry_after is a placeholder best-guess.
                ProviderError::RateLimited {
                    retry_after: Duration::from_secs(60),
                }
            } else {
                ProviderError::RequestFailed {
                    status: 0,
                    body: Some(msg),
                }
            }
        }
        E::HttpError {
            status,
            canonical_reason: _,
            body,
        } => {
            if status.as_u16() == 429 {
                ProviderError::RateLimited {
                    retry_after: Duration::from_secs(60),
                }
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
        E::WebStream { cause, .. } => ProviderError::RequestFailed {
            status: 0,
            body: Some(format!("stream transport error: {cause}")),
        },
        other => ProviderError::RequestFailed {
            status: 0,
            body: Some(other.to_string()),
        },
    }
}

// ---- Per-tier auth header composition ----

fn auth_headers_for_tier(
    resolved: &ResolvedCredential,
    adapter: AdapterKind,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    // Common: every Anthropic request needs anthropic-version.
    if matches!(adapter, AdapterKind::Anthropic) {
        headers.push(("anthropic-version".into(), "2023-06-01".into()));
    }

    let token = resolved.token.access_token.expose_secret().to_string();
    match resolved.source {
        AuthTier::ApiKey => match adapter {
            AdapterKind::Anthropic => {
                headers.push(("x-api-key".into(), token));
            }
            AdapterKind::Gemini => {
                headers.push(("x-goog-api-key".into(), token));
            }
            _ => {
                headers.push(("Authorization".into(), format!("Bearer {token}")));
            }
        },
        #[cfg(feature = "subscription-oauth")]
        AuthTier::SessionPickup | AuthTier::Pkce => {
            headers.push(("Authorization".into(), format!("Bearer {token}")));
            if matches!(adapter, AdapterKind::Anthropic) {
                headers.push(("anthropic-beta".into(), "oauth-2025-04-20".into()));
            }
        }
    }

    headers
}

// ---- ServiceTarget construction ----

fn service_target(
    adapter: AdapterKind,
    model: &str,
    headers: Vec<(String, String)>,
) -> ServiceTarget {
    let url = chat_url_for(adapter, model).to_string();
    ServiceTarget {
        model: ModelIden::new(adapter, model.to_string()),
        // Endpoint is irrelevant under RequestOverride but must be non-empty.
        endpoint: Endpoint::from_static("https://pattern-gateway-override.invalid"),
        auth: AuthData::RequestOverride {
            url,
            headers: Headers::from(headers),
        },
    }
}

/// Hardcoded chat URLs per adapter. Pattern's gateway uses
/// [`AuthData::RequestOverride`] which fully replaces the URL genai would
/// have computed, so we need to know the canonical endpoint ourselves.
/// Add entries as we extend provider coverage.
fn chat_url_for(adapter: AdapterKind, model: &str) -> String {
    match adapter {
        AdapterKind::Anthropic => "https://api.anthropic.com/v1/messages".to_string(),
        // Gemini's endpoint embeds the model name and the service verb; for
        // now pattern only uses RequestOverride for Anthropic, but return a
        // best-guess here for Phase 4 so other adapters at least get a
        // plausible url if they slip through the provider dispatch.
        AdapterKind::Gemini => format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent"
        ),
        _ => format!("https://pattern-gateway-unsupported-adapter-{adapter:?}.invalid"),
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
    use crate::auth::ApiKeyTier;
    use crate::ratelimit::ProviderRateLimiter;
    use crate::shaper::{HonestPatternShaper, ShaperCompatMode, ShaperConfig};
    use futures::StreamExt;
    use jiff::Timestamp;
    use pattern_core::types::provider::{
        ChatMessage, ChatOptions, ChatStreamEvent, ProviderCredential,
    };
    use secrecy::SecretString;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn min_shaper_config() -> ShaperConfig {
        ShaperConfig {
            x_app: "pattern".into(),
            compat_mode: ShaperCompatMode::HonestPattern,
            target_is_first_party: false,
            enable_interleaved_thinking: false,
            enable_dev_full_thinking: false,
            enable_context_management: false,
            enable_extended_cache_ttl: false,
            enable_1m_context: false,
        }
    }

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
        let names: Vec<&str> = hdrs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"x-api-key"));
        assert!(names.contains(&"anthropic-version"));
        assert!(!names.contains(&"Authorization"));
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn auth_headers_oauth_anthropic() {
        let resolved = ResolvedCredential {
            source: AuthTier::Pkce,
            token: api_key_auth_token(),
        };
        let hdrs = auth_headers_for_tier(&resolved, AdapterKind::Anthropic);
        let names: Vec<&str> = hdrs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"Authorization"));
        assert!(names.contains(&"anthropic-beta"));
        assert!(names.contains(&"anthropic-version"));
        assert!(!names.contains(&"x-api-key"));
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
        let names: Vec<&str> = hdrs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"x-goog-api-key"));
        assert!(!names.contains(&"anthropic-version"));
    }

    struct TestApiKeyChain {
        tier: ApiKeyTier,
    }

    #[async_trait]
    impl CredentialChain for TestApiKeyChain {
        fn provider(&self) -> &str {
            "anthropic"
        }

        async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
            let token = self.tier.resolve().ok_or(ProviderError::NoAuthAvailable {
                provider: "anthropic".into(),
            })?;
            Ok(ResolvedCredential {
                source: AuthTier::ApiKey,
                token,
            })
        }
    }

    /// End-to-end streaming round trip via wiremock.
    ///
    /// `#[ignore]` for now: the gateway currently hardcodes Anthropic's
    /// canonical URL inside [`chat_url_for`], so the wiremock server at a
    /// random port can't actually service the request. Task 19 adds a
    /// per-provider URL-override knob on the builder and un-ignores this.
    /// Kept in-tree as a documentation artifact of the shape we want to
    /// verify end-to-end.
    #[ignore]
    #[tokio::test]
    async fn complete_end_to_end_streams_anthropic_response() {
        let server = MockServer::start().await;

        let sse_body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-opus-4-7\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(header("x-api-key", "sk-ant-test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let chain: Arc<dyn CredentialChain> = Arc::new(TestApiKeyChain {
            tier: ApiKeyTier::anthropic(),
        });
        let shaper: Arc<dyn RequestShaper> =
            Arc::new(HonestPatternShaper::new(min_shaper_config()).unwrap());
        let limiter = Arc::new(ProviderRateLimiter::anthropic_default());

        // Set the API-key env so ApiKeyTier resolves.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        }

        let gateway = PatternGatewayClient::builder()
            .with_provider("anthropic", chain, shaper, limiter)
            .build()
            .expect("gateway builds");

        let req = CompletionRequest::new("claude-opus-4-7")
            .append_message(ChatMessage::user("hi"))
            .with_options(ChatOptions::default());

        let mut stream = gateway.complete(req).await.expect("complete opens stream");

        // Drain events; content-delta events should surface.
        let mut saw_chunk = false;
        let mut saw_end = false;
        while let Some(evt) = stream.next().await {
            match evt {
                Ok(ChatStreamEvent::Chunk(_)) => saw_chunk = true,
                Ok(ChatStreamEvent::End(_)) => saw_end = true,
                _ => {}
            }
        }

        assert!(saw_chunk, "should receive at least one content chunk");
        assert!(saw_end, "should receive end event");

        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }
}
