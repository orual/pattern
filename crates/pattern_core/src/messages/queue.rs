use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::{QueuedMessageId, WakeupId};
use crate::runtime::router::MessageOrigin;
use crate::{AgentId, UserId};

/// A queued message for agent-to-agent or user-to-agent communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Unique identifier for this queued message
    pub id: QueuedMessageId,

    /// Agent ID sending the message (None if from user)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_agent: Option<AgentId>,

    /// User ID sending the message (None if from agent)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_user: Option<UserId>,

    /// Target agent ID
    pub to_agent: AgentId,

    /// Message content (could be text or structured data)
    pub content: String,

    /// Optional metadata (e.g., priority, type, context)
    #[serde(default)]
    pub metadata: Value,

    /// Call chain for loop prevention (list of agent IDs that have processed this message)
    #[serde(default)]
    pub call_chain: Vec<AgentId>,

    /// Whether this message has been read/processed
    #[serde(default)]
    pub read: bool,

    /// When this message was created
    pub created_at: DateTime<Utc>,

    /// When this message was read (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,

    pub origin: Option<MessageOrigin>,
}

impl QueuedMessage {
    /// Create a new agent-to-agent message
    pub fn agent_to_agent(
        from: AgentId,
        to: AgentId,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Self {
        let call_chain = vec![from.clone()];

        Self {
            id: QueuedMessageId::generate(),
            from_agent: Some(from),
            from_user: None,
            to_agent: to,
            content,
            metadata: metadata.unwrap_or_else(|| Value::Object(Default::default())),
            call_chain,
            read: false,
            created_at: Utc::now(),
            read_at: None,
            origin,
        }
    }

    /// Create a new user-to-agent message
    pub fn user_to_agent(
        from: UserId,
        to: AgentId,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Self {
        Self {
            id: QueuedMessageId::generate(),
            from_agent: None,
            from_user: Some(from),
            to_agent: to,
            content,
            metadata: metadata.unwrap_or_else(|| Value::Object(Default::default())),
            call_chain: vec![], // No call chain for user messages
            read: false,
            created_at: Utc::now(),
            read_at: None,
            origin,
        }
    }

    /// Check if an agent is already in the call chain (for loop prevention)
    pub fn is_in_call_chain(&self, agent_id: &AgentId) -> bool {
        self.call_chain.contains(agent_id)
    }

    /// Count how many times an agent appears in the call chain
    pub fn count_in_call_chain(&self, agent_id: &AgentId) -> usize {
        self.call_chain.iter().filter(|id| *id == agent_id).count()
    }

    /// Add an agent to the call chain
    pub fn add_to_call_chain(&mut self, agent_id: AgentId) {
        self.call_chain.push(agent_id);
    }

    /// Mark this message as read
    pub fn mark_read(&mut self) {
        self.read = true;
        self.read_at = Some(Utc::now());
    }
}

/// A scheduled wakeup for an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledWakeup {
    /// Unique identifier
    pub id: WakeupId,

    /// Agent to wake up
    pub agent_id: AgentId,

    /// When to wake up the agent
    pub scheduled_for: DateTime<Utc>,

    /// Reason for the wakeup (shown to agent)
    pub reason: String,

    /// Optional recurring interval in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurring_seconds: Option<i64>,

    /// Whether this wakeup is active
    #[serde(default = "default_true")]
    pub active: bool,

    /// When this wakeup was created
    pub created_at: DateTime<Utc>,

    /// Last time this wakeup was triggered
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_triggered: Option<DateTime<Utc>>,

    /// Additional metadata
    #[serde(default)]
    pub metadata: Value,
}

fn default_true() -> bool {
    true
}

impl ScheduledWakeup {
    /// Create a one-time wakeup
    pub fn once(agent_id: AgentId, scheduled_for: DateTime<Utc>, reason: String) -> Self {
        Self {
            id: WakeupId::generate(),
            agent_id,
            scheduled_for,
            reason,
            recurring_seconds: None,
            active: true,
            created_at: Utc::now(),
            last_triggered: None,
            metadata: Value::Object(Default::default()),
        }
    }

    /// Create a recurring wakeup
    pub fn recurring(
        agent_id: AgentId,
        scheduled_for: DateTime<Utc>,
        reason: String,
        interval_seconds: i64,
    ) -> Self {
        Self {
            id: WakeupId::generate(),
            agent_id,
            scheduled_for,
            reason,
            recurring_seconds: Some(interval_seconds),
            active: true,
            created_at: Utc::now(),
            last_triggered: None,
            metadata: Value::Object(Default::default()),
        }
    }

    /// Check if this wakeup is due
    pub fn is_due(&self) -> bool {
        self.active && Utc::now() >= self.scheduled_for
    }

    /// Update for next recurrence (if recurring)
    pub fn update_for_next_recurrence(&mut self) {
        if let Some(seconds) = self.recurring_seconds {
            self.last_triggered = Some(self.scheduled_for);
            self.scheduled_for = self.scheduled_for + chrono::Duration::seconds(seconds);
        } else {
            // One-time wakeup, deactivate after triggering
            self.active = false;
            self.last_triggered = Some(Utc::now());
        }
    }
}
