//! Wire-turn event sink for the Phase 5 agent loop.
//!
//! The agent loop emits incremental events as they arrive from the
//! provider stream + the Haskell eval worker + the `Display` effect
//! handler. A [`TurnSink`] is the pluggable destination: CLI bindings
//! push to stdout, TUI bindings update panels, tests collect into a
//! `Vec` for assertions, headless runs use [`NoOpSink`].
//!
//! # Naming
//!
//! `TurnEvent` / `TurnSink` (rather than the more generic
//! `StreamEvent` / `StreamSink`) because `pattern_core::traits::data_stream`
//! already exposes a `StreamEvent` struct for data-source payloads;
//! the turn-centric naming keeps the two concepts separable at a
//! glance and avoids a path collision at the `traits::` re-export
//! level.
//!
//! # Why a sink instead of a `Stream` return value
//!
//! `Session::step` returns aggregated output at the end of the
//! user-visible exchange. Callers that need mid-turn visibility (text
//! chunks as they arrive, tool dispatch notifications, per-turn
//! boundaries) subscribe via the sink. That keeps the aggregate return
//! type clean while still supporting real-time UX.
//!
//! # Thread-safety
//!
//! `SessionContext` holds `Arc<dyn TurnSink>` and hands it to both the
//! async agent loop (which emits `Text` / `ToolCall` / `ToolResult` /
//! `Stop` events) and the synchronous Haskell handlers running inside
//! `spawn_blocking` (which emit `Display` events). Implementations
//! MUST be `Send + Sync` and must handle concurrent `emit` calls from
//! those two contexts.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::types::provider::{CompletionRequest, ToolCall, ToolResult};
use crate::types::turn::StopReason;

/// Sub-variant of [`TurnEvent::Display`] — which Haskell
/// `Pattern.Display.*` constructor produced the output.
///
/// Preserved through the sink so UIs can render the three styles
/// distinctly:
///
/// - [`Chunk`](DisplayKind::Chunk) — partial streaming text from an
///   agent's assembled response. UIs typically render these
///   concatenated into a single growing buffer.
/// - [`Final`](DisplayKind::Final) — terminal assembled content for
///   one `Message.Ask` turn. Fires once per round-trip; UIs may
///   close a "thinking" indicator here.
/// - [`Note`](DisplayKind::Note) — side-channel status message
///   (tool-call progress, typing indicator, agent commentary). UIs
///   typically render these distinctly from main content — dimmer,
///   parenthesised, or in a separate pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayKind {
    /// Partial streaming chunk (`Pattern.Display.chunk`).
    Chunk,
    /// Terminal assembled content (`Pattern.Display.final_`).
    Final,
    /// Side-channel agent note (`Pattern.Display.note`).
    Note,
}

