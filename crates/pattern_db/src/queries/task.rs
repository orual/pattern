//! ADHD task queries.
//!
//! These functions target the post-migration-0011 `tasks` table shape:
//! `subject` (was `title`), no `priority` column (priority lives in
//! freeform `metadata_json` on the TaskList block layer).

use chrono::Utc;
use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{Task, TaskSummary, UserTaskStatus};

// ============================================================================
// from_row implementations
// ============================================================================

impl Task {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            subject: row.get("subject")?,
            description: row.get("description")?,
            status: row.get("status")?,
            due_at: row.get("due_at")?,
            scheduled_at: row.get("scheduled_at")?,
            completed_at: row.get("completed_at")?,
            parent_task_id: row.get("parent_task_id")?,
            tags: row.get("tags")?,
            estimated_minutes: row.get("estimated_minutes")?,
            actual_minutes: row.get("actual_minutes")?,
            notes: row.get("notes")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

// ============================================================================
// Task CRUD
// ============================================================================

/// Create a new user task.
pub fn create_user_task(conn: &rusqlite::Connection, task: &Task) -> DbResult<()> {
    conn.execute(
        "INSERT INTO tasks (id, agent_id, subject, description, status, due_at, scheduled_at, completed_at, parent_task_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            task.id,
            task.agent_id,
            task.subject,
            task.description,
            task.status,
            task.due_at,
            task.scheduled_at,
            task.completed_at,
            task.parent_task_id,
            task.created_at,
            task.updated_at,
        ],
    )?;
    Ok(())
}

/// Get a user task by ID.
pub fn get_user_task(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<Task>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, subject, description, status,
                due_at, scheduled_at, completed_at, parent_task_id,
                tags, estimated_minutes, actual_minutes, notes,
                created_at, updated_at
         FROM tasks WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], Task::from_row)
        .optional()?;
    Ok(result)
}

/// List tasks for an agent (or constellation-level if agent_id is None).
pub fn list_tasks(
    conn: &rusqlite::Connection,
    agent_id: Option<&str>,
    include_completed: bool,
) -> DbResult<Vec<Task>> {
    let sql = match (agent_id, include_completed) {
        (Some(_), true) => {
            "SELECT id, agent_id, subject, description, status,
                    due_at, scheduled_at, completed_at, parent_task_id,
                    tags, estimated_minutes, actual_minutes, notes,
                    created_at, updated_at
             FROM tasks WHERE agent_id = ?1 ORDER BY due_at ASC NULLS LAST, created_at ASC"
        }
        (Some(_), false) => {
            "SELECT id, agent_id, subject, description, status,
                    due_at, scheduled_at, completed_at, parent_task_id,
                    tags, estimated_minutes, actual_minutes, notes,
                    created_at, updated_at
             FROM tasks WHERE agent_id = ?1 AND status NOT IN ('completed', 'cancelled')
             ORDER BY due_at ASC NULLS LAST, created_at ASC"
        }
        (None, true) => {
            "SELECT id, agent_id, subject, description, status,
                    due_at, scheduled_at, completed_at, parent_task_id,
                    tags, estimated_minutes, actual_minutes, notes,
                    created_at, updated_at
             FROM tasks WHERE agent_id IS NULL ORDER BY due_at ASC NULLS LAST, created_at ASC"
        }
        (None, false) => {
            "SELECT id, agent_id, subject, description, status,
                    due_at, scheduled_at, completed_at, parent_task_id,
                    tags, estimated_minutes, actual_minutes, notes,
                    created_at, updated_at
             FROM tasks WHERE agent_id IS NULL AND status NOT IN ('completed', 'cancelled')
             ORDER BY due_at ASC NULLS LAST, created_at ASC"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mut tasks = Vec::new();
    match agent_id {
        Some(aid) => {
            let rows = stmt.query_map(rusqlite::params![aid], Task::from_row)?;
            for row in rows {
                tasks.push(row?);
            }
        }
        None => {
            let rows = stmt.query_map([], Task::from_row)?;
            for row in rows {
                tasks.push(row?);
            }
        }
    }
    Ok(tasks)
}

/// Get subtasks of a parent task.
pub fn get_subtasks(conn: &rusqlite::Connection, parent_id: &str) -> DbResult<Vec<Task>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, subject, description, status,
                due_at, scheduled_at, completed_at, parent_task_id,
                tags, estimated_minutes, actual_minutes, notes,
                created_at, updated_at
         FROM tasks WHERE parent_task_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![parent_id], Task::from_row)?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    Ok(tasks)
}

