//! Integration tests for `query_task_graph_bfs`.
//!
//! Covers the BFS walker contract in isolation:
//! 1. depth=0 returns root only, zero edges.
//! 2. 5-node chain (A→B→C→D→E), Forward, depth=unlimited → 5 nodes + 4 edges.
//! 3. Same chain, depth=2 → 3 nodes + 2 edges.
//! 4. Cycle (A→B→C→A), Forward, depth=10 → terminates; 3 nodes + 3 edges (visited-set).
//! 5. 10k-node graph, max_nodes=1000 → truncated=true, completes in < 1 second.
//! 6. Direction::Reverse on a block-level target (target_item=NULL) returns sources.
//!
//! These tests verify Phase 3 AC5.4/AC5.6b/AC5.7/AC5.8 primitives in isolation.

use std::time::Instant;

use pattern_db::ConstellationDb;
use pattern_db::queries::{GraphDirection, upsert_task_edges, query_task_graph_bfs};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open an in-memory DB with all migrations applied.
fn fresh_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().unwrap()
}

/// Insert a directed edge `source_item → (target_block, target_item)`.
///
/// All source/target nodes live in the same "blk-main" block for simplicity
/// unless `target_block` is explicitly different.
fn insert_edge(
    conn: &mut rusqlite::Connection,
    source_item: &str,
    target_block: &str,
    target_item: Option<&str>,
) {
    let tx = conn.transaction().unwrap();
    // Read the existing edges for this source so we can append rather than clobber.
    let existing: Vec<(String, Option<String>)> = {
        let mut stmt = tx
            .prepare(
                "SELECT target_block, target_item FROM task_edges \
                 WHERE source_block = 'blk-main' AND source_item = ?1",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![source_item], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };

    let mut edges = existing;
    edges.push((
        target_block.to_string(),
        target_item.map(|s| s.to_string()),
    ));
    upsert_task_edges(&tx, "blk-main", source_item, &edges).unwrap();
    tx.commit().unwrap();
}

/// Build a linear chain: node-0 → node-1 → … → node-(n-1).
///
/// All nodes live in "blk-main". The chain represents `n` distinct task item
/// IDs (`"n-0"`, `"n-1"`, …, `"n-{n-1}"`).
fn build_chain(conn: &mut rusqlite::Connection, n: usize) {
    for i in 0..(n.saturating_sub(1)) {
        insert_edge(
            conn,
            &format!("n-{i}"),
            "blk-main",
            Some(&format!("n-{}", i + 1)),
        );
    }
}

// ---------------------------------------------------------------------------
// Test 1: depth=0 returns root only, zero edges
// ---------------------------------------------------------------------------

#[test]
fn depth_zero_returns_root_only() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // Build a 3-node chain so there are edges to traverse — but we cap at depth 0.
    build_chain(&mut conn, 3);

    let result = query_task_graph_bfs(&conn, "blk-main", Some("n-0"), GraphDirection::Forward, 0, 1000).unwrap();

    assert_eq!(result.nodes.len(), 1, "depth=0 must return only the root node");
    assert_eq!(
        result.nodes[0],
        ("blk-main".to_string(), Some("n-0".to_string()))
    );
    assert!(
        result.edges.is_empty(),
        "depth=0 must return zero edges; got {:?}",
        result.edges
    );
    assert!(!result.truncated, "depth=0 on a small graph must not truncate");
}

// ---------------------------------------------------------------------------
// Test 2: 5-node chain, Forward, depth=unlimited → 5 nodes + 4 edges
// ---------------------------------------------------------------------------

#[test]
fn five_node_chain_forward_unlimited_depth() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // A→B→C→D→E  (n-0 through n-4)
    build_chain(&mut conn, 5);

    // u32::MAX as "unlimited" — the chain is only 5 nodes so we'll exhaust it.
    let result =
        query_task_graph_bfs(&conn, "blk-main", Some("n-0"), GraphDirection::Forward, u32::MAX, 1000)
            .unwrap();

    assert_eq!(
        result.nodes.len(),
        5,
        "5-node chain must yield 5 nodes; got {:?}",
        result.nodes
    );
    assert_eq!(
        result.edges.len(),
        4,
        "5-node chain must yield 4 directed edges; got {:?}",
        result.edges
    );
    assert!(!result.truncated);

    // Verify BFS ordering: root first.
    assert_eq!(
        result.nodes[0],
        ("blk-main".to_string(), Some("n-0".to_string())),
        "first node must be the root"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Same 5-node chain, depth=2 → 3 nodes + 2 edges
// ---------------------------------------------------------------------------

#[test]
fn five_node_chain_forward_depth_two() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    build_chain(&mut conn, 5);

    let result =
        query_task_graph_bfs(&conn, "blk-main", Some("n-0"), GraphDirection::Forward, 2, 1000)
            .unwrap();

    // depth=2: root (depth 0) + n-1 (depth 1) + n-2 (depth 2). n-3 would be depth 3 — excluded.
    assert_eq!(
        result.nodes.len(),
        3,
        "depth=2 on 5-node chain must yield 3 nodes; got {:?}",
        result.nodes
    );
    assert_eq!(
        result.edges.len(),
        2,
        "depth=2 must yield 2 edges; got {:?}",
        result.edges
    );
    assert!(!result.truncated);

    let node_items: Vec<Option<&str>> =
        result.nodes.iter().map(|(_, i)| i.as_deref()).collect();
    assert!(node_items.contains(&Some("n-0")));
    assert!(node_items.contains(&Some("n-1")));
    assert!(node_items.contains(&Some("n-2")));
    assert!(!node_items.contains(&Some("n-3")));
}

