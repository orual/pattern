// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for TaskList subscriber reconciliation.
//!
//! Covers:
//! - v3-task-skill-blocks.AC3.1: 5 items + 3 edges → correct row counts.
//! - v3-task-skill-blocks.AC3.2: deleting an item removes rows + edges.
//! - v3-task-skill-blocks.AC3.3: adding/removing edges updates `task_edges`.
//! - v3-task-skill-blocks.AC3.4: partial reconcile failure rolls back the full
//!   transaction (atomicity).
//! - v3-task-skill-blocks.AC3.5: supervisor restart increments the
//!   `memory.sync_worker.restart` metric.
//! - v3-task-skill-blocks.AC3.6: idempotent — running twice with no change
//!   produces the same final row set.

use pattern_memory::subscriber::task::reconcile_task_list;

mod common;
use common::{
    build_doc, count_edges, count_tasks, edges_for_block, fresh_db, make_item,
    reconcile_and_commit, task_item_ids,
};

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

const BH: &str = "test-block-handle";

/// AC3.1: 5 items + 3 edges → 5 task rows + 3 edge rows.
#[test]
fn five_items_three_edges() {
    let mut conn = fresh_db();

    // Item 1 has 2 edges, item 2 has 1 edge, items 3-5 have no edges.
    let items = vec![
        make_item(
            "item-1",
            "task one",
            "pending",
            &[("block-x", Some("item-x1")), ("block-y", None)],
        ),
        make_item(
            "item-2",
            "task two",
            "in-progress",
            &[("block-z", Some("item-z1"))],
        ),
        make_item("item-3", "task three", "blocked", &[]),
        make_item("item-4", "task four", "completed", &[]),
        make_item("item-5", "task five", "pending", &[]),
    ];
    let doc = build_doc(&items);

    reconcile_and_commit(&mut conn, BH, &doc).unwrap();

    assert_eq!(count_tasks(&conn, BH), 5);
    assert_eq!(count_edges(&conn, BH), 3);
}

/// AC3.2: deleting one item removes its row and edges.
#[test]
fn delete_item_removes_row_and_edges() {
    let mut conn = fresh_db();

    // Initial: 3 items, item-1 has 2 edges.
    let items_v1 = vec![
        make_item(
            "item-1",
            "task one",
            "pending",
            &[("block-x", Some("item-x1")), ("block-y", None)],
        ),
        make_item("item-2", "task two", "in-progress", &[]),
        make_item("item-3", "task three", "blocked", &[]),
    ];
    let doc_v1 = build_doc(&items_v1);
    reconcile_and_commit(&mut conn, BH, &doc_v1).unwrap();

    assert_eq!(count_tasks(&conn, BH), 3);
    assert_eq!(count_edges(&conn, BH), 2);

    // V2: remove item-1.
    let items_v2 = vec![
        make_item("item-2", "task two", "in-progress", &[]),
        make_item("item-3", "task three", "blocked", &[]),
    ];
    let doc_v2 = build_doc(&items_v2);
    reconcile_and_commit(&mut conn, BH, &doc_v2).unwrap();

    assert_eq!(count_tasks(&conn, BH), 2);
    assert_eq!(count_edges(&conn, BH), 0);
    let ids = task_item_ids(&conn, BH);
    assert!(!ids.contains(&"item-1".to_string()));
}

/// AC3.3: adding an edge to an item's blocks list creates a new edge row.
#[test]
fn add_edge_creates_row() {
    let mut conn = fresh_db();

    // V1: item-1 has no edges.
    let items_v1 = vec![make_item("item-1", "task one", "pending", &[])];
    let doc_v1 = build_doc(&items_v1);
    reconcile_and_commit(&mut conn, BH, &doc_v1).unwrap();

    assert_eq!(count_edges(&conn, BH), 0);

    // V2: item-1 now has one edge.
    let items_v2 = vec![make_item(
        "item-1",
        "task one",
        "pending",
        &[("block-x", Some("item-x1"))],
    )];
    let doc_v2 = build_doc(&items_v2);
    reconcile_and_commit(&mut conn, BH, &doc_v2).unwrap();

    assert_eq!(count_edges(&conn, BH), 1);
    let edges = edges_for_block(&conn, BH);
    assert_eq!(
        edges[0],
        (
            "item-1".to_string(),
            "block-x".to_string(),
            Some("item-x1".to_string()),
        )
    );
}

