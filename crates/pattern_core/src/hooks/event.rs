//! Core hook event types.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A hook event emitted at a specific point in Pattern's lifecycle.
///
/// Open string-tag dispatch: the `tag` field is a hierarchical string
/// (e.g. `turn.before`, `task.transitioned.done`). Subscribers match
/// via glob patterns. Adding new hook points is non-breaking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookEvent {
    /// Hierarchical event tag (e.g. `turn.before`, `memory.write`).
    pub tag: SmolStr,
    /// Event-specific payload. Subscribers deserialize lazily via
    /// `try_payload::<T>()` after matching on `tag`.
    pub payload: serde_json::Value,
    /// Contextual metadata: who, where, when.
    pub metadata: HookEventMetadata,
    /// Whether the emitter expects to wait for subscriber responses.
    pub semantics: HookSemantics,
}

impl HookEvent {
    /// Construct a notification event (fire-and-forget).
    pub fn notification(tag: impl Into<SmolStr>, payload: serde_json::Value) -> Self {
        Self {
            tag: tag.into(),
            payload,
            metadata: HookEventMetadata::now(),
            semantics: HookSemantics::Notification,
        }
    }

    /// Construct a blocking event (emitter waits for responses).
    pub fn blocking(tag: impl Into<SmolStr>, payload: serde_json::Value) -> Self {
        Self {
            tag: tag.into(),
            payload,
            metadata: HookEventMetadata::now(),
            semantics: HookSemantics::Blocking,
        }
    }

    /// Lazy typed-payload deserialization.
    pub fn try_payload<'de, T: Deserialize<'de>>(&'de self) -> Result<T, serde_json::Error> {
        T::deserialize(&self.payload)
    }
}

/// Contextual metadata attached to every hook event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookEventMetadata {
    pub session_id: Option<SmolStr>,
    pub agent_id: Option<SmolStr>,
    pub batch_id: Option<SmolStr>,
    pub partner_id: Option<SmolStr>,
    pub mount_id: Option<SmolStr>,
    /// Author kind: "Partner", "Agent", "System", "Plugin".
    pub origin_author_kind: Option<SmolStr>,
    pub emitted_at: Timestamp,
}

impl HookEventMetadata {
    /// Construct metadata with current timestamp and no context.
    pub fn now() -> Self {
        Self {
            session_id: None,
            agent_id: None,
            batch_id: None,
            partner_id: None,
            mount_id: None,
            origin_author_kind: None,
            emitted_at: Timestamp::now(),
        }
    }

    /// Set the agent_id.
    pub fn with_agent(mut self, id: impl Into<SmolStr>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// Set the session_id.
    pub fn with_session(mut self, id: impl Into<SmolStr>) -> Self {
        self.session_id = Some(id.into());
        self
    }
}

/// Whether the hook emitter waits for subscriber responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HookSemantics {
    /// Emitter awaits all subscriber responses (with per-event timeout).
    Blocking,
    /// Fire-and-forget. Emitter does not wait.
    Notification,
}

/// Subscriber response to a blocking hook event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HookResponse {
    /// Allow the operation to proceed.
    Continue,
    /// Block the operation with a reason.
    Block { reason: SmolStr },
    /// Modify the event payload (subscriber returns updated data).
    Modify(serde_json::Value),
}
