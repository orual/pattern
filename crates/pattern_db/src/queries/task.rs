//! ADHD task queries.
//!
//! These functions target the post-migration-0011 `tasks` table shape:
//! `subject` (was `title`), no `priority` column (priority lives in
//! freeform `metadata_json` on the TaskList block layer).
//!
//! ## Block-index query layer
//!
//! The second half of this module exposes sync functions for the
//! `tasks`/`task_edges`/`tasks_fts` block-index tables introduced by
//! migration 0011. These are used by the TaskList subscriber reconciler
//! in `pattern_memory` and by Phase 3's SDK handlers in `pattern_runtime`.
//!
//! Types like `TaskFilter`, `Direction`, `GraphSlice` live in `pattern_core`
//! (which depends on `pattern_db`). To avoid the circular dependency,
//! the functions here accept plain primitives or local mirror structs
//! (`FilterArgs`, `GraphDirection`, `GraphSliceRows`). The conversion
//! between `pattern_core` types and these primitives is done by callers
//! in `pattern_memory` or `pattern_runtime`.

use std::collections::{HashSet, VecDeque};

use chrono::Utc;
use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{Task, TaskSummary, UserTaskStatus};
use crate::queries::task_row::TaskRow;

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

// ============================================================================
// Block-index query layer (post-migration 0011)
// ============================================================================

// region: local types

/// Filter arguments for [`list_tasks_filtered`].
///
/// All fields are `Option`; `None` means "no constraint on this axis".
/// Callers in `pattern_memory`/`pattern_runtime` convert from
/// `pattern_core::types::memory_types::task_query::TaskFilter` into this
/// plain-data struct.
#[derive(Debug, Clone, Default)]
pub struct FilterArgs {
    /// Only return tasks whose status matches one of these kebab-case strings.
    pub status: Option<Vec<String>>,
    /// Only return tasks assigned to this owner agent.
    pub owner: Option<String>,
    /// If `Some(true)`, only tasks that have at least one incoming edge
    /// (i.e. something blocks them). If `Some(false)`, only tasks with
    /// zero incoming edges. `None` skips the check.
    pub has_blockers: Option<bool>,
    /// FTS5 keyword query. When set, results are ordered by BM25 relevance.
    pub keyword: Option<String>,
}

/// BFS traversal direction for [`query_task_graph_bfs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    /// Follow edges from source to target.
    Forward,
    /// Follow edges from target to source (reverse lookup).
    Reverse,
    /// Follow edges in both directions.
    Both,
}

/// A node in the graph result, expressed as `(block, Option<item>)`.
pub type GraphNode = (String, Option<String>);

/// Result of a BFS graph traversal via [`query_task_graph_bfs`].
#[derive(Debug, Clone)]
pub struct GraphSliceRows {
    /// Discovered nodes in BFS visitation order.
    pub nodes: Vec<GraphNode>,
    /// Directed edges `(from, to)` discovered during traversal.
    pub edges: Vec<(GraphNode, GraphNode)>,
    /// `true` if the traversal hit `max_nodes` before exhausting the frontier.
    pub truncated: bool,
}

// endregion: local types

// region: upsert / delete

/// Insert or replace a task row keyed on `(block_handle, task_item_id)`.
///
/// Because the `idx_tasks_block` index is NOT unique, this uses an explicit
/// delete-then-insert inside the caller's transaction. Must be called within
/// a [`rusqlite::Transaction`].
pub fn upsert_task_row(tx: &rusqlite::Transaction, row: &TaskRow) -> rusqlite::Result<()> {
    // Delete any existing row with the same (block_handle, task_item_id) pair.
    if let (Some(bh), Some(ti)) = (&row.block_handle, &row.task_item_id) {
        tx.execute(
            "DELETE FROM tasks WHERE block_handle = ?1 AND task_item_id = ?2",
            rusqlite::params![bh, ti],
        )?;
    }

    tx.execute(
        "INSERT INTO tasks (id, agent_id, subject, description, status,
                due_at, scheduled_at, completed_at, parent_task_id,
                block_handle, task_item_id, owner_agent_id, comments_json,
                created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            row.id,
            row.agent_id,
            row.subject,
            row.description,
            row.status,
            row.due_at,
            row.scheduled_at,
            row.completed_at,
            row.parent_task_id,
            row.block_handle,
            row.task_item_id,
            row.owner_agent_id,
            row.comments_json,
            row.created_at,
            row.updated_at,
        ],
    )?;
    Ok(())
}

