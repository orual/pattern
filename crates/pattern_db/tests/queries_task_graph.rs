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

use pattern_core::types::memory_types::{
    TaskEdgeRef,
    task_query::{Direction, GraphQuery},
};
use pattern_db::ConstellationDb;
use pattern_db::queries::{query_task_graph_bfs, upsert_task_edges};
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open an in-memory DB with all migrations applied.
fn fresh_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().unwrap()
}

/// Build a [`TaskEdgeRef`] for a node in "blk-main".
fn node(item: &str) -> TaskEdgeRef {
    TaskEdgeRef {
        block: SmolStr::new("blk-main"),
        task_item: Some(SmolStr::new(item)),
    }
}

/// Build a block-level [`TaskEdgeRef`] (no item_id).
fn block_ref(block: &str) -> TaskEdgeRef {
    TaskEdgeRef {
        block: SmolStr::new(block),
        task_item: None,
    }
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
    edges.push((target_block.to_string(), target_item.map(|s| s.to_string())));
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

    let root = node("n-0");
    let query = GraphQuery {
        direction: Direction::Forward,
        depth: Some(0),
        max_nodes: Some(1000),
    };
    let result = query_task_graph_bfs(&conn, &root, &query).unwrap();

    assert_eq!(
        result.nodes.len(),
        1,
        "depth=0 must return only the root node"
    );
    assert_eq!(result.nodes[0], node("n-0"));
    assert!(
        result.edges.is_empty(),
        "depth=0 must return zero edges; got {:?}",
        result.edges
    );
    assert!(
        !result.truncated,
        "depth=0 on a small graph must not truncate"
    );
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
    let query = GraphQuery {
        direction: Direction::Forward,
        depth: Some(u32::MAX),
        max_nodes: Some(1000),
    };
    let result = query_task_graph_bfs(&conn, &node("n-0"), &query).unwrap();

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
    assert_eq!(result.nodes[0], node("n-0"), "first node must be the root");
}

// ---------------------------------------------------------------------------
// Test 3: Same 5-node chain, depth=2 → 3 nodes + 2 edges
// ---------------------------------------------------------------------------

#[test]
fn five_node_chain_forward_depth_two() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    build_chain(&mut conn, 5);

    let query = GraphQuery {
        direction: Direction::Forward,
        depth: Some(2),
        max_nodes: Some(1000),
    };
    let result = query_task_graph_bfs(&conn, &node("n-0"), &query).unwrap();

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

    let node_items: Vec<Option<&str>> = result
        .nodes
        .iter()
        .map(|r| r.task_item.as_deref())
        .collect();
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

    let query = GraphQuery {
        direction: Direction::Forward,
        depth: Some(10),
        max_nodes: Some(1000),
    };
    let result = query_task_graph_bfs(&conn, &node("n-0"), &query).unwrap();

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
            stmt.execute(rusqlite::params![format!("n-{i}"), format!("n-{}", i + 1),])
                .unwrap();
        }
        drop(stmt);
        tx.commit().unwrap();
    }

    let query = GraphQuery {
        direction: Direction::Forward,
        depth: Some(u32::MAX),
        max_nodes: Some(1000),
    };
    let start = Instant::now();
    let result = query_task_graph_bfs(&conn, &node("n-0"), &query).unwrap();
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
    let root = block_ref("blk-target");
    let query = GraphQuery {
        direction: Direction::Reverse,
        depth: Some(u32::MAX),
        max_nodes: Some(1000),
    };
    let result = query_task_graph_bfs(&conn, &root, &query).unwrap();

    // Root + 2 sources = 3 nodes.
    assert_eq!(
        result.nodes.len(),
        3,
        "reverse BFS must find root + 2 sources; got {:?}",
        result.nodes
    );

    let node_items: Vec<Option<&str>> = result
        .nodes
        .iter()
        .map(|r| r.task_item.as_deref())
        .collect();
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

// ---------------------------------------------------------------------------
// Test 7: Direction::Both — simple bidirectional graph
// ---------------------------------------------------------------------------

