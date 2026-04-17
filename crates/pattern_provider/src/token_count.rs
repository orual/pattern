//! Pre-request token counting via Anthropic's
//! `/v1/messages/count_tokens` endpoint.
//!
//! Separate from the chat-completion path because:
//!
//! - It's a distinct HTTP round trip (the caller chooses when to spend
//!   the cost).
//! - It's metered independently server-side. AC5b.5 requires pattern's
//!   rate limiter to mirror that — count_tokens consumption must not
//!   block chat completions.
//!
//! The rebased rust-genai fork doesn't expose this endpoint directly,
//! so we call it via `reqwest` reusing the same auth + shaper
//! identification headers as chat completions.

use std::sync::Arc;

use pattern_core::error::ProviderError;
use reqwest::StatusCode;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::auth::{AuthTier, ResolvedCredential};
use crate::ratelimit::ProviderRateLimiter;
use crate::shaper::{RequestShaper, ShapeContext};

/// Request payload for Anthropic's `/v1/messages/count_tokens`.
///
/// Uses `genai::chat` types directly (per the phase plan's "no pattern_core
/// mirror layer for config-shaped types" policy). `system_blocks` is the
/// array variant from the fork's Task 3 patch; `system` is the legacy
/// string variant for callers that don't need per-block cache_control.
#[derive(Debug, Clone, Serialize)]
pub struct CountTokensRequest {
    pub model: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_blocks: Option<Vec<genai::chat::SystemBlock>>,

    pub messages: Vec<genai::chat::ChatMessage>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<genai::chat::Tool>>,
}

