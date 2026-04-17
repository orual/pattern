//! Provider-client trait: async LLM completion and token counting.
//!
//! Implemented by `pattern_provider::AnthropicClient` (Phase 4). The trait is
//! intentionally minimal — rate limiting, retries, session-UUID management,
//! and cache-TTL selection are internal concerns of the concrete impl, not
//! surfaced here. Options that vary per request live inside
//! [`CompletionRequest`] as fields or nested params.
//!
//! # Design notes
//!
//! - `complete` returns a streaming chunk stream; callers assemble chunks
//!   into a final response.
//! - `count_tokens` replaces the pre-v3 heuristic token approximation with a
//!   provider-native count, per v3-foundation.AC5b.
//! - There is intentionally **no** `usage()` method. Post-response usage
//!   accounting is captured as part of the response stream itself (Phase 4
//!   detail) rather than through a separate trait method; keeping the trait
//!   narrow avoids locking in a shape before Phase 4 concretises it.

use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::Stream;

use crate::error::ProviderError;
use crate::types::provider::{CompletionChunk, CompletionRequest, TokenCount};

/// Streaming chunk result alias used by [`ProviderClient::complete`].
pub type ChunkStream =
    Pin<Box<dyn Stream<Item = Result<CompletionChunk, ProviderError>> + Send + 'static>>;

/// Minimal trait for a streaming LLM provider.
///
/// # Example
///
/// ```no_run
/// use async_trait::async_trait;
/// use pattern_core::error::ProviderError;
/// use pattern_core::traits::provider_client::{ChunkStream, ProviderClient};
/// use pattern_core::types::provider::{CompletionRequest, TokenCount};
///
/// struct Dummy;
///
/// #[async_trait]
/// impl ProviderClient for Dummy {
///     async fn complete(&self, _r: CompletionRequest) -> Result<ChunkStream, ProviderError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn count_tokens(
///         &self,
///         _r: &CompletionRequest,
///     ) -> Result<TokenCount, ProviderError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
/// ```
#[async_trait]
pub trait ProviderClient: Send + Sync {
    /// Stream completion chunks for a composed request.
    ///
    /// The returned stream emits [`CompletionChunk`]s until a terminal chunk
    /// (`is_final: true`) or an error. Callers typically assemble the chunk
    /// stream into a [`crate::types::provider::CompletionResponse`].
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream, ProviderError>;

    /// Return the provider-reported input token count for a composed request.
    ///
    /// Used pre-request by compaction and context-length decisions; replaces
    /// the pre-v3 heuristic token approximation. See v3-foundation.AC5b.
    async fn count_tokens(&self, request: &CompletionRequest) -> Result<TokenCount, ProviderError>;
}
