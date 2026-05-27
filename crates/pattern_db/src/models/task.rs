// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! User-facing ADHD task model.
//!
//! `Task` and `UserTaskStatus` are retained for migration compatibility and
//! for potential re-use by `pattern_nd` when that crate is re-integrated.
//! `UserTaskPriority` and the CRUD query functions (`create_user_task`,
//! `get_user_task`, etc.) were removed on 2026-04-23 — they had no active
//! callers in the current workspace. See `queries/task.rs` module doc for
//! the full removal rationale.
//!
//! ## Schema alignment note (migration 0011)
//!
//! Migration 0011 renamed `title` → `subject` (aligning with `TaskItem.subject`
//! in the CRDT layer) and dropped the `priority` column (priority is now
//! carried as freeform metadata JSON in the TaskList block layer).
//! `Task` here reflects the post-migration shape.

use crate::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A user-facing task.
///
/// Tasks can be assigned to agents or be constellation-level.
/// They support hierarchical breakdown which is crucial for ADHD:
/// large overwhelming tasks can be broken into smaller, actionable steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier.
    pub id: String,

    /// Agent responsible for this task (None = constellation-level).
    pub agent_id: Option<String>,

    /// Brief imperative description of what needs to be done.
    ///
    /// Renamed from `title` in migration 0011 to align with `TaskItem.subject`.
    pub subject: String,

    /// Detailed description (optional).
    pub description: Option<String>,

    /// Current status.
    pub status: UserTaskStatus,

    /// When the task is due (hard deadline).
    pub due_at: Option<DateTime<Utc>>,

    /// When the task is scheduled to be worked on.
    pub scheduled_at: Option<DateTime<Utc>>,

    /// When the task was completed.
    pub completed_at: Option<DateTime<Utc>>,

    /// Parent task for hierarchy (None = top-level).
    pub parent_task_id: Option<String>,

    /// Optional tags/labels as JSON array.
    pub tags: Option<Json<Vec<String>>>,

    /// Estimated duration in minutes (for time-boxing).
    pub estimated_minutes: Option<i64>,

    /// Actual duration in minutes (filled on completion).
    pub actual_minutes: Option<i64>,

    /// Optional notes/context.
    pub notes: Option<String>,

    /// Creation timestamp.
    pub created_at: DateTime<Utc>,

    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// User task status.
///
/// More nuanced than coordination task status to support ADHD workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum UserTaskStatus {
    /// Task exists but isn't ready to work on yet
    /// (e.g., waiting for something, needs breakdown)
    Backlog,

    /// Task is ready to be worked on
    #[default]
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