/// Detailed provider-reported token breakdown. `pattern_core`'s
/// [`pattern_core::types::provider::TokenCount`] is a simpler shape;
/// [`From<TokenCountDetails> for TokenCount`] narrows to the basic
/// input-tokens count for consumers that only need that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCountDetails {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl From<TokenCountDetails> for pattern_core::types::provider::TokenCount {
    fn from(d: TokenCountDetails) -> Self {
        Self {
            input_tokens: d.input_tokens as u32,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenCountResponse {
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

// ---- Post-response Usage capture (Task 17 / AC5b.2) ----

/// Provider-reported token usage captured from a chat-completion response.
///
/// The gateway surfaces this through its `complete()` return shape and
/// through the stream-end event for streaming calls. Callers (especially
/// Phase 5's compaction path) use these counts directly instead of
/// re-running heuristic estimates.
///
/// Conversion is lossy by design — some upstream fields map to several
/// detail buckets in Anthropic's response. Where both kinds of cache
/// accounting are reported we preserve both; where only an aggregate
/// is present it's stored under `cache_read_input_tokens` and the
/// creation bucket stays zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub reasoning_tokens: u64,
}

impl Usage {
    /// Best-effort conversion from upstream genai `Usage`. Negative values
    /// (upstream uses `i32`) and absent fields map to zero rather than
    /// erroring — a missing count is not a protocol failure.
    pub fn from_genai(g: &genai::chat::Usage) -> Self {
        fn u(x: Option<i32>) -> u64 {
            x.filter(|v| *v >= 0).map(|v| v as u64).unwrap_or(0)
        }

        let prompt = u(g.prompt_tokens);
        let completion = u(g.completion_tokens);
        let cache_creation = g
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cache_creation_tokens)
            .filter(|v| *v >= 0)
            .map(|v| v as u64)
            .unwrap_or(0);
        let cache_read = g
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .filter(|v| *v >= 0)
            .map(|v| v as u64)
            .unwrap_or(0);
        let reasoning = g
            .completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .filter(|v| *v >= 0)
            .map(|v| v as u64)
            .unwrap_or(0);

        Self {
            input_tokens: prompt,
            output_tokens: completion,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
            reasoning_tokens: reasoning,
        }
    }
}

impl From<&genai::chat::Usage> for Usage {
    fn from(g: &genai::chat::Usage) -> Self {
        Self::from_genai(g)
    }
}

/// Async wrapper for the `/v1/messages/count_tokens` endpoint.
///
/// Holds its own rate-limiter reference so count_tokens calls go through
/// their dedicated bucket (AC5b.5). One instance per provider, typically
/// held by the gateway.
pub struct TokenCounter {
    http: reqwest::Client,
    /// Base URL of the provider, e.g. `https://api.anthropic.com`. No
    /// trailing slash.
    base_url: String,
    rate_limiter: Arc<ProviderRateLimiter>,
    /// `anthropic-version` header value. Defaults to `"2023-06-01"` per
    /// the fork's pinned constant.
    anthropic_version: String,
}

impl TokenCounter {
    pub fn new(base_url: impl Into<String>, rate_limiter: Arc<ProviderRateLimiter>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            rate_limiter,
            anthropic_version: "2023-06-01".into(),
        }
    }

    /// Anthropic preset — `https://api.anthropic.com` + shared rate limiter.
    pub fn anthropic(rate_limiter: Arc<ProviderRateLimiter>) -> Self {
        Self::new("https://api.anthropic.com", rate_limiter)
    }

    /// Call the count_tokens endpoint.
    ///
    /// Errors (all as [`ProviderError::TokenCountFailed`] per AC5b.4):
    /// - network error
    /// - non-2xx response (status + body included in the reason)
    /// - malformed JSON response
    ///
    /// The caller is responsible for deciding whether to fall back to a
    /// heuristic estimate — pattern never silently falls back.
    pub async fn count(
        &self,
        auth: &ResolvedCredential,
        shaper: &dyn RequestShaper,
        shape_ctx: &ShapeContext<'_>,
        request: &CountTokensRequest,
    ) -> Result<TokenCountDetails, ProviderError> {
        // AC5b.5: acquire from the count_tokens bucket specifically.
        self.rate_limiter.acquire_count_tokens().await;

        let url = format!("{}/v1/messages/count_tokens", self.base_url);

        let mut req_builder = self
            .http
            .post(&url)
            .header("anthropic-version", self.anthropic_version.clone());

        // Identification + beta headers from the shaper.
        for (k, v) in shaper.identification_headers(shape_ctx)? {
            req_builder = req_builder.header(k, v);
        }

        // Auth — `x-api-key` for API-key tier, `Authorization: Bearer`
        // otherwise (session-pickup / PKCE both produce Bearer tokens on
        // Anthropic).
        req_builder = match auth.source {
            AuthTier::ApiKey => req_builder.header(
                "x-api-key",
                auth.token.access_token.expose_secret().to_string(),
            ),
            #[cfg(feature = "subscription-oauth")]
            AuthTier::SessionPickup | AuthTier::Pkce => req_builder
                .header(
                    "Authorization",
                    format!("Bearer {}", auth.token.access_token.expose_secret()),
                )
                .header("anthropic-beta", "oauth-2025-04-20"),
        };

        let response = req_builder.json(request).send().await.map_err(|e| {
            ProviderError::TokenCountFailed {
                reason: format!("HTTP request failed: {e}"),
            }
        })?;

        let status = response.status();
        if status != StatusCode::OK {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::TokenCountFailed {
                reason: format!("provider returned HTTP {status}: {body}"),
            });
        }

        let parsed: TokenCountResponse =
            response
                .json()
                .await
                .map_err(|e| ProviderError::TokenCountFailed {
                    reason: format!("response parse failed: {e}"),
                })?;

        Ok(TokenCountDetails {
            input_tokens: parsed.input_tokens,
            cache_creation_input_tokens: parsed.cache_creation_input_tokens,
            cache_read_input_tokens: parsed.cache_read_input_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use secrecy::SecretString;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::auth::AuthTier;
    use crate::session_uuid::SessionUuidRotator;
    use crate::shaper::{HonestPatternShaper, ShapeContext, ShaperCompatMode, ShaperConfig};
    use pattern_core::types::provider::ProviderCredential;

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

    fn api_key_credential(key: &str) -> ResolvedCredential {
        let now = Timestamp::now();
        ResolvedCredential {
            source: AuthTier::ApiKey,
            token: ProviderCredential {
                provider: "anthropic".into(),
                access_token: SecretString::from(key.to_string()),
                refresh_token: None,
                expires_at: None,
                scope: None,
                session_id: None,
                created_at: now,
                updated_at: now,
            },
        }
    }

    fn sample_count_request() -> CountTokensRequest {
        CountTokensRequest {
            model: "claude-opus-4-7".into(),
            system: None,
            system_blocks: None,
            messages: vec![genai::chat::ChatMessage::user("hello")],
            tools: None,
        }
    }

    #[tokio::test]
    async fn count_ok_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages/count_tokens"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(header("x-api-key", "sk-ant-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "input_tokens": 1234,
                "cache_creation_input_tokens": 10,
                "cache_read_input_tokens": 20
            })))
            .mount(&server)
            .await;

        let limiter = Arc::new(ProviderRateLimiter::anthropic_default());
        let counter = TokenCounter::new(server.uri(), limiter);
        let shaper = HonestPatternShaper::new(min_shaper_config()).unwrap();
        let uuid_rotator = SessionUuidRotator::new();
        let session = uuid_rotator.current();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::ApiKey,
            persona: "",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };

        let auth = api_key_credential("sk-ant-test");
        let details = counter
            .count(&auth, &shaper, &ctx, &sample_count_request())
            .await
            .expect("count ok");

        assert_eq!(details.input_tokens, 1234);
        assert_eq!(details.cache_creation_input_tokens, 10);
        assert_eq!(details.cache_read_input_tokens, 20);
    }

    #[tokio::test]
    async fn non_2xx_surfaces_as_token_count_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages/count_tokens"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let limiter = Arc::new(ProviderRateLimiter::anthropic_default());
        let counter = TokenCounter::new(server.uri(), limiter);
        let shaper = HonestPatternShaper::new(min_shaper_config()).unwrap();
        let uuid_rotator = SessionUuidRotator::new();
        let session = uuid_rotator.current();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::ApiKey,
            persona: "",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };

        let err = counter
            .count(
                &api_key_credential("sk-ant-test"),
                &shaper,
                &ctx,
                &sample_count_request(),
            )
            .await
            .expect_err("500 → error");
        assert!(matches!(
            &err,
            ProviderError::TokenCountFailed { reason } if reason.contains("500")
        ));
    }

    #[tokio::test]
    async fn malformed_response_surfaces_as_token_count_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages/count_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not valid json"))
            .mount(&server)
            .await;

        let limiter = Arc::new(ProviderRateLimiter::anthropic_default());
        let counter = TokenCounter::new(server.uri(), limiter);
        let shaper = HonestPatternShaper::new(min_shaper_config()).unwrap();
        let uuid_rotator = SessionUuidRotator::new();
        let session = uuid_rotator.current();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::ApiKey,
            persona: "",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };

        let err = counter
            .count(
                &api_key_credential("sk-ant-test"),
                &shaper,
                &ctx,
                &sample_count_request(),
            )
            .await
            .expect_err("malformed → error");
        assert!(matches!(
            &err,
            ProviderError::TokenCountFailed { reason } if reason.contains("parse failed")
        ));
    }

    #[test]
    fn token_count_details_narrows_to_pattern_core_token_count() {
        let details = TokenCountDetails {
            input_tokens: 1234,
            cache_creation_input_tokens: 10,
            cache_read_input_tokens: 20,
        };
        let narrowed: pattern_core::types::provider::TokenCount = details.into();
        assert_eq!(narrowed.input_tokens, 1234);
    }

    #[test]
    fn usage_from_genai_captures_all_buckets() {
        let g = genai::chat::Usage {
            prompt_tokens: Some(100),
            prompt_tokens_details: Some(genai::chat::PromptTokensDetails {
                cache_creation_tokens: Some(10),
                cache_creation_details: None,
                cached_tokens: Some(20),
                audio_tokens: None,
            }),
            completion_tokens: Some(50),
            completion_tokens_details: Some(genai::chat::CompletionTokensDetails {
                accepted_prediction_tokens: None,
                rejected_prediction_tokens: None,
                reasoning_tokens: Some(15),
                audio_tokens: None,
            }),
            total_tokens: Some(150),
        };
        let usage = Usage::from_genai(&g);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, 10);
        assert_eq!(usage.cache_read_input_tokens, 20);
        assert_eq!(usage.reasoning_tokens, 15);
    }

    #[test]
    fn usage_from_genai_treats_missing_fields_as_zero() {
        let g = genai::chat::Usage {
            prompt_tokens: None,
            prompt_tokens_details: None,
            completion_tokens: Some(50),
            completion_tokens_details: None,
            total_tokens: None,
        };
        let usage = Usage::from_genai(&g);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.reasoning_tokens, 0);
    }

    #[test]
    fn usage_from_genai_clamps_negatives_to_zero() {
        let g = genai::chat::Usage {
            prompt_tokens: Some(-5),
            prompt_tokens_details: None,
            completion_tokens: Some(50),
            completion_tokens_details: None,
            total_tokens: None,
        };
        let usage = Usage::from_genai(&g);
        assert_eq!(
            usage.input_tokens, 0,
            "negative upstream value must clamp to zero"
        );
    }
}
