//! Event-log checkpoint / restore (Phase 3 Task 15, AC2.4).
//!
//! Records each `(tag, request, response)` effect exchange during a turn.
//! On restore, a `ReplayingBundle` would re-drive the JIT against the log
//! until exhausted, then continue with live handlers. Phase 3 lands the
//! log plumbing + serialisation round-trip; wiring the replay bundle into
//! the run loop is deferred to the phase that adds rich turn outputs
//! (Phase 4/5) — until then, restore populates the log and a subsequent
//! step naturally produces the same result because the handlers are
//! pure-with-respect-to-agent-id (Time is the exception; deterministic
//! replay will freeze time at the recorded timestamp when the replay
//! bundle ships).
//!
//! CBOR is used via `serde_cbor` for the on-wire shape of each exchange
//! so both requests and responses (represented as
//! [`tidepool_eval::Value`]) survive a round-trip. Values are already
//! serde-serialisable in `tidepool-eval`.

use pattern_core::error::RuntimeError;
use pattern_core::types::ids::new_id;
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};
use serde::{Deserialize, Serialize};
use tidepool_eval::Value;

/// One recorded effect exchange: the handler tag and the `Debug`
/// representations of the request and response values.
///
/// # Why Debug and not a proper serde round-trip?
///
/// [`tidepool_eval::Value`] does not implement `serde::Serialize` /
/// `Deserialize` — it contains function pointers and closure data that
/// cannot round-trip. For replay to be faithful we would need CBOR via
/// `tidepool_repr`, but that payload is not yet stabilised. Phase 3
/// lands the event-log plumbing with a debug-string shape so tests can
/// assert sequence + tag ordering and snapshot/restore round-trips
/// survive without information loss on those fields. Faithful replay
/// (re-driving the JIT with recorded responses) is deferred until the
/// replay bundle lands in a later phase; the event shape can be
/// extended then.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointEvent {
    /// Effect tag assigned by the tidepool dispatcher (position in the
    /// handler HList).
    pub tag: u32,
    /// Debug representation of the request value.
    pub request_repr: String,
    /// Debug representation of the handler's response value.
    pub response_repr: String,
    /// Turn number assigned by the session when recording.
    pub turn: u64,
    /// Sequence within the turn; monotonic per record.
    pub sequence: u64,
}

impl CheckpointEvent {
    /// Construct an event from raw values. `sequence` is assigned by the
    /// log when the event is recorded; callers pass 0 and the log
    /// overwrites.
    pub fn new(tag: u32, request: &Value, response: &Value, turn: u64) -> Self {
        Self {
            tag,
            request_repr: format!("{request:?}"),
            response_repr: format!("{response:?}"),
            turn,
            sequence: 0,
        }
    }

    /// Construct an event from an already-formatted request repr and a
    /// response [`Value`]. Used by handlers that only have the typed
    /// request (not a raw `Value`) to record their exchange — the
    /// Debug-string shape of [`Self::request_repr`] is unchanged so the
    /// round-trip contract holds.
    pub fn from_request_repr(
        tag: u32,
        request_repr: impl Into<String>,
        response: &Value,
        turn: u64,
    ) -> Self {
        Self {
            tag,
            request_repr: request_repr.into(),
            response_repr: format!("{response:?}"),
            turn,
            sequence: 0,
        }
    }
}

/// Append-only log of effect exchanges for the current session.
#[derive(Debug, Default)]
pub struct CheckpointLog {
    events: Vec<CheckpointEvent>,
    next_sequence: u64,
}

impl CheckpointLog {
    /// Fresh log with no events.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the underlying events.
    pub fn events(&self) -> &[CheckpointEvent] {
        &self.events
    }

    /// Number of recorded events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// True when no events have been recorded.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append `event` to the log, assigning it the next sequence number.
    pub fn record(&mut self, mut event: CheckpointEvent) {
        event.sequence = self.next_sequence;
        self.next_sequence += 1;
        self.events.push(event);
    }

    /// Replace the log's contents with `events` (used during restore).
    pub fn reset_to(&mut self, events: Vec<CheckpointEvent>) {
        self.next_sequence = events.iter().map(|e| e.sequence + 1).max().unwrap_or(0);
        self.events = events;
    }

