//! Shared test fixture helpers for TaskList subscriber integration tests.
//!
//! Used by `subscriber_task_list.rs` and `subscriber_task_list_concurrent.rs`
//! to avoid duplicating helper functions. Anything specific to a single test
//! file stays in that file; only universally-needed fixtures live here.
//!
//! Each integration test binary compiles `mod common` independently, so
//! helpers that are only used in one binary generate dead-code warnings from
//! the other. The allow below suppresses that expected noise.
#![allow(dead_code)]

use loro::{LoroDoc, LoroValue};
use pattern_db::migrations::run_memory_migrations;
use pattern_memory::subscriber::task::{ReconcileError, reconcile_task_list};
use rusqlite::Connection;

/// Open an in-memory DB with all memory migrations applied.
pub fn fresh_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    run_memory_migrations(&mut conn).unwrap();
    conn
}

/// Build a single task item as a `LoroValue::Map`.
///
/// `edges` is a slice of `(target_block, Option<target_item>)`.
pub fn make_item(
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
            let mut edge: Vec<(String, LoroValue)> =
                vec![("block".into(), LoroValue::String((*block).into()))];
            if let Some(ti) = item {
                edge.push(("task_item".into(), LoroValue::String((*ti).into())));
            }
            LoroValue::Map(edge.into_iter().collect())
        })
        .collect();

    map.push(("blocks".into(), LoroValue::List(edge_list.into())));
    LoroValue::Map(map.into_iter().collect())
}

/// Build a `LoroDoc` with the given items in a movable list named `items`.
pub fn build_doc(items: &[LoroValue]) -> LoroDoc {
    let doc = LoroDoc::new();
    let list = doc.get_movable_list("items");
    for (i, item) in items.iter().enumerate() {
        list.insert(i, item.clone()).unwrap();
    }
    doc.commit();
    doc
}

/// Run `reconcile_task_list` inside a transaction and commit.
pub fn reconcile_and_commit(
    conn: &mut Connection,
    block_handle: &str,
    doc: &LoroDoc,
) -> Result<(), ReconcileError> {
    let tx = conn.transaction().unwrap();
    reconcile_task_list(&tx, block_handle, doc)?;
    tx.commit().map_err(ReconcileError::from)
}

/// Count rows in `tasks` for a given `block_handle`.
pub fn count_tasks(conn: &Connection, block_handle: &str) -> usize {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE block_handle = ?1",
        rusqlite::params![block_handle],
        |r| r.get::<_, i64>(0).map(|v| v as usize),
    )
    .unwrap()
}

/// Count rows in `task_edges` for a given `source_block`.
pub fn count_edges(conn: &Connection, source_block: &str) -> usize {
    conn.query_row(
        "SELECT COUNT(*) FROM task_edges WHERE source_block = ?1",
        rusqlite::params![source_block],
        |r| r.get::<_, i64>(0).map(|v| v as usize),
    )
    .unwrap()
}

/// Get all `task_item_id` values for a block, sorted.
pub fn task_item_ids(conn: &Connection, block_handle: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT task_item_id FROM tasks WHERE block_handle = ?1 ORDER BY task_item_id")
        .unwrap();
    stmt.query_map(rusqlite::params![block_handle], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Get all `(source_item, target_block, target_item)` triples for a block.
pub fn edges_for_block(
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