/// Delete a task row by `(block_handle, task_item_id)`. Returns rows affected.
pub fn delete_task_row(
    tx: &rusqlite::Transaction,
    block: &str,
    item: &str,
) -> rusqlite::Result<usize> {
    let count = tx.execute(
        "DELETE FROM tasks WHERE block_handle = ?1 AND task_item_id = ?2",
        rusqlite::params![block, item],
    )?;
    Ok(count)
}

/// Replace all outgoing edges for a `(source_block, source_item)` pair.
///
/// Deletes existing edges then inserts the new set. Idempotent: calling
/// with the same `edges` twice produces the same final state.
pub fn upsert_task_edges(
    tx: &rusqlite::Transaction,
    source_block: &str,
    source_item: &str,
    edges: &[(String, Option<String>)],
) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM task_edges WHERE source_block = ?1 AND source_item = ?2",
        rusqlite::params![source_block, source_item],
    )?;

    let mut stmt = tx.prepare_cached(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (target_block, target_item) in edges {
        stmt.execute(rusqlite::params![
            source_block,
            source_item,
            target_block,
            target_item,
        ])?;
    }
    Ok(())
}

/// Delete all outgoing edges for a source item. Returns rows affected.
pub fn delete_task_edges_for_item(
    tx: &rusqlite::Transaction,
    block: &str,
    item: &str,
) -> rusqlite::Result<usize> {
    let count = tx.execute(
        "DELETE FROM task_edges WHERE source_block = ?1 AND source_item = ?2",
        rusqlite::params![block, item],
    )?;
    Ok(count)
}

/// Delete all edges targeting a specific `(target_block, target_item)`. Returns rows affected.
pub fn delete_task_edges_targeting(
    tx: &rusqlite::Transaction,
    target_block: &str,
    target_item: Option<&str>,
) -> rusqlite::Result<usize> {
    let count = match target_item {
        Some(ti) => tx.execute(
            "DELETE FROM task_edges WHERE target_block = ?1 AND target_item = ?2",
            rusqlite::params![target_block, ti],
        )?,
        None => tx.execute(
            "DELETE FROM task_edges WHERE target_block = ?1 AND target_item IS NULL",
            rusqlite::params![target_block],
        )?,
    };
    Ok(count)
}

// endregion: upsert / delete

// region: list_tasks_filtered

/// List tasks matching the given filter criteria.
///
/// When `filter.keyword` is set, results are ordered by FTS5 BM25 relevance
/// (most relevant first). Otherwise, results are ordered by `created_at ASC`.
///
/// The `has_blockers` filter checks whether the task appears as a target in
/// `task_edges` (i.e. something blocks it).
pub fn list_tasks_filtered(
    conn: &rusqlite::Connection,
    filter: &FilterArgs,
) -> rusqlite::Result<Vec<TaskRow>> {
    let mut sql = String::with_capacity(512);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1u32;
    let mut conditions: Vec<String> = Vec::new();

    // Base SELECT with all TaskRow columns.
    if filter.keyword.is_some() {
        sql.push_str(
            "SELECT t.rowid, t.id, t.agent_id, t.subject, t.description, t.status,
                    t.due_at, t.scheduled_at, t.completed_at, t.parent_task_id,
                    t.block_handle, t.task_item_id, t.owner_agent_id,
                    t.comments_json, t.created_at, t.updated_at
             FROM tasks t
             JOIN tasks_fts ON tasks_fts.rowid = t.rowid",
        );
    } else {
        sql.push_str(
            "SELECT t.rowid, t.id, t.agent_id, t.subject, t.description, t.status,
                    t.due_at, t.scheduled_at, t.completed_at, t.parent_task_id,
                    t.block_handle, t.task_item_id, t.owner_agent_id,
                    t.comments_json, t.created_at, t.updated_at
             FROM tasks t",
        );
    }

    // Status filter.
    if let Some(ref statuses) = filter.status {
        if !statuses.is_empty() {
            let placeholders: Vec<String> = statuses
                .iter()
                .map(|s| {
                    let p = format!("?{param_idx}");
                    params.push(Box::new(s.clone()));
                    param_idx += 1;
                    p
                })
                .collect();
            conditions.push(format!("t.status IN ({})", placeholders.join(", ")));
        }
    }

    // Owner filter.
    if let Some(ref owner) = filter.owner {
        conditions.push(format!("t.owner_agent_id = ?{param_idx}"));
        params.push(Box::new(owner.clone()));
        param_idx += 1;
    }

    // has_blockers filter.
    if let Some(has_blockers) = filter.has_blockers {
        if has_blockers {
            conditions.push(
                "EXISTS (SELECT 1 FROM task_edges e WHERE e.target_block = t.block_handle AND (e.target_item = t.task_item_id OR (e.target_item IS NULL AND t.task_item_id IS NULL)))".to_string(),
            );
        } else {
            conditions.push(
                "NOT EXISTS (SELECT 1 FROM task_edges e WHERE e.target_block = t.block_handle AND (e.target_item = t.task_item_id OR (e.target_item IS NULL AND t.task_item_id IS NULL)))".to_string(),
            );
        }
    }

    // FTS5 keyword filter.
    if let Some(ref keyword) = filter.keyword {
        conditions.push(format!("tasks_fts MATCH ?{param_idx}"));
        params.push(Box::new(keyword.clone()));
        param_idx += 1;
    }
    // Suppress unused-variable warning.
    let _ = param_idx;

    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }

    // Ordering.
    if filter.keyword.is_some() {
        sql.push_str(" ORDER BY rank");
    } else {
        sql.push_str(" ORDER BY t.created_at ASC");
    }

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), TaskRow::from_row)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

