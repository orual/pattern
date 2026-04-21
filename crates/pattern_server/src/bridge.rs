//! [`TurnSinkBridge`]: bridges the synchronous [`TurnSink`] API used by the
//! agent runtime to the daemon's async event bus.
//!
//! The bridge is **per-batch**: it captures `batch_id` and `agent_id` at
//! construction time and tags every emitted event with those identifiers.
//! The daemon actor receives the tagged events and fans them out to all
//! subscribed TUI clients that have registered interest in the same agent.
//!
//! ## Why unbounded mpsc
//!
//! `tokio::sync::mpsc::unbounded_channel` is used (rather than `broadcast`)
//! because:
//! - There is exactly one producer per batch (the bridge) and one consumer
//!   (the daemon actor). Fan-out to multiple subscribers happens inside the
//!   actor, not at the channel level.
//! - `UnboundedSender::send()` uses atomic operations internally and never
//!   blocks, satisfying the `TurnSink::emit` contract of non-blocking
//!   emission.
//! - A bounded channel would risk deadlock if the actor is temporarily
//!   behind (e.g. processing a concurrent request), which would stall the
//!   agent loop.
//!
//! The downside (unbounded memory growth) is acceptable because batches are
//! short-lived and the actor processes events as fast as the runtime produces
//! them.

use std::sync::Arc;

use pattern_core::traits::turn_sink::{TurnEvent, TurnSink};
use smol_str::SmolStr;

use crate::protocol::TaggedTurnEvent;

/// Sender half of the daemon's event bus channel.
///
/// Held by each [`TurnSinkBridge`] to forward tagged events to the daemon actor.
pub type EventTx = tokio::sync::mpsc::UnboundedSender<TaggedTurnEvent>;

/// Receiver half of the daemon's event bus channel.
///
/// Held by the daemon actor, which reads events and fans them out to
/// per-subscriber irpc mpsc channels.
pub type EventRx = tokio::sync::mpsc::UnboundedReceiver<TaggedTurnEvent>;

/// Create a new event bus channel pair.
pub fn new_event_channel() -> (EventTx, EventRx) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Synchronous [`TurnSink`] that forwards tagged events to the daemon actor.
///
/// Constructed once per batch: the `batch_id` and `agent_id` are captured at
/// creation time so every emitted [`TurnEvent`] is automatically annotated
/// with the correct routing metadata before being forwarded.
///
/// `emit()` is lock-free — `UnboundedSender::send()` uses atomic operations
/// internally and never blocks.  If the receiver (daemon actor) has been
/// dropped, the send fails silently: the batch is orphaned and any further
/// events are discarded rather than panicking.
#[derive(Debug, Clone)]
pub struct TurnSinkBridge {
    batch_id: SmolStr,
    agent_id: SmolStr,
    tx: EventTx,
}

impl TurnSinkBridge {
    /// Create a new bridge for a single batch.
    pub fn new(batch_id: SmolStr, agent_id: SmolStr, tx: EventTx) -> Self {
        Self {
            batch_id,
            agent_id,
            tx,
        }
    }
}

impl TurnSink for TurnSinkBridge {
    fn emit(&self, event: TurnEvent) {
        let tagged = TaggedTurnEvent {
            batch_id: self.batch_id.clone(),
            agent_id: self.agent_id.clone(),
            event,
        };
        // Lock-free, unbounded, never blocks.
        // Failure means the daemon actor has been dropped — discard silently.
        let _ = self.tx.send(tagged);
    }
}

/// Atomically-swappable [`TurnSink`] that delegates to an inner sink.
///
/// Used by the daemon to share a single sink reference with a
/// [`TidepoolSession`] at open time, then swap the inner bridge
/// before each `step_with_agent_loop` call so events are tagged with
/// the correct per-batch `batch_id` and `agent_id`.
///
/// The inner sink defaults to [`pattern_core::traits::NoOpSink`] and
/// is swapped via [`MultiplexSink::set_inner`] before each step.
#[derive(Debug)]
pub struct MultiplexSink {
    inner: std::sync::RwLock<Arc<dyn TurnSink>>,
}

