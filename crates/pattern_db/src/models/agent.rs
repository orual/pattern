//! Agent-related models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

// ============================================================================
// Model Routing Configuration
// ============================================================================

/// Configuration for dynamic model routing.
///
/// Allows agents to switch between models from the same provider
/// based on rules (cost, latency, capability requirements).
/// Stored as JSON in the agent's config field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelRoutingConfig {
    /// Fallback models to try if primary fails (in order of preference)
    #[serde(default)]
    pub fallback_models: Vec<String>,

    /// Rules for dynamic model selection
    #[serde(default)]
    pub rules: Vec<ModelRoutingRule>,

    /// Whether to allow automatic fallback on rate limits
    #[serde(default = "default_true")]
    pub fallback_on_rate_limit: bool,

    /// Whether to allow automatic fallback on context length exceeded
    #[serde(default = "default_true")]
    pub fallback_on_context_overflow: bool,

    /// Maximum retries before giving up
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_true() -> bool {
    true
}

fn default_max_retries() -> u32 {
    2
}

/// A rule for selecting which model to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutingRule {
    /// Condition that triggers this rule
    pub condition: RoutingCondition,

    /// Model to use when condition matches
    pub model: String,

    /// Optional: override other settings when this rule matches
    pub temperature_override: Option<f32>,
    pub max_tokens_override: Option<u32>,
}

/// Conditions for model routing rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingCondition {
    /// Use this model when estimated cost exceeds threshold
    CostThreshold {
        /// Maximum cost in USD before switching
        max_usd: f32,
    },

    /// Use this model when context length exceeds threshold
    ContextLength {
        /// Minimum tokens to trigger this rule
        min_tokens: u32,
    },

    /// Use this model for specific tool calls
    ToolCall {
        /// Tool names that trigger this rule
        tools: Vec<String>,
    },

    /// Use this model during specific time windows (e.g., off-peak for expensive models)
    TimeWindow {
        /// Start hour (0-23, UTC)
        start_hour: u8,
        /// End hour (0-23, UTC)
        end_hour: u8,
    },

    /// Use this model for specific source types
    Source {
        /// Source types that trigger this rule
        sources: Vec<String>,
    },

    /// Always use this model (useful as a catch-all)
    Always,
}

// ============================================================================
// Agent Models
// ============================================================================

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

// ============================================================================
// Agent ATProto Endpoints
// ============================================================================

/// Endpoint type constant for Bluesky posting.
///
/// Used as the `endpoint_type` value in `AgentAtprotoEndpoint` for standard
/// Bluesky post/reply functionality.
pub const ENDPOINT_TYPE_BLUESKY: &str = "bluesky";

/// Links an agent to their ATProto identity for a specific endpoint type.
///
/// This enables agents to post to Bluesky or interact with ATProto services
/// using a specific identity. The DID references a session stored in auth.db.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentAtprotoEndpoint {
    /// Agent ID (references agents table)
    pub agent_id: String,

    /// ATProto DID (e.g., "did:plc:...") - references session in auth.db
    pub did: String,

    /// Type of endpoint: Use [`ENDPOINT_TYPE_BLUESKY`] for posting, 'bluesky_firehose', etc.
    pub endpoint_type: String,

    /// Session ID to use (optional, defaults to "_constellation_" if null)
    pub session_id: Option<String>,

    /// Optional JSON configuration specific to this endpoint
    pub config: Option<String>,

    /// Creation timestamp (Unix epoch seconds)
    pub created_at: i64,

    /// Last update timestamp (Unix epoch seconds)
    pub updated_at: i64,
}
