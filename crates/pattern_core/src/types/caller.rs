//! Caller identity for memory writes and turn initiation.
//!
//! [`Caller`] identifies *who* is responsible for starting a turn or writing
//! to a memory block. It is intentionally minimal: transport-specific identity
//! (Discord user IDs, CLI session tokens, etc.) lives on the accompanying
//! [`super::turn::TurnInput`] or the message itself.

use serde::{Deserialize, Serialize};

use crate::types::ids::{AgentId, UserId};

/// The initiator of a turn or a memory-block write.
///
/// This enum is `#[non_exhaustive]` because future subsystems may add new
/// caller kinds without breaking existing match arms. Known future candidates
/// include `Plugin(PluginId)` (when the plugin subsystem ships) and
/// `Scheduler` (for sleeptime-triggered turns). Callers should use a
/// wildcard arm (`_ => …`) when matching exhaustively is not required.
///
/// # Examples
///
/// ```
/// use pattern_core::types::caller::Caller;
/// use pattern_core::types::ids::{AgentId, UserId};
///
/// let human = Caller::Human(UserId::generate());
/// let agent = Caller::Agent(AgentId::new("orual-companion"));
///
/// match &human {
///     Caller::Human(id) => assert!(id.to_string().starts_with("user:")),
///     Caller::Agent(_) => unreachable!(),
///     _ => {}
/// }
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Caller {
    /// An agent acting on its own, typically mid-loop or via scheduled wake.
    Agent(AgentId),
    /// A human interacting via some transport (CLI, Discord, etc.).
    ///
    /// Transport-specific identity lives on the accompanying message or
    /// `TurnInput`; this variant carries only the stable `UserId`.
    Human(UserId),
}

impl std::fmt::Display for Caller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Caller::Agent(id) => write!(f, "agent:{}", id),
            Caller::Human(id) => write!(f, "human:{}", id),
        }
    }
}
