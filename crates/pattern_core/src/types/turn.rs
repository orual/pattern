//! Turn boundary types: `TurnInput`, `TurnOutput`, and `TurnId`.
//!
//! A *turn* is the unit of agent execution: one activation of the agent loop,
//! from receiving caller input through producing a final reply and recording
//! all side effects. Turns are checkpointable (Phase 3) and their outputs
//! drive pseudo-message emission (Phase 5).
//!
//! # Turn contract
//!
//! Every turn begins with a [`TurnInput`] that carries the caller identity,
//! the incoming messages, and a stable [`TurnId`] assigned before the turn
//! starts. When the agent loop completes, it produces a [`TurnOutput`] that
//! collects all reply messages, the memory block writes that occurred, token
//! usage if available, and the completion timestamp.
//!
//! The [`TurnId`] serves as a checkpoint key: `block_changes_since(turn)` can
//! reconstruct exactly which blocks changed during that turn.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::block::BlockWrite;
use crate::types::caller::Caller;
use crate::types::message::Message;

/// Stable identifier for a single agent-loop activation.
///
/// `TurnId` is a UUID formatted without hyphens and prefixed with `"turn_"`.
/// It is assigned *before* the turn starts so it can be embedded in
/// checkpoints and log entries.
///
/// # Examples
///
/// ```
/// use pattern_core::types::turn::TurnId;
///
/// let id = TurnId::generate();
/// assert!(id.to_string().starts_with("turn_"));
/// let parsed: TurnId = id.to_string().parse().expect("roundtrip");
/// assert_eq!(id, parsed);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TurnId(pub String);

impl TurnId {
    /// Generate a new random `TurnId`.
    pub fn generate() -> Self {
        TurnId(format!("turn_{}", Uuid::new_v4().simple()))
    }

    /// Return the nil `TurnId` (for testing and defaults).
    pub fn nil() -> Self {
        TurnId(format!("turn_{}", Uuid::nil().simple()))
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for TurnId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(TurnId(s.to_string()))
    }
}

/// Input to a single agent turn.
///
/// Carries the caller identity, the messages being delivered to the agent, and
/// the pre-assigned [`TurnId`]. The agent loop consumes `TurnInput` at the
/// start of each activation.
///
/// # Examples
///
/// ```
/// use pattern_core::types::turn::{TurnId, TurnInput};
/// use pattern_core::types::caller::Caller;
/// use pattern_core::types::ids::UserId;
///
/// let input = TurnInput {
///     turn_id: TurnId::generate(),
///     caller: Caller::Human(UserId::generate()),
///     messages: vec![],
/// };
/// assert!(input.turn_id.to_string().starts_with("turn_"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInput {
    /// Stable identifier assigned before the turn begins.
    pub turn_id: TurnId,
    /// Who initiated this turn.
    pub caller: Caller,
    /// Messages delivered to the agent for this activation.
    pub messages: Vec<Message>,
}

/// Output produced by a completed agent turn.
///
/// Collects everything the agent loop produced: reply messages, memory block
/// writes, token usage if the provider reports it, and the wall-clock
/// completion time.
///
/// `block_writes` is the authoritative record of what changed in memory during
/// this turn. Phase 5 uses `block_writes` to generate pseudo-messages; Phase 3
/// uses `TurnId` + `block_writes` to restore checkpoints.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use pattern_core::types::turn::TurnOutput;
///
/// let output = TurnOutput {
///     messages: vec![],
///     block_writes: vec![],
///     usage: None,
///     completed_at: Timestamp::now(),
/// };
/// assert!(output.block_writes.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutput {
    /// Reply messages produced during this turn (assistant + tool responses).
    pub messages: Vec<Message>,
    /// Memory block writes that occurred during this turn, in order.
    pub block_writes: Vec<BlockWrite>,
    /// Token usage reported by the provider, if available.
    pub usage: Option<genai::chat::Usage>,
    /// Wall-clock time at which the turn completed.
    pub completed_at: Timestamp,
}
