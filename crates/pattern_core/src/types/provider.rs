//! Request and response types for the [`crate::traits::ProviderClient`] trait.
//!
//! # Type policy
//!
//! The trait is the boundary at which pattern hands a request to a concrete
//! backend (`pattern_provider`, or any future alternative). Rather than
//! defining a parallel type hierarchy for chat messages, tool calls,
//! streaming events, and sampling options, this module **re-exports
//! `genai::chat` types directly**. Pattern is already tightly integrated
//! with `rust-genai` through the `pattern_provider::gateway` implementation;
//! introducing a translation layer would just add lines of code without
//! giving us any extra flexibility — the gateway consumes genai types on
//! the outbound side regardless.
//!
//! Pattern-specific types live here too:
//!
//! - [`CompletionRequest`] — thin wrapper around `ChatRequest` + `ChatOptions`
//!   that bundles the target model string and leaves room for pattern-side
//!   metadata (persona hints, routing preferences, cache-TTL overrides, etc.)
//!   as they become needed.
//! - [`ProviderCredential`] — the credential shape stored by
//!   `pattern_provider::creds_store` and produced by each auth tier.
//! - [`TokenCount`] — the result of a pre-request `/v1/messages/count_tokens`
//!   call. Complements (does not replace) the post-response `Usage` that
//!   comes back in [`ChatStreamEvent::End`].
//!
//! # Streaming model
//!
//! `ProviderClient::complete` returns a [`futures::Stream`] of
//! [`ChatStreamEvent`]s
//! verbatim from genai, modulo error mapping. Callers match on the event
//! variants (`Chunk` / `ReasoningChunk` / `ToolCallChunk` / `End`) and
//! assemble whatever they need — pattern does not buffer the stream on the
//! way through.

use jiff::Timestamp;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---- Re-exports from genai ----
//
// These are the types `ProviderClient::complete` / `count_tokens` traffic in.
// Pattern does not define parallel types for these — the gateway consumes
// genai types directly.
pub use genai::chat::{
    CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatResponse, ChatRole, ChatStream,
    ChatStreamEvent, ChatStreamResponse, ReasoningEffort, StreamChunk, StreamEnd, SystemBlock,
    Tool, ToolCall, ToolChunk, ToolResponse, Usage,
};

// ---- ToolOutcome / ToolResult (Pattern-side tool-eval bookkeeping) ----

/// Result of executing a single tool call at the agent-loop layer.
///
/// [`ToolResponse`] (re-exported from `genai`) only carries
/// `{ call_id, content }` as a bare string — it cannot distinguish a
/// success payload from an error. Pattern's agent loop needs that
/// distinction (so a failed Haskell eval doesn't look like a valid JSON
/// result to the LLM), so we encode the variant at this layer and
/// flatten to `ToolResponse` when handing off to the wire. When genai
/// gains a native `is_error` field we widen this bridge accordingly.
///
/// # Examples
///
/// ```
/// use pattern_core::types::provider::ToolOutcome;
/// use serde_json::json;
///
/// let ok = ToolOutcome::Success(json!({"value": 42}));
/// let err = ToolOutcome::Error("invalid input".into());
/// assert!(matches!(ok, ToolOutcome::Success(_)));
/// assert!(matches!(err, ToolOutcome::Error(_)));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolOutcome {
    /// Tool ran to completion; payload is the JSON result the agent
    /// will see.
    Success(serde_json::Value),
    /// Tool failed; payload is the human-readable error (sent back
    /// to the LLM as the tool_result content so it can recover).
    Error(String),
}

impl ToolOutcome {
    /// `true` when this is the error variant.
    pub fn is_error(&self) -> bool {
        matches!(self, ToolOutcome::Error(_))
    }

    /// Render the outcome as a plain string suitable for
    /// [`ToolResponse::content`]. Success payloads are JSON-serialised;
    /// errors pass through as-is.
    pub fn to_content_string(&self) -> String {
        match self {
            ToolOutcome::Success(v) => serde_json::to_string(v).unwrap_or_else(|_| v.to_string()),
            ToolOutcome::Error(msg) => msg.clone(),
        }
    }
}

