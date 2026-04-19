//! Test fixtures for `pattern_runtime` and downstream integration tests.
//!
//! Re-exports commonly-needed `tidepool_testing` helpers under paths that
//! work under Rust 2024 edition. The upstream `tidepool-testing` crate is on
//! edition 2021 and defines a `gen` submodule, which is a reserved keyword
//! under 2024 — downstream callers would need `tidepool_testing::r#gen::…` at
//! every call site. This module localises that escape to one place.
//!
//! Remove the `gen` re-export (and update call sites to the upstream paths)
//! when tidepool either renames its `gen` module or moves to edition 2024
//! itself.

use async_trait::async_trait;
use pattern_core::ProviderClient;
use pattern_core::error::ProviderError;
use pattern_core::traits::provider_client::ChunkStream;
use pattern_core::types::provider::{CompletionRequest, TokenCount};

/// Standard Haskell-boxing `DataConTable` with `I#`, `W#`, `D#`, `()`,
/// `Maybe`/`Just`/`Nothing`, `Bool`/`True`/`False`, pair `(,)`, and list
/// `[]`/`:` constructors pre-registered. Use in handler tests rather than
/// hand-building a table per test.
///
/// Gated on `cfg(test)` because `tidepool-testing` is a dev-dependency
/// (not available to library builds); consumers who need this in their
/// own `#[cfg(test)]` scope should depend on `tidepool-testing` directly.
#[cfg(test)]
pub use tidepool_testing::r#gen::standard_datacon_table;

pub mod in_memory_store;
pub use in_memory_store::InMemoryMemoryStore;

/// Minimal `ProviderClient` implementation that panics on any method call.
///
/// Used in tests that construct `TidepoolRuntime` but never invoke the
/// provider. If a test actually needs to call provider methods, use
/// [`MockProviderClient`] instead.
#[derive(Debug)]
pub struct NopProviderClient;

#[async_trait]
impl ProviderClient for NopProviderClient {
    async fn complete(&self, _: CompletionRequest) -> Result<ChunkStream, ProviderError> {
        panic!("NopProviderClient::complete called — test must not invoke the provider");
    }

    async fn count_tokens(&self, _: &CompletionRequest) -> Result<TokenCount, ProviderError> {
        panic!("NopProviderClient::count_tokens called — test must not invoke the provider");
    }
}

// ---- MockProviderClient -------------------------------------------------

use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use genai::chat::{
    ChatStreamEvent, ContentPart, MessageContent, StopReason as GenaiStopReason, StreamChunk,
    StreamEnd, ToolCall, ToolChunk, Usage,
};

/// Scripted [`ProviderClient`] for integration tests.
///
/// Each call to [`ProviderClient::complete`] pops the next script from
/// the queue and replays its events in order. Use
/// [`MockProviderClient::with_turns`] to construct from a list of
/// pre-built turn scripts, or the helper constructors
/// ([`MockProviderClient::text_turn`], [`MockProviderClient::tool_use_turn`])
/// to build scripts that match common patterns.
///
/// # Exhaustion
///
/// Calling `complete` more times than there are scripts panics with a
/// message identifying which call index was unexpected. This is
/// deliberate: an integration test that makes more provider calls
/// than it scripted for is almost certainly wrong (usually indicates
/// the wire-turn loop didn't terminate when expected).
///
/// # Usage example
///
/// ```no_run
/// # use std::sync::Arc;
/// # use pattern_core::ProviderClient;
/// # use pattern_runtime::testing::MockProviderClient;
/// let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![
///     MockProviderClient::text_turn("Hello! How can I help?"),
/// ]));
/// // wire `provider` into SessionContext via `from_persona`.
/// ```
#[derive(Debug, Default)]
pub struct MockProviderClient {
    scripts: StdMutex<VecDeque<Vec<ChatStreamEvent>>>,
    call_count: AtomicUsize,
}

impl MockProviderClient {
    /// Build from a list of turn scripts. Each inner `Vec` is the
    /// stream for one provider call, replayed in order.
    pub fn with_turns(turns: Vec<Vec<ChatStreamEvent>>) -> Self {
        Self {
            scripts: StdMutex::new(turns.into()),
            call_count: AtomicUsize::new(0),
        }
    }

