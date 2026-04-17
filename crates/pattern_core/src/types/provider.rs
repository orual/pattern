//! Request and response types for the [`crate::traits::ProviderClient`] trait.
//!
//! These types are opaque-but-serializable shapes that cross the provider
//! boundary. Concrete backends (e.g. `pattern_provider::AnthropicClient`)
//! translate them to and from provider-native formats.
//!
//! Extension points — cache-TTL hints, sampling parameters, tool shaping —
//! belong as fields inside [`CompletionRequest`] rather than as additional
//! trait methods. This keeps the trait surface minimal and stable while the
//! request shape evolves.
//!
//! # Phase 2 shape
//!
//! These types are stubs sufficient to satisfy AC1.3 (dummy-impl
//! satisfiability). Phase 4 (`pattern_provider`) fleshes them out with
//! provider-native field mappings and caching hints.

use jiff::Timestamp;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::message::Message;

/// Serde helper: write a [`SecretString`] as its plaintext string form.
///
/// Used only by `ProviderCredential`'s at-rest serialization. `SecretString`
/// deliberately declines automatic `Serialize` to prevent accidental leak via
/// `Debug`/`tracing`; the credential store explicitly opts in here because
/// it's the one place the token legitimately crosses the wire (to disk).
fn serialize_secret<S: Serializer>(value: &SecretString, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.expose_secret())
}

fn deserialize_secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SecretString, D::Error> {
    let s = String::deserialize(deserializer)?;
    Ok(SecretString::from(s))
}

fn serialize_opt_secret<S: Serializer>(
    value: &Option<SecretString>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(s) => serializer.serialize_some(s.expose_secret()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_opt_secret<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error> {
    let s: Option<String> = Option::deserialize(deserializer)?;
    Ok(s.map(SecretString::from))
}

/// A composed request to an LLM provider.
///
/// Carries the messages to send, the target model identifier, and an opaque
/// parameter bag for provider-specific options. Phase 4 expands this shape
/// with typed fields for common options (temperature, max tokens, tool
/// shaping, etc.).
///
/// # Examples
///
/// ```
/// use pattern_core::types::provider::CompletionRequest;
///
/// let req = CompletionRequest {
///     model: "claude-sonnet-4".to_string(),
///     messages: vec![],
///     params: serde_json::json!({}),
/// };
/// assert_eq!(req.model, "claude-sonnet-4");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Target model identifier in the provider's naming scheme.
    pub model: String,
    /// The conversation so far. Provider impls translate into their native
    /// message/role format.
    pub messages: Vec<Message>,
    /// Provider-specific options (temperature, tools, cache hints, etc.).
    ///
    /// Opaque in Phase 2; Phase 4 replaces this with a typed options struct.
    pub params: serde_json::Value,
}

/// A single chunk of a streamed completion response.
///
/// Providers emit chunks incrementally. Callers assemble them into a final
/// [`CompletionResponse`] when the stream terminates.
///
/// # Examples
///
/// ```
/// use pattern_core::types::provider::CompletionChunk;
///
/// let chunk = CompletionChunk {
///     delta_text: "hello".to_string(),
///     is_final: false,
/// };
/// assert!(!chunk.is_final);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionChunk {
    /// Incremental text produced by the provider for this chunk.
    pub delta_text: String,
    /// Whether this is the terminal chunk of the stream.
    pub is_final: bool,
}

/// A completed provider response, assembled from all streamed chunks.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::provider::CompletionResponse;
///
/// let resp = CompletionResponse {
///     text: "hello world".to_string(),
///     completed_at: Timestamp::now(),
/// };
/// assert!(resp.text.contains("hello"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Final assembled text from the provider.
    pub text: String,
    /// Wall-clock time at which the response terminated.
    pub completed_at: Timestamp,
}

/// Provider-reported input token count for a request.
///
/// Returned by [`crate::traits::ProviderClient::count_tokens`] and used
/// pre-request by compaction and context-length decisions. Only the
/// input-token count is surfaced here; output-token accounting and
/// cache-read accounting are post-response concerns, read from the
/// provider's response `Usage` rather than projected pre-flight.
///
/// # Examples
///
/// ```
/// use pattern_core::types::provider::TokenCount;
///
/// let tc = TokenCount { input_tokens: 1_234 };
/// assert_eq!(tc.input_tokens, 1_234);
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenCount {
    /// Number of input tokens the provider reports for the composed request.
    pub input_tokens: u32,
}