/// A single completed tool call paired with its originating call id.
///
/// Produced by the Phase 5 agent loop after dispatching a [`ToolCall`]
/// to the Haskell eval worker. Converts to [`ToolResponse`] via
/// [`Self::to_tool_response`] when the driver assembles the next wire
/// turn's request.
///
/// # Examples
///
/// ```
/// use pattern_core::types::provider::{ToolOutcome, ToolResult};
/// use serde_json::json;
///
/// let r = ToolResult {
///     call_id: "call_123".into(),
///     outcome: ToolOutcome::Success(json!({"ok": true})),
/// };
/// let wire = r.to_tool_response();
/// assert_eq!(wire.call_id, "call_123");
/// // to_tool_response wraps the JSON-stringified outcome as Value::String.
/// let s = wire.content.as_str().unwrap();
/// assert!(s.contains("\"ok\""));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Identifier of the originating [`ToolCall`]. Must round-trip
    /// through the wire unchanged — Anthropic matches tool_use and
    /// tool_result by this id.
    pub call_id: String,
    /// Outcome of the eval.
    pub outcome: ToolOutcome,
}

impl ToolResult {
    /// Convert to [`ToolResponse`] (the genai/wire type).
    ///
    /// Uses the string-accepting `ToolResponse::new()` constructor —
    /// content is flattened to a JSON-string via
    /// `outcome.to_content_string()`. For tools that later return
    /// structured/multi-block payloads (e.g. text+image), switch to
    /// `ToolResponse::new_content(call_id, serde_json::Value::Array(..))`.
    ///
    /// The `is_error` signal is currently lost at the boundary since
    /// genai doesn't surface it; errors are encoded in the content
    /// string. When genai gains a native `is_error` field we widen
    /// this conversion.
    pub fn to_tool_response(&self) -> ToolResponse {
        ToolResponse::new(self.call_id.clone(), self.outcome.to_content_string())
    }
}

#[cfg(test)]
mod tool_result_tests {
    use super::*;

    #[test]
    fn outcome_is_error_discriminates() {
        assert!(!ToolOutcome::Success(serde_json::Value::Null).is_error());
        assert!(ToolOutcome::Error("boom".into()).is_error());
    }

    #[test]
    fn outcome_to_content_string_serialises_json() {
        let outcome = ToolOutcome::Success(serde_json::json!({"x": 1, "y": [2, 3]}));
        let s = outcome.to_content_string();
        assert!(s.contains("\"x\":1"));
        assert!(s.contains("[2,3]"));
    }

    #[test]
    fn outcome_to_content_string_passes_error_through() {
        let outcome = ToolOutcome::Error("file not found".into());
        assert_eq!(outcome.to_content_string(), "file not found");
    }

    #[test]
    fn tool_result_to_tool_response_preserves_call_id_and_content() {
        let r = ToolResult {
            call_id: "toolu_01ABC".into(),
            outcome: ToolOutcome::Success(serde_json::json!({"result": 42})),
        };
        let wire = r.to_tool_response();
        assert_eq!(wire.call_id, "toolu_01ABC");
        // to_tool_response uses ToolResponse::new() which wraps the
        // JSON-stringified outcome as Value::String. Extract the string
        // and check that the serialized JSON is embedded within it.
        let content_str = wire.content.as_str().expect("expected Value::String");
        assert!(content_str.contains("\"result\":42"));
    }

    #[test]
    fn tool_result_to_tool_response_on_error() {
        let r = ToolResult {
            call_id: "toolu_01XYZ".into(),
            outcome: ToolOutcome::Error("eval timed out".into()),
        };
        let wire = r.to_tool_response();
        assert_eq!(wire.call_id, "toolu_01XYZ");
        // Error outcomes are plain strings; Value::String comparison.
        assert_eq!(
            wire.content,
            serde_json::Value::String("eval timed out".into())
        );
    }

    #[test]
    fn tool_outcome_serde_round_trip() {
        let ok = ToolOutcome::Success(serde_json::json!({"a": 1}));
        let j = serde_json::to_string(&ok).unwrap();
        let back: ToolOutcome = serde_json::from_str(&j).unwrap();
        assert!(matches!(back, ToolOutcome::Success(_)));

        let err = ToolOutcome::Error("oops".into());
        let j = serde_json::to_string(&err).unwrap();
        let back: ToolOutcome = serde_json::from_str(&j).unwrap();
        assert!(matches!(back, ToolOutcome::Error(ref m) if m == "oops"));
    }
}

// ---- Serde helpers (SecretString round-trip) ----

