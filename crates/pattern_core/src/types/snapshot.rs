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

/// Configuration required to open a new session for an agent.
///
/// [`crate::traits::AgentRuntime::open_session`] consumes a `PersonaConfig`
/// when constructing a fresh session. Carries the agent's identity, the
/// Haskell program the runtime will compile, and runtime-policy knobs
/// (timeout budgets, nursery size). `extra` is a free-form `serde_json::Value`
/// slot for configuration that hasn't earned a first-class field yet
/// (persona-specific tool toggles, model-name overrides, experiments).
///
/// Construct with [`PersonaConfig::new`] to pick up default optional fields;
/// use builder-style setters for the optional knobs. `#[non_exhaustive]` so
/// future fields can be added without breaking callers.
///
/// # Examples
///
/// ```
/// use pattern_core::types::snapshot::PersonaConfig;
///
/// let cfg = PersonaConfig::new(
///     "orual-companion",
///     "Companion",
///     "module Agent where\nagent = pure ()",
/// )
/// .with_wall_budget_ms(30_000)
/// .with_cpu_budget_ms(10_000);
/// assert_eq!(cfg.agent_id.as_str(), "orual-companion");
/// assert_eq!(cfg.wall_budget_ms, Some(30_000));
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaConfig {
    /// Stable identifier for this agent.
    pub agent_id: AgentId,
    /// Human-readable name for logs / display. Smol since it's short and cloned often.
    pub name: smol_str::SmolStr,
    /// The Haskell agent program source. The runtime hands it to
    /// `tidepool-extract` with the SDK directory on the include path; agent
    /// programs import from the `Pattern.*` module tree directly.
    pub program: String,
    /// Wall-clock time-in-JIT budget per turn, in milliseconds. `None` means
    /// use the runtime's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_budget_ms: Option<u64>,
    /// CPU time-in-JIT budget per turn, in milliseconds. `None` means use
    /// the runtime's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_budget_ms: Option<u64>,
    /// Additional milliseconds of runaway compute (no effect yields) to
    /// tolerate after the CPU budget is exhausted before escalating from
    /// soft-cancel to hard-abandon. `None` means runtime default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_abandon_ms: Option<u64>,
    /// JIT nursery size in bytes. `None` means the runtime's default
    /// (32 MiB per pattern_runtime's `TidepoolSession::open`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nursery_size: Option<usize>,
    /// Free-form persona metadata that hasn't earned a first-class field
    /// yet. Phase 4+ may promote particular keys to named fields.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl PersonaConfig {
    /// Build a minimal config with only the required fields; optional knobs
    /// default to `None` (runtime chooses) and `extra` defaults to `null`.
    pub fn new(
        agent_id: impl Into<AgentId>,
        name: impl Into<smol_str::SmolStr>,
        program: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            name: name.into(),
            program: program.into(),
            wall_budget_ms: None,
            cpu_budget_ms: None,
            hard_abandon_ms: None,
            nursery_size: None,
            extra: serde_json::Value::Null,
        }
    }

    /// Set the per-turn wall-clock budget in milliseconds.
    pub fn with_wall_budget_ms(mut self, ms: u64) -> Self {
        self.wall_budget_ms = Some(ms);
        self
    }

    /// Set the per-turn CPU budget in milliseconds.
    pub fn with_cpu_budget_ms(mut self, ms: u64) -> Self {
        self.cpu_budget_ms = Some(ms);
        self
    }

    /// Set the additional milliseconds of runaway compute tolerated beyond
    /// the CPU budget before hard-abandonment fires.
    pub fn with_hard_abandon_ms(mut self, ms: u64) -> Self {
        self.hard_abandon_ms = Some(ms);
        self
    }

    /// Set the JIT nursery size in bytes.
    pub fn with_nursery_size(mut self, bytes: usize) -> Self {
        self.nursery_size = Some(bytes);
        self
    }

    /// Attach free-form persona metadata.
    pub fn with_extra(mut self, extra: serde_json::Value) -> Self {
        self.extra = extra;
        self
    }
}

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
/// use pattern_core::types::ids::{AgentId, new_id};
/// use smol_str::SmolStr;
///
/// let snap = PersonaSnapshot {
///     agent_id: SmolStr::new("orual-companion"),
///     as_of_turn: new_id(),
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
/// use pattern_core::types::ids::{AgentId, new_id};
/// use smol_str::SmolStr;
///
/// let persona = PersonaSnapshot {
///     agent_id: SmolStr::new("orual-companion"),
///     as_of_turn: new_id(),
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
