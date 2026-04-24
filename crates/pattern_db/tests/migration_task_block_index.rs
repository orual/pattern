//! Migration 0011 (`task_block_index`) round-trip tests.
//!
//! Verifies:
//! - AC2.1: migration applies cleanly; `tasks`, `task_edges`, `tasks_fts` all exist.
//! - AC2.3: `coordination_tasks` table is absent after migration.
//! - AC2.4: `task_edges` schema — `source_item NOT NULL`, `target_item` nullable.
//! - AC2.5: duplicate edge insert is rejected by the unique expression index.
//! - AC2.6: indexes on `coordination_tasks` are dropped before the table;
//!   no DROP INDEX failure occurs. The `priority` column is absent.
//! - AC2.7: block-level target (`target_item = NULL`) and item-level target
//!   with a different value both insert successfully.
//! - FTS5 trigger: inserting a task row makes the subject searchable.
//! - Pre-migration task rows survive migration (subject preserved, new columns
//!   present with defaults, `priority` column absent).
//!
//! ## Design note on `pre_migration_db` helper
//!
//! The AC2.2 fixture test (pre-existing task rows survive migration) is
//! implemented here via a helper that applies migrations 0001–0010 then
//! inserts test data before running 0011. This verifies the round-trip
//! end-to-end rather than skipping it.

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

// ---------------------------------------------------------------------------
// Migration sets
// ---------------------------------------------------------------------------

/// All memory migrations through 0010 (pre-0011).
fn pre_0011_migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../migrations/memory/0001_initial.sql")),
        M::up(include_str!("../migrations/memory/0002_fts5.sql")),
        M::up(include_str!("../migrations/memory/0003_model_fields.sql")),
        M::up(include_str!("../migrations/memory/0004_memory_updates.sql")),
        M::up(include_str!(
            "../migrations/memory/0005_archival_fts_metadata.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0006_agent_atproto_endpoints.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0007_add_session_id_to_atproto_endpoints.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0008_member_capabilities.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0009_update_frontiers.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0010_collapse_block_types.sql"
        )),
    ])
}

/// All memory migrations through 0011 (full set).
fn all_migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../migrations/memory/0001_initial.sql")),
        M::up(include_str!("../migrations/memory/0002_fts5.sql")),
        M::up(include_str!("../migrations/memory/0003_model_fields.sql")),
        M::up(include_str!("../migrations/memory/0004_memory_updates.sql")),
        M::up(include_str!(
            "../migrations/memory/0005_archival_fts_metadata.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0006_agent_atproto_endpoints.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0007_add_session_id_to_atproto_endpoints.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0008_member_capabilities.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0009_update_frontiers.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0010_collapse_block_types.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0011_task_block_index.sql"
        )),
    ])
}

/// Open an in-memory DB with all migrations (0001–0011) applied.
fn fresh_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    all_migrations().to_latest(&mut conn).unwrap();
    conn
}

/// Open an in-memory DB with migrations 0001–0010 only.
fn pre_migration_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    pre_0011_migrations().to_latest(&mut conn).unwrap();
    conn
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Insert a minimal agent row (required for FK on tasks.agent_id in 0001).
fn insert_agent(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
         VALUES (?1, ?1, 'test', 'test', 'p', '{}', '[]', 'active', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![id],
    )
    .unwrap();
}

/// Query whether a table exists in `sqlite_master`.
fn table_exists(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','shadow','virtual') AND name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
}

/// Return true if a virtual table exists (checks `sqlite_master` for type='table').
fn virtual_table_exists(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
}

// ---------------------------------------------------------------------------
// AC2.1: migration applies to a fresh DB — tables exist
// ---------------------------------------------------------------------------

#[test]
fn migration_applies_to_empty_db_and_creates_tables() {
    let conn = fresh_db();

    // tasks must exist (was already present; shape extended).
    assert!(
        table_exists(&conn, "tasks"),
        "tasks table must exist after migration"
    );

    // task_edges is a new table from 0011.
    assert!(
        table_exists(&conn, "task_edges"),
        "task_edges table must exist after migration"
    );

    // tasks_fts is a new FTS5 virtual table from 0011.
    assert!(
        virtual_table_exists(&conn, "tasks_fts"),
        "tasks_fts virtual table must exist after migration"
    );
}

// ---------------------------------------------------------------------------
// AC2.3: coordination_tasks is absent
// ---------------------------------------------------------------------------

#[test]
fn coordination_tasks_absent_after_migration() {
    let conn = fresh_db();

    // The DROP TABLE IF EXISTS in 0011 must have removed this.
    assert!(
        !table_exists(&conn, "coordination_tasks"),
        "coordination_tasks must be absent after 0011"
    );
}