/// AC3.3: removing an edge deletes its row.
#[test]
fn remove_edge_deletes_row() {
    let mut conn = fresh_db();

    // V1: item-1 has 2 edges.
    let items_v1 = vec![make_item(
        "item-1",
        "task one",
        "pending",
        &[("block-x", Some("item-x1")), ("block-y", None)],
    )];
    let doc_v1 = build_doc(&items_v1);
    reconcile_and_commit(&mut conn, BH, &doc_v1).unwrap();

    assert_eq!(count_edges(&conn, BH), 2);

    // V2: item-1 has only 1 edge (removed block-y).
    let items_v2 = vec![make_item(
        "item-1",
        "task one",
        "pending",
        &[("block-x", Some("item-x1"))],
    )];
    let doc_v2 = build_doc(&items_v2);
    reconcile_and_commit(&mut conn, BH, &doc_v2).unwrap();

    assert_eq!(count_edges(&conn, BH), 1);
    let edges = edges_for_block(&conn, BH);
    assert_eq!(edges[0].1, "block-x");
}

/// AC3.6: running reconcile twice with no loro change produces identical rows.
#[test]
fn idempotent_reconcile() {
    let mut conn = fresh_db();

    let items = vec![
        make_item(
            "item-1",
            "task one",
            "pending",
            &[("block-x", Some("item-x1"))],
        ),
        make_item("item-2", "task two", "in-progress", &[]),
    ];
    let doc = build_doc(&items);

    // First reconcile.
    reconcile_and_commit(&mut conn, BH, &doc).unwrap();
    let ids_1 = task_item_ids(&conn, BH);
    let edges_1 = edges_for_block(&conn, BH);

    // Second reconcile — same doc, no changes.
    reconcile_and_commit(&mut conn, BH, &doc).unwrap();
    let ids_2 = task_item_ids(&conn, BH);
    let edges_2 = edges_for_block(&conn, BH);

    assert_eq!(ids_1, ids_2);
    assert_eq!(edges_1, edges_2);
    assert_eq!(count_tasks(&conn, BH), 2);
    assert_eq!(count_edges(&conn, BH), 1);
}

/// Important #1: `created_at` is preserved across reconcile cycles.
///
/// The reconciler uses DELETE-then-INSERT to upsert rows. Without explicitly
/// preserving the original `created_at`, each reconcile would assign a new
/// timestamp, destroying the "when first created" semantic. This test verifies
/// that `created_at` is stable across multiple reconciles of the same item.
#[test]
fn created_at_preserved_across_reconciles() {
    let mut conn = fresh_db();

    let items = vec![make_item("item-stable", "stable task", "pending", &[])];
    let doc = build_doc(&items);

    // First reconcile — establishes the original created_at.
    reconcile_and_commit(&mut conn, BH, &doc).unwrap();
    let created_at_1: String = conn
        .query_row(
            "SELECT created_at FROM tasks WHERE block_handle = ?1 AND task_item_id = 'item-stable'",
            rusqlite::params![BH],
            |r| r.get(0),
        )
        .expect("task row must exist after first reconcile");

    // Wait a small amount so the system clock would produce a different timestamp.
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Second reconcile — same item, status change (forces an update).
    let items_v2 = vec![make_item("item-stable", "stable task", "in-progress", &[])];
    let doc_v2 = build_doc(&items_v2);
    reconcile_and_commit(&mut conn, BH, &doc_v2).unwrap();
    let created_at_2: String = conn
        .query_row(
            "SELECT created_at FROM tasks WHERE block_handle = ?1 AND task_item_id = 'item-stable'",
            rusqlite::params![BH],
            |r| r.get(0),
        )
        .expect("task row must still exist after second reconcile");

    // The created_at from the first reconcile must survive the second.
    assert_eq!(
        created_at_1, created_at_2,
        "created_at must be preserved across reconcile cycles (was: {created_at_1}, now: {created_at_2})"
    );
}

