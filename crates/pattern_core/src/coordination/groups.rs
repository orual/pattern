//! Agent groups and constellation management

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::types::{CoordinationPattern, GroupMemberRole, GroupState};
use crate::{
    AgentId, Result, UserId,
    agent::Agent,
    id::{ConstellationId, GroupId, MessageId},
    messages::{Message, Response},
};
use pattern_db::Agent as AgentModel;

/// A constellation represents a collection of agents working together for a specific user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constellation {
    /// Unique identifier for this constellation
    pub id: ConstellationId,
    /// The user who owns this constellation of agents
    pub owner_id: UserId,
    /// Human-readable name
    pub name: String,
    /// Description of this constellation's purpose
    pub description: Option<String>,
    /// When this constellation was created
    pub created_at: DateTime<Utc>,
    /// Last update time
    pub updated_at: DateTime<Utc>,
    /// Whether this constellation is active
    pub is_active: bool,

    // Relations
    /// Agents in this constellation with membership metadata
    pub agents: Vec<(AgentModel, ConstellationMembership)>,

    /// Groups within this constellation
    pub groups: Vec<GroupId>,
}

/// Edge entity for constellation membership

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstellationMembership {
    pub constellation_id: ConstellationId,
    pub agent_id: AgentId,
    /// When this agent joined the constellation
    pub joined_at: DateTime<Utc>,
    /// Is this the primary orchestrator agent?
    pub is_primary: bool,
}

/// A group of agents that coordinate together
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentGroup {
    /// Unique identifier for this group
    pub id: GroupId,
    /// Human-readable name for this group
    pub name: String,
    /// Description of this group's purpose
    pub description: String,
    /// How agents in this group coordinate their actions
    pub coordination_pattern: CoordinationPattern,
    /// When this group was created
    pub created_at: DateTime<Utc>,
    /// Last update time
    pub updated_at: DateTime<Utc>,
    /// Whether this group is active
    pub is_active: bool,

    /// Pattern-specific state stored here for now
    pub state: GroupState,

    // Relations
    /// Members of this group with their roles
    pub members: Vec<(AgentModel, GroupMembership)>,
}

/// Edge entity for group membership
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMembership {
    pub agent_id: AgentId,
    pub group_id: GroupId,
    /// When this agent joined the group
    pub joined_at: DateTime<Utc>,
    /// Role of this agent in the group
    pub role: GroupMemberRole,
    /// Whether this member is active
    pub is_active: bool,
    /// Capabilities this agent brings to the group
    pub capabilities: Vec<String>,
}

/// Response from a group coordination
#[derive(Debug, Clone)]
pub struct GroupResponse {
    /// Which group handled this
    pub group_id: GroupId,
    /// Which coordination pattern was used
    pub pattern: String,
    /// Responses from individual agents
    pub responses: Vec<AgentResponse>,
    /// Time taken to process
    pub execution_time: std::time::Duration,
    /// Any state changes that occurred
    pub state_changes: Option<GroupState>,
}

/// Response from a single agent in a group
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// Which agent responded
    pub agent_id: AgentId,
    /// Their response
    pub response: Response,
    /// When they responded
    pub responded_at: DateTime<Utc>,
}

/// Events emitted during group message processing
#[derive(Debug, Clone)]
pub enum GroupResponseEvent {
    /// Processing has started
    Started {
        group_id: GroupId,
        pattern: String,
        agent_count: usize,
    },

    /// An agent is starting to process the message
    AgentStarted {
        agent_id: AgentId,
        agent_name: String,
        role: GroupMemberRole,
    },

    /// Text chunk from an agent
    TextChunk {
        agent_id: AgentId,
        text: String,
        is_final: bool,
    },

    /// Reasoning chunk from an agent
    ReasoningChunk {
        agent_id: AgentId,
        text: String,
        is_final: bool,
    },

    /// Tool call started by an agent
    ToolCallStarted {
        agent_id: AgentId,
        call_id: String,
        fn_name: String,
        args: serde_json::Value,
    },

    /// Tool call completed by an agent
    ToolCallCompleted {
        agent_id: AgentId,
        call_id: String,
        result: std::result::Result<String, String>,
    },

    /// An agent has completed processing
    AgentCompleted {
        agent_id: AgentId,
        agent_name: String,
        message_id: Option<MessageId>,
    },

    /// Group processing is complete
    Complete {
        group_id: GroupId,
        pattern: String,
        execution_time: std::time::Duration,
        agent_responses: Vec<AgentResponse>,
        state_changes: Option<GroupState>,
    },

    /// Error occurred during processing
    Error {
        agent_id: Option<AgentId>,
        message: String,
        recoverable: bool,
    },
}

/// Trait for implementing group coordination managers
#[async_trait]
pub trait GroupManager: Send + Sync {
    /// Route a message through this group, returning a stream of events
    async fn route_message(
        &self,
        group: &AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>>;

    /// Update group state after execution
    async fn update_state(
        &self,
        current_state: &GroupState,
        response: &GroupResponse,
    ) -> Result<Option<GroupState>>;
}

/// Agent with group membership metadata
#[derive(Clone)]
pub struct AgentWithMembership<A> {
    pub agent: A,
    pub membership: GroupMembership,
}
