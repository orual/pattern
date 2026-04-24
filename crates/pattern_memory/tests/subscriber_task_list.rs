//! Integration tests for TaskList subscriber reconciliation.
//!
//! Covers:
//! - v3-task-skill-blocks.AC3.1: 5 items + 3 edges → correct row counts.
//! - v3-task-skill-blocks.AC3.2: deleting an item removes rows + edges.
//! - v3-task-skill-blocks.AC3.3: adding/removing edges updates `task_edges`.
//! - v3-task-skill-blocks.AC3.6: idempotent — running twice with no change
//!   produces the same final row set.

use loro::{LoroDoc, LoroValue};
use pattern_db::migrations::run_memory_migrations;
use pattern_memory::subscriber::task::{reconcile_task_list, ReconcileError};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Open an in-memory DB with all memory migrations applied.
fn fresh_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    run_memory_migrations(&mut conn).unwrap();
    conn
}

/// Build a single task item as a LoroValue::Map.
fn make_item(
    id: &str,
    subject: &str,
    status: &str,
    edges: &[(&str, Option<&str>)],
) -> LoroValue {
    let mut map: Vec<(String, LoroValue)> = vec![
        ("id".into(), LoroValue::String(id.into())),
        ("subject".into(), LoroValue::String(subject.into())),
        ("status".into(), LoroValue::String(status.into())),
    ];

    let edge_list: Vec<LoroValue> = edges
        .iter()
        .map(|(block, item)| {
            let mut edge: Vec<(String, LoroValue)> = vec![
                ("block".into(), LoroValue::String((*block).into())),
            ];
            if let Some(ti) = item {
                edge.push(("task_item".into(), LoroValue::String((*ti).into())));
            }
            LoroValue::Map(edge.into_iter().collect())
        })
        .collect();

    map.push(("blocks".into(), LoroValue::List(edge_list.into())));

    LoroValue::Map(map.into_iter().collect())
}

/// Build a LoroDoc with the given items in a movable list named `items`.
fn build_doc(items: &[LoroValue]) -> LoroDoc {
    let doc = LoroDoc::new();
    let list = doc.get_movable_list("items");
    for (i, item) in items.iter().enumerate() {
        list.insert(i, item.clone()).unwrap();
    }
    doc.commit();
    doc
}

/// Count rows in `tasks` for a given block_handle.
fn count_tasks(conn: &Connection, block_handle: &str) -> usize {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE block_handle = ?1",
        rusqlite::params![block_handle],
        |r| r.get::<_, i64>(0).map(|v| v as usize),
    )
    .unwrap()
}

/// Count rows in `task_edges` for a given source_block.
fn count_edges(conn: &Connection, source_block: &str) -> usize {
    conn.query_row(
        "SELECT COUNT(*) FROM task_edges WHERE source_block = ?1",
        rusqlite::params![source_block],
        |r| r.get::<_, i64>(0).map(|v| v as usize),
    )
    .unwrap()
}

/// Get all task_item_ids for a block.
fn task_item_ids(conn: &Connection, block_handle: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT task_item_id FROM tasks WHERE block_handle = ?1 ORDER BY task_item_id")
        .unwrap();
    stmt.query_map(rusqlite::params![block_handle], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Get all edges for a source block as `(source_item, target_block, target_item)`.
fn edges_for_block(
    conn: &Connection,
    source_block: &str,
) -> Vec<(String, String, Option<String>)> {
    let mut stmt = conn
        .prepare(
            "SELECT source_item, target_block, target_item FROM task_edges
             WHERE source_block = ?1
             ORDER BY source_item, target_block, target_item",
        )
        .unwrap();
    stmt.query_map(rusqlite::params![source_block], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// Run reconcile inside a transaction and commit.
fn reconcile_and_commit(
    conn: &mut Connection,
    block_handle: &str,
    doc: &LoroDoc,
) -> Result<(), ReconcileError> {
    let tx = conn.transaction().unwrap();
    reconcile_task_list(&tx, block_handle, doc)?;
    tx.commit().map_err(ReconcileError::from)
}

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
        make_item("item-1", "task one", "pending", &[
            ("block-x", Some("item-x1")),
            ("block-y", None),
        ]),
        make_item("item-2", "task two", "in-progress", &[
            ("block-z", Some("item-z1")),
        ]),
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
        make_item("item-1", "task one", "pending", &[
            ("block-x", Some("item-x1")),
            ("block-y", None),
        ]),
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
    let items_v1 = vec![
        make_item("item-1", "task one", "pending", &[]),
    ];
    let doc_v1 = build_doc(&items_v1);
    reconcile_and_commit(&mut conn, BH, &doc_v1).unwrap();

    assert_eq!(count_edges(&conn, BH), 0);

    // V2: item-1 now has one edge.
    let items_v2 = vec![
        make_item("item-1", "task one", "pending", &[
            ("block-x", Some("item-x1")),
        ]),
    ];
    let doc_v2 = build_doc(&items_v2);
    reconcile_and_commit(&mut conn, BH, &doc_v2).unwrap();

    assert_eq!(count_edges(&conn, BH), 1);
    let edges = edges_for_block(&conn, BH);
    assert_eq!(edges[0], (
        "item-1".to_string(),
        "block-x".to_string(),
        Some("item-x1".to_string()),
    ));
}

/// AC3.3: removing an edge deletes its row.
#[test]
fn remove_edge_deletes_row() {
    let mut conn = fresh_db();

    // V1: item-1 has 2 edges.
    let items_v1 = vec![
        make_item("item-1", "task one", "pending", &[
            ("block-x", Some("item-x1")),
            ("block-y", None),
        ]),
    ];
    let doc_v1 = build_doc(&items_v1);
    reconcile_and_commit(&mut conn, BH, &doc_v1).unwrap();

    assert_eq!(count_edges(&conn, BH), 2);

    // V2: item-1 has only 1 edge (removed block-y).
    let items_v2 = vec![
        make_item("item-1", "task one", "pending", &[
            ("block-x", Some("item-x1")),
        ]),
    ];
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
        make_item("item-1", "task one", "pending", &[
            ("block-x", Some("item-x1")),
        ]),
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