// ---------------------------------------------------------------------------
// AC2.4 + AC2.7: task_edges column nullability
// ---------------------------------------------------------------------------

#[test]
fn task_edges_source_item_not_null_target_item_nullable() {
    let conn = fresh_db();

    // Attempt to insert a row with source_item = NULL — should fail.
    let result = conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-a', NULL, 'blk-b', NULL)",
        [],
    );
    assert!(
        result.is_err(),
        "source_item NOT NULL must reject NULL (AC2.4)"
    );

    // target_item = NULL must succeed (block-level reference, AC2.4 / AC2.7).
    conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-a', 'item-1', 'blk-b', NULL)",
        [],
    )
    .unwrap_or_else(|e| panic!("NULL target_item must be allowed: {e}"));

    // target_item = non-null string must succeed (item-level reference, AC2.7).
    conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-a', 'item-1', 'blk-b', 'item-99')",
        [],
    )
    .unwrap_or_else(|e| panic!("non-NULL target_item must be allowed: {e}"));
}

// ---------------------------------------------------------------------------
// AC2.5: duplicate edge insert rejected by unique index
// ---------------------------------------------------------------------------

#[test]
fn task_edges_unique_constraint_rejects_duplicate_with_null_target() {
    let conn = fresh_db();

    // First insert (target_item = NULL, i.e., block-level edge) must succeed.
    conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-src', 'item-src', 'blk-tgt', NULL)",
        [],
    )
    .unwrap();

    // Second insert with identical key must fail.
    let result = conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-src', 'item-src', 'blk-tgt', NULL)",
        [],
    );
    assert!(
        result.is_err(),
        "duplicate edge with NULL target must be rejected (AC2.5)"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("UNIQUE"),
        "error must mention UNIQUE constraint; got: {err}"
    );
}

#[test]
fn task_edges_unique_constraint_rejects_duplicate_with_item_target() {
    let conn = fresh_db();

    // First insert (target_item = "item-xyz") must succeed.
    conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-src', 'item-src', 'blk-tgt', 'item-xyz')",
        [],
    )
    .unwrap();

    // Second insert with identical key must fail.
    let result = conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-src', 'item-src', 'blk-tgt', 'item-xyz')",
        [],
    );
    assert!(
        result.is_err(),
        "duplicate edge with item target must be rejected (AC2.5)"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("UNIQUE"),
        "error must mention UNIQUE constraint; got: {err}"
    );
}

// ---------------------------------------------------------------------------
// AC2.7: block-level and item-level targets are distinct
// ---------------------------------------------------------------------------

#[test]
fn task_edges_null_and_item_target_to_same_block_are_distinct() {
    let conn = fresh_db();

    // Block-level edge (target_item = NULL).
    conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-a', 'item-1', 'blk-b', NULL)",
        [],
    )
    .unwrap_or_else(|e| panic!("block-level edge must succeed: {e}"));

    // Item-level edge to a different target (target_item = 'some-item').
    conn.execute(
        "INSERT INTO task_edges (source_block, source_item, target_block, target_item)
         VALUES ('blk-a', 'item-1', 'blk-b', 'some-item')",
        [],
    )
    .unwrap_or_else(|e| panic!("item-level edge must succeed alongside block-level edge: {e}"));

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "both NULL and non-NULL target edges must exist");
}

// ---------------------------------------------------------------------------
// AC2.6: priority column absent; pre-existing task rows survive with defaults
// ---------------------------------------------------------------------------

