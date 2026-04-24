//! Block-index and BFS graph query layer for the `tasks` / `task_edges` /
//! `tasks_fts` tables introduced by migration 0011.
//!
//! These functions are used by the TaskList subscriber reconciler in
//! `pattern_memory` and by Phase 3 SDK handlers in `pattern_runtime`.
//!
//! Query types (`TaskFilter`, `Direction`, `GraphQuery`, `GraphSlice`,
//! `TaskEdgeRef`) come from `pattern_core::types::memory_types::task_query`
//! and `pattern_core::types::memory_types::task`.
//!
//! ## Removed legacy user-task surface (2026-04-23)
//!
//! `create_user_task`, `get_user_task`, `list_tasks`, `get_subtasks`,
//! `get_tasks_due_soon`, `update_user_task_status`, `update_user_task`,
//! `delete_user_task`, and `get_task_summaries` were removed along with
//! `UserTaskPriority`. These operated on the pre-v3 ADHD task model (`Task`,
//! `UserTaskStatus`) which has no active callers in the current workspace.
//! If `pattern_nd` is re-integrated, these should be re-introduced there
//! rather than in this crate's general query layer.

use std::collections::{HashSet, VecDeque};

use pattern_core::types::memory_types::{
    TaskEdgeRef,
    task_query::{Direction, GraphQuery, GraphSlice, TaskFilter},
};

use crate::queries::task_row::TaskRow;

// ============================================================================
// Block-index query layer (post-migration 0011)
// ============================================================================

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
///
/// When `filter.blocks` is `Some(vec![])` (empty vec), this returns no results
/// immediately — callers should pass `None` when no block scoping is desired.
pub fn list_tasks_filtered(
    conn: &rusqlite::Connection,
    filter: &TaskFilter,
) -> rusqlite::Result<Vec<TaskRow>> {
    // Short-circuit: Some(empty vec) means "no results", not "all results".
    if let Some(ref blocks) = filter.blocks
        && blocks.is_empty()
    {
        return Ok(Vec::new());
    }

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

    // Block handle filter.
    if let Some(ref blocks) = filter.blocks {
        // Empty vec is short-circuited above; here blocks is non-empty.
        let placeholders: Vec<String> = blocks
            .iter()
            .map(|_| {
                let p = format!("?{param_idx}");
                param_idx += 1;
                p
            })
            .collect();
        for b in blocks {
            params.push(Box::new(b.to_string()));
        }
        conditions.push(format!("t.block_handle IN ({})", placeholders.join(", ")));
    }

    // Status filter — serialize each TaskStatus to its kebab-case string.
    if let Some(ref statuses) = filter.status
        && !statuses.is_empty()
    {
        let placeholders: Vec<String> = statuses
            .iter()
            .map(|s| {
                let p = format!("?{param_idx}");
                params.push(Box::new(s.as_str().to_string()));
                param_idx += 1;
                p
            })
            .collect();
        conditions.push(format!("t.status IN ({})", placeholders.join(", ")));
    }

    // Owner filter — AgentId is SmolStr; pass as str.
    if let Some(ref owner) = filter.owner {
        conditions.push(format!("t.owner_agent_id = ?{param_idx}"));
        params.push(Box::new(owner.as_str().to_string()));
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
/// Starts from `root` and walks edges according to `query.direction`, up to
/// `query.depth` hops and `query.max_nodes` total nodes. Default caps of
/// `depth=16` and `max_nodes=1000` are applied when the fields are `None`.
///
/// Returns the discovered nodes and edges as [`TaskEdgeRef`] values, and
/// whether the traversal was truncated.
pub fn query_task_graph_bfs(
    conn: &rusqlite::Connection,
    root: &TaskEdgeRef,
    query: &GraphQuery,
) -> rusqlite::Result<GraphSlice> {
    let max_depth = query.depth.unwrap_or(16);
    let max_nodes = query.max_nodes.unwrap_or(1000);
    let direction = query.direction;

    // Internal BFS uses (block, Option<item>) tuples for hashing.
    type Node = (String, Option<String>);

    fn ref_to_node(r: &TaskEdgeRef) -> Node {
        (
            r.block.to_string(),
            r.task_item.as_ref().map(|s| s.to_string()),
        )
    }
    fn node_to_ref(n: &Node) -> TaskEdgeRef {
        use smol_str::SmolStr;
        TaskEdgeRef {
            block: SmolStr::new(&n.0),
            task_item: n.1.as_deref().map(SmolStr::new),
        }
    }

    let root_node: Node = ref_to_node(root);
    let mut visited: HashSet<Node> = HashSet::new();
    visited.insert(root_node.clone());
    let mut frontier: VecDeque<(Node, u32)> = VecDeque::new();
    frontier.push_back((root_node.clone(), 0));
    let mut nodes: Vec<Node> = vec![root_node];
    let mut edges: Vec<(Node, Node)> = Vec::new();
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

        let mut neighbours: Vec<(Node, Node)> = Vec::new();

        // Forward neighbours.
        if matches!(direction, Direction::Forward | Direction::Both) {
            // Forward lookup requires a non-null source_item.
            if let Some(ref item) = current.1 {
                let rows = forward_stmt.query_map(rusqlite::params![&current.0, item], |row| {
                    let tb: String = row.get(0)?;
                    let ti: Option<String> = row.get(1)?;
                    Ok((tb, ti))
                })?;
                for row in rows {
                    let neighbour = row?;
                    neighbours.push((current.clone(), neighbour));
                }
            }
        }

        // Reverse neighbours.
        if matches!(direction, Direction::Reverse | Direction::Both) {
            let rows =
                reverse_stmt.query_map(rusqlite::params![&current.0, &current.1], |row| {
                    let sb: String = row.get(0)?;
                    let si: String = row.get(1)?;
                    Ok((sb, Some(si)))
                })?;
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
                    return Ok(GraphSlice {
                        nodes: nodes.iter().map(node_to_ref).collect(),
                        edges: edges
                            .iter()
                            .map(|(f, t)| (node_to_ref(f), node_to_ref(t)))
                            .collect(),
                        truncated,
                    });
                }
                frontier.push_back((target, depth + 1));
            } else {
                // Still record the edge even if the node was already visited.
                edges.push((from, to));
            }
        }
    }

    Ok(GraphSlice {
        nodes: nodes.iter().map(node_to_ref).collect(),
        edges: edges
            .iter()
            .map(|(f, t)| (node_to_ref(f), node_to_ref(t)))
            .collect(),
        truncated,
    })
}

// endregion: query_task_graph_bfs
