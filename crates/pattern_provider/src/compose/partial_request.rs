//! [`PartialRequest`] — mutable request being assembled by composer
//! passes.
//!
//! The pipeline takes a `PartialRequest` through a sequence of
//! [`super::pipeline::ComposerPass`] applications; each pass mutates
//! fields and records breakpoint placements. When all passes have run,
//! [`super::pipeline::finalize`] converts the accumulated partial into
//! a [`pattern_core::types::provider::CompletionRequest`] ready for
//! [`pattern_core::traits::provider_client::ProviderClient::complete`].
//!
//! # Field selection
//!
//! `PartialRequest` holds the fields the gateway ultimately needs for a
//! [`genai::chat::ChatRequest`] (`system_blocks`, `messages`, `tools`),
//! composer-specific state (`extra_headers`, `breakpoints`), plus the
//! target `model` + request-level `options`.
//!
//! Fields deliberately NOT carried:
//! - `genai::chat::ChatRequest::system` (legacy single-string form):
//!   the composer always emits per-block `system_blocks` so
//!   `cache_control` can attach per-block. The legacy scalar field
//!   would lose that granularity.
//! - `previous_response_id` / `store` (OpenAI Responses-API fields):
//!   pattern doesn't use them. If a future OpenAI adapter needs them
//!   they can join this struct.

use std::collections::BTreeMap;

use genai::chat::{ChatMessage, ChatOptions, SystemBlock, Tool};

use super::breakpoints::BreakpointTracker;

/// Mutable request being assembled by composer passes. See
/// [module docs][self] for the lifecycle + field-selection rationale.
#[derive(Debug, Clone)]
pub struct PartialRequest {
    /// Target model identifier (e.g. `"claude-opus-4-7"`).
    pub model: String,

    /// System-prompt blocks. The composer always uses this field;
    /// the legacy [`genai::chat::ChatRequest::system`] string field
    /// is left `None` at finalize so `cache_control` markers can be
    /// attached per-block rather than to a scalar.
    pub system_blocks: Vec<SystemBlock>,

    /// Message history, pseudo-messages, and (after Segment3Pass runs)
    /// the fresh user turn. Passes append into this vector in pipeline
    /// order; the composer does not reorder.
    pub messages: Vec<ChatMessage>,

    /// Tool schemas. An empty vec means "no tools" — finalize emits
    /// `ChatRequest::tools = None` in that case rather than an empty
    /// `Some(vec![])` (which some adapters treat as explicit absence
    /// of tools instead of "no tools available").
    pub tools: Vec<Tool>,

    /// Request-level options (max_tokens, temperature, reasoning
    /// effort, etc.) carried through to the final [`pattern_core::types::provider::CompletionRequest`].
    pub options: ChatOptions,

    /// Extra outbound headers to merge with the shaper's + auth-tier's
    /// header set. Keys MUST be lowercase to match the shaper/gateway
    /// convention (see `shaper/anthropic/headers.rs`); the gateway
    /// merges via `BTreeMap::extend` and relies on case-insensitive HTTP
    /// semantics being preserved through the lowercase invariant.
    pub extra_headers: BTreeMap<String, String>,

    /// Cache-breakpoint placements accumulated across passes.
    /// [`super::pipeline::finalize`] walks this tracker to apply
    /// markers to their target blocks (Task 10) and validates count +
    /// beta-header presence.
    pub breakpoints: BreakpointTracker,
}

impl PartialRequest {
    /// Construct an empty `PartialRequest` targeting `model` with
    /// default options. Composer passes populate the rest.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system_blocks: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            options: ChatOptions::default(),
            extra_headers: BTreeMap::new(),
            breakpoints: BreakpointTracker::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_initializes_empty_collections() {
        let p = PartialRequest::new("claude-opus-4-7");
        assert_eq!(p.model, "claude-opus-4-7");
        assert!(p.system_blocks.is_empty());
        assert!(p.messages.is_empty());
        assert!(p.tools.is_empty());
        assert!(p.extra_headers.is_empty());
        assert_eq!(p.breakpoints.count(), 0);
    }

    #[test]
    fn new_accepts_str_and_string() {
        let _ = PartialRequest::new("model-a");
        let _ = PartialRequest::new(String::from("model-b"));
    }
}
