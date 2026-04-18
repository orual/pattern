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

use crate::types::provider::{ToolCall, ToolResult};
use crate::types::turn::StopReason;

/// Fine-grained event emitted during a single wire turn.
///
/// The variants correspond 1:1 to observable state transitions in the
/// agent loop:
///
/// - [`Text`](TurnEvent::Text) — the provider stream emitted a chunk
///   of assistant text. These arrive many times per turn as the model
///   generates output.
/// - [`ToolCall`](TurnEvent::ToolCall) — the provider stream completed
///   a tool_use block; the agent loop is about to dispatch it to the
///   eval worker. Fires BEFORE the corresponding
///   [`ToolResult`](TurnEvent::ToolResult).
/// - [`ToolResult`](TurnEvent::ToolResult) — the eval worker returned
///   a result (success or error). Fires after the stream closes and
///   the worker replies; callers get a chance to display the result
///   before the next wire turn composes.
/// - [`Display`](TurnEvent::Display) — the Haskell `Display.Show`
///   effect handler emitted text. Distinct from `Text` because it
///   comes from the agent's side of the effect boundary, not the raw
///   LLM output; UIs may render it differently (e.g. without the
///   streaming-text animation).
/// - [`Stop`](TurnEvent::Stop) — the wire turn completed. Terminal
///   reasons (anything except `ToolUse`) mark the end of the
///   user-visible exchange; the next `Stop` will belong to a fresh
///   `Session::step` call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TurnEvent {
    /// A chunk of assistant-authored text from the LLM stream.
    Text(String),
    /// The LLM has requested a tool to be executed. The eval is
    /// dispatched in parallel with remaining stream work; pair with
    /// the matching [`ToolResult`](Self::ToolResult) by `call_id`.
    ToolCall(ToolCall),
    /// The eval worker returned a result for a prior
    /// [`ToolCall`](Self::ToolCall). Success / error is encoded on the
    /// outcome.
    ToolResult(ToolResult),
    /// Text emitted by the Haskell `Display.Show` effect handler.
    /// Semantically distinct from LLM-authored `Text` chunks.
    Display {
        /// The displayed text, rendered by the handler.
        text: String,
    },
    /// The wire turn ended. If [`StopReason::is_terminal`] is `true`,
    /// the user-visible exchange is complete; otherwise the driver
    /// will issue a follow-up turn with tool results.
    Stop(StopReason),
}

/// Destination for [`TurnEvent`]s emitted during a wire turn.
///
/// Implementations must be `Send + Sync` because the agent loop and
/// Haskell handlers run on different tasks / threads.
pub trait TurnSink: Send + Sync {
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
        sink.emit(TurnEvent::Stop(StopReason::EndTurn));
        let events = sink.snapshot();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], TurnEvent::Text(ref s) if s == "hello"));
        assert!(matches!(events[1], TurnEvent::Stop(StopReason::EndTurn)));
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
