// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for TaskList CRDT merge and scope enforcement.
//!
//! Covers:
//! - v3-task-skill-blocks.AC3.7: concurrent edits by two agents via two
//!   independent LoroDoc instances merge cleanly into a third doc, and the
//!   subscriber reconciles to the correct final state.
//! - Phase 2 scope enforcement: a TaskList block written at project scope is
//!   visible to a project-scope query but invisible to a persona-only query.

use loro::{ExportMode, LoroDoc};
use pattern_core::types::memory_types::task_query::TaskFilter;
use pattern_db::queries::list_tasks_filtered;

mod common;
use common::{
    build_doc, edges_for_block, fresh_db, make_item, reconcile_and_commit, task_item_ids,
};

// ---------------------------------------------------------------------------
// AC3.7: concurrent CRDT merge
// ---------------------------------------------------------------------------

/// AC3.7: Two agents edit independent tasks in concurrent LoroDoc instances;
/// both change sets merge cleanly and the subscriber reconciles to the
/// correct final state reflecting both agents' work.
///
/// Scenario:
/// - Base doc has two tasks: `item-c` (with edge C→D) and `item-e` (no edges).
/// - Agent A operates on its own doc (seeded from the same snapshot as agent B).
///   Agent A adds a new task `item-a` with an outgoing edge to `block-b/item-b1`.
/// - Agent B operates on its own doc. Agent B removes the C→D edge from `item-c`
///   (updates `item-c` to have no edges).
/// - A third "merge" doc imports the base snapshot, then agent A's updates,
///   then agent B's updates. This is the standard Loro CRDT merge protocol.
/// - After reconcile on the merged doc, `task_edges` must have exactly one edge:
///   A's new edge from `item-a` to `block-b/item-b1`. The C→D edge must be gone.
#[test]
fn concurrent_edits_merge_cleanly() {
    // ---- Build shared base doc ----
    // Two tasks: item-c has an outgoing edge (C→D), item-e has none.
    let base_items = vec![
        make_item(
            "item-c",
            "task c",
            "pending",
            &[("block-d", Some("item-d1"))],
        ),
        make_item("item-e", "task e", "in-progress", &[]),
    ];
    let base_doc = build_doc(&base_items);

    // Export a snapshot so both agents start from an identical base state.
    let base_snapshot = base_doc
        .export(ExportMode::Snapshot)
        .expect("base snapshot export failed");

    // Record the base version vector so we can isolate each agent's delta.
    let base_vv = base_doc.oplog_vv();

    // ---- Agent A: add item-a with edge A→B ----
    let doc_a = LoroDoc::new();
    doc_a
        .import(&base_snapshot)
        .expect("agent A: import base snapshot failed");

    // Verify agent A starts from the same state as the base.
    assert_eq!(
        doc_a.oplog_vv(),
        base_vv,
        "agent A vv must match base after snapshot import"
    );

    // Agent A appends a new item with an outgoing edge to block-b/item-b1.
    let list_a = doc_a.get_movable_list("items");
    let new_item_a = make_item(
        "item-a",
        "task a",
        "pending",
        &[("block-b", Some("item-b1"))],
    );
    // Append after the two existing items (index 2).
    list_a
        .insert(2, new_item_a)
        .expect("agent A: insert item-a failed");
    doc_a.commit();

    // Export only agent A's changes since the base.
    let updates_a = doc_a
        .export(ExportMode::updates(&base_vv))
        .expect("agent A: update export failed");
    assert!(
        !updates_a.is_empty(),
        "agent A must produce non-empty updates"
    );

    // ---- Agent B: remove the C→D edge from item-c ----
    let doc_b = LoroDoc::new();
    doc_b
        .import(&base_snapshot)
        .expect("agent B: import base snapshot failed");

    assert_eq!(
        doc_b.oplog_vv(),
        base_vv,
        "agent B vv must match base after snapshot import"
    );

    // Agent B replaces item-c (index 0) with a version that has no outgoing edges.
    let list_b = doc_b.get_movable_list("items");
    let item_c_no_edges = make_item("item-c", "task c", "pending", &[]);
    // Replace position 0 (item-c) with the edge-free version.
    list_b
        .set(0, item_c_no_edges)
        .expect("agent B: set item-c failed");
    doc_b.commit();

    // Export only agent B's changes since the base.
    let updates_b = doc_b
        .export(ExportMode::updates(&base_vv))
        .expect("agent B: update export failed");
    assert!(
        !updates_b.is_empty(),
        "agent B must produce non-empty updates"
    );

    // ---- Merge doc: base + A's updates + B's updates ----
    // The canonical Loro merge protocol: start from the same snapshot, then
    // import each agent's update bytes. Order of import is deterministic
    // because Loro's CRDT semantics are order-independent for non-conflicting
    // ops; we verify that both changes survive the merge.
    let doc_merge = LoroDoc::new();
    doc_merge
        .import(&base_snapshot)
        .expect("merge doc: base import failed");
    doc_merge
        .import(&updates_a)
        .expect("merge doc: agent A updates import failed");
    doc_merge
        .import(&updates_b)
        .expect("merge doc: agent B updates import failed");

    // Sanity: the merge doc's VV must be strictly ahead of the base.
    let merge_vv = doc_merge.oplog_vv();
    assert_ne!(
        merge_vv, base_vv,
        "merge doc vv must advance beyond base after applying both agents' updates"
    );

    // ---- Reconcile and assert ----
    let mut conn = fresh_db();
    const BH: &str = "concurrent-test-block";

    reconcile_and_commit(&mut conn, BH, &doc_merge).unwrap();

    // Expect 3 tasks: item-c, item-e (from base), item-a (from agent A).
    let ids = task_item_ids(&conn, BH);
    assert_eq!(
        ids.len(),
        3,
        "merged state must have 3 task rows; got: {ids:?}"
    );
    assert!(
        ids.contains(&"item-a".to_string()),
        "item-a must be present after merge"
    );
    assert!(
        ids.contains(&"item-c".to_string()),
        "item-c must be present after merge"
    );
    assert!(
        ids.contains(&"item-e".to_string()),
        "item-e must be present after merge"
    );

    // Expect exactly 1 edge: item-a → block-b/item-b1.
    // item-c's C→D edge was removed by agent B and must not appear.
    let edges = edges_for_block(&conn, BH);
    assert_eq!(
        edges.len(),
        1,
        "merged state must have exactly 1 edge (item-a's A→B); C→D must be gone; got: {edges:?}"
    );

    let (src_item, tgt_block, tgt_item) = &edges[0];
    assert_eq!(src_item, "item-a", "surviving edge source must be item-a");
    assert_eq!(
        tgt_block, "block-b",
        "surviving edge target block must be block-b"
    );
    assert_eq!(
        tgt_item.as_deref(),
        Some("item-b1"),
        "surviving edge target item must be item-b1"
    );
}

