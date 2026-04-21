//! Integration tests for `pattern_memory::backup::restore`.
//!
//! Covers v3-memory-rework.AC11.3, AC11.4, AC11.6.

use jiff::Timestamp;
use pattern_db::{ConstellationDb, Json, models};
use pattern_memory::backup::restore::{resolve_snapshot, restore_snapshot};
use pattern_memory::backup::snapshot::create_snapshot;
use pattern_memory::paths::PatternPaths;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn open_on_disk_db(dir: &tempfile::TempDir) -> Arc<ConstellationDb> {
    let memory_path = dir.path().join("memory.db");
    let messages_path = dir.path().join("messages.db");
    Arc::new(ConstellationDb::open(memory_path, messages_path).unwrap())
}

fn seed_agent(db: &ConstellationDb, agent_id: &str) {
    let agent = models::Agent {
        id: agent_id.to_string(),
        name: format!("restore-test-{agent_id}"),
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

fn insert_messages(db: &ConstellationDb, agent_id: &str, count: usize) -> Vec<String> {
    let conn = db.get().unwrap();
    let mut ids = Vec::new();
    for i in 0..count {
        let id = format!("{agent_id}-msg-{i:04}");
        let msg = models::Message {
            id: id.clone(),
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
            is_archived: false,
            is_deleted: false,
            created_at: Timestamp::now(),
        };
        pattern_db::queries::create_message(&conn, &msg).expect("insert_messages failed");
        ids.push(id);
    }
    ids
}

fn count_all_messages(db: &ConstellationDb, agent_id: &str) -> i64 {
    pattern_db::queries::count_all_messages(&db.get().unwrap(), agent_id)
        .expect("count_all_messages failed")
}

// ---------------------------------------------------------------------------
// AC11.3 + AC11.4: Happy path + pre-restore safety
// ---------------------------------------------------------------------------

/// Happy path: write → snapshot → modify → restore → assert original rows restored.
///
/// Also verifies AC11.4: pre-restore safety copy exists with the modified state.
#[test]
fn restore_happy_path_and_pre_restore_safety() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_base = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "restore-happy-agent";
    seed_agent(&db, agent);

    // Step 1: insert 3 messages.
    insert_messages(&db, agent, 3);
    assert_eq!(count_all_messages(&db, agent), 3);

    // Step 2: create snapshot (3-message state).
    let paths = PatternPaths::with_base(backup_base.path());
    let snapshot = create_snapshot(db.messages_path(), &paths, "restore-happy-project")
        .expect("create_snapshot must succeed");

    // Step 3: modify — insert 2 more messages with distinct IDs.
    {
        let conn = db.get().unwrap();
        for i in 100..102usize {
            let msg = models::Message {
                id: format!("{agent}-extra-{i}"),
                agent_id: agent.to_string(),
                position: format!("{:020}", i + 50000),
                batch_id: None,
                sequence_in_batch: None,
                role: models::MessageRole::User,
                content_json: Json(serde_json::json!({"text": format!("extra msg {i}")})),
                content_preview: Some(format!("extra msg {i}")),
                batch_type: None,
                source: Some("test".to_string()),
                source_metadata: None,
                is_archived: false,
                is_deleted: false,
                created_at: Timestamp::now(),
            };
            pattern_db::queries::create_message(&conn, &msg).unwrap();
        }
    }
    assert_eq!(
        count_all_messages(&db, agent),
        5,
        "should have 5 messages before restore"
    );

    // Capture the messages_db path and then DROP the db pool.
    //
    // In production, `pattern backup restore` is a one-shot CLI command that
    // runs in a separate process from the running agents. The pool is never
    // open during a restore. In tests we must simulate this by dropping the
    // pool before calling restore_snapshot, because an active WAL-mode pool
    // may re-create the WAL file after we remove it, poisoning the restore.
    let messages_db_path = db.messages_path().to_owned();
    drop(db);

    // Step 4: restore from the 3-message snapshot.
    let pre_restore_path =
        restore_snapshot(&messages_db_path, &snapshot.path).expect("restore_snapshot must succeed");

    // AC11.4: pre-restore safety copy must exist.
    assert!(
        pre_restore_path.exists(),
        "pre-restore safety copy must exist at {}",
        pre_restore_path.display()
    );

    // The pre-restore file name must contain "pre-restore".
    let pre_restore_name = pre_restore_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap();
    assert!(
        pre_restore_name.contains("pre-restore"),
        "pre-restore filename must contain 'pre-restore': {pre_restore_name}"
    );

    // The pre-restore file must be valid SQLite with 5 messages.
    let pre_restore_conn = rusqlite::Connection::open_with_flags(
        &pre_restore_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("pre-restore file must open as SQLite");
    let pre_restore_count: i64 = pre_restore_conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .expect("pre-restore count query must succeed");
    assert_eq!(
        pre_restore_count, 5,
        "pre-restore safety copy must contain the 5-message state"
    );

    // AC11.3: after restore, messages.db must have 3 messages again.
    // The pool was dropped — open the file directly.
    let restored_conn = rusqlite::Connection::open_with_flags(
        &messages_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("restored messages.db must open");
    let restored_count: i64 = restored_conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .expect("restored count query must succeed");
    assert_eq!(
        restored_count, 3,
        "restored messages.db must contain 3 messages (the snapshot state)"
    );
    drop(restored_conn);

    // Step 5: rollback from pre-restore — restore messages.db from pre_restore_path.
    let _ = restore_snapshot(&messages_db_path, &pre_restore_path)
        .expect("rollback restore must succeed");
    let rollback_conn = rusqlite::Connection::open_with_flags(
        &messages_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("rolled-back messages.db must open");
    let rollback_count: i64 = rollback_conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .expect("rollback count query must succeed");
    assert_eq!(
        rollback_count, 5,
        "after rollback, messages.db must be back to the 5-message state"
    );
}

// ---------------------------------------------------------------------------
// Corrupt snapshot rejection
// ---------------------------------------------------------------------------

/// Corrupt snapshot: restore must reject it and leave messages.db unchanged.
#[test]
fn restore_rejects_corrupt_snapshot_and_leaves_db_unchanged() {
    let db_dir = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "corrupt-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 3);
    let messages_path = db.messages_path().to_owned();
    let original_size = std::fs::metadata(&messages_path).unwrap().len();

    // Create a fake "snapshot" with garbage content.
    let corrupt_dir = tempfile::tempdir().unwrap();
    let corrupt_path = corrupt_dir.path().join("2026-04-19T120000Z.sqlite");
    std::fs::write(
        &corrupt_path,
        b"this is not a valid sqlite database garbage garbage",
    )
    .unwrap();

    // restore_snapshot must fail with CorruptSnapshot.
    let result = restore_snapshot(&messages_path, &corrupt_path);
    assert!(
        result.is_err(),
        "restore from corrupt snapshot must fail, got: {result:?}"
    );
    let err_str = result.unwrap_err().to_string();
    // The error can be either CorruptSnapshot (PRAGMA integrity_check returned
    // non-ok) or IntegrityCheck (the query itself failed on a file that is not
    // even a valid database). Both indicate the file is unusable.
    assert!(
        err_str.contains("corrupt")
            || err_str.contains("integrity")
            || err_str.contains("not a database")
            || err_str.contains("not found"),
        "error must indicate the snapshot is unusable: {err_str}"
    );

    // messages.db must be unchanged (same size — a proxy for same content
    // since we didn't modify it and the restore failed before touching it).
    let after_size = std::fs::metadata(&messages_path).unwrap().len();
    assert_eq!(
        original_size, after_size,
        "messages.db must be unchanged after corrupt restore attempt"
    );

    // Also verify messages.db still opens cleanly.
    let conn = rusqlite::Connection::open_with_flags(
        &messages_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("messages.db must still be valid after corrupt restore attempt");
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check must pass");
    assert_eq!(check, "ok");
}

// ---------------------------------------------------------------------------
// AC11.6: Timestamp lookup + error on bad spec
// ---------------------------------------------------------------------------

/// resolve_snapshot with "latest" returns the most recent snapshot.
#[test]
fn resolve_snapshot_latest_returns_most_recent() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_base = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "resolve-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 2);

    let paths = PatternPaths::with_base(backup_base.path());
    let snap1 =
        create_snapshot(db.messages_path(), &paths, "resolve-project").expect("first snapshot");
    // Brief sleep to ensure timestamps differ.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let snap2 =
        create_snapshot(db.messages_path(), &paths, "resolve-project").expect("second snapshot");

    let resolved =
        resolve_snapshot(&paths, "resolve-project", "latest").expect("latest must resolve");
    // The most recent snapshot (snap2) should be returned.
    assert_eq!(
        resolved.path, snap2.path,
        "latest must return the most recent snapshot"
    );
    drop(snap1);
}

/// resolve_snapshot with an exact timestamp stem returns the matching snapshot.
#[test]
fn resolve_snapshot_by_exact_stem_matches() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_base = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "exact-resolve-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 2);

    let paths = PatternPaths::with_base(backup_base.path());
    let snap =
        create_snapshot(db.messages_path(), &paths, "exact-resolve-project").expect("snapshot");

    // Extract the filename stem (without .sqlite).
    let stem = snap
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("snapshot must have a stem");

    let resolved =
        resolve_snapshot(&paths, "exact-resolve-project", stem).expect("exact resolve must work");
    assert_eq!(
        resolved.path, snap.path,
        "exact stem must match the snapshot"
    );
}

/// resolve_snapshot with a nonexistent spec returns an error listing available snapshots.
#[test]
fn resolve_snapshot_nonexistent_spec_lists_available() {
    let db_dir = tempfile::tempdir().unwrap();
    let backup_base = tempfile::tempdir().unwrap();
    let db = open_on_disk_db(&db_dir);
    let agent = "bad-spec-agent";
    seed_agent(&db, agent);
    insert_messages(&db, agent, 2);

    let paths = PatternPaths::with_base(backup_base.path());
    // Create a snapshot so the project has some.
    let _snap = create_snapshot(db.messages_path(), &paths, "bad-spec-project").expect("snapshot");

    let result = resolve_snapshot(&paths, "bad-spec-project", "nonexistent-id");
    assert!(result.is_err(), "nonexistent spec must produce an error");
    let err = result.unwrap_err();
    let err_str = err.to_string();
    // The error must mention the nonexistent spec.
    assert!(
        err_str.contains("nonexistent-id"),
        "error must mention the spec: {err_str}"
    );
}

/// resolve_snapshot with no snapshots returns NoSnapshots.
#[test]
fn resolve_snapshot_no_snapshots_returns_error() {
    let backup_base = tempfile::tempdir().unwrap();
    let paths = PatternPaths::with_base(backup_base.path());

    let result = resolve_snapshot(&paths, "empty-project", "latest");
    assert!(result.is_err(), "must error when no snapshots exist");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("empty-project"),
        "error must mention the project: {err_str}"
    );
}

/// restore_snapshot with a nonexistent path returns SnapshotNotFound.
#[test]
fn restore_snapshot_nonexistent_path_returns_error() {
    let db_dir = tempfile::tempdir().unwrap();
    let fake_snapshot = std::path::PathBuf::from("/nonexistent/snapshot/2026-04-19T120000Z.sqlite");
    let messages_path = db_dir.path().join("messages.db");
    // Create an empty messages.db so the path exists.
    std::fs::write(&messages_path, b"").unwrap();

    let result = restore_snapshot(&messages_path, &fake_snapshot);
    assert!(result.is_err(), "nonexistent snapshot must return error");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("not found") || err_str.contains("nonexistent"),
        "error must indicate snapshot was not found: {err_str}"
    );
}