/// A stored credential for a specific provider.
///
/// Used by `pattern_provider::creds_store::CredsStore` implementations and by
/// every auth tier (session-pickup, PKCE, API key) to carry the credential
/// across the provider boundary. The name avoids the "OAuth" qualifier
/// because the same shape also represents API keys and session-pickup
/// credentials — fields like `refresh_token`, `expires_at`, `scope`, and
/// `session_id` are OAuth-flavoured but optional, and remain `None` on
/// non-OAuth credential paths.
///
/// Access and refresh tokens wrap in [`secrecy::SecretString`] so a stray
/// `Debug` or `tracing::info!` cannot accidentally leak them to logs.
///
/// **Absorbed from:** `pattern_auth::providers::oauth::ProviderOAuthToken`
/// (retired in Phase 4; renamed here to reflect the broader role).
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::provider::ProviderCredential;
/// use secrecy::SecretString;
///
/// let now = Timestamp::now();
/// let tok = ProviderCredential {
///     provider: "anthropic".into(),
///     access_token: "at-xxx".to_string().into(),
///     refresh_token: Some("rt-xxx".to_string().into()),
///     expires_at: None,
///     scope: Some("user:inference".into()),
///     session_id: None,
///     created_at: now,
///     updated_at: now,
/// };
/// assert_eq!(tok.provider, "anthropic");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCredential {
    /// Provider name (`"anthropic"`, `"gemini"`, etc.). Keys the per-provider
    /// credential store.
    pub provider: String,

    /// The bearer access token. Wrapped in [`secrecy::SecretString`] so
    /// accidental logging does not leak the value.
    ///
    /// Serialization explicitly exposes the inner string via the module's
    /// `serialize_secret` / `deserialize_secret` helpers — the credential
    /// store is the one place this token legitimately round-trips through
    /// JSON.
    #[serde(serialize_with = "serialize_secret", deserialize_with = "deserialize_secret")]
    pub access_token: SecretString,

    /// Optional refresh token, when the provider issues one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_opt_secret",
        deserialize_with = "deserialize_opt_secret"
    )]
    pub refresh_token: Option<SecretString>,

    /// Wall-clock expiry of the access token, if known.
    pub expires_at: Option<Timestamp>,

    /// OAuth scopes granted. Stored for diagnostic purposes; scope gating
    /// happens at the provider's authorize step, not here.
    pub scope: Option<String>,

    /// Opaque provider-side session identifier, when applicable (e.g.
    /// Anthropic's per-session token metadata).
    pub session_id: Option<String>,

    /// When the token was first stored.
    pub created_at: Timestamp,

    /// When the token was last refreshed / updated.
    pub updated_at: Timestamp,
}

impl ProviderCredential {
    /// `true` when `expires_at` is set and is in the past.
    ///
    /// # Examples
    ///
    /// ```
    /// use jiff::{Timestamp, ToSpan};
    /// use pattern_core::types::provider::ProviderCredential;
    /// use secrecy::SecretString;
    ///
    /// let now = Timestamp::now();
    /// let past = now.checked_sub(1.hour()).unwrap();
    /// let tok = ProviderCredential {
    ///     provider: "anthropic".into(),
    ///     access_token: "at".to_string().into(),
    ///     refresh_token: None,
    ///     expires_at: Some(past),
    ///     scope: None,
    ///     session_id: None,
    ///     created_at: now,
    ///     updated_at: now,
    /// };
    /// assert!(tok.is_expired());
    /// ```
    pub fn is_expired(&self) -> bool {
        matches!(self.expires_at, Some(t) if t <= Timestamp::now())
    }

    /// `true` when the token is within 5 minutes of expiry. Callers use this
    /// to trigger a proactive refresh before the next request.
    pub fn needs_refresh(&self) -> bool {
        use jiff::ToSpan;
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        let threshold = Timestamp::now()
            .checked_add(5.minutes())
            .unwrap_or(Timestamp::now());
        expires_at <= threshold
    }
}
