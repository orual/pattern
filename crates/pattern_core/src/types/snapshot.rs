//! Checkpoint snapshot types for persona and session state.
//!
//! These types are the shapes that `Session::checkpoint()` and
//! `Session::restore()` (Phase 3) serialize and deserialize. Phase 2 lands the
//! type shape only; the implementation detail — which fields are populated and
//! how the CRDT state is serialized — is deferred to Phase 3.
//!
//! Callers should treat these types as opaque blobs: construct them via
//! `Session::checkpoint()` and restore them via `Session::restore()`. Do not
//! pattern-match on the `data` field directly across crate versions.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::types::ids::AgentId;
use crate::types::turn::TurnId;

/// A serializable snapshot of a single agent's persona-scoped state.
///
/// Captures the Loro CRDT snapshot of an agent's memory blocks plus any
/// persona-level configuration needed to deterministically restart a turn.
///
/// > **Implementation detail deferred to Phase 3.** Phase 2 lands the shape
/// > only. The `data` field is an opaque `serde_json::Value`; Phase 3 will
/// > replace it with a typed CRDT-snapshot wrapper.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::snapshot::PersonaSnapshot;
/// use pattern_core::types::ids::AgentId;
/// use pattern_core::types::turn::TurnId;
///
/// let snap = PersonaSnapshot {
///     agent_id: AgentId::new("orual-companion"),
///     as_of_turn: TurnId::generate(),
///     captured_at: Timestamp::now(),
///     data: serde_json::json!({}),
/// };
/// assert_eq!(snap.agent_id.as_str(), "orual-companion");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaSnapshot {
    /// The agent whose persona this snapshot captures.
    pub agent_id: AgentId,
    /// The turn after which this snapshot was taken.
    pub as_of_turn: TurnId,
    /// Wall-clock time the snapshot was captured.
    pub captured_at: Timestamp,
    /// Opaque CRDT / persona data. Implementation defined by Phase 3.
    pub data: serde_json::Value,
}

/// A serializable snapshot of a complete session (one or more agents).
///
/// Combines per-agent [`PersonaSnapshot`]s with session-level metadata needed
/// to restart an entire multi-agent constellation from a known-good state.
///
/// > **Implementation detail deferred to Phase 3.** Phase 2 lands the shape
/// > only. The `data` field is an opaque `serde_json::Value`; Phase 3 will
/// > replace it with a typed session-state wrapper.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};
/// use pattern_core::types::ids::AgentId;
/// use pattern_core::types::turn::TurnId;
///
/// let persona = PersonaSnapshot {
///     agent_id: AgentId::new("orual-companion"),
///     as_of_turn: TurnId::nil(),
///     captured_at: Timestamp::now(),
///     data: serde_json::json!({}),
/// };
/// let session = SessionSnapshot {
///     personas: vec![persona],
///     captured_at: Timestamp::now(),
///     schema_version: 1,
///     data: serde_json::json!({}),
/// };
/// assert_eq!(session.schema_version, 1);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Per-agent persona snapshots included in this session checkpoint.
    pub personas: Vec<PersonaSnapshot>,
    /// Wall-clock time the session snapshot was captured.
    pub captured_at: Timestamp,
    /// Schema version for forward-compatibility checks. Starts at `1`.
    pub schema_version: u32,
    /// Opaque session-level data. Implementation defined by Phase 3.
    pub data: serde_json::Value,
}