#[test]
fn migration_preserves_pre_existing_task_rows() {
    // Apply migrations 0001–0010, insert a task row with the old shape, then
    // apply 0011 and assert the row survives with new columns at defaults.
    let mut conn = pre_migration_db();

    insert_agent(&conn, "agent-001");

    // Insert a task with the pre-0011 schema (title + priority columns).
    conn.execute(
        "INSERT INTO tasks (id, agent_id, title, description, status, priority, created_at, updated_at)
         VALUES ('task-001', 'agent-001', 'Triage inbox', 'Review pending messages.', 'pending', 'high', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Verify row exists before migration.
    let pre_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = 'task-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pre_count, 1);

    // Apply 0011.
    all_migrations().to_latest(&mut conn).unwrap();

    // Row must survive (subject is the renamed column).
    let subject: String = conn
        .query_row("SELECT subject FROM tasks WHERE id = 'task-001'", [], |r| {
            r.get(0)
        })
        .unwrap_or_else(|e| panic!("row must survive migration with 'subject' column: {e}"));
    assert_eq!(subject, "Triage inbox");

    // New columns must exist with correct defaults.
    let (block_handle, task_item_id, owner_agent_id, comments_json): (
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "SELECT block_handle, task_item_id, owner_agent_id, comments_json FROM tasks WHERE id = 'task-001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();

    assert!(
        block_handle.is_none(),
        "block_handle must default to NULL for legacy rows"
    );
    assert!(
        task_item_id.is_none(),
        "task_item_id must default to NULL for legacy rows"
    );
    assert!(
        owner_agent_id.is_none(),
        "owner_agent_id must default to NULL for legacy rows"
    );
    assert_eq!(
        comments_json, "[]",
        "comments_json must default to '[]' for legacy rows"
    );

    // priority column must be absent after DROP COLUMN.
    let priority_result = conn.query_row(
        "SELECT priority FROM tasks WHERE id = 'task-001'",
        [],
        |r| r.get::<_, String>(0),
    );
    assert!(
        priority_result.is_err(),
        "priority column must not exist after migration 0011 (AC2.6)"
    );
}

#[test]
fn priority_column_absent_on_fresh_db() {
    let conn = fresh_db();

    // A query referencing the dropped priority column must fail at runtime.
    let result = conn.query_row("SELECT priority FROM tasks LIMIT 1", [], |r| {
        r.get::<_, String>(0)
    });
    assert!(
        result.is_err(),
        "priority column must not exist on a fresh post-0011 DB"
    );
}

// ---------------------------------------------------------------------------
// FTS5 trigger: INSERT fires tasks_fts_insert trigger
// ---------------------------------------------------------------------------

#[test]
fn fts5_trigger_fires_on_task_insert() {
    let conn = fresh_db();

    insert_agent(&conn, "agent-fts");

    // Insert a task — the trigger should populate tasks_fts.
    conn.execute(
        "INSERT INTO tasks (id, agent_id, subject, description, status, created_at, updated_at)
         VALUES ('task-fts-1', 'agent-fts', 'fix login timeout', 'Authentication requests expire too early.', 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // FTS5 match on subject.
    let matched: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'login'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        matched, 1,
        "FTS5 trigger must make 'login' searchable after task insert"
    );

    // FTS5 match on description.
    let matched_desc: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'Authentication'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        matched_desc, 1,
        "FTS5 trigger must make description term searchable"
    );
}

#[test]
fn fts5_trigger_removes_on_task_delete() {
    let conn = fresh_db();

    insert_agent(&conn, "agent-fts");

    conn.execute(
        "INSERT INTO tasks (id, agent_id, subject, description, status, created_at, updated_at)
         VALUES ('task-del', 'agent-fts', 'refactor token rotation', 'Rotate tokens on expiry.', 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Verify it is indexed.
    let pre: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'rotation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pre, 1, "task must be searchable before delete");

    // Delete the task — the delete trigger fires.
    conn.execute("DELETE FROM tasks WHERE id = 'task-del'", [])
        .unwrap();

    let post: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'rotation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        post, 0,
        "FTS5 delete trigger must remove entry on task delete"
    );
}

#[test]
fn fts5_trigger_updates_on_task_update() {
    let conn = fresh_db();

    insert_agent(&conn, "agent-fts");

    conn.execute(
        "INSERT INTO tasks (id, agent_id, subject, description, status, created_at, updated_at)
         VALUES ('task-upd', 'agent-fts', 'old subject term', 'Old description.', 'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    // Search for 'old' (from "old subject term") — must match before update.
    let pre_match: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pre_match, 1, "old subject must be searchable before update");

    // Update the task subject.
    conn.execute(
        "UPDATE tasks SET subject = 'new migrated subject', updated_at = '2026-01-02T00:00:00Z' WHERE id = 'task-upd'",
        [],
    )
    .unwrap();

    // 'old' also appears in "Old description." so it may still match — but
    // 'term' (from "old subject term") should be gone after the update trigger.
    let post_term: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'term'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        post_term, 0,
        "old-only subject word must be gone after FTS5 update trigger fires"
    );

    // New term must be searchable.
    let post_new: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'migrated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        post_new, 1,
        "new subject term must be searchable after FTS5 update trigger fires"
    );
}

// ---------------------------------------------------------------------------
// Indexes exist (smoke test)
// ---------------------------------------------------------------------------

#[test]
fn expected_indexes_exist_after_migration() {
    let conn = fresh_db();

    let index_names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    for expected in &[
        "idx_task_edges_pk",
        "idx_task_edges_source",
        "idx_task_edges_target",
        "idx_tasks_block",
        "idx_tasks_owner",
    ] {
        assert!(
            index_names.iter().any(|n| n == *expected),
            "index {expected} must exist after migration; got: {index_names:?}"
        );
    }

    // coordination_tasks indexes must be absent.
    for absent in &["idx_tasks_status", "idx_tasks_assigned"] {
        assert!(
            !index_names.iter().any(|n| n == *absent),
            "coordination_tasks index {absent} must be absent after 0011"
        );
    }
}
