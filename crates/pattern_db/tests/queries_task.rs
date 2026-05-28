// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for `pattern_db::queries::task` block-index query layer.
//!
//! Covers:
//! - `upsert_task_row` / `delete_task_row` CRUD.
//! - `upsert_task_edges` / `delete_task_edges_for_item` / `delete_task_edges_targeting`.
//! - `list_tasks_filtered` with status, owner, has_blockers, keyword, and blocks filters.
//! - FTS5 BM25 relevance ordering stability (insta snapshots).

use pattern_core::types::memory_types::task_query::TaskFilter;
use pattern_db::ConstellationDb;
use pattern_db::queries::task_row::TaskStatus;
use pattern_db::queries::{
    TaskRow, delete_task_edges_for_item, delete_task_edges_targeting, delete_task_row,
    list_tasks_filtered, upsert_task_edges, upsert_task_row,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open an in-memory DB with all migrations applied.
fn fresh_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().unwrap()
}

/// Insert a test agent to satisfy FK constraints on `tasks.agent_id`.
fn insert_test_agent(conn: &rusqlite::Connection) {
    conn.execute(
        "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, \
         config, enabled_tools, status, created_at, updated_at) \
         VALUES ('agent-1', 'TestAgent', 'test', 'test', '', '{}', '[]', 'active', \
         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
}

/// Build a minimal `TaskRow` with the given block/item IDs and subject.
fn make_task_row(
    id: &str,
    block_handle: &str,
    task_item_id: &str,
    subject: &str,
    status: TaskStatus,
    owner: Option<&str>,
    description: Option<&str>,
) -> TaskRow {
    use chrono::TimeZone;
    let ts = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    TaskRow {
        rowid: 0, // ignored on insert.
        id: id.to_string(),
        agent_id: Some("agent-1".to_string()),
        subject: subject.to_string(),
        description: description.map(|s| s.to_string()),
        status,
        due_at: None,
        scheduled_at: None,
        completed_at: None,
        parent_task_id: None,
        block_handle: Some(block_handle.to_string()),
        task_item_id: Some(task_item_id.to_string()),
        owner_agent_id: owner.map(|s| s.to_string()),
        comments_json: "[]".to_string(),
        created_at: ts,
        updated_at: ts,
    }
}

// ---------------------------------------------------------------------------
// upsert / delete tests
// ---------------------------------------------------------------------------

#[test]
fn upsert_one_row_then_list() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_test_agent(&conn);

    let row = make_task_row(
        "t-1",
        "blk-a",
        "item-1",
        "write tests",
        TaskStatus::Pending,
        None,
        None,
    );
    {
        let tx = conn.transaction().unwrap();
        upsert_task_row(&tx, &row).unwrap();
        tx.commit().unwrap();
    }

    let results = list_tasks_filtered(&conn, &TaskFilter::default()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].subject, "write tests");
}

#[test]
fn upsert_twice_same_key_produces_one_row() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_test_agent(&conn);

    let row1 = make_task_row(
        "t-1",
        "blk-a",
        "item-1",
        "original",
        TaskStatus::Pending,
        None,
        None,
    );
    let row2 = make_task_row(
        "t-2",
        "blk-a",
        "item-1",
        "updated",
        TaskStatus::InProgress,
        None,
        None,
    );
    {
        let tx = conn.transaction().unwrap();
        upsert_task_row(&tx, &row1).unwrap();
        upsert_task_row(&tx, &row2).unwrap();
        tx.commit().unwrap();
    }

    let results = list_tasks_filtered(&conn, &TaskFilter::default()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].subject, "updated");
    assert_eq!(results[0].status, TaskStatus::InProgress);
}

#[test]
fn delete_row_makes_list_empty() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_test_agent(&conn);

    let row = make_task_row(
        "t-1",
        "blk-a",
        "item-1",
        "doomed",
        TaskStatus::Pending,
        None,
        None,
    );
    {
        let tx = conn.transaction().unwrap();
        upsert_task_row(&tx, &row).unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        let deleted = delete_task_row(&tx, "blk-a", "item-1").unwrap();
        assert_eq!(deleted, 1);
        tx.commit().unwrap();
    }

    let results = list_tasks_filtered(&conn, &TaskFilter::default()).unwrap();
    assert!(results.is_empty());
}