    /// Produce a [`SessionSnapshot`] carrying the current event log.
    ///
    /// The snapshot wraps per-agent [`PersonaSnapshot`]s; Phase 3 records
    /// a single agent per session (no constellation yet), so the returned
    /// snapshot has exactly one persona entry. Its `data` field holds
    /// `serde_json::to_value(&events)` so it round-trips through the
    /// existing opaque-Value contract of SessionSnapshot.
    pub fn snapshot(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> Result<SessionSnapshot, RuntimeError> {
        let events_json =
            serde_json::to_value(&self.events).map_err(|e| RuntimeError::CheckpointFailed {
                reason: format!("failed to serialise event log: {e}"),
            })?;
        let persona = PersonaSnapshot {
            agent_id: agent_id.into(),
            // `as_of_turn` is an id; mint a fresh one for this capture so
            // consumers can trace snapshots back to a specific checkpoint.
            as_of_turn: new_id(),
            captured_at: jiff::Timestamp::now(),
            data: events_json,
        };
        Ok(SessionSnapshot {
            personas: vec![persona],
            captured_at: jiff::Timestamp::now(),
            schema_version: 1,
            data: serde_json::json!({ "session_id": session_id }),
        })
    }

    /// Inverse of [`Self::snapshot`]: extract the event list for replay.
    pub fn decode_events(snapshot: &SessionSnapshot) -> Result<Vec<CheckpointEvent>, RuntimeError> {
        let persona = snapshot
            .personas
            .first()
            .ok_or_else(|| RuntimeError::CheckpointFailed {
                reason: "snapshot contains no persona entries; cannot restore".into(),
            })?;
        serde_json::from_value::<Vec<CheckpointEvent>>(persona.data.clone()).map_err(|e| {
            RuntimeError::CheckpointFailed {
                reason: format!("failed to deserialise event log: {e}"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::Literal;

    fn v_int(n: i64) -> Value {
        Value::Lit(Literal::LitInt(n))
    }

    #[test]
    fn record_assigns_monotonic_sequence() {
        let mut log = CheckpointLog::new();
        log.record(CheckpointEvent::new(0, &v_int(1), &v_int(2), 1));
        log.record(CheckpointEvent::new(0, &v_int(3), &v_int(4), 1));
        assert_eq!(log.events()[0].sequence, 0);
        assert_eq!(log.events()[1].sequence, 1);
    }

    #[test]
    fn snapshot_and_decode_round_trip() {
        let mut log = CheckpointLog::new();
        log.record(CheckpointEvent::new(0, &v_int(10), &v_int(20), 1));
        log.record(CheckpointEvent::new(7, &v_int(11), &v_int(21), 2));
        let snap = log.snapshot("session-abc", "agent-xyz").unwrap();

        let recovered = CheckpointLog::decode_events(&snap).unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].tag, 0);
        assert_eq!(recovered[0].sequence, 0);
        assert!(recovered[0].request_repr.contains("10"));
        assert!(recovered[0].response_repr.contains("20"));
        assert_eq!(recovered[1].tag, 7);
        assert_eq!(recovered[1].turn, 2);
    }

    #[test]
    fn reset_to_recomputes_next_sequence() {
        let mut log = CheckpointLog::new();
        let mut e = CheckpointEvent::new(0, &v_int(1), &v_int(2), 1);
        e.sequence = 5;
        log.reset_to(vec![e]);
        // Recording a new event should pick up sequence = 6.
        log.record(CheckpointEvent::new(0, &v_int(7), &v_int(8), 1));
        assert_eq!(log.events()[1].sequence, 6);
    }

    #[test]
    fn decode_events_errors_on_empty_personas() {
        let snap = SessionSnapshot {
            personas: vec![],
            captured_at: jiff::Timestamp::now(),
            schema_version: 1,
            data: serde_json::Value::Null,
        };
        let err = CheckpointLog::decode_events(&snap).unwrap_err();
        match err {
            RuntimeError::CheckpointFailed { ref reason } => {
                assert!(reason.contains("no persona"), "got: {reason}");
            }
            other => panic!("expected CheckpointFailed, got {other:?}"),
        }
    }
}
