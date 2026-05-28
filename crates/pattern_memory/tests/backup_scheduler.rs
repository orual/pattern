// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for the backup scheduler.
//!
//! Tests the tokio interval task that periodically snapshots messages.db when
//! new messages have been written since the last snapshot. The scheduler is
//! tied to `MountedStore` lifecycle via a `CancellationToken`.

use std::sync::Arc;
use std::time::Duration;

use pattern_memory::backup::rotation::list_snapshots;
use pattern_memory::backup::scheduler::{BackupPolicy, BackupScheduler};
use pattern_memory::backup::types::RetentionPolicy;
use pattern_memory::paths::PatternPaths;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a minimal messages.db for tests.
fn create_messages_db(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS messages (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            position TEXT NOT NULL,
            batch_id TEXT,
            sequence_in_batch INTEGER,
            role TEXT NOT NULL,
            content_json JSON NOT NULL,
            content_preview TEXT,
            batch_type TEXT,
            source TEXT,
            source_metadata JSON,
            is_archived INTEGER NOT NULL DEFAULT 0,
            is_deleted INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );",
    )
    .unwrap();
    // Enable WAL mode to simulate production conditions.
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
}

/// Insert a message into messages.db.
fn insert_message(db_path: &std::path::Path, id: &str) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute(
        "INSERT INTO messages (id, agent_id, position, role, content_json, created_at)
         VALUES (?1, 'agent-1', ?1, 'user', '{}', datetime('now'))",
        rusqlite::params![id],
    )
    .unwrap();
}

#[allow(dead_code)]
fn count_messages(db_path: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Scheduler creates a snapshot after `snapshot_interval` when messages exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_creates_snapshots_periodically() {
    let base_tmp = TempDir::new().unwrap();
    let db_tmp = TempDir::new().unwrap();

    let paths = PatternPaths::with_base(base_tmp.path());
    let project_id = "test-scheduler-periodic";

    let messages_db_path = db_tmp.path().join("messages.db");
    create_messages_db(&messages_db_path);
    insert_message(&messages_db_path, "msg-1");
    insert_message(&messages_db_path, "msg-2");

    let policy = Arc::new(BackupPolicy {
        snapshot_interval: Duration::from_millis(400),
        retention: RetentionPolicy {
            keep_recent: 10,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: false,
        },
    });

    let scheduler = BackupScheduler::spawn(
        Arc::new(messages_db_path.clone()),
        project_id.to_string(),
        policy,
        Arc::new(paths.clone()),
    );

    // Wait long enough for at least 2 ticks.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    scheduler.cancel();
    scheduler
        .join()
        .await
        .expect("scheduler task should not panic");

    let snapshots = list_snapshots(&paths, project_id).unwrap();
    assert!(
        !snapshots.is_empty(),
        "at least one snapshot should have been created"
    );
}

/// Scheduler skips ticks when no new messages have been written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_skips_when_no_new_messages() {
    let base_tmp = TempDir::new().unwrap();
    let db_tmp = TempDir::new().unwrap();

    let paths = PatternPaths::with_base(base_tmp.path());
    let project_id = "test-scheduler-skip";

    let messages_db_path = db_tmp.path().join("messages.db");
    create_messages_db(&messages_db_path);
    insert_message(&messages_db_path, "initial-msg");

    let policy = Arc::new(BackupPolicy {
        snapshot_interval: Duration::from_millis(300),
        retention: RetentionPolicy {
            keep_recent: 10,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: false,
        },
    });

    let scheduler = BackupScheduler::spawn(
        Arc::new(messages_db_path.clone()),
        project_id.to_string(),
        policy,
        Arc::new(paths.clone()),
    );

    // Wait for the initial snapshot (messages exist → should snapshot on first tick).
    tokio::time::sleep(Duration::from_millis(600)).await;

    let snapshots_after_first = list_snapshots(&paths, project_id).unwrap();
    let count_after_first = snapshots_after_first.len();

    // No new messages written; subsequent ticks should be skipped.
    tokio::time::sleep(Duration::from_millis(800)).await;

    scheduler.cancel();
    scheduler
        .join()
        .await
        .expect("scheduler task should not panic");

    let snapshots_final = list_snapshots(&paths, project_id).unwrap();
    // May have 1 initial snapshot. No more should be added without new messages.
    // We allow up to count_after_first+1 in case of a race, but not many more.
    assert!(
        snapshots_final.len() <= count_after_first + 1,
        "scheduler should not create redundant snapshots; first={count_after_first} final={}",
        snapshots_final.len()
    );
}