// ---------------------------------------------------------------------------
// Edge tests
// ---------------------------------------------------------------------------

#[test]
fn insert_edges_and_delete_one() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    let edges = vec![
        ("blk-b".to_string(), Some("item-2".to_string())),
        ("blk-c".to_string(), Some("item-3".to_string())),
        ("blk-d".to_string(), None),
    ];
    {
        let tx = conn.transaction().unwrap();
        upsert_task_edges(&tx, "blk-a", "item-1", &edges).unwrap();
        tx.commit().unwrap();
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);

    {
        let tx = conn.transaction().unwrap();
        let deleted = delete_task_edges_targeting(&tx, "blk-c", Some("item-3")).unwrap();
        assert_eq!(deleted, 1);
        tx.commit().unwrap();
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn delete_edges_for_item_wipes_all() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    let edges = vec![
        ("blk-b".to_string(), Some("item-2".to_string())),
        ("blk-c".to_string(), Some("item-3".to_string())),
    ];
    {
        let tx = conn.transaction().unwrap();
        upsert_task_edges(&tx, "blk-a", "item-1", &edges).unwrap();
        tx.commit().unwrap();
    }

    {
        let tx = conn.transaction().unwrap();
        let deleted = delete_task_edges_for_item(&tx, "blk-a", "item-1").unwrap();
        assert_eq!(deleted, 2);
        tx.commit().unwrap();
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// list_tasks_filtered tests
// ---------------------------------------------------------------------------

/// Insert a 10-row fixture for filter tests.
fn insert_filter_fixture(conn: &mut rusqlite::Connection) {
    insert_test_agent(conn);
    conn.execute(
        "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, \
         config, enabled_tools, status, created_at, updated_at) \
         VALUES ('agent-2', 'Agent2', 'test', 'test', '', '{}', '[]', 'active', \
         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let tasks = vec![
        (
            "t-01",
            "blk-a",
            "i-01",
            "fix login timeout",
            TaskStatus::Pending,
            Some("agent-1"),
            Some("investigate the login timeout bug"),
        ),
        (
            "t-02",
            "blk-a",
            "i-02",
            "update auth docs",
            TaskStatus::Pending,
            Some("agent-1"),
            Some("refresh auth documentation"),
        ),
        (
            "t-03",
            "blk-a",
            "i-03",
            "review migration safety",
            TaskStatus::InProgress,
            Some("agent-2"),
            Some("check migration for data safety"),
        ),
        (
            "t-04",
            "blk-a",
            "i-04",
            "refactor token rotation",
            TaskStatus::InProgress,
            Some("agent-1"),
            Some("improve token rotation logic"),
        ),
        (
            "t-05",
            "blk-a",
            "i-05",
            "audit password hashing",
            TaskStatus::Completed,
            Some("agent-2"),
            Some("audit bcrypt password hashing"),
        ),
        (
            "t-06",
            "blk-a",
            "i-06",
            "add rate limiting",
            TaskStatus::Pending,
            Some("agent-1"),
            None,
        ),
        (
            "t-07",
            "blk-a",
            "i-07",
            "fix session expiry",
            TaskStatus::Blocked,
            Some("agent-2"),
            Some("session tokens expire too early"),
        ),
        (
            "t-08",
            "blk-a",
            "i-08",
            "update error messages",
            TaskStatus::Pending,
            None,
            Some("improve user-facing error messages"),
        ),
        (
            "t-09",
            "blk-a",
            "i-09",
            "add MFA support",
            TaskStatus::Cancelled,
            Some("agent-1"),
            None,
        ),
        (
            "t-10",
            "blk-a",
            "i-10",
            "deploy auth service",
            TaskStatus::Pending,
            Some("agent-2"),
            Some("deploy the auth service to production"),
        ),
    ];

    let tx = conn.transaction().unwrap();
    for (id, blk, item, subject, status, owner, desc) in &tasks {
        let row = make_task_row(id, blk, item, subject, *status, *owner, *desc);
        upsert_task_row(&tx, &row).unwrap();
    }

    // Edges for has_blockers testing: i-04 blocks i-01, i-10 blocks i-07.
    upsert_task_edges(
        &tx,
        "blk-a",
        "i-04",
        &[("blk-a".to_string(), Some("i-01".to_string()))],
    )
    .unwrap();
    upsert_task_edges(
        &tx,
        "blk-a",
        "i-10",
        &[("blk-a".to_string(), Some("i-07".to_string()))],
    )
    .unwrap();

    tx.commit().unwrap();
}

#[test]
fn filter_by_status() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_filter_fixture(&mut conn);

    let filter = TaskFilter {
        status: Some(vec![TaskStatus::Pending]),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    // t-01, t-02, t-06, t-08, t-10 are pending.
    assert_eq!(results.len(), 5);
}

#[test]
fn filter_by_owner() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_filter_fixture(&mut conn);

    let filter = TaskFilter {
        owner: Some(smol_str::SmolStr::new("agent-2")),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    // t-03, t-05, t-07, t-10.
    assert_eq!(results.len(), 4);
}

#[test]
fn filter_by_has_blockers_true() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_filter_fixture(&mut conn);

    let filter = TaskFilter {
        has_blockers: Some(true),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    let ids: Vec<&str> = results
        .iter()
        .filter_map(|r| r.task_item_id.as_deref())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"i-01"));
    assert!(ids.contains(&"i-07"));
}