    /// Number of `complete` calls observed so far.
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    /// Build a "just text" turn — one chunk of assistant text, ends
    /// with `stop_reason = Completed("end_turn")`. The orchestrator
    /// will map this to [`pattern_core::types::turn::StopReason::EndTurn`].
    pub fn text_turn(text: &str) -> Vec<ChatStreamEvent> {
        let text_string = text.to_string();
        vec![
            ChatStreamEvent::Start,
            ChatStreamEvent::Chunk(StreamChunk {
                content: text_string.clone(),
            }),
            ChatStreamEvent::End(StreamEnd {
                captured_usage: Some(Usage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    total_tokens: Some(15),
                    prompt_tokens_details: None,
                    completion_tokens_details: None,
                }),
                captured_stop_reason: Some(GenaiStopReason::Completed("end_turn".into())),
                captured_content: Some(MessageContent::from_text(text_string)),
                captured_reasoning_content: None,
                captured_response_id: None,
            }),
        ]
    }

    /// Build a text turn with caller-supplied [`Usage`].
    ///
    /// Useful for integration tests that need to assert on specific cache
    /// token counts in the returned [`TurnOutput::cache_metrics`].
    /// The `usage` is placed verbatim in `StreamEnd.captured_usage`.
    pub fn text_turn_with_usage(text: &str, usage: Usage) -> Vec<ChatStreamEvent> {
        let text_string = text.to_string();
        vec![
            ChatStreamEvent::Start,
            ChatStreamEvent::Chunk(StreamChunk {
                content: text_string.clone(),
            }),
            ChatStreamEvent::End(StreamEnd {
                captured_usage: Some(usage),
                captured_stop_reason: Some(GenaiStopReason::Completed("end_turn".into())),
                captured_content: Some(MessageContent::from_text(text_string)),
                captured_reasoning_content: None,
                captured_response_id: None,
            }),
        ]
    }

    /// Build a "thinking + text" turn — thinking chunks + text chunks,
    /// ends with `stop_reason = Completed("end_turn")`. Useful for
    /// asserting `TurnEvent::Thinking` surfaces on the sink
    /// distinctly from `TurnEvent::Text`.
    pub fn thinking_then_text_turn(thinking: &str, text: &str) -> Vec<ChatStreamEvent> {
        let thinking_string = thinking.to_string();
        let text_string = text.to_string();
        vec![
            ChatStreamEvent::Start,
            ChatStreamEvent::ReasoningChunk(StreamChunk {
                content: thinking_string.clone(),
            }),
            ChatStreamEvent::Chunk(StreamChunk {
                content: text_string.clone(),
            }),
            ChatStreamEvent::End(StreamEnd {
                captured_usage: Some(Usage {
                    prompt_tokens: Some(20),
                    completion_tokens: Some(10),
                    total_tokens: Some(30),
                    prompt_tokens_details: None,
                    completion_tokens_details: None,
                }),
                captured_stop_reason: Some(GenaiStopReason::Completed("end_turn".into())),
                captured_content: Some(MessageContent::from_text(text_string)),
                captured_reasoning_content: Some(thinking_string),
                captured_response_id: None,
            }),
        ]
    }

    /// Build a "tool_use" turn — emits a single `code` tool call with
    /// the given arguments, ends with `stop_reason = ToolCall`. The
    /// orchestrator will dispatch the tool call to its configured
    /// [`crate::agent_loop::EvalDispatcher`].
    pub fn tool_use_turn(
        call_id: impl Into<String>,
        fn_name: impl Into<String>,
        args: serde_json::Value,
    ) -> Vec<ChatStreamEvent> {
        let tool_call = ToolCall {
            call_id: call_id.into(),
            fn_name: fn_name.into(),
            fn_arguments: args,
            thought_signatures: None,
            thought_signatures_provenance: None,
        };
        vec![
            ChatStreamEvent::Start,
            ChatStreamEvent::ToolCallChunk(ToolChunk {
                tool_call: tool_call.clone(),
            }),
            ChatStreamEvent::End(StreamEnd {
                captured_usage: Some(Usage {
                    prompt_tokens: Some(50),
                    completion_tokens: Some(15),
                    total_tokens: Some(65),
                    prompt_tokens_details: None,
                    completion_tokens_details: None,
                }),
                captured_stop_reason: Some(GenaiStopReason::ToolCall("tool_use".into())),
                captured_content: Some(MessageContent::from_parts(vec![ContentPart::ToolCall(
                    tool_call,
                )])),
                captured_reasoning_content: None,
                captured_response_id: None,
            }),
        ]
    }
}