/// `Direction::Both` follows edges in both directions from the root.
///
/// Deduplication: each *node* is visited at most once, but the *edge* between
/// two already-visited nodes is still recorded. This means edge count can
/// exceed node count minus 1 when cycles or bidirectional edges are present.
///
/// Test graph: A→B (forward) and A←C (reverse), root = A, depth = 1.
/// Both: discovers B (forward) and C (reverse) from A.
/// Expected: 3 nodes, 2 edges.
#[test]
fn both_direction_discovers_forward_and_reverse_neighbours() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // Set up: B is a forward neighbour of A (A→B).
    //         C has a forward edge to A (C→A), so A←C in reverse direction.
    {
        let tx = conn.transaction().unwrap();
        upsert_task_edges(
            &tx,
            "blk-main",
            "n-a",
            &[("blk-main".to_string(), Some("n-b".to_string()))],
        )
        .unwrap();
        upsert_task_edges(
            &tx,
            "blk-main",
            "n-c",
            &[("blk-main".to_string(), Some("n-a".to_string()))],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Both direction from n-a at depth=1.
    let query = GraphQuery {
        direction: Direction::Both,
        depth: Some(1),
        max_nodes: Some(1000),
    };
    let result = query_task_graph_bfs(&conn, &node("n-a"), &query).unwrap();

    // Root n-a + forward n-b + reverse n-c = 3 nodes.
    assert_eq!(
        result.nodes.len(),
        3,
        "Both at depth=1 must discover root + forward + reverse neighbour; got {:?}",
        result.nodes
    );

    let node_items: Vec<Option<&str>> = result
        .nodes
        .iter()
        .map(|r| r.task_item.as_deref())
        .collect();
    assert!(
        node_items.contains(&Some("n-a")),
        "root n-a must be present"
    );
    assert!(
        node_items.contains(&Some("n-b")),
        "forward neighbour n-b must be present"
    );
    assert!(
        node_items.contains(&Some("n-c")),
        "reverse neighbour n-c must be present"
    );

    // 2 edges: A→B (forward) and C→A recorded as (C, A) in the reverse direction.
    assert_eq!(
        result.edges.len(),
        2,
        "Both at depth=1 must record 2 edges; got {:?}",
        result.edges
    );
    assert!(!result.truncated);
}

/// `Direction::Both` produces different results from Forward or Reverse alone.
///
/// Graph: A→B, C→A. Root = A, depth = 1.
/// - Forward only: discovers B (1 additional node, 1 edge).
/// - Reverse only: discovers C (1 additional node, 1 edge).
/// - Both: discovers B and C (2 additional nodes, 2 edges).
#[test]
fn both_direction_differs_from_forward_and_reverse_alone() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    {
        let tx = conn.transaction().unwrap();
        upsert_task_edges(
            &tx,
            "blk-main",
            "n-a",
            &[("blk-main".to_string(), Some("n-b".to_string()))],
        )
        .unwrap();
        upsert_task_edges(
            &tx,
            "blk-main",
            "n-c",
            &[("blk-main".to_string(), Some("n-a".to_string()))],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Forward from n-a: only n-b reachable.
    let fwd = query_task_graph_bfs(
        &conn,
        &node("n-a"),
        &GraphQuery {
            direction: Direction::Forward,
            depth: Some(1),
            max_nodes: Some(1000),
        },
    )
    .unwrap();
    assert_eq!(
        fwd.nodes.len(),
        2,
        "Forward must reach 2 nodes (root + n-b)"
    );

    // Reverse from n-a: only n-c reachable.
    let rev = query_task_graph_bfs(
        &conn,
        &node("n-a"),
        &GraphQuery {
            direction: Direction::Reverse,
            depth: Some(1),
            max_nodes: Some(1000),
        },
    )
    .unwrap();
    assert_eq!(
        rev.nodes.len(),
        2,
        "Reverse must reach 2 nodes (root + n-c)"
    );

    // Both from n-a: reaches n-b and n-c.
    let both = query_task_graph_bfs(
        &conn,
        &node("n-a"),
        &GraphQuery {
            direction: Direction::Both,
            depth: Some(1),
            max_nodes: Some(1000),
        },
    )
    .unwrap();
    assert_eq!(
        both.nodes.len(),
        3,
        "Both must reach 3 nodes (root + n-b + n-c); got {:?}",
        both.nodes
    );

    // Confirm the union superset relationship.
    assert!(
        both.nodes.len() > fwd.nodes.len(),
        "Both must discover strictly more nodes than Forward alone"
    );
    assert!(
        both.nodes.len() > rev.nodes.len(),
        "Both must discover strictly more nodes than Reverse alone"
    );
}

// ---------------------------------------------------------------------------
// Test 8: GraphQuery::default() caps (depth=16, max_nodes=1000) are applied.
// ---------------------------------------------------------------------------

/// When `GraphQuery::default()` is passed, the function applies the built-in
/// caps of depth=16 and max_nodes=1000 rather than panicking or doing
/// unbounded traversal.
///
/// A 20-node chain with default caps: depth=16 means nodes at depth ≤16
/// are visited. Root is depth 0; node-16 is at depth 16 (included);
/// node-17 is at depth 17 (excluded). So 17 nodes are returned.
#[test]
fn default_query_applies_built_in_caps() {
    let db = fresh_db();
    let mut conn = db.get().unwrap();

    // Build a 20-node chain.
    build_chain(&mut conn, 20);

    let result = query_task_graph_bfs(&conn, &node("n-0"), &GraphQuery::default()).unwrap();

    // depth=16: nodes at depths 0..=16 → 17 nodes.
    assert_eq!(
        result.nodes.len(),
        17,
        "default depth=16 must return 17 nodes (depths 0..=16); got {:?}",
        result
            .nodes
            .iter()
            .map(|r| r.task_item.as_deref())
            .collect::<Vec<_>>()
    );
    assert!(
        !result.truncated,
        "20-node chain must not hit max_nodes=1000 cap"
    );
}