/// Get tasks due soon (within the next N hours).
pub fn get_tasks_due_soon(conn: &rusqlite::Connection, hours: i64) -> DbResult<Vec<Task>> {
    let deadline = Utc::now() + chrono::Duration::hours(hours);
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, subject, description, status,
                due_at, scheduled_at, completed_at, parent_task_id,
                tags, estimated_minutes, actual_minutes, notes,
                created_at, updated_at
         FROM tasks
         WHERE due_at IS NOT NULL AND due_at <= ?1 AND status NOT IN ('completed', 'cancelled')
         ORDER BY due_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![deadline], Task::from_row)?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    Ok(tasks)
}

/// Update user task status.
pub fn update_user_task_status(
    conn: &rusqlite::Connection,
    id: &str,
    status: UserTaskStatus,
) -> DbResult<bool> {
    let now = Utc::now();
    let completed_at = if status == UserTaskStatus::Completed {
        Some(now)
    } else {
        None
    };
    let count = conn.execute(
        "UPDATE tasks SET status = ?1, completed_at = COALESCE(?2, completed_at), updated_at = ?3 WHERE id = ?4",
        rusqlite::params![status, completed_at, now, id],
    )?;
    Ok(count > 0)
}

/// Update a user task.
pub fn update_user_task(conn: &rusqlite::Connection, task: &Task) -> DbResult<bool> {
    let count = conn.execute(
        "UPDATE tasks SET subject = ?1, description = ?2, status = ?3,
             due_at = ?4, scheduled_at = ?5, completed_at = ?6,
             parent_task_id = ?7, updated_at = ?8
         WHERE id = ?9",
        rusqlite::params![
            task.subject,
            task.description,
            task.status,
            task.due_at,
            task.scheduled_at,
            task.completed_at,
            task.parent_task_id,
            task.updated_at,
            task.id
        ],
    )?;
    Ok(count > 0)
}

/// Delete a user task (and its subtasks via CASCADE).
pub fn delete_user_task(conn: &rusqlite::Connection, id: &str) -> DbResult<bool> {
    let count = conn.execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
    Ok(count > 0)
}

/// Get task summaries for quick listing.
pub fn get_task_summaries(
    conn: &rusqlite::Connection,
    agent_id: Option<&str>,
) -> DbResult<Vec<TaskSummary>> {
    let sql = match agent_id {
        Some(_) => {
            "SELECT t.id, t.subject, t.status, t.due_at, t.parent_task_id,
                    (SELECT COUNT(*) FROM tasks WHERE parent_task_id = t.id) as subtask_count
             FROM tasks t
             WHERE t.agent_id = ?1 AND t.status NOT IN ('completed', 'cancelled')
             ORDER BY t.due_at ASC NULLS LAST, t.created_at ASC"
        }
        None => {
            "SELECT t.id, t.subject, t.status, t.due_at, t.parent_task_id,
                    (SELECT COUNT(*) FROM tasks WHERE parent_task_id = t.id) as subtask_count
             FROM tasks t
             WHERE t.agent_id IS NULL AND t.status NOT IN ('completed', 'cancelled')
             ORDER BY t.due_at ASC NULLS LAST, t.created_at ASC"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mapper = |row: &rusqlite::Row| {
        Ok(TaskSummary {
            id: row.get("id")?,
            subject: row.get("subject")?,
            status: row.get("status")?,
            due_at: row.get("due_at")?,
            parent_task_id: row.get("parent_task_id")?,
            subtask_count: row.get("subtask_count")?,
        })
    };

    let mut summaries = Vec::new();
    match agent_id {
        Some(aid) => {
            let rows = stmt.query_map(rusqlite::params![aid], mapper)?;
            for row in rows {
                summaries.push(row?);
            }
        }
        None => {
            let rows = stmt.query_map([], mapper)?;
            for row in rows {
                summaries.push(row?);
            }
        }
    }
    Ok(summaries)
}
