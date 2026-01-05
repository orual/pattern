//! ADHD task models.
//!
//! User-facing task management with ADHD-aware features:
//! - Hierarchical breakdown (big tasks → small steps)
//! - Flexible scheduling (due dates, scheduled times)
//! - Priority levels with urgency distinction
//!
//! Distinct from CoordinationTask which is for internal agent work assignment.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// A user-facing task.
///
/// Tasks can be assigned to agents or be constellation-level.
/// They support hierarchical breakdown which is crucial for ADHD:
/// large overwhelming tasks can be broken into smaller, actionable steps.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier
    pub id: String,

    /// Agent responsible for this task (None = constellation-level)
    pub agent_id: Option<String>,

    /// Task title (short, actionable)
    pub title: String,

    /// Detailed description (optional)
    pub description: Option<String>,

    /// Current status
    pub status: UserTaskStatus,

    /// Priority level
    pub priority: UserTaskPriority,

    /// When the task is due (hard deadline)
    pub due_at: Option<DateTime<Utc>>,

    /// When the task is scheduled to be worked on
    pub scheduled_at: Option<DateTime<Utc>>,

    /// When the task was completed
    pub completed_at: Option<DateTime<Utc>>,

    /// Parent task for hierarchy (None = top-level)
    pub parent_task_id: Option<String>,

    /// Optional tags/labels as JSON array
    pub tags: Option<Json<Vec<String>>>,

    /// Estimated duration in minutes (for time-boxing)
    pub estimated_minutes: Option<i64>,

    /// Actual duration in minutes (filled on completion)
    pub actual_minutes: Option<i64>,

    /// Optional notes/context
    pub notes: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

/// User task status.
///
/// More nuanced than coordination task status to support ADHD workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum UserTaskStatus {
    /// Task exists but isn't ready to work on yet
    /// (e.g., waiting for something, needs breakdown)
    Backlog,

    /// Task is ready to be worked on
    Pending,

    /// Currently being worked on
    InProgress,

    /// Blocked by external factor
    Blocked,

    /// Task is done
    Completed,

    /// Task was intentionally skipped/dropped
    Cancelled,

    /// Task was deferred to a later time
    Deferred,
}

impl Default for UserTaskStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl std::fmt::Display for UserTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backlog => write!(f, "backlog"),
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in progress"),
            Self::Blocked => write!(f, "blocked"),
            Self::Completed => write!(f, "completed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Deferred => write!(f, "deferred"),
        }
    }
}

/// User task priority.
///
/// Distinguishes between importance and urgency (Eisenhower matrix style).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum UserTaskPriority {
    /// Can wait, nice to have
    Low,

    /// Normal priority, should get done
    Medium,

    /// Important, prioritize this
    High,

    /// Time-sensitive AND important - do this now
    Urgent,

    /// Critical blocker - everything else waits
    Critical,
}

impl Default for UserTaskPriority {
    fn default() -> Self {
        Self::Medium
    }
}

impl std::fmt::Display for UserTaskPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Urgent => write!(f, "urgent"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

/// Lightweight task projection for lists.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TaskSummary {
    /// Task ID
    pub id: String,

    /// Task title
    pub title: String,

    /// Current status
    pub status: UserTaskStatus,

    /// Priority level
    pub priority: UserTaskPriority,

    /// Due date if set
    pub due_at: Option<DateTime<Utc>>,

    /// Parent task ID for hierarchy display
    pub parent_task_id: Option<String>,

    /// Number of subtasks (computed)
    pub subtask_count: Option<i64>,
}