impl Default for MultiplexSink {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiplexSink {
    /// Create a new multiplex sink with a [`NoOpSink`] as the initial delegate.
    pub fn new() -> Self {
        Self {
            inner: std::sync::RwLock::new(Arc::new(pattern_core::traits::NoOpSink)),
        }
    }

    /// Swap the inner sink. Subsequent `emit()` calls will be
    /// forwarded to the new sink. The previous sink is dropped.
    pub fn set_inner(&self, sink: Arc<dyn TurnSink>) {
        let mut guard = self.inner.write().expect("multiplex sink lock poisoned");
        *guard = sink;
    }
}

impl TurnSink for MultiplexSink {
    fn emit(&self, event: TurnEvent) {
        let guard = self.inner.read().expect("multiplex sink lock poisoned");
        guard.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::traits::turn_sink::TurnEvent;
    use pattern_core::types::turn::StopReason;

    #[test]
    fn bridge_emits_tagged_events() {
        let (tx, mut rx) = new_event_channel();
        let bridge = TurnSinkBridge::new("batch-1".into(), "agent-1".into(), tx);

        bridge.emit(TurnEvent::Text("hello".into()));
        bridge.emit(TurnEvent::Stop(StopReason::EndTurn));

        let ev1 = rx.try_recv().unwrap();
        assert_eq!(ev1.batch_id, "batch-1");
        assert_eq!(ev1.agent_id, "agent-1");
        assert!(matches!(ev1.event, TurnEvent::Text(ref s) if s == "hello"));

        let ev2 = rx.try_recv().unwrap();
        assert_eq!(ev2.batch_id, "batch-1");
        assert_eq!(ev2.agent_id, "agent-1");
        assert!(matches!(ev2.event, TurnEvent::Stop(StopReason::EndTurn)));
    }

    #[test]
    fn emit_with_dropped_receiver_does_not_panic() {
        let (tx, rx) = new_event_channel();
        let bridge = TurnSinkBridge::new("batch-1".into(), "agent-1".into(), tx);
        // Explicitly drop the receiver before emitting.
        drop(rx);
        // Receiver dropped — send must fail silently, not panic.
        bridge.emit(TurnEvent::Text("orphaned".into()));
    }

    #[test]
    fn bridge_tags_every_event_with_same_batch_and_agent() {
        let (tx, mut rx) = new_event_channel();
        let bridge = TurnSinkBridge::new("batch-xyz".into(), "agent-abc".into(), tx);

        bridge.emit(TurnEvent::Text("chunk 1".into()));
        bridge.emit(TurnEvent::Thinking("reasoning".into()));
        bridge.emit(TurnEvent::Stop(StopReason::MaxTokens));

        let events: Vec<_> = (0..3).map(|_| rx.try_recv().unwrap()).collect();
        for ev in &events {
            assert_eq!(ev.batch_id, "batch-xyz");
            assert_eq!(ev.agent_id, "agent-abc");
        }
    }

    #[test]
    fn clone_of_bridge_shares_channel() {
        let (tx, mut rx) = new_event_channel();
        let bridge1 = TurnSinkBridge::new("b".into(), "a".into(), tx);
        let bridge2 = bridge1.clone();

        bridge1.emit(TurnEvent::Text("from 1".into()));
        bridge2.emit(TurnEvent::Text("from 2".into()));

        let ev1 = rx.try_recv().unwrap();
        let ev2 = rx.try_recv().unwrap();
        assert!(matches!(ev1.event, TurnEvent::Text(ref s) if s == "from 1"));
        assert!(matches!(ev2.event, TurnEvent::Text(ref s) if s == "from 2"));
    }
}