// ---------------------------------------------------------------------------
// Test 4: Cycle A→B→C→A, Forward, depth=10 — terminates; 3 nodes + 3 edges
// ---------------------------------------------------------------------------

#[test]
fn cycle_terminates_with_visited_set() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // A → B → C → A  (cycle).
    insert_edge(&mut conn, "n-0", "blk-main", Some("n-1")); // A → B
    insert_edge(&mut conn, "n-1", "blk-main", Some("n-2")); // B → C
    insert_edge(&mut conn, "n-2", "blk-main", Some("n-0")); // C → A  (back-edge)

    let result =
        query_task_graph_bfs(&conn, "blk-main", Some("n-0"), GraphDirection::Forward, 10, 1000)
            .unwrap();

    // The visited-set prevents re-enqueuing n-0 when the cycle closes.
    assert_eq!(
        result.nodes.len(),
        3,
        "cycle must produce exactly 3 unique nodes; got {:?}",
        result.nodes
    );
    // All 3 directed edges — including the back-edge — are recorded.
    assert_eq!(
        result.edges.len(),
        3,
        "cycle must produce 3 edges (including back-edge); got {:?}",
        result.edges
    );
    assert!(!result.truncated, "small cycle must not truncate");
}

// ---------------------------------------------------------------------------
// Test 5: 10k-node graph, max_nodes=1000 → truncated=true, < 1 second
// ---------------------------------------------------------------------------

#[test]
fn large_graph_truncates_at_max_nodes_within_one_second() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // Build a 10 000-node linear chain.  The BFS will stop at max_nodes=1000.
    // We insert edges in bulk via a single transaction for speed.
    {
        let tx = conn.transaction().unwrap();
        let mut stmt = tx
            .prepare(
                "INSERT INTO task_edges (source_block, source_item, target_block, target_item) \
                 VALUES ('blk-main', ?1, 'blk-main', ?2)",
            )
            .unwrap();
        for i in 0..9999usize {
            stmt.execute(rusqlite::params![
                format!("n-{i}"),
                format!("n-{}", i + 1),
            ])
            .unwrap();
        }
        drop(stmt);
        tx.commit().unwrap();
    }

    let start = Instant::now();
    let result =
        query_task_graph_bfs(&conn, "blk-main", Some("n-0"), GraphDirection::Forward, u32::MAX, 1000)
            .unwrap();
    let elapsed = start.elapsed();

    assert!(
        result.truncated,
        "10k-node walk with max_nodes=1000 must set truncated=true"
    );
    assert_eq!(
        result.nodes.len(),
        1000,
        "truncated result must contain exactly max_nodes nodes"
    );
    assert!(
        elapsed.as_secs() < 1,
        "BFS over 10k-node graph truncated at 1000 must complete in < 1 second; took {:?}",
        elapsed
    );
}

// ---------------------------------------------------------------------------
// Test 6: Direction::Reverse on block-level target (target_item=NULL)
// ---------------------------------------------------------------------------

#[test]
fn reverse_direction_block_level_target_returns_sources() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // Two source items both target the block "blk-target" at the block level
    // (target_item = NULL).  Reverse BFS from the root of "blk-target" must
    // discover both sources.
    {
        let tx = conn.transaction().unwrap();
        // source-1 → blk-target (block-level, NULL target_item)
        upsert_task_edges(
            &tx,
            "blk-main",
            "src-1",
            &[("blk-target".to_string(), None)],
        )
        .unwrap();
        // source-2 → blk-target (block-level)
        upsert_task_edges(
            &tx,
            "blk-main",
            "src-2",
            &[("blk-target".to_string(), None)],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Reverse walk starting from the block-level root of "blk-target".
    // root_item = None to match the NULL target_item in the edges above.
    let result =
        query_task_graph_bfs(&conn, "blk-target", None, GraphDirection::Reverse, u32::MAX, 1000)
            .unwrap();

    // Root + 2 sources = 3 nodes.
    assert_eq!(
        result.nodes.len(),
        3,
        "reverse BFS must find root + 2 sources; got {:?}",
        result.nodes
    );

    let node_items: Vec<Option<&str>> =
        result.nodes.iter().map(|(_, i)| i.as_deref()).collect();
    assert!(
        node_items.contains(&Some("src-1")),
        "src-1 must appear in reverse traversal"
    );
    assert!(
        node_items.contains(&Some("src-2")),
        "src-2 must appear in reverse traversal"
    );

    // Two edges: blk-target←src-1 and blk-target←src-2.
    assert_eq!(
        result.edges.len(),
        2,
        "reverse BFS must yield 2 edges; got {:?}",
        result.edges
    );
}
