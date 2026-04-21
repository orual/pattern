//! Migration round-trip tests: verify tables are created in both schemas,
//! that opening twice on the same path is idempotent, and that migration
//! 0010 (collapse BlockType::Archival/Log) converts data correctly.

use pattern_db::ConstellationDb;
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

#[test]
fn open_creates_tables_in_both_schemas() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem_path = tmp.path().join("memory.db");
    let msg_path = tmp.path().join("messages.db");

    let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
    let conn = db.get().unwrap();

    // Memory-side tables (main schema).
    let memory_tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert!(
        memory_tables.contains(&"agents".to_string()),
        "agents table missing from memory.db; got: {memory_tables:?}"
    );
    assert!(
        memory_tables.contains(&"memory_blocks".to_string()),
        "memory_blocks table missing"
    );
    assert!(
        memory_tables.contains(&"archival_entries".to_string()),
        "archival_entries table missing"
    );
    // Messages should NOT be in the main schema.
    assert!(
        !memory_tables.contains(&"messages".to_string()),
        "messages table should not be in memory.db main schema"
    );

    // Messages-side tables (msg schema).
    let msg_tables: Vec<String> = conn
        .prepare("SELECT name FROM msg.sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert!(
        msg_tables.contains(&"messages".to_string()),
        "messages table missing from messages.db; got: {msg_tables:?}"
    );
    assert!(
        msg_tables.contains(&"queued_messages".to_string()),
        "queued_messages table missing from messages.db"
    );
}

#[test]
fn open_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem_path = tmp.path().join("memory.db");
    let msg_path = tmp.path().join("messages.db");

    // Open once.
    let db1 = ConstellationDb::open(&mem_path, &msg_path).unwrap();
    db1.health_check().unwrap();
    drop(db1);

    // Open again on the same paths — should not fail or re-apply migrations.
    let db2 = ConstellationDb::open(&mem_path, &msg_path).unwrap();
    db2.health_check().unwrap();
}

// ---------------------------------------------------------------------------
// Migration 0010: collapse BlockType::Archival/Log
// ---------------------------------------------------------------------------

/// Build migrations for memory.db up through migration 0009 (pre-collapse).
fn pre_collapse_migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../migrations/memory/0001_initial.sql")),
        M::up(include_str!("../migrations/memory/0002_fts5.sql")),
        M::up(include_str!("../migrations/memory/0003_model_fields.sql")),
        M::up(include_str!("../migrations/memory/0004_memory_updates.sql")),
        M::up(include_str!("../migrations/memory/0005_archival_fts_metadata.sql")),
        M::up(include_str!("../migrations/memory/0006_agent_atproto_endpoints.sql")),
        M::up(include_str!("../migrations/memory/0007_add_session_id_to_atproto_endpoints.sql")),
        M::up(include_str!("../migrations/memory/0008_member_capabilities.sql")),
        M::up(include_str!("../migrations/memory/0009_update_frontiers.sql")),
    ])
}

/// Build all memory.db migrations (including 0010 collapse).
fn all_memory_migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../migrations/memory/0001_initial.sql")),
        M::up(include_str!("../migrations/memory/0002_fts5.sql")),
        M::up(include_str!("../migrations/memory/0003_model_fields.sql")),
        M::up(include_str!("../migrations/memory/0004_memory_updates.sql")),
        M::up(include_str!("../migrations/memory/0005_archival_fts_metadata.sql")),
        M::up(include_str!("../migrations/memory/0006_agent_atproto_endpoints.sql")),
        M::up(include_str!("../migrations/memory/0007_add_session_id_to_atproto_endpoints.sql")),
        M::up(include_str!("../migrations/memory/0008_member_capabilities.sql")),
        M::up(include_str!("../migrations/memory/0009_update_frontiers.sql")),
        M::up(include_str!("../migrations/memory/0010_collapse_block_types.sql")),
    ])
}