// endregion: list_tasks_filtered

// region: query_task_graph_bfs

/// BFS traversal over the `task_edges` graph.
///
/// Starts from `root` and walks edges according to `direction`, up to
/// `max_depth` hops and `max_nodes` total nodes. Returns the discovered
/// nodes, edges, and whether the traversal was truncated.
pub fn query_task_graph_bfs(
    conn: &rusqlite::Connection,
    root_block: &str,
    root_item: Option<&str>,
    direction: GraphDirection,
    max_depth: u32,
    max_nodes: u32,
) -> rusqlite::Result<GraphSliceRows> {
    let root: GraphNode = (root_block.to_string(), root_item.map(|s| s.to_string()));
    let mut visited: HashSet<GraphNode> = HashSet::new();
    visited.insert(root.clone());
    let mut frontier: VecDeque<(GraphNode, u32)> = VecDeque::new();
    frontier.push_back((root.clone(), 0));
    let mut nodes: Vec<GraphNode> = vec![root];
    let mut edges: Vec<(GraphNode, GraphNode)> = Vec::new();
    let mut truncated = false;

    // Prepare statements for forward/reverse lookups.
    let forward_sql = "SELECT target_block, target_item FROM task_edges
                       WHERE source_block = ?1 AND source_item = ?2";
    let reverse_sql = "SELECT source_block, source_item FROM task_edges
                       WHERE target_block = ?1 AND (target_item = ?2 OR (target_item IS NULL AND ?2 IS NULL))";

    let mut forward_stmt = conn.prepare_cached(forward_sql)?;
    let mut reverse_stmt = conn.prepare_cached(reverse_sql)?;

    while let Some((current, depth)) = frontier.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let mut neighbours: Vec<(GraphNode, GraphNode)> = Vec::new();

        // Forward neighbours.
        if matches!(direction, GraphDirection::Forward | GraphDirection::Both) {
            // Forward lookup requires a non-null source_item.
            if let Some(ref item) = current.1 {
                let rows = forward_stmt.query_map(
                    rusqlite::params![&current.0, item],
                    |row| {
                        let tb: String = row.get(0)?;
                        let ti: Option<String> = row.get(1)?;
                        Ok((tb, ti))
                    },
                )?;
                for row in rows {
                    let neighbour = row?;
                    neighbours.push((current.clone(), neighbour));
                }
            }
        }

        // Reverse neighbours.
        if matches!(direction, GraphDirection::Reverse | GraphDirection::Both) {
            let rows = reverse_stmt.query_map(
                rusqlite::params![&current.0, &current.1],
                |row| {
                    let sb: String = row.get(0)?;
                    let si: String = row.get(1)?;
                    Ok((sb, Some(si)))
                },
            )?;
            for row in rows {
                let neighbour = row?;
                neighbours.push((current.clone(), neighbour));
            }
        }

        for (from, to) in neighbours {
            let target = to.clone();
            if !visited.contains(&target) {
                visited.insert(target.clone());
                edges.push((from, to));
                nodes.push(target.clone());
                if nodes.len() as u32 >= max_nodes {
                    truncated = true;
                    return Ok(GraphSliceRows {
                        nodes,
                        edges,
                        truncated,
                    });
                }
                frontier.push_back((target, depth + 1));
            } else {
                // Still record the edge even if node already visited.
                edges.push((from, to));
            }
        }
    }

    Ok(GraphSliceRows {
        nodes,
        edges,
        truncated,
    })
}

// endregion: query_task_graph_bfs
