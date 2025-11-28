//! Agent-related models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// An agent in the constellation.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Agent {
    /// Unique identifier
    pub id: String,

    /// Human-readable name (unique within constellation)
    pub name: String,

    /// Optional description
    pub description: Option<String>,

    /// Model provider: 'anthropic', 'openai', 'google', etc.
    pub model_provider: String,

    /// Model name: 'claude-3-5-sonnet', 'gpt-4o', etc.
    pub model_name: String,

    /// System prompt / base instructions
    pub system_prompt: String,

    /// Agent configuration as JSON
    /// Contains: max_messages, compression_threshold, temperature, etc.
    pub config: Json<serde_json::Value>,

    /// List of enabled tool names
    pub enabled_tools: Json<Vec<String>>,

    /// Tool-specific rules as JSON (optional)
    pub tool_rules: Option<Json<serde_json::Value>>,

    /// Agent status
    pub status: AgentStatus,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

/// Agent status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// Agent is active and can process messages
    Active,
    /// Agent is hibernated (not processing, but data preserved)
    Hibernated,
    /// Agent is archived (read-only)
    Archived,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self::Active
    }
}

/// An agent group for coordination.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentGroup {
    /// Unique identifier
    pub id: String,

    /// Human-readable name (unique within constellation)
    pub name: String,

    /// Optional description
    pub description: Option<String>,

    /// Coordination pattern type
    pub pattern_type: PatternType,

    /// Pattern-specific configuration as JSON
    pub pattern_config: Json<serde_json::Value>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

/// Coordination pattern types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    /// Round-robin message distribution
    RoundRobin,
    /// Dynamic routing based on selector
    Dynamic,
    /// Pipeline of sequential processing
    Pipeline,
    /// Supervisor delegates to workers
    Supervisor,
    /// Voting-based consensus
    Voting,
    /// Background monitoring (sleeptime)
    Sleeptime,
}

/// Group membership.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct GroupMember {
    /// Group ID
    pub group_id: String,

    /// Agent ID
    pub agent_id: String,

    /// Role within the group (pattern-specific)
    pub role: Option<GroupMemberRole>,

    /// When the agent joined the group
    pub joined_at: DateTime<Utc>,
}

/// Member roles within a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum GroupMemberRole {
    /// Supervisor role (for supervisor pattern)
    Supervisor,
    /// Worker role
    Worker,
    /// Observer (receives messages but doesn't respond)
    Observer,
}