#[test]
fn filter_by_has_blockers_false() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_filter_fixture(&mut conn);

    let filter = TaskFilter {
        has_blockers: Some(false),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    // 10 total - 2 blocked = 8.
    assert_eq!(results.len(), 8);
}

#[test]
fn filter_by_keyword_fts5() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_filter_fixture(&mut conn);

    let filter = TaskFilter {
        keyword: Some("auth".to_string()),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    assert!(
        results.len() >= 2,
        "expected at least 2 results for 'auth', got {}",
        results.len()
    );
    let subjects: Vec<&str> = results.iter().map(|r| r.subject.as_str()).collect();
    assert!(subjects.iter().any(|s| s.contains("auth")));
}

#[test]
fn filter_combined_status_and_owner() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_filter_fixture(&mut conn);

    let filter = TaskFilter {
        status: Some(vec![TaskStatus::InProgress]),
        owner: Some(smol_str::SmolStr::new("agent-2")),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    // Only t-03 is in-progress AND owned by agent-2.
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].task_item_id.as_deref(), Some("i-03"));
}

// ---------------------------------------------------------------------------
// blocks filter tests
// ---------------------------------------------------------------------------

/// Insert tasks across two block handles for blocks-filter testing.
fn insert_two_block_fixture(conn: &mut rusqlite::Connection) {
    insert_test_agent(conn);

    let tasks = vec![
        (
            "t-b1-01",
            "blk-alpha",
            "i-01",
            "alpha task one",
            TaskStatus::Pending,
        ),
        (
            "t-b1-02",
            "blk-alpha",
            "i-02",
            "alpha task two",
            TaskStatus::InProgress,
        ),
        (
            "t-b2-01",
            "blk-beta",
            "i-01",
            "beta task one",
            TaskStatus::Pending,
        ),
        (
            "t-b2-02",
            "blk-beta",
            "i-02",
            "beta task two",
            TaskStatus::Completed,
        ),
        (
            "t-b2-03",
            "blk-beta",
            "i-03",
            "beta task three",
            TaskStatus::Blocked,
        ),
    ];

    let tx = conn.transaction().unwrap();
    for (id, blk, item, subject, status) in &tasks {
        let row = make_task_row(id, blk, item, subject, *status, None, None);
        upsert_task_row(&tx, &row).unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn filter_blocks_single_handle_returns_only_that_block() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_two_block_fixture(&mut conn);

    let filter = TaskFilter {
        blocks: Some(vec![smol_str::SmolStr::new("blk-alpha")]),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();

    assert_eq!(
        results.len(),
        2,
        "blk-alpha has 2 tasks; got: {:?}",
        results
            .iter()
            .map(|r| r.task_item_id.as_deref())
            .collect::<Vec<_>>()
    );
    assert!(
        results
            .iter()
            .all(|r| r.block_handle.as_deref() == Some("blk-alpha")),
        "all results must be from blk-alpha"
    );
}

#[test]
fn filter_blocks_multiple_handles_returns_union() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_two_block_fixture(&mut conn);

    let filter = TaskFilter {
        blocks: Some(vec![
            smol_str::SmolStr::new("blk-alpha"),
            smol_str::SmolStr::new("blk-beta"),
        ]),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();

    // 2 from blk-alpha + 3 from blk-beta = 5 total.
    assert_eq!(results.len(), 5, "union of both blocks must return 5 tasks");
}

#[test]
fn filter_blocks_none_returns_all() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_two_block_fixture(&mut conn);

    let results = list_tasks_filtered(&conn, &TaskFilter::default()).unwrap();
    assert_eq!(
        results.len(),
        5,
        "no block constraint must return all 5 tasks"
    );
}

#[test]
fn filter_blocks_empty_vec_returns_no_results() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_two_block_fixture(&mut conn);

    let filter = TaskFilter {
        blocks: Some(vec![]),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();
    assert!(
        results.is_empty(),
        "Some(empty vec) must return no results — it is 'no block constraint' vs 'all results'"
    );
}

#[test]
fn filter_blocks_combined_with_status() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_two_block_fixture(&mut conn);

    // Only pending tasks in blk-beta — only beta task one.
    let filter = TaskFilter {
        blocks: Some(vec![smol_str::SmolStr::new("blk-beta")]),
        status: Some(vec![TaskStatus::Pending]),
        ..Default::default()
    };
    let results = list_tasks_filtered(&conn, &filter).unwrap();

    assert_eq!(results.len(), 1, "blk-beta has 1 pending task");
    assert_eq!(results[0].subject, "beta task one");
}

