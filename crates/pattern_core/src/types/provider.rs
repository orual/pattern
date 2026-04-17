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
use serde::{Deserialize, Serialize};

use crate::types::message::Message;

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
/// input-token count is surfaced here; output-token accounting is a
/// post-response concern, not a pre-request one.
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
