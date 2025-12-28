//! WebSocket event types for real-time updates

use pattern_core::{
    agent::AgentState,
    id::{AgentId, GroupId, MessageId, UserId},
    messages::{ChatRole, MessageContent},
};
use serde::{Deserialize, Serialize};

/// WebSocket event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSocketEvent {
    /// New message received
    MessageReceived {
        message_id: MessageId,
        agent_id: AgentId,
        role: ChatRole,
        content: MessageContent,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Agent state changed
    AgentStateChanged {
        agent_id: AgentId,
        old_state: AgentState,
        new_state: AgentState,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Agent is typing
    AgentTyping { agent_id: AgentId, is_typing: bool },

    /// Group member added
    GroupMemberAdded {
        group_id: GroupId,
        agent_id: AgentId,
        role: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Group member removed
    GroupMemberRemoved {
        group_id: GroupId,
        agent_id: AgentId,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Tool execution started
    ToolExecutionStarted {
        agent_id: AgentId,
        tool_name: String,
        tool_call_id: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Tool execution completed
    ToolExecutionCompleted {
        agent_id: AgentId,
        tool_name: String,
        tool_call_id: String,
        success: bool,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Memory updated
    MemoryUpdated {
        agent_id: AgentId,
        memory_type: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Error occurred
    Error {
        error_type: String,
        message: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Connection established
    Connected {
        user_id: UserId,
        session_id: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Heartbeat/ping
    Ping {
        timestamp: chrono::DateTime<chrono::Utc>,
    },

    /// Heartbeat/pong response
    Pong {
        timestamp: chrono::DateTime<chrono::Utc>,
    },
}

/// WebSocket command types (client to server)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSocketCommand {
    /// Subscribe to agent events
    SubscribeAgent { agent_id: AgentId },

    /// Unsubscribe from agent events
    UnsubscribeAgent { agent_id: AgentId },

    /// Subscribe to group events
    SubscribeGroup { group_id: GroupId },

    /// Unsubscribe from group events
    UnsubscribeGroup { group_id: GroupId },

    /// Subscribe to all user events
    SubscribeUser { user_id: UserId },

    /// Send typing indicator
    SetTyping { agent_id: AgentId, is_typing: bool },

    /// Heartbeat ping
    Ping,
}

/// WebSocket message wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketMessage<T> {
    /// Message ID for tracking
    pub id: uuid::Uuid,
    /// Message payload
    pub payload: T,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl<T> WebSocketMessage<T> {
    pub fn new(payload: T) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            payload,
            timestamp: chrono::Utc::now(),
        }
    }
}

/// Subscription confirmation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionConfirmation {
    pub subscription_type: String,
    pub resource_id: String,
    pub success: bool,
    pub message: Option<String>,
}