// ---------------------------------------------------------------------------
// FTS5 BM25 relevance ordering snapshot tests
// ---------------------------------------------------------------------------

/// Insert the 5-row FTS5 fixture specified in the task plan.
fn insert_fts5_fixture(conn: &mut rusqlite::Connection) {
    insert_test_agent(conn);

    let tasks = vec![
        (
            "t-01",
            "blk-a",
            "i-01",
            "fix login timeout",
            "users report login page timing out after 30 seconds",
        ),
        (
            "t-02",
            "blk-a",
            "i-02",
            "update auth docs",
            "refresh authentication documentation for the new OAuth2 flow",
        ),
        (
            "t-03",
            "blk-a",
            "i-03",
            "review migration safety",
            "review the database migration for data safety and rollback support",
        ),
        (
            "t-04",
            "blk-a",
            "i-04",
            "refactor token rotation",
            "improve auth token rotation logic to reduce latency",
        ),
        (
            "t-05",
            "blk-a",
            "i-05",
            "audit password hashing",
            "audit bcrypt password hashing configuration and salt rounds",
        ),
    ];

    let tx = conn.transaction().unwrap();
    for (id, blk, item, subject, desc) in &tasks {
        let row = make_task_row(
            id,
            blk,
            item,
            subject,
            TaskStatus::Pending,
            None,
            Some(desc),
        );
        upsert_task_row(&tx, &row).unwrap();
    }
    tx.commit().unwrap();
}

/// Query FTS5 with BM25 scores and return `(task_item_id, rounded_score)` pairs.
fn fts5_ranked_query(conn: &rusqlite::Connection, keyword: &str) -> Vec<(String, f64)> {
    let sql = "SELECT t.task_item_id, bm25(tasks_fts) as score \
               FROM tasks t \
               JOIN tasks_fts ON tasks_fts.rowid = t.rowid \
               WHERE tasks_fts MATCH ?1 \
               ORDER BY score ASC";
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt
        .query_map(rusqlite::params![keyword], |row| {
            let item_id: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            Ok((item_id, (score * 10000.0).round() / 10000.0))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn fts5_relevance_auth() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_fts5_fixture(&mut conn);

    let results = fts5_ranked_query(&conn, "auth");
    insta::assert_yaml_snapshot!("fts5_task_relevance_auth", results);
}

#[test]
fn fts5_relevance_review() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_fts5_fixture(&mut conn);

    let results = fts5_ranked_query(&conn, "review");
    insta::assert_yaml_snapshot!("fts5_task_relevance_review", results);
}

#[test]
fn fts5_relevance_timeout() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();
    insert_fts5_fixture(&mut conn);

    let results = fts5_ranked_query(&conn, "timeout");
    insta::assert_yaml_snapshot!("fts5_task_relevance_timeout", results);
}