/// Serde helper: write a [`SecretString`] as its plaintext string form.
///
/// Used only by `ProviderCredential`'s at-rest serialization. `SecretString`
/// deliberately declines automatic `Serialize` to prevent accidental leak via
/// `Debug`/`tracing`; the credential store explicitly opts in here because
/// it's the one place the token legitimately crosses the wire (to disk).
fn serialize_secret<S: Serializer>(value: &SecretString, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.expose_secret())
}

fn deserialize_secret<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<SecretString, D::Error> {
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

// ---- CompletionRequest ----

/// A composed request to an LLM provider.
///
/// Thin wrapper around the three things every call needs: the target model,
/// the conversation payload ([`ChatRequest`]), and the sampling / tooling /
/// routing options ([`ChatOptions`]). Pattern-specific metadata (persona
/// hints, priority, cache-TTL overrides, future model-router directives)
/// is reserved for additional fields on this struct rather than piggybacked
/// onto `ChatOptions::extra_headers` or similar side channels.
///
/// Callers typically use the builder methods to shape the request
/// incrementally:
///
/// ```
/// use pattern_core::types::provider::{ChatMessage, ChatOptions, CompletionRequest};
///
/// let req = CompletionRequest::new("claude-opus-4-7")
///     .with_system("You are a helpful assistant.")
///     .append_message(ChatMessage::user("hello"))
///     .with_options(ChatOptions::default().with_temperature(0.7));
///
/// assert_eq!(req.model, "claude-opus-4-7");
/// assert_eq!(req.chat.messages.len(), 1);
/// ```
///
/// Fields are public so callers with unusual needs can reach into `chat` or
/// `options` directly — the builders are convenience, not an encapsulation
/// barrier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Target model identifier in the provider's naming scheme (e.g.
    /// `"claude-opus-4-7"`, `"gemini-2.5-pro"`). Drives adapter inference
    /// inside the gateway.
    pub model: String,

    /// Messages + system prompt + tool definitions. See
    /// [`genai::chat::ChatRequest`].
    pub chat: ChatRequest,

    /// Sampling, tool config, cache-control, extra headers, reasoning
    /// effort, etc. See [`genai::chat::ChatOptions`].
    pub options: ChatOptions,
}

impl CompletionRequest {
    /// Construct a fresh request targeting `model`, with default
    /// [`ChatRequest`] and [`ChatOptions`].
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            chat: ChatRequest::default(),
            options: ChatOptions::default(),
        }
    }

    /// Set or replace the legacy string-form system prompt. For
    /// per-block cache-control, use [`Self::with_system_blocks`].
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.chat = self.chat.with_system(system);
        self
    }

    /// Set or replace the per-block system prompts. Enables the
    /// three-segment cache layout via the fork's `SystemBlock` patch.
    pub fn with_system_blocks(mut self, blocks: Vec<SystemBlock>) -> Self {
        self.chat.system_blocks = Some(blocks);
        self
    }

    /// Replace the message list wholesale.
    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.chat.messages = messages;
        self
    }

    /// Append a single message to the conversation.
    pub fn append_message(mut self, message: impl Into<ChatMessage>) -> Self {
        self.chat = self.chat.append_message(message);
        self
    }

    /// Replace the tool set.
    pub fn with_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Tool>,
    {
        self.chat = self.chat.with_tools(tools);
        self
    }

    /// Replace the options block wholesale.
    pub fn with_options(mut self, options: ChatOptions) -> Self {
        self.options = options;
        self
    }
}

// ---- TokenCount (pre-request sizing) ----

/// Provider-reported input token count for a request.
///
/// Returned by [`crate::traits::ProviderClient::count_tokens`] and used
/// pre-request by compaction and context-length decisions. Only the
/// input-token count is surfaced here; output-token accounting and
/// cache-read accounting are post-response concerns, read from the
/// [`Usage`] carried by [`ChatStreamEvent::End`].
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
    ///
    /// `u64` matches the provider's native type (Anthropic's
    /// `/v1/messages/count_tokens` returns `u64`). An earlier `u32` would
    /// silently truncate on overflow for very large contexts.
    pub input_tokens: u64,
}

// ---- ProviderCredential ----

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
    #[serde(
        serialize_with = "serialize_secret",
        deserialize_with = "deserialize_secret"
    )]
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