#[async_trait]
impl ProviderClient for MockProviderClient {
    async fn complete(&self, _req: CompletionRequest) -> Result<ChunkStream, ProviderError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let script = self
            .scripts
            .lock()
            .expect("MockProviderClient scripts mutex poisoned")
            .pop_front();
        let script = script.unwrap_or_else(|| {
            panic!(
                "MockProviderClient exhausted: call #{} has no scripted response \
                 (add more turns to MockProviderClient::with_turns)",
                idx
            )
        });
        let stream = futures::stream::iter(script.into_iter().map(Ok));
        Ok(Box::pin(stream))
    }

    async fn count_tokens(&self, _req: &CompletionRequest) -> Result<TokenCount, ProviderError> {
        // Arbitrary stub — tests that need precise token counts should
        // override via a custom impl rather than MockProviderClient.
        Ok(TokenCount { input_tokens: 0 })
    }
}

#[cfg(test)]
mod mock_tests {
    use super::*;
    use futures::StreamExt;

    async fn count_events(events: Vec<ChatStreamEvent>) -> usize {
        let provider = MockProviderClient::with_turns(vec![events]);
        let req = CompletionRequest::new("claude-sonnet-4-20250514");
        let mut stream = provider.complete(req).await.unwrap();
        let mut count = 0;
        while stream.next().await.is_some() {
            count += 1;
        }
        count
    }

    #[tokio::test]
    async fn text_turn_produces_start_chunk_end() {
        let events = MockProviderClient::text_turn("hello");
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], ChatStreamEvent::Start));
        assert!(matches!(events[1], ChatStreamEvent::Chunk(_)));
        assert!(matches!(events[2], ChatStreamEvent::End(_)));
        assert_eq!(count_events(MockProviderClient::text_turn("x")).await, 3);
    }

    #[tokio::test]
    async fn tool_use_turn_includes_tool_call_in_captured_content() {
        let events = MockProviderClient::tool_use_turn(
            "toolu_01",
            "code",
            serde_json::json!({"code": "pure ()"}),
        );
        let end = events.into_iter().last().unwrap();
        if let ChatStreamEvent::End(end) = end {
            let content = end.captured_content.expect("captured_content populated");
            let tool_calls: Vec<_> = content
                .parts()
                .iter()
                .filter_map(|p| p.as_tool_call())
                .collect();
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].call_id, "toolu_01");
        } else {
            panic!("last event should be End");
        }
    }

    #[tokio::test]
    async fn exhaustion_panics_with_descriptive_message() {
        let provider = MockProviderClient::with_turns(vec![]);
        let req = CompletionRequest::new("claude-sonnet-4-20250514");
        let result = std::panic::AssertUnwindSafe(provider.complete(req));
        let outcome = futures::FutureExt::catch_unwind(result).await;
        // outcome.Result's Ok type isn't Debug, so match manually.
        let err = match outcome {
            Ok(_) => panic!("expected panic from exhausted MockProviderClient"),
            Err(e) => e,
        };
        let msg = err.downcast_ref::<String>().cloned().unwrap_or_else(|| {
            err.downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .unwrap_or_default()
        });
        assert!(
            msg.contains("exhausted"),
            "panic message should mention exhaustion: {msg}"
        );
    }

    #[tokio::test]
    async fn call_count_increments_per_complete_call() {
        let provider = MockProviderClient::with_turns(vec![
            MockProviderClient::text_turn("one"),
            MockProviderClient::text_turn("two"),
        ]);
        let req = CompletionRequest::new("claude-sonnet-4-20250514");
        assert_eq!(provider.call_count(), 0);
        let _ = provider.complete(req.clone()).await.unwrap();
        assert_eq!(provider.call_count(), 1);
        let _ = provider.complete(req).await.unwrap();
        assert_eq!(provider.call_count(), 2);
    }
}
