//! SQLite row types for the `tasks` and `task_edges` block-index tables.
//!
//! These structs mirror the post-migration-0011 schema and are used by the
//! TaskList block reconciler in `pattern_memory`. They are distinct from
//! [`crate::models::Task`] (the user-facing ADHD task model) and
//! [`crate::models::UserTaskStatus`] (the snake_case user-task status).
//!
//! ## Dependency isolation
//!
//! `pattern_db` cannot depend on `pattern_core` (circular: `pattern_core`
//! already depends on `pattern_db`). [`TaskStatus`] defined here mirrors
//! `pattern_core::types::memory_types::TaskStatus` with identical variants
//! and the same kebab-case SQLite encoding. The conversion between the two
//! types is the responsibility of `pattern_memory`, which can see both.
//!
//! ## Column order for SELECT statements
//!
//! [`TaskRow::from_row`] uses named column access (`row.get("col")`), so
//! the SELECT column order does not matter. Callers in Task 6 may list
//! columns in any order as long as the name strings match the post-migration
//! schema.
//!
//! Post-migration-0011 `tasks` columns referenced by [`TaskRow`]:
//! `rowid`, `id`, `agent_id`, `subject`, `description`, `status`,
//! `due_at`, `scheduled_at`, `completed_at`, `parent_task_id`,
//! `block_handle`, `task_item_id`, `owner_agent_id`, `comments_json`,
//! `created_at`, `updated_at`.
//!
//! `task_edges` columns (all four):
//! `source_block`, `source_item`, `target_block`, `target_item`.

use chrono::{DateTime, Utc};

// region: TaskStatus

/// Lifecycle state of a task item stored in the `tasks` block-index table.
///
/// Stored in SQLite as a kebab-case TEXT column (e.g. `"pending"`,
/// `"in-progress"`). This mirrors `pattern_core::types::memory_types::TaskStatus`
/// variant-for-variant; the conversion is done in `pattern_memory` which can
/// see both crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TaskStatus {
    /// Task has not been started.
    Pending,
    /// Task is actively being worked on.
    InProgress,
    /// Task cannot proceed until an external dependency is resolved.
    Blocked,
    /// Task finished successfully.
    Completed,
    /// Task will not be done.
    Cancelled,
}

impl TaskStatus {
    /// Returns the canonical kebab-case string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in-progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = UnknownTaskStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in-progress" => Ok(Self::InProgress),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(UnknownTaskStatusError(other.to_owned())),
        }
    }
}

/// Error returned when an unknown task status string is read from SQLite.
#[derive(Debug)]
pub struct UnknownTaskStatusError(pub String);

impl std::fmt::Display for UnknownTaskStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown task status '{}'", self.0)
    }
}

impl std::error::Error for UnknownTaskStatusError {}

// endregion: TaskStatus

// region: TaskRow

/// A row from the post-migration-0011 `tasks` table, representing a task item
/// derived from a `TaskList` loro block.
///
/// Fields that are exclusive to the legacy user-task surface (`tags`,
/// `estimated_minutes`, `actual_minutes`, `notes`) are deliberately omitted
/// here — they are accessed via [`crate::models::Task`] instead.
///
/// See module-level docs for the column-order note on SELECT statements.
#[derive(Debug, Clone)]
pub struct TaskRow {
    /// SQLite `rowid` (implicit integer primary key).
    pub rowid: i64,
    /// Task identifier (human-readable slug or UUID).
    pub id: String,
    /// Legacy "responsible agent" field (pre-v3; may be None for new rows).
    pub agent_id: Option<String>,
    /// Brief imperative description (aligns with `TaskItem.subject`).
    pub subject: String,
    /// Extended markdown body. `None` for legacy tasks or when omitted.
    pub description: Option<String>,
    /// Lifecycle state. Stored as kebab-case TEXT.
    pub status: TaskStatus,
    /// Hard deadline (stored as RFC 3339 TEXT in SQLite; chrono handles this).
    pub due_at: Option<DateTime<Utc>>,
    /// When the task is scheduled to be worked on.
    pub scheduled_at: Option<DateTime<Utc>>,
    /// When the task was completed (`None` if not yet done).
    pub completed_at: Option<DateTime<Utc>>,
    /// Parent task ID for hierarchy (NULL = top-level).
    pub parent_task_id: Option<String>,
    /// Handle of the TaskList loro block that sourced this row.
    /// `None` for legacy/manually-created tasks not linked to a block.
    pub block_handle: Option<String>,
    /// ID of the specific item within the TaskList block.
    /// `None` for legacy tasks not linked to a block item.
    pub task_item_id: Option<String>,
    /// Agent that owns this task item (may differ from `agent_id`).
    pub owner_agent_id: Option<String>,
    /// Serialised JSON array of comment objects. Never NULL; defaults to `'[]'`.
    pub comments_json: String,
    /// Row creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last-updated timestamp.
    pub updated_at: DateTime<Utc>,
}