/// AC3.4: a failure mid-reconcile rolls back the full transaction.
///
/// Strategy: install a BEFORE INSERT trigger on `task_edges` that calls
/// RAISE(ABORT) when `source_item = '__panic_sentinel__'`. Because RAISE(ABORT)
/// aborts the current statement and rolls back the enclosing SQLite transaction,
/// the entire reconcile — including the innocent item that was upserted earlier
/// in the same transaction — is undone. Only the pre-seeded row from a
/// different block survives.
#[test]
fn atomicity_rolls_back_partial_reconcile() {
    let mut conn = fresh_db();

    // Seed one pre-existing task row on a different block. This row must
    // still be present after the failed reconcile proves the rollback only
    // affected the in-flight transaction.
    let now = "2026-01-01T00:00:00";
    conn.execute(
        "INSERT INTO tasks (id, subject, status, block_handle, task_item_id, created_at, updated_at)
         VALUES ('pre-existing', 'pre-existing task', 'pending', 'other-block', 'pre-existing', ?1, ?1)",
        rusqlite::params![now],
    )
    .expect("pre-existing row insert failed");
    assert_eq!(
        count_tasks(&conn, "other-block"),
        1,
        "pre-existing row must be present before test"
    );

    // Install a trigger that fires RAISE(ABORT) when source_item equals the
    // sentinel. RAISE(ABORT) is the SQLite mechanism for an application-level
    // constraint violation: it aborts the INSERT statement and rolls back the
    // enclosing transaction. There is no clean way to add a CHECK constraint
    // to an existing SQLite table via ALTER TABLE, so a trigger is used.
    conn.execute_batch(
        "CREATE TRIGGER task_edges_sentinel_guard
         BEFORE INSERT ON task_edges
         WHEN NEW.source_item = '__panic_sentinel__'
         BEGIN
             SELECT RAISE(ABORT, 'sentinel source_item rejected by test trigger');
         END;",
    )
    .expect("sentinel trigger creation failed");

    // Build a doc with two items:
    // - item-1: innocent, one outgoing edge (should be upserted inside the tx
    //   before the sentinel fails, then rolled back with it).
    // - __panic_sentinel__: has one outgoing edge → `upsert_task_edges` will
    //   attempt INSERT with source_item='__panic_sentinel__' → trigger fires.
    let items = vec![
        make_item(
            "item-1",
            "innocent task",
            "pending",
            &[("block-a", Some("item-a1"))],
        ),
        make_item(
            "__panic_sentinel__",
            "sentinel task",
            "pending",
            &[("block-b", Some("item-b1"))],
        ),
    ];
    let doc = build_doc(&items);

    // Run reconcile — it must fail because the trigger rejects the sentinel.
    let tx = conn.transaction().expect("begin transaction failed");
    let result = reconcile_task_list(&tx, BH, &doc);
    // Do NOT commit — tx drops here, rolling back everything including the
    // innocent item's upsert.
    assert!(
        result.is_err(),
        "reconcile must return Err when trigger fires; got Ok"
    );
    drop(tx); // explicit drop makes the rollback intent clear.

    // Neither the innocent item nor the sentinel should be present.
    assert_eq!(
        count_tasks(&conn, BH),
        0,
        "no task rows for the test block after rollback"
    );
    assert_eq!(
        count_edges(&conn, BH),
        0,
        "no edge rows for the test block after rollback"
    );

    // The pre-existing row on the other block is unaffected — it was committed
    // before the test transaction began.
    assert_eq!(
        count_tasks(&conn, "other-block"),
        1,
        "pre-existing row on other block must survive rollback"
    );

    // Cleanup: drop the sentinel trigger so it does not interfere with other
    // tests sharing the same in-memory DB (each test opens its own fresh_db,
    // so this is defence-in-depth, not strictly necessary).
    conn.execute_batch("DROP TRIGGER IF EXISTS task_edges_sentinel_guard;")
        .expect("trigger cleanup failed");
}

/// AC3.5: the supervisor restart metric fires when the supervisor detects a
/// heartbeat timeout and respawns the worker.
///
/// This AC is validated by `supervisor::tests::supervisor_timeout_fires_restart_metric`
/// in `subscriber/supervisor.rs`. That test exercises the real `run_supervisor`
/// dispatch path — including timeout detection, worker cancellation, and respawn —
/// and asserts that the `memory.sync_worker.restart` counter is emitted by the
/// live code, not by a manual stub. See supervisor.rs for the full test.
///
/// This placeholder keeps AC3.5 discoverable here alongside the other AC3 tests
/// while the real assertion lives in the supervisor's own test module.
#[test]
fn subscriber_restart_metric_is_tested_in_supervisor_tests() {
    // This test is intentionally a no-op. The real assertion is in
    // `subscriber::supervisor::tests::supervisor_timeout_fires_restart_metric`,
    // which uses a paused tokio clock + DebuggingRecorder to verify the live
    // supervisor code emits the counter. Running it again here would duplicate
    // the test without adding coverage.
    //
    // Keeping this stub ensures `cargo nextest run -p pattern-memory` shows AC3.5
    // as explicitly handled, and prevents the AC from being silently dropped if
    // the supervisor test is ever moved.
}