/// Fine-grained event emitted during a single wire turn.
///
/// The variants correspond 1:1 to observable state transitions in the
/// agent loop:
///
/// - [`Text`](TurnEvent::Text) — the provider stream emitted a chunk
///   of LLM-authored response text. These arrive many times per turn
///   as the model generates output. Carry no sub-kind — UIs typically
///   concatenate them into a streaming buffer.
/// - [`Thinking`](TurnEvent::Thinking) — the provider stream emitted
///   a chunk of LLM reasoning content (Anthropic Extended Thinking,
///   OpenAI o-series reasoning summaries, etc.). Distinct from
///   `Text`: reasoning is the "how" the model got to its answer;
///   text is the answer itself. UIs typically dim, collapse, or
///   hide-by-default thinking chunks.
/// - [`ToolCall`](TurnEvent::ToolCall) — the provider stream completed
///   a tool_use block; the agent loop is about to dispatch it to the
///   eval worker. Fires BEFORE the corresponding
///   [`ToolResult`](TurnEvent::ToolResult).
/// - [`ToolResult`](TurnEvent::ToolResult) — the eval worker returned
///   a result (success or error). Fires after the stream closes and
///   the worker replies; callers get a chance to display the result
///   before the next wire turn composes.
/// - [`Display`](TurnEvent::Display) — the Haskell `Pattern.Display.*`
///   effect handler emitted text. Semantically distinct from
///   `Text`: LLM-authored streaming output vs agent-authored
///   deliberate surfacing. Carries a [`DisplayKind`] so UIs can
///   further distinguish Chunk / Final / Note.
/// - [`Stop`](TurnEvent::Stop) — the wire turn completed. Terminal
///   reasons (anything except `ToolUse`) mark the end of the
///   user-visible exchange; the next `Stop` will belong to a fresh
///   `Session::step` call.
/// - [`ComposedRequest`](TurnEvent::ComposedRequest) — the composer
///   produced a complete `CompletionRequest` for this wire turn.
///   Emitted once per wire turn, immediately before the provider
///   call. Full request struct (boxed); sinks decide how much to
///   render or discard.
///
/// # UX guidance for the text-bearing variants
///
/// Four variants carry text; distinguishing them in the CLI / TUI
/// matters because they mean different things:
///
/// | Variant           | Source           | Meaning                             |
/// |-------------------|------------------|-------------------------------------|
/// | `Text`            | LLM stream       | "model is generating its answer"    |
/// | `Thinking`        | LLM stream       | "model is reasoning about it"       |
/// | `Display::Chunk`  | agent's Haskell  | "agent is typing assembled text"    |
/// | `Display::Final`  | agent's Haskell  | "agent completed an assembled reply"|
/// | `Display::Note`   | agent's Haskell  | "agent side-channel status"         |
///
/// Recommended rendering conventions:
/// - `Text` — default style, concatenated into a streaming buffer.
/// - `Thinking` — dimmed / indented / collapsed / hidden-by-default
///   (operator-configurable). The content is useful for debugging
///   but often noisy for routine interaction.
/// - `Display::Chunk` / `::Final` — distinct from both (e.g.
///   prefixed with a glyph, rendered in a framed block).
/// - `Display::Note` — dimmed or parenthesised so it doesn't
///   compete with primary content.
///
/// # Thinking preservation across tool cycles
///
/// For providers with Extended Thinking (Anthropic) or equivalent,
/// the reasoning blocks must be echoed back verbatim on the
/// follow-up tool_result wire turn — otherwise the model can't
/// continue its reasoning chain, and for Anthropic the signed
/// blocks will be stripped or rejected. The agent loop handles this
/// at the message level: the reasoning content + signatures captured
/// at stream-end ride along on the assistant message's content
/// parts, and the next wire turn's composer includes them. `Thinking`
/// events on the sink are for UI display only; the sink doesn't
/// participate in preservation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TurnEvent {
    /// A chunk of LLM-authored response text from the provider
    /// stream. The model's answer, not its reasoning.
    Text(String),
    /// A chunk of LLM reasoning content (Anthropic Extended Thinking,
    /// OpenAI o-series reasoning summary, etc.). Semantically
    /// distinct from [`Self::Text`] — see UX guidance in the enum
    /// doc. Thought signatures (when present) are carried on the
    /// assistant message's content parts, not this event.
    Thinking(String),
    /// The LLM has requested a tool to be executed. The eval is
    /// dispatched in parallel with remaining stream work; pair with
    /// the matching [`ToolResult`](Self::ToolResult) by `call_id`.
    ToolCall(ToolCall),
    /// The eval worker returned a result for a prior
    /// [`ToolCall`](Self::ToolCall). Success / error is encoded on the
    /// outcome.
    ToolResult(ToolResult),
    /// Text emitted by the Haskell `Pattern.Display.*` effect
    /// handler. Distinct from [`Self::Text`]: LLM-authored streaming
    /// vs agent-authored deliberate output. `kind` distinguishes
    /// Chunk / Final / Note — see [`DisplayKind`] for UX guidance.
    Display {
        /// Which `Pattern.Display.*` constructor produced this.
        kind: DisplayKind,
        /// The displayed text.
        text: String,
    },
    /// The wire turn ended. If [`StopReason::is_terminal`] is `true`,
    /// the user-visible exchange is complete; otherwise the driver
    /// will issue a follow-up turn with tool results.
    Stop(StopReason),
    /// The composer produced a complete [`CompletionRequest`]; the
    /// orchestrator is about to hand it to the provider. Emitted
    /// once per wire turn, immediately before
    /// `ProviderClient::complete`.
    ///
    /// Intended for debugging, request replay / snapshot testing,
    /// and cache-behaviour inspection. The event carries the FULL
    /// request struct — sinks choose how much to render or log.
    /// [`NoOpSink`] drops it immediately (free); [`VecSink`]
    /// retains it (memory grows linearly with wire-turn count; call
    /// `drain()` periodically for long sessions).
    ///
    /// Boxed to keep the enum stable-sized —
    /// [`CompletionRequest`] is large and variable.
    ///
    /// Historical note: the previous "dump via `tracing::debug`"
    /// approach produced massive logs that were painful to grep
    /// and noisy in CI. The sink-based tap is opt-in — only
    /// subscribers that care pay the clone cost.
    ComposedRequest(Box<CompletionRequest>),
}

/// Destination for [`TurnEvent`]s emitted during a wire turn.
///
/// Implementations must be `Send + Sync + Debug`:
/// - `Send + Sync` because the agent loop and Haskell handlers run on
///   different tasks / threads.
/// - `Debug` so structs that hold `Arc<dyn TurnSink>` (like
///   `SessionContext`) can derive `Debug` too.
pub trait TurnSink: Send + Sync + std::fmt::Debug {
    /// Emit one event. Implementations should NOT block indefinitely;
    /// a bounded queue + drop-oldest or drop-newest policy is
    /// preferable to blocking the agent loop.
    fn emit(&self, event: TurnEvent);
}

/// No-op sink that drops every event.
///
/// Used by tests and headless runs where nothing subscribes. Keeping
/// the `SessionContext::turn_sink` field non-optional simplifies the
/// emit call sites at the cost of one pointer-sized allocation per
/// session.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpSink;

impl TurnSink for NoOpSink {
    fn emit(&self, _event: TurnEvent) {
        // intentional no-op
    }
}