/// Scheduler creates a NEW snapshot after a snapshot already exists and new
/// messages arrive since the last snapshot.
///
/// This is the steady-state path: `should_snapshot` must compare the
/// `created_at` column against the last snapshot timestamp and correctly
/// detect new messages even when the text formats differ between what chrono
/// stores ("2026-04-19 12:00:00+00:00") and what jiff formats. The previous
/// ISO 8601 comparison was broken; the unix-epoch comparison this test
/// exercises must work correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_detects_new_messages_after_snapshot() {
    use pattern_memory::backup::snapshot::create_snapshot;

    let base_tmp = TempDir::new().unwrap();
    let db_tmp = TempDir::new().unwrap();

    let paths = PatternPaths::with_base(base_tmp.path());
    let project_id = "test-scheduler-steady-state";

    let messages_db_path = db_tmp.path().join("messages.db");
    create_messages_db(&messages_db_path);

    // Insert initial messages and create a manual snapshot to simulate a
    // pre-existing snapshot that predates the messages we'll insert next.
    insert_message(&messages_db_path, "pre-snapshot-msg-1");
    insert_message(&messages_db_path, "pre-snapshot-msg-2");

    // Create a baseline snapshot (simulates the scheduler already having run
    // once and snapshotted the initial messages).
    create_snapshot(&messages_db_path, &paths, project_id)
        .expect("initial snapshot should succeed");

    let snapshots_after_initial = list_snapshots(&paths, project_id).unwrap();
    assert_eq!(
        snapshots_after_initial.len(),
        1,
        "should have exactly one snapshot after manual create"
    );

    // Wait 1.1s to ensure the next messages have a created_at that is strictly
    // after the snapshot timestamp. SQLite's datetime('now') has 1s resolution
    // in some configurations, so a small sleep guarantees temporal ordering.
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // Insert NEW messages after the snapshot — the scheduler must detect these.
    insert_message(&messages_db_path, "post-snapshot-msg-1");
    insert_message(&messages_db_path, "post-snapshot-msg-2");

    let policy = Arc::new(BackupPolicy {
        snapshot_interval: Duration::from_millis(300),
        retention: RetentionPolicy {
            keep_recent: 10,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: false,
        },
    });

    // Start the scheduler; it should detect the post-snapshot messages and
    // create a second snapshot.
    let scheduler = BackupScheduler::spawn(
        Arc::new(messages_db_path.clone()),
        project_id.to_string(),
        policy,
        Arc::new(paths.clone()),
    );

    // Wait for at least two ticks to give the scheduler time to detect and
    // snapshot the new messages.
    tokio::time::sleep(Duration::from_millis(900)).await;

    scheduler.cancel();
    scheduler
        .join()
        .await
        .expect("scheduler task should not panic");

    let snapshots_final = list_snapshots(&paths, project_id).unwrap();
    assert!(
        snapshots_final.len() > 1,
        "scheduler should have created a second snapshot after detecting new messages; \
         got {} snapshot(s)",
        snapshots_final.len()
    );
}

/// Scheduler task is cancelled cleanly on `cancel()` + `join()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_cancels_cleanly() {
    let base_tmp = TempDir::new().unwrap();
    let db_tmp = TempDir::new().unwrap();

    let paths = PatternPaths::with_base(base_tmp.path());
    let project_id = "test-scheduler-cancel";

    let messages_db_path = db_tmp.path().join("messages.db");
    create_messages_db(&messages_db_path);

    let policy = Arc::new(BackupPolicy {
        // Very long interval — we cancel before it fires.
        snapshot_interval: Duration::from_secs(3600),
        retention: RetentionPolicy::default(),
    });

    let scheduler = BackupScheduler::spawn(
        Arc::new(messages_db_path.clone()),
        project_id.to_string(),
        policy,
        Arc::new(paths.clone()),
    );

    // Cancel immediately.
    scheduler.cancel();

    // Join with a short timeout — should complete quickly.
    let result = tokio::time::timeout(Duration::from_secs(2), scheduler.join()).await;

    assert!(
        result.is_ok(),
        "scheduler should join within 2s after cancel"
    );
    assert!(
        result.unwrap().is_ok(),
        "scheduler task should not have panicked"
    );
}
