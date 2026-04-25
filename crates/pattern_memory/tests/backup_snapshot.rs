//! Integration tests for `pattern_memory::backup::snapshot`.
//!
//! Covers v3-memory-rework.AC11.1, AC11.2, AC11.7.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use pattern_db::{ConstellationDb, Json, models};
use pattern_memory::backup::snapshot::create_snapshot;
use pattern_memory::paths::PatternPaths;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Open an on-disk `ConstellationDb` in a temp directory.
fn open_on_disk_db(dir: &tempfile::TempDir) -> Arc<ConstellationDb> {
    let memory_path = dir.path().join("memory.db");
    let messages_path = dir.path().join("messages.db");
    Arc::new(ConstellationDb::open(memory_path, messages_path).unwrap())
}

/// Seed a minimal agent row (FK constraint satisfaction).
fn seed_agent(db: &ConstellationDb, agent_id: &str) {
    let agent = models::Agent {
        id: agent_id.to_string(),
        name: format!("backup-test-{agent_id}"),
        description: None,
        model_provider: "test".to_string(),
        model_name: "test".to_string(),
        system_prompt: "test".to_string(),
        config: Json(serde_json::json!({})),
        enabled_tools: Json(vec![]),
        tool_rules: None,
        status: models::AgentStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("failed to seed agent");
}

/// Insert N messages into `db` for `agent_id`.
fn insert_messages(db: &ConstellationDb, agent_id: &str, count: usize) {
    let conn = db.get().unwrap();
    for i in 0..count {
        let msg = models::Message {
            id: format!("{agent_id}-msg-{i}"),
            agent_id: agent_id.to_string(),
            position: format!("{:020}", i),
            batch_id: None,
            sequence_in_batch: None,
            role: models::MessageRole::User,
            content_json: Json(serde_json::json!({"text": format!("message {i}")})),
            content_preview: Some(format!("message {i}")),
            batch_type: None,
            source: Some("test".to_string()),
            source_metadata: None,
            attachments_json: None,
            origin_json: None,
            is_archived: false,
            is_deleted: false,
            created_at: Timestamp::now(),
        };
        pattern_db::queries::create_message(&conn, &msg).expect("failed to insert message");
    }
}

/// Count all non-deleted messages for `agent_id` in `db`.
fn count_messages(db: &ConstellationDb, agent_id: &str) -> i64 {
    pattern_db::queries::count_all_messages(&db.get().unwrap(), agent_id)
        .expect("failed to count messages")
}

// ---------------------------------------------------------------------------
// AC11.1 + AC11.2: Happy path
// ---------------------------------------------------------------------------

/// Happy path: create messages.db, insert N messages, snapshot, verify.
///
/// Verifies AC11.1 (snapshot at correct path) and AC11.2 (snapshot is valid
/// SQLite with same schema and same row count).
#[test]
fn snapshot_happy_path_valid_sqlite_with_same_row_count() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_dir = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "snap-happy-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 5);
    assert_eq!(count_messages(&db, agent), 5);

    let paths = PatternPaths::with_base(backup_dir.path());
    let info = create_snapshot(db.messages_path(), &paths, "test-project")
        .expect("create_snapshot must succeed");

    // AC11.1: snapshot file exists at expected location.
    assert!(
        info.path.exists(),
        "snapshot file must exist at {}",
        info.path.display()
    );
    assert!(
        info.path.extension().and_then(|e| e.to_str()) == Some("sqlite"),
        "snapshot must have .sqlite extension"
    );
    assert!(info.size_bytes > 0, "snapshot must not be empty");
    // The content_hash must be non-zero (blake3 of a non-empty file is never all-zeros).
    assert_ne!(
        info.content_hash, [0u8; 32],
        "content_hash must be populated"
    );

    // AC11.2: snapshot opens cleanly as SQLite and has same row count.
    let snap_conn = rusqlite::Connection::open_with_flags(
        &info.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("snapshot must open as SQLite");

    // PRAGMA integrity_check must pass.
    let check: String = snap_conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check must succeed");
    assert_eq!(check, "ok", "snapshot must pass integrity_check");

    // Same tables exist in the snapshot.
    let table_count: i64 = snap_conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
            [],
            |r| r.get(0),
        )
        .expect("sqlite_master query must succeed");
    assert_eq!(table_count, 1, "snapshot must contain the messages table");

    // Same row count.
    let snap_row_count: i64 = snap_conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE is_deleted = 0",
            [],
            |r| r.get(0),
        )
        .expect("row count query must succeed");
    assert_eq!(snap_row_count, 5, "snapshot must contain 5 messages");
}

