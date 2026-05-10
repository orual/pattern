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

use pattern_core::spawn::SpawnSource;
use pattern_core::traits::SpawnSinkFactory;
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
    /// Origin tag stamped on every emitted event. Defaults to
    /// [`SpawnSource::Main`]; set to an Ephemeral / Sibling / Fork
    /// variant when the bridge is constructed for a sub-spawn.
    source: SpawnSource,
}

impl TurnSinkBridge {
    /// Create a new main-batch bridge. Source defaults to
    /// [`SpawnSource::Main`]. For sub-spawns use [`Self::with_source`].
    pub fn new(batch_id: SmolStr, agent_id: SmolStr, tx: EventTx) -> Self {
        Self {
            batch_id,
            agent_id,
            tx,
            source: SpawnSource::Main,
        }
    }

    /// Create a bridge for a sub-spawn that shares the parent's event
    /// channel but tags emitted events with a different origin (and
    /// usually a different batch_id and agent_id).
    ///
    /// Used by `run_ephemeral` and the future sibling/fork wiring to
    /// route child output to a sidebar surface in the TUI without
    /// dropping it into the main conversation transcript.
    pub fn with_source(
        batch_id: SmolStr,
        agent_id: SmolStr,
        tx: EventTx,
        source: SpawnSource,
    ) -> Self {
        Self {
            batch_id,
            agent_id,
            tx,
            source,
        }
    }

    /// Clone of the underlying event channel sender. Lets a parent
    /// bridge fork off child bridges that share the same actor
    /// destination.
    pub fn event_tx(&self) -> EventTx {
        self.tx.clone()
    }
}

impl TurnSink for TurnSinkBridge {
    fn emit(&self, event: TurnEvent) {
        // Convert to wire-safe format. Events that can't be represented on
        // the wire (e.g. ComposedRequest) are filtered out here.
        let Some(wire_event) = crate::protocol::WireTurnEvent::from_turn_event(&event) else {
            tracing::trace!(
                batch_id = %self.batch_id,
                "event filtered from wire (not wire-representable)"
            );
            return;
        };

        let tagged = TaggedTurnEvent {
            batch_id: self.batch_id.clone(),
            agent_id: self.agent_id.clone(),
            event: wire_event,
            // Per-agent emitters leave mount_path None; the actor's
            // fan_out resolves agent → mount via `agent_to_mount`.
            mount_path: None,
            source: self.source.clone(),
        };
        // Lock-free, unbounded, never blocks.
        // Failure means the daemon actor has been dropped — discard silently.
        //
        // Diagnostic info-level log naming the source variant so we can
        // verify SpawnSource tagging end-to-end without TUI plumbing.
        // Drop back to trace once issue 1 is fully wired.
        let source_kind = match &self.source {
            SpawnSource::Main => "Main",
            SpawnSource::Ephemeral { .. } => "Ephemeral",
            SpawnSource::Sibling { .. } => "Sibling",
            SpawnSource::Fork { .. } => "Fork",
        };
        tracing::trace!(
            batch_id = %self.batch_id,
            agent_id = %self.agent_id,
            source = %source_kind,
            event = ?tagged.event,
            "TurnSinkBridge::emit"
        );
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

/// [`SpawnSinkFactory`] implementation that mints fresh
/// [`TurnSinkBridge`]s sharing a single durable [`EventTx`] channel.
///
/// The daemon constructs one of these per session and installs it on
/// the [`SessionContext`] so that
/// `fork_for_ephemeral` (and the future sibling/fork wirings) can
/// mint child sinks with the right [`SpawnSource`] tag. Decoupling
/// the factory from the per-batch [`MultiplexSink`] keeps the
/// factory durable across the daemon's batch-by-batch sink swaps.
#[derive(Debug, Clone)]
pub struct BridgeFactory {
    event_tx: EventTx,
}

impl BridgeFactory {
    pub fn new(event_tx: EventTx) -> Self {
        Self { event_tx }
    }
}

impl SpawnSinkFactory for BridgeFactory {
    fn fork_for_spawn(
        &self,
        batch_id: SmolStr,
        agent_id: SmolStr,
        source: SpawnSource,
    ) -> Arc<dyn TurnSink> {
        Arc::new(TurnSinkBridge::with_source(
            batch_id,
            agent_id,
            self.event_tx.clone(),
            source,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::WireTurnEvent;
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
        assert!(matches!(ev1.event, WireTurnEvent::Text(ref s) if s == "hello"));

        let ev2 = rx.try_recv().unwrap();
        assert_eq!(ev2.batch_id, "batch-1");
        assert_eq!(ev2.agent_id, "agent-1");
        assert!(matches!(
            ev2.event,
            WireTurnEvent::Stop(StopReason::EndTurn)
        ));
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
        assert!(matches!(ev1.event, WireTurnEvent::Text(ref s) if s == "from 1"));
        assert!(matches!(ev2.event, WireTurnEvent::Text(ref s) if s == "from 2"));
    }

    // --- MultiplexSink tests ---

    /// Emitting to a MultiplexSink with an inner set forwards the event
    /// to that inner sink.
    #[test]
    fn multiplex_sink_delegates_to_inner() {
        let (tx, mut rx) = new_event_channel();
        let bridge = Arc::new(TurnSinkBridge::new("batch-1".into(), "agent-1".into(), tx));

        let mux = MultiplexSink::new();
        mux.set_inner(bridge);

        mux.emit(TurnEvent::Text("delegated".into()));

        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.batch_id, "batch-1");
        assert!(matches!(ev.event, WireTurnEvent::Text(ref s) if s == "delegated"));
    }

    /// After swapping the inner sink, new events go to the new inner while
    /// events emitted before the swap went to the old inner.
    #[test]
    fn multiplex_sink_swap_routes_to_new_inner() {
        let (tx_a, mut rx_a) = new_event_channel();
        let (tx_b, mut rx_b) = new_event_channel();

        let bridge_a = Arc::new(TurnSinkBridge::new(
            "batch-a".into(),
            "agent-1".into(),
            tx_a,
        ));
        let bridge_b = Arc::new(TurnSinkBridge::new(
            "batch-b".into(),
            "agent-1".into(),
            tx_b,
        ));

        let mux = MultiplexSink::new();

        // First inner — event goes to rx_a.
        mux.set_inner(bridge_a);
        mux.emit(TurnEvent::Text("first".into()));

        // Swap inner — event goes to rx_b.
        mux.set_inner(bridge_b);
        mux.emit(TurnEvent::Text("second".into()));

        let ev_a = rx_a.try_recv().unwrap();
        assert_eq!(ev_a.batch_id, "batch-a");
        assert!(matches!(ev_a.event, WireTurnEvent::Text(ref s) if s == "first"));

        // rx_a should have nothing more.
        assert!(rx_a.try_recv().is_err());

        let ev_b = rx_b.try_recv().unwrap();
        assert_eq!(ev_b.batch_id, "batch-b");
        assert!(matches!(ev_b.event, WireTurnEvent::Text(ref s) if s == "second"));
    }

    /// The default MultiplexSink (backed by NoOpSink) must not panic when
    /// events are emitted before any inner is installed.
    #[test]
    fn multiplex_sink_default_drops_events() {
        let mux = MultiplexSink::new();
        // Must not panic — NoOpSink discards events silently.
        mux.emit(TurnEvent::Text("before any inner".into()));
        mux.emit(TurnEvent::Stop(StopReason::EndTurn));
    }
}