impl TaskRow {
    /// Construct a [`TaskRow`] from a rusqlite [`Row`].
    ///
    /// Uses named column access so the caller's SELECT statement may list
    /// columns in any order. All column names must be present in the result
    /// set; missing columns produce a rusqlite [`InvalidColumnName`] error.
    ///
    /// [`Row`]: rusqlite::Row
    /// [`InvalidColumnName`]: rusqlite::Error::InvalidColumnName
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            rowid: row.get("rowid")?,
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            subject: row.get("subject")?,
            description: row.get("description")?,
            status: row.get("status")?,
            due_at: row.get("due_at")?,
            scheduled_at: row.get("scheduled_at")?,
            completed_at: row.get("completed_at")?,
            parent_task_id: row.get("parent_task_id")?,
            block_handle: row.get("block_handle")?,
            task_item_id: row.get("task_item_id")?,
            owner_agent_id: row.get("owner_agent_id")?,
            comments_json: row.get("comments_json")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

// endregion: TaskRow

// region: TaskEdgeRow

/// A row from the `task_edges` table.
///
/// Edges are single-direction: `source_item` blocks `target_block` /
/// `target_item`. Reverse lookups are answered by querying
/// `target_block + target_item` indexes rather than storing duplicate rows.
///
/// All fields are plain `String` rather than domain newtypes because
/// `pattern_db` cannot depend on `pattern_core` (circular dependency).
/// Callers in `pattern_memory` convert to/from
/// `pattern_core::types::block::BlockHandle` etc.
#[derive(Debug, Clone)]
pub struct TaskEdgeRow {
    /// Handle of the TaskList block containing the source item.
    pub source_block: String,
    /// ID of the source task item within `source_block`.
    pub source_item: String,
    /// Handle of the TaskList block containing the target.
    pub target_block: String,
    /// ID of the target item within `target_block`.
    /// `None` means the edge targets the entire block (block-level ref).
    pub target_item: Option<String>,
}

impl TaskEdgeRow {
    /// Construct a [`TaskEdgeRow`] from a rusqlite [`Row`].
    ///
    /// Column names must match the `task_edges` schema exactly:
    /// `source_block`, `source_item`, `target_block`, `target_item`.
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            source_block: row.get("source_block")?,
            source_item: row.get("source_item")?,
            target_block: row.get("target_block")?,
            target_item: row.get("target_item")?,
        })
    }
}

// endregion: TaskEdgeRow