// ---------------------------------------------------------------------------
// AC11.7: Concurrent writer atomicity
// ---------------------------------------------------------------------------

/// Concurrent writer test: snapshot atomicity holds while INSERTs race.
///
/// Spawns a background thread doing rapid inserts into messages.db.
/// Takes a snapshot mid-flight. Verifies the snapshot:
/// - opens cleanly (no corruption),
/// - passes PRAGMA integrity_check,
/// - has a consistent row count ≤ the final source count (no partial writes).
#[test]
fn snapshot_concurrent_writer_produces_valid_sqlite() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_dir = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "concurrent-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 3);

    let messages_path = db.messages_path().to_owned();
    let paths = PatternPaths::with_base(backup_dir.path());

    // Background inserter: write ~50 messages with brief pauses.
    let db_writer = Arc::clone(&db);
    let writer_agent = agent.to_string();
    let writer_handle = std::thread::spawn(move || {
        for i in 100..150usize {
            let conn = db_writer.get().unwrap();
            let msg = models::Message {
                id: format!("{writer_agent}-concurrent-{i}"),
                agent_id: writer_agent.clone(),
                position: format!("{:020}", i + 100_000),
                batch_id: None,
                sequence_in_batch: None,
                role: models::MessageRole::User,
                content_json: Json(serde_json::json!({"text": format!("concurrent msg {i}")})),
                content_preview: Some(format!("concurrent msg {i}")),
                batch_type: None,
                source: Some("test".to_string()),
                source_metadata: None,
                attachments_json: None,
                origin_json: None,
                is_archived: false,
                is_deleted: false,
                created_at: Timestamp::now(),
            };
            let _ = pattern_db::queries::create_message(&conn, &msg);
            // Brief pause to allow the backup thread to interleave.
            std::thread::sleep(Duration::from_micros(100));
        }
    });

    // Give the writer a tiny head start, then snapshot mid-flight.
    std::thread::sleep(Duration::from_millis(5));
    let info = create_snapshot(&messages_path, &paths, "concurrent-project")
        .expect("create_snapshot must succeed even under concurrent writes");

    writer_handle.join().expect("writer thread must finish");

    // Snapshot must open cleanly.
    let snap_conn = rusqlite::Connection::open_with_flags(
        &info.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("snapshot must open as SQLite");

    // PRAGMA integrity_check must pass — no corruption.
    let check: String = snap_conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check must succeed");
    assert_eq!(
        check, "ok",
        "snapshot must pass integrity_check under concurrent writes"
    );

    // Row count must be ≤ final source count (a consistent point-in-time view).
    let snap_count: i64 = snap_conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .expect("row count query must succeed");
    let final_count = count_messages(&db, agent);
    assert!(
        snap_count <= final_count,
        "snapshot row count ({snap_count}) must be ≤ final source count ({final_count})"
    );
    // Sanity: at least the 3 pre-existing messages must be in the snapshot.
    assert!(
        snap_count >= 3,
        "snapshot must contain at least the 3 pre-existing messages, got {snap_count}"
    );
}

// ---------------------------------------------------------------------------
// Destination dir auto-create
// ---------------------------------------------------------------------------

/// Auto-create: backup dir doesn't exist → create_snapshot creates it.
#[test]
fn snapshot_auto_creates_backup_directory() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_base = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "autocreate-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 2);

    let paths = PatternPaths::with_base(backup_base.path());
    // The backup dir doesn't exist yet.
    let expected_backup_dir = paths.backup_dir("autocreate-project");
    assert!(
        !expected_backup_dir.exists(),
        "backup dir must not exist before first snapshot"
    );

    let info = create_snapshot(db.messages_path(), &paths, "autocreate-project")
        .expect("create_snapshot must create the backup dir and succeed");

    assert!(
        expected_backup_dir.exists(),
        "backup dir must be created by create_snapshot"
    );
    assert!(info.path.exists(), "snapshot file must exist");
}
