//! Coordination-related models.
//!
//! These models support cross-agent coordination:
//! - Activity stream for constellation-wide event logging
//! - Summaries for agent catch-up after hibernation
//! - Tasks for structured work assignment
//! - Handoff notes for agent-to-agent communication

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// An event in the constellation's activity stream.
///
/// The activity stream provides a unified timeline of events for
/// coordinating agents and enabling catch-up for returning agents.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// Unique identifier
    pub id: String,

    /// When the event occurred
    pub timestamp: DateTime<Utc>,

    /// Agent that caused the event (None for system events)
    pub agent_id: Option<String>,

    /// Event type
    pub event_type: ActivityEventType,

    /// Event-specific details as JSON
    pub details: Json<serde_json::Value>,

    /// Importance level for filtering
    pub importance: Option<EventImportance>,
}

/// Activity event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ActivityEventType {
    /// Agent sent a message
    MessageSent,
    /// Agent used a tool
    ToolUsed,
    /// Memory was updated
    MemoryUpdated,
    /// Task was created/updated
    TaskChanged,
    /// Agent status changed (activated, hibernated, etc.)
    AgentStatusChanged,
    /// External event (Discord message, Bluesky post, etc.)
    ExternalEvent,
    /// Coordination event (handoff, delegation, etc.)
    Coordination,
    /// System event (startup, shutdown, error, etc.)
    System,
}

/// Event importance levels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EventImportance {
    /// Routine event, can be skipped in summaries
    Low,
    /// Normal event, included in standard summaries
    Medium,
    /// Important event, always included in summaries
    High,
    /// Critical event, requires attention
    Critical,
}

impl Default for EventImportance {
    fn default() -> Self {
        Self::Medium
    }
}

/// Per-agent activity summary.
///
/// LLM-generated summary of an agent's recent activity,
/// used to help other agents understand what this agent has been doing.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agent this summary is for (also the primary key)
    pub agent_id: String,

    /// LLM-generated summary
    pub summary: String,

    /// Number of messages covered by this summary
    pub messages_covered: i64,

    /// When this summary was generated
    pub generated_at: DateTime<Utc>,

    /// When the agent was last active
    pub last_active: DateTime<Utc>,
}

/// Constellation-wide summary.
///
/// Periodic roll-up of activity across all agents,
/// used for long-term context and catch-up.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ConstellationSummary {
    /// Unique identifier
    pub id: String,

    /// Start of the summarized period
    pub period_start: DateTime<Utc>,

    /// End of the summarized period
    pub period_end: DateTime<Utc>,

    /// LLM-generated summary
    pub summary: String,

    /// Key decisions made during this period
    pub key_decisions: Option<Json<Vec<String>>>,

    /// Open threads/topics that need follow-up
    pub open_threads: Option<Json<Vec<String>>>,

    /// When this summary was created
    pub created_at: DateTime<Utc>,
}

/// A notable event flagged for long-term memory.
///
/// Unlike regular activity events, notable events are explicitly
/// preserved for historical context and agent training.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct NotableEvent {
    /// Unique identifier
    pub id: String,

    /// When the event occurred
    pub timestamp: DateTime<Utc>,

    /// Type of event
    pub event_type: String,

    /// Human-readable description
    pub description: String,

    /// Agents involved in this event
    pub agents_involved: Option<Json<Vec<String>>>,

    /// Importance level
    pub importance: EventImportance,

    /// When this was recorded
    pub created_at: DateTime<Utc>,
}

/// A coordination task.
///
/// Structured task assignment for cross-agent work.
/// More formal than handoff notes, used for tracked deliverables.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CoordinationTask {
    /// Unique identifier
    pub id: String,

    /// Task description
    pub description: String,

    /// Agent assigned to this task (None = unassigned)
    pub assigned_to: Option<String>,

    /// Task status
    pub status: TaskStatus,

    /// Task priority
    pub priority: TaskPriority,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

/// Task status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is pending, not yet started
    Pending,
    /// Task is in progress
    InProgress,
    /// Task is completed
    Completed,
    /// Task was cancelled
    Cancelled,
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// Task priority.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    /// Low priority
    Low,
    /// Medium priority (default)
    Medium,
    /// High priority
    High,
    /// Urgent priority
    Urgent,
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self::Medium
    }
}

/// A handoff note from one agent to another.
///
/// Used for informal agent-to-agent communication,
/// like leaving a note for the next shift.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct HandoffNote {
    /// Unique identifier
    pub id: String,

    /// Agent that left the note
    pub from_agent: String,

    /// Target agent (None = for any agent)
    pub to_agent: Option<String>,

    /// Note content
    pub content: String,

    /// When the note was created
    pub created_at: DateTime<Utc>,

    /// When the note was read (None = unread)
    pub read_at: Option<DateTime<Utc>>,
}

/// Coordination key-value state entry.
///
/// Flexible shared state for coordination patterns.
/// Used for things like round-robin counters, vote tallies, etc.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CoordinationState {
    /// Key for this state entry
    pub key: String,

    /// Value as JSON
    pub value: Json<serde_json::Value>,

    /// When this was last updated
    pub updated_at: DateTime<Utc>,

    /// Who updated it last
    pub updated_by: Option<String>,
}