// region: tests

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::migrations::run_memory_migrations;

    // ---- TaskStatus round-trips ----

    fn round_trip_status(value: TaskStatus, expected_str: &str) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (v TEXT)", []).unwrap();
        conn.execute(
            "INSERT INTO t (v) VALUES (?1)",
            [&value as &dyn rusqlite::types::ToSql],
        )
        .unwrap();

        let stored: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(
            stored, expected_str,
            "stored kebab-case mismatch for {value:?}"
        );

        let loaded: TaskStatus = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(loaded, value, "round-trip variant mismatch");
    }

    #[test]
    fn task_status_pending_round_trips() {
        round_trip_status(TaskStatus::Pending, "pending");
    }

    #[test]
    fn task_status_in_progress_round_trips() {
        round_trip_status(TaskStatus::InProgress, "in-progress");
    }

    #[test]
    fn task_status_blocked_round_trips() {
        round_trip_status(TaskStatus::Blocked, "blocked");
    }

    #[test]
    fn task_status_completed_round_trips() {
        round_trip_status(TaskStatus::Completed, "completed");
    }

    #[test]
    fn task_status_cancelled_round_trips() {
        round_trip_status(TaskStatus::Cancelled, "cancelled");
    }

    #[test]
    fn task_status_unknown_variant_returns_error() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (v TEXT)", []).unwrap();
        conn.execute("INSERT INTO t (v) VALUES ('unknown')", [])
            .unwrap();

        let result = conn.query_row("SELECT v FROM t", [], |r| r.get::<_, TaskStatus>(0));
        assert!(
            result.is_err(),
            "expected Err for unknown status 'unknown', got Ok"
        );
    }

    #[test]
    fn task_status_garbage_returns_error() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (v TEXT)", []).unwrap();
        conn.execute("INSERT INTO t (v) VALUES ('in_progress')", [])
            .unwrap();

        // snake_case variant should be rejected (the db format is kebab-case).
        let result = conn.query_row("SELECT v FROM t", [], |r| r.get::<_, TaskStatus>(0));
        assert!(
            result.is_err(),
            "expected Err for snake_case 'in_progress', got Ok"
        );
    }

    // ---- TaskRow from_row smoke test ----

    /// Open an in-memory DB with all migrations applied and insert a
    /// post-migration-0011 task row, then read it back via `TaskRow::from_row`.
    fn fresh_migrated_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();
        conn
    }

    #[test]
    fn task_row_from_row_smoke() {
        let conn = fresh_migrated_db();

        // Insert an agent to satisfy the FK constraint.
        conn.execute(
            "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
             VALUES ('agent-1', 'TestAgent', 'anthropic', 'claude-3', '', '{}', '[]', 'active', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO tasks (id, agent_id, subject, description, status, due_at, scheduled_at, completed_at, parent_task_id, block_handle, task_item_id, owner_agent_id, comments_json, created_at, updated_at)
             VALUES ('t-1', 'agent-1', 'write tests', 'write the test suite', 'in-progress', NULL, NULL, NULL, NULL, 'task-list-handle', 'item-001', 'agent-1', '[{\"author\":\"agent-1\",\"text\":\"started\"}]', '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();

        let row = conn
            .query_row(
                "SELECT rowid, id, agent_id, subject, description, status,
                        due_at, scheduled_at, completed_at, parent_task_id,
                        block_handle, task_item_id, owner_agent_id,
                        comments_json, created_at, updated_at
                 FROM tasks WHERE id = 't-1'",
                [],
                TaskRow::from_row,
            )
            .unwrap();

        assert_eq!(row.id, "t-1");
        assert_eq!(row.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(row.subject, "write tests");
        assert_eq!(row.description.as_deref(), Some("write the test suite"));
        assert_eq!(row.status, TaskStatus::InProgress);
        assert!(row.due_at.is_none());
        assert!(row.scheduled_at.is_none());
        assert!(row.completed_at.is_none());
        assert!(row.parent_task_id.is_none());
        assert_eq!(row.block_handle.as_deref(), Some("task-list-handle"));
        assert_eq!(row.task_item_id.as_deref(), Some("item-001"));
        assert_eq!(row.owner_agent_id.as_deref(), Some("agent-1"));
        assert_eq!(
            row.comments_json,
            "[{\"author\":\"agent-1\",\"text\":\"started\"}]"
        );
    }

    #[test]
    fn task_row_from_row_null_optional_fields() {
        let conn = fresh_migrated_db();

        conn.execute(
            "INSERT INTO tasks (id, subject, status, created_at, updated_at)
             VALUES ('t-2', 'bare task', 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let row = conn
            .query_row(
                "SELECT rowid, id, agent_id, subject, description, status,
                        due_at, scheduled_at, completed_at, parent_task_id,
                        block_handle, task_item_id, owner_agent_id,
                        comments_json, created_at, updated_at
                 FROM tasks WHERE id = 't-2'",
                [],
                TaskRow::from_row,
            )
            .unwrap();

        assert_eq!(row.id, "t-2");
        assert!(row.agent_id.is_none());
        assert!(row.description.is_none());
        assert_eq!(row.status, TaskStatus::Pending);
        assert!(row.block_handle.is_none());
        assert!(row.task_item_id.is_none());
        assert!(row.owner_agent_id.is_none());
        // DEFAULT '[]' kicks in for comments_json.
        assert_eq!(row.comments_json, "[]");
    }

    // ---- TaskEdgeRow from_row smoke test ----

    #[test]
    fn task_edge_row_from_row_item_level() {
        let conn = fresh_migrated_db();

        conn.execute(
            "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
             VALUES ('block-a', 'item-001', 'block-b', 'item-002')",
            [],
        )
        .unwrap();

        let row = conn
            .query_row(
                "SELECT source_block, source_item, target_block, target_item
                 FROM task_edges",
                [],
                TaskEdgeRow::from_row,
            )
            .unwrap();

        assert_eq!(row.source_block, "block-a");
        assert_eq!(row.source_item, "item-001");
        assert_eq!(row.target_block, "block-b");
        assert_eq!(row.target_item.as_deref(), Some("item-002"));
    }

    #[test]
    fn task_edge_row_from_row_block_level_target() {
        let conn = fresh_migrated_db();

        // target_item = NULL means block-level reference.
        conn.execute(
            "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
             VALUES ('block-a', 'item-001', 'block-c', NULL)",
            [],
        )
        .unwrap();

        let row = conn
            .query_row(
                "SELECT source_block, source_item, target_block, target_item
                 FROM task_edges",
                [],
                TaskEdgeRow::from_row,
            )
            .unwrap();

        assert_eq!(row.source_block, "block-a");
        assert_eq!(row.source_item, "item-001");
        assert_eq!(row.target_block, "block-c");
        assert!(row.target_item.is_none(), "block-level target must be None");
    }
}

// endregion: tests