// ---------------------------------------------------------------------------
// Scope enforcement: project-scope tasks invisible to persona-only session
// ---------------------------------------------------------------------------

/// Phase 2 scope enforcement: a TaskList block written at project scope
/// is visible to a project-scope query but invisible to a persona-only query.
///
/// Scope in the DB layer is determined by the `block_handle` column — the
/// subscriber stores each TaskList block's rows under the handle provided at
/// reconcile time. A "project-scope session" queries tasks by the project
/// block handle and finds them; a "persona-scope session" queries by a
/// different (persona) block handle and correctly sees nothing.
///
/// This test exercises the end-to-end path described in the Phase 2 plan:
/// "Scope routing is the sibling plan's concern; this test just exercises it
/// end-to-end for TaskList." The block_handle IS the scope discriminator at
/// the DB layer — querying by a different handle is the scope-isolation
/// mechanism.
#[test]
fn scope_enforcement_project_only() {
    let mut conn = fresh_db();

    // ---- Project-scope session writes a TaskList block ----
    // The project session identifies its TaskList block by this handle.
    const PROJECT_BLOCK: &str = "project-scope-task-block";
    // The persona session has its own (different) block handle.
    const PERSONA_BLOCK: &str = "persona-scope-task-block";

    // Write 3 tasks into the project-scoped block.
    let project_items = vec![
        make_item("proj-task-1", "write tests", "pending", &[]),
        make_item("proj-task-2", "review PR", "in-progress", &[]),
        make_item(
            "proj-task-3",
            "deploy",
            "pending",
            &[("project-block-dep", Some("proj-task-1"))],
        ),
    ];
    let project_doc = build_doc(&project_items);
    reconcile_and_commit(&mut conn, PROJECT_BLOCK, &project_doc)
        .expect("project block reconcile failed");

    // The persona session has its own block with different tasks.
    let persona_items = vec![make_item("persona-task-1", "personal note", "pending", &[])];
    let persona_doc = build_doc(&persona_items);
    reconcile_and_commit(&mut conn, PERSONA_BLOCK, &persona_doc)
        .expect("persona block reconcile failed");

    // ---- Project-scope session queries by its block handle ----
    // list_tasks_filtered with no filter returns ALL tasks. The scope
    // enforcement at the DB layer is done by filtering on block_handle
    // — here we use the underlying SQL directly to mirror what a
    // project-scoped caller would do: query only the block handle it owns.
    let project_rows = {
        let mut stmt = conn
            .prepare("SELECT task_item_id FROM tasks WHERE block_handle = ?1 ORDER BY task_item_id")
            .unwrap();
        stmt.query_map(rusqlite::params![PROJECT_BLOCK], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        project_rows.len(),
        3,
        "project-scope query must return 3 task rows; got: {project_rows:?}"
    );
    assert!(
        project_rows.contains(&"proj-task-1".to_string()),
        "proj-task-1 must be visible to project-scope query"
    );
    assert!(
        project_rows.contains(&"proj-task-2".to_string()),
        "proj-task-2 must be visible to project-scope query"
    );
    assert!(
        project_rows.contains(&"proj-task-3".to_string()),
        "proj-task-3 must be visible to project-scope query"
    );

    // ---- Persona-scope session cannot see project tasks ----
    // A persona-only session queries by its own block handle. Project tasks
    // stored under the project block handle are not returned, because they
    // are indexed under a different handle — this is the scope-isolation
    // boundary at the DB layer.
    let persona_rows = {
        let mut stmt = conn
            .prepare("SELECT task_item_id FROM tasks WHERE block_handle = ?1 ORDER BY task_item_id")
            .unwrap();
        stmt.query_map(rusqlite::params![PERSONA_BLOCK], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        persona_rows.len(),
        1,
        "persona-scope query must return only 1 task (its own); got: {persona_rows:?}"
    );
    assert_eq!(
        persona_rows[0], "persona-task-1",
        "persona-scope query must return persona-task-1 only"
    );

    // Critically: the persona session must NOT see any of the project tasks.
    for proj_task in &["proj-task-1", "proj-task-2", "proj-task-3"] {
        assert!(
            !persona_rows.contains(&(*proj_task).to_string()),
            "persona-scope query must not see project task '{proj_task}'"
        );
    }

    // ---- Cross-check via list_tasks_filtered ----
    // list_tasks_filtered returns all tasks across all block handles when
    // no status/owner/keyword filter is applied. We verify that the total
    // row count is 4 (3 project + 1 persona), confirming that the two sets
    // of tasks are stored under separate handles and not interleaved.
    let all_rows = list_tasks_filtered(&conn, &TaskFilter::default())
        .expect("list_tasks_filtered must succeed");
    assert_eq!(
        all_rows.len(),
        4,
        "unfiltered list_tasks_filtered must return all 4 tasks (3 project + 1 persona); \
         got: {} rows",
        all_rows.len()
    );

    // Verify that the block_handle column in the returned rows correctly
    // identifies which scope each task belongs to.
    let project_task_ids: Vec<&str> = all_rows
        .iter()
        .filter(|r| r.block_handle.as_deref() == Some(PROJECT_BLOCK))
        .filter_map(|r| r.task_item_id.as_deref())
        .collect();
    let persona_task_ids: Vec<&str> = all_rows
        .iter()
        .filter(|r| r.block_handle.as_deref() == Some(PERSONA_BLOCK))
        .filter_map(|r| r.task_item_id.as_deref())
        .collect();

    assert_eq!(
        project_task_ids.len(),
        3,
        "3 rows must carry the project block_handle; got: {project_task_ids:?}"
    );
    assert_eq!(
        persona_task_ids.len(),
        1,
        "1 row must carry the persona block_handle; got: {persona_task_ids:?}"
    );

    // The persona session, enforcing scope isolation, sees only the persona
    // block's tasks — the project rows are there in the same DB but scoped
    // away by block_handle. This is the DB-layer scope enforcement guarantee.
    assert!(
        !persona_task_ids.contains(&"proj-task-1"),
        "proj-task-1 must not appear under the persona block_handle"
    );
    assert!(
        !persona_task_ids.contains(&"proj-task-2"),
        "proj-task-2 must not appear under the persona block_handle"
    );
    assert!(
        !persona_task_ids.contains(&"proj-task-3"),
        "proj-task-3 must not appear under the persona block_handle"
    );
}