/// Shared sink that records every emitted event into a `Vec`.
///
/// Primarily a test fixture — implementors targeting real UIs (CLI
/// stdout, TUI channel) should write a small custom impl for their
/// target. This type lives in `pattern_core` so downstream crates'
/// unit tests can reuse it without re-deriving the pattern.
#[derive(Debug, Default, Clone)]
pub struct VecSink {
    inner: Arc<Mutex<Vec<TurnEvent>>>,
}

impl VecSink {
    /// Create an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume and return the events observed so far, in order.
    pub fn drain(&self) -> Vec<TurnEvent> {
        let mut guard = self.inner.lock().expect("VecSink mutex poisoned");
        std::mem::take(&mut *guard)
    }

    /// Copy the events observed so far without draining. Cheap for
    /// small event counts; intended for assertions in tests.
    pub fn snapshot(&self) -> Vec<TurnEvent> {
        self.inner.lock().expect("VecSink mutex poisoned").clone()
    }

    /// Number of events captured so far.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("VecSink mutex poisoned").len()
    }

    /// `true` if no events have been emitted yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TurnSink for VecSink {
    fn emit(&self, event: TurnEvent) {
        self.inner
            .lock()
            .expect("VecSink mutex poisoned")
            .push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_sink_accepts_events_without_effect() {
        let sink = NoOpSink;
        sink.emit(TurnEvent::Text("hello".into()));
        sink.emit(TurnEvent::Stop(StopReason::EndTurn));
        // no observable state to assert — the point is that it compiles
        // + doesn't panic.
    }

    #[test]
    fn vec_sink_records_events_in_order() {
        let sink = VecSink::new();
        sink.emit(TurnEvent::Text("hello".into()));
        sink.emit(TurnEvent::Display {
            kind: DisplayKind::Note,
            text: "tool running...".into(),
        });
        sink.emit(TurnEvent::Stop(StopReason::EndTurn));
        let events = sink.snapshot();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], TurnEvent::Text(ref s) if s == "hello"));
        assert!(
            matches!(events[1], TurnEvent::Display { kind: DisplayKind::Note, ref text } if text == "tool running...")
        );
        assert!(matches!(events[2], TurnEvent::Stop(StopReason::EndTurn)));
    }

    #[test]
    fn display_kind_serde_snake_case() {
        let j = serde_json::to_string(&DisplayKind::Chunk).unwrap();
        assert_eq!(j, r#""chunk""#);
        let j = serde_json::to_string(&DisplayKind::Final).unwrap();
        assert_eq!(j, r#""final""#);
        let j = serde_json::to_string(&DisplayKind::Note).unwrap();
        assert_eq!(j, r#""note""#);
    }

    #[test]
    fn vec_sink_captures_composed_request() {
        let sink = VecSink::new();
        let req = CompletionRequest::new("claude-opus-4-7");
        sink.emit(TurnEvent::ComposedRequest(Box::new(req)));
        sink.emit(TurnEvent::Stop(StopReason::EndTurn));

        let events = sink.snapshot();
        assert_eq!(events.len(), 2);
        match &events[0] {
            TurnEvent::ComposedRequest(boxed) => {
                assert_eq!(boxed.model, "claude-opus-4-7");
            }
            other => panic!("expected ComposedRequest, got {other:?}"),
        }
    }

    #[test]
    fn noop_sink_drops_composed_request_without_panicking() {
        // The full-struct clone cost is opt-in — NoOpSink callers pay
        // nothing for this variant at runtime.
        let sink = NoOpSink;
        let req = CompletionRequest::new("claude-sonnet-4-20250514");
        sink.emit(TurnEvent::ComposedRequest(Box::new(req)));
    }

    #[test]
    fn vec_sink_distinguishes_text_from_thinking() {
        let sink = VecSink::new();
        sink.emit(TurnEvent::Thinking("hmm, the user wants...".into()));
        sink.emit(TurnEvent::Text("The answer is 42.".into()));
        sink.emit(TurnEvent::Thinking("also considering...".into()));
        sink.emit(TurnEvent::Stop(StopReason::EndTurn));

        let events = sink.snapshot();
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], TurnEvent::Thinking(ref s) if s.contains("hmm")));
        assert!(matches!(events[1], TurnEvent::Text(ref s) if s.starts_with("The answer")));
        assert!(matches!(events[2], TurnEvent::Thinking(ref s) if s.contains("also")));
        assert!(matches!(events[3], TurnEvent::Stop(StopReason::EndTurn)));
    }

    #[test]
    fn vec_sink_drain_empties() {
        let sink = VecSink::new();
        sink.emit(TurnEvent::Text("a".into()));
        let first = sink.drain();
        assert_eq!(first.len(), 1);
        assert!(sink.is_empty());
        assert!(sink.drain().is_empty());
    }

    #[test]
    fn vec_sink_is_send_sync() {
        // Compile-time check: TurnSink is dyn-compatible + the concrete
        // type can cross threads.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VecSink>();
        assert_send_sync::<Arc<dyn TurnSink>>();
    }
}
