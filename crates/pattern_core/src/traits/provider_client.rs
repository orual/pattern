// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Provider-client trait: streaming LLM completion and token counting.
//!
//! Implemented by `pattern_provider::gateway::PatternGatewayClient` (Phase 4).
//! The trait is intentionally minimal — rate limiting, retries, session-UUID
//! management, credential resolution, and cache-TTL selection are internal
//! concerns of the concrete impl, not surfaced here.
//!
//! # Streaming contract
//!
//! `complete` returns a [`Stream`] of [`ChatStreamEvent`]s (re-exported from
//! `genai::chat`). Callers match on the variants — `Chunk`, `ReasoningChunk`,
//! `ToolCallChunk`, `End` — and assemble whatever shape they need. Pattern
//! does not buffer the stream on the way through; the gateway emits genai
//! events verbatim, only mapping errors to [`ProviderError`] so callers
//! deal with a single error type.
//!
//! The final event is always `End(StreamEnd)`, which carries provider-
//! reported [`Usage`]. Phase 5's compaction path consumes that usage
//! directly.

use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::Stream;

use crate::error::ProviderError;
use crate::types::provider::{ChatStreamEvent, CompletionRequest, TokenCount};

// Re-exports for the doctest + callers that want `use pattern_core::traits::provider_client::*;`.
pub use crate::types::provider::{ChatStreamEvent as ProviderEvent, Usage};

/// Streaming event stream produced by [`ProviderClient::complete`].
///
/// Each `Ok` item is a [`ChatStreamEvent`] emitted verbatim from the
/// underlying provider (modulo gateway-side transformations). Errors in the
/// stream map from provider-specific failures into [`ProviderError`].
pub type ChunkStream =
    Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ProviderError>> + Send + 'static>>;

/// Minimal trait for a streaming LLM provider.
///
/// # Example
///
/// ```no_run
/// use async_trait::async_trait;
/// use futures::stream::StreamExt;
/// use pattern_core::error::ProviderError;
/// use pattern_core::traits::provider_client::{ChunkStream, ProviderClient};
/// use pattern_core::types::provider::{
///     ChatMessage, ChatStreamEvent, CompletionRequest, TokenCount,
/// };
///
/// #[derive(Debug)]
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
///
/// async fn example(client: &dyn ProviderClient) -> Result<(), ProviderError> {
///     let req = CompletionRequest::new("claude-opus-4-7")
///         .append_message(ChatMessage::user("hi"));
///     let mut stream = client.complete(req).await?;
///     while let Some(event) = stream.next().await {
///         match event? {
///             ChatStreamEvent::Chunk(c) => eprint!("{}", c.content),
///             ChatStreamEvent::End(_end) => eprintln!("\n[done]"),
///             _ => {}
///         }
///     }
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait ProviderClient: Send + Sync + std::fmt::Debug {
    /// Stream completion events for a composed request.
    ///
    /// The returned stream emits [`ChatStreamEvent`]s until the terminal
    /// `End(StreamEnd)` event or an error. Callers can match on the
    /// variants to surface partial content, tool calls, reasoning, etc.
    ///
    /// Post-response [`Usage`] arrives on the `End` variant's
    /// `StreamEnd.usage` field.
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream, ProviderError>;

    /// Return the provider-reported input token count for a composed request.
    ///
    /// Used pre-request by compaction and context-length decisions; replaces
    /// the pre-v3 heuristic token approximation. See v3-foundation.AC5b.
    async fn count_tokens(&self, request: &CompletionRequest) -> Result<TokenCount, ProviderError>;

    /// Signal a session-UUID rotation boundary to the client.
    ///
    /// Called by the compaction layer when `CompactionOutcome::Fired` —
    /// the compaction cycle end is the primary rotation trigger so the
    /// provider sees a fresh session UUID after each compaction. Persona
    /// detach is a secondary trigger handled at the session close path.
    ///
    /// The default implementation is a no-op: test doubles and providers
    /// that do not carry per-session UUID state can leave this unimplemented.
    /// `PatternGatewayClient` overrides it to forward to its
    /// [`crate::session_uuid::SessionUuidRotator`].
    fn rotate_session_uuid(&self) {
        // No-op by default; concrete clients that carry a session UUID
        // (PatternGatewayClient) override this.
    }
}