/// Insert a test agent into the agents table.
fn insert_test_agent(conn: &Connection, agent_id: &str) {
    conn.execute(
        "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
         VALUES (?1, ?1, 'test', 'test', 'prompt', '{}', '[]', 'active', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![agent_id],
    )
    .unwrap();
}

/// Insert a test memory block with the given block_type.
fn insert_test_block(
    conn: &Connection,
    id: &str,
    agent_id: &str,
    label: &str,
    block_type: &str,
    content_preview: &str,
) {
    // Create a minimal Loro document snapshot for the blob.
    let loro_doc = loro::LoroDoc::new();
    let text = loro_doc.get_text("content");
    text.insert(0, content_preview).unwrap();
    let snapshot = loro_doc
        .export(loro::ExportMode::Snapshot)
        .unwrap_or_default();

    conn.execute(
        "INSERT INTO memory_blocks (id, agent_id, label, description, block_type, char_limit, permission, pinned, loro_snapshot, content_preview, is_active, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'test block', ?4, 5000, 'read_write', 0, ?5, ?6, 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![id, agent_id, label, block_type, snapshot, content_preview],
    )
    .unwrap();
}

#[test]
fn migration_0010_archival_rows_become_archival_entries() {
    let mut conn = Connection::open_in_memory().unwrap();

    // Apply migrations 0001-0009.
    pre_collapse_migrations().to_latest(&mut conn).unwrap();

    // Insert test data.
    insert_test_agent(&conn, "agent-001");

    insert_test_block(
        &conn,
        "block-core-1",
        "agent-001",
        "persona",
        "core",
        "I am a test agent.",
    );
    insert_test_block(
        &conn,
        "block-working-1",
        "agent-001",
        "scratchpad",
        "working",
        "Some working notes.",
    );
    insert_test_block(
        &conn,
        "block-archival-1",
        "agent-001",
        "archive_1",
        "archival",
        "Long-term memory content.",
    );
    insert_test_block(
        &conn,
        "block-archival-2",
        "agent-001",
        "archive_2",
        "archival",
        "Another archival entry.",
    );
    insert_test_block(
        &conn,
        "block-log-1",
        "agent-001",
        "session_log",
        "log",
        "Log entry content.",
    );

    // Record pre-migration counts.
    let pre_blocks: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_blocks", [], |r| r.get(0))
        .unwrap();
    let pre_archival_entries: i64 = conn
        .query_row("SELECT COUNT(*) FROM archival_entries", [], |r| r.get(0))
        .unwrap();
    let pre_total = pre_blocks + pre_archival_entries;

    assert_eq!(pre_blocks, 5);
    assert_eq!(pre_archival_entries, 0);

    // Apply migration 0010.
    all_memory_migrations().to_latest(&mut conn).unwrap();

    // Verify: no archival or log rows remain in memory_blocks.
    let archival_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_blocks WHERE block_type = 'archival'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let log_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_blocks WHERE block_type = 'log'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archival_count, 0, "archival rows should be gone");
    assert_eq!(log_count, 0, "log rows should be gone");

    // Verify: archival rows became archival_entries.
    let post_archival_entries: i64 = conn
        .query_row("SELECT COUNT(*) FROM archival_entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        post_archival_entries, 2,
        "2 archival blocks should become 2 archival entries"
    );

    // Verify content was transferred.
    let content: String = conn
        .query_row(
            "SELECT content FROM archival_entries WHERE id = 'block-archival-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "Long-term memory content.");

    // Verify: log rows became working with kind=log in metadata.
    let log_block_type: String = conn
        .query_row(
            "SELECT block_type FROM memory_blocks WHERE id = 'block-log-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(log_block_type, "working");

    let log_metadata: String = conn
        .query_row(
            "SELECT metadata FROM memory_blocks WHERE id = 'block-log-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&log_metadata).unwrap();
    assert_eq!(metadata["kind"], "log");

    // AC3.3/AC3.4 total count invariant: no data loss.
    let post_blocks: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_blocks", [], |r| r.get(0))
        .unwrap();
    let post_total = post_blocks + post_archival_entries;
    assert_eq!(
        pre_total, post_total,
        "total count invariant: pre={pre_total}, post={post_total}"
    );
}

#[test]
fn migration_0010_from_sql_rejects_stale_block_types() {
    let mut conn = Connection::open_in_memory().unwrap();
    all_memory_migrations().to_latest(&mut conn).unwrap();
    insert_test_agent(&conn, "agent-001");

    // Directly insert a row with a stale block_type (bypassing the enum).
    let loro_doc = loro::LoroDoc::new();
    let snapshot = loro_doc
        .export(loro::ExportMode::Snapshot)
        .unwrap_or_default();
    conn.execute(
        "INSERT INTO memory_blocks (id, agent_id, label, description, block_type, char_limit, permission, pinned, loro_snapshot, content_preview, is_active, created_at, updated_at)
         VALUES ('stale-1', 'agent-001', 'stale_block', 'test', 'archival', 5000, 'read_write', 0, ?1, 'stale content', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![snapshot],
    )
    .unwrap();

    // Attempt to read block_type via FromSql — should fail with a clear error.
    let result = conn.query_row(
        "SELECT block_type FROM memory_blocks WHERE id = 'stale-1'",
        [],
        |r| r.get::<_, pattern_db::models::MemoryBlockType>(0),
    );
    assert!(result.is_err(), "stale 'archival' should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("removed") || err_msg.contains("0010"),
        "error should mention removal or migration; got: {err_msg}"
    );
}

#[test]
fn migration_0010_preserves_core_and_working_blocks() {
    let mut conn = Connection::open_in_memory().unwrap();
    pre_collapse_migrations().to_latest(&mut conn).unwrap();

    insert_test_agent(&conn, "agent-001");
    insert_test_block(&conn, "b1", "agent-001", "persona", "core", "Core content.");
    insert_test_block(
        &conn,
        "b2",
        "agent-001",
        "scratchpad",
        "working",
        "Working content.",
    );

    all_memory_migrations().to_latest(&mut conn).unwrap();

    // Core and working blocks should be untouched.
    let core_type: String = conn
        .query_row(
            "SELECT block_type FROM memory_blocks WHERE id = 'b1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(core_type, "core");

    let working_type: String = conn
        .query_row(
            "SELECT block_type FROM memory_blocks WHERE id = 'b2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(working_type, "working");
}

#[test]
fn in_memory_has_all_tables() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();

    // Memory-side tables (main schema).
    let memory_tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert!(
        memory_tables.contains(&"agents".to_string()),
        "agents table missing from main schema; got: {memory_tables:?}"
    );
    assert!(
        memory_tables.contains(&"memory_blocks".to_string()),
        "memory_blocks table missing from main schema"
    );

    // Messages should be in the msg schema, not main.
    assert!(
        !memory_tables.contains(&"messages".to_string()),
        "messages table should not be in main schema; it belongs in msg"
    );

    // Messages-side tables (msg schema).
    let msg_tables: Vec<String> = conn
        .prepare("SELECT name FROM msg.sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert!(
        msg_tables.contains(&"messages".to_string()),
        "messages table missing from msg schema; got: {msg_tables:?}"
    );
    assert!(
        msg_tables.contains(&"queued_messages".to_string()),
        "queued_messages table missing from msg schema"
    );
}
