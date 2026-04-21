//! Transaction atomicity tests for pattern-db.
//!
//! Verifies AC2.6: failed transactions roll back fully, and successful
//! transactions commit all mutations. Tests both the raw transaction mechanism
//! and the three transactional query functions in `queries/memory.rs`.

use chrono::Utc;
use pattern_db::{
    ConstellationDb,
    models::{Agent, AgentStatus, MemoryBlock, MemoryBlockType, MemoryPermission},
    queries,
};

// ============================================================================
// Helpers
// ============================================================================

fn open_test_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().unwrap()
}

fn insert_test_agent(conn: &rusqlite::Connection, id: &str) {
    let agent = Agent {
        id: id.to_string(),
        name: format!("Agent {id}"),
        description: None,
        model_provider: "test".to_string(),
        model_name: "test-model".to_string(),
        system_prompt: "Test prompt".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: AgentStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    queries::create_agent(conn, &agent).unwrap();
}

fn insert_test_block(conn: &rusqlite::Connection, id: &str, agent_id: &str, label: &str) {
    let block = MemoryBlock {
        id: id.to_string(),
        agent_id: agent_id.to_string(),
        label: label.to_string(),
        description: "Test block.".to_string(),
        block_type: MemoryBlockType::Working,
        char_limit: 1000,
        permission: MemoryPermission::ReadWrite,
        pinned: false,
        loro_snapshot: vec![],
        content_preview: None,
        metadata: None,
        embedding_model: None,
        is_active: true,
        frontier: None,
        last_seq: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    queries::create_block(conn, &block).unwrap();
}

// ============================================================================
// Raw transaction rollback — proves the mechanism works with our DB setup
// (shared-cache URIs, ATTACH, r2d2 pool)
// ============================================================================

/// Verifies that a successful mutation within a transaction is rolled back
/// when the transaction is dropped without commit.
#[test]
fn raw_transaction_implicit_rollback_on_drop() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    // Verify initial state.
    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.last_seq, 0);

    // Start a transaction, make a successful mutation, then drop without commit.
    {
        let tx = conn.transaction().unwrap();

        tx.execute(
            "UPDATE memory_blocks SET last_seq = 42 WHERE id = ?1",
            rusqlite::params!["block-1"],
        )
        .unwrap();

        // Mutation is visible within the transaction.
        let seq: i64 = tx
            .query_row(
                "SELECT last_seq FROM memory_blocks WHERE id = ?1",
                rusqlite::params!["block-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(seq, 42, "mutation should be visible within tx");

        // Drop without commit → implicit ROLLBACK.
    }

    // Mutation must be rolled back.
    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.last_seq, 0, "mutation must be rolled back on drop");
}

/// Verifies that a multi-statement transaction commits atomically —
/// all mutations become visible only after commit.
#[test]
fn raw_transaction_commit_makes_all_visible() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");
    insert_test_block(&conn, "block-2", "agent-1", "notes");

    {
        let tx = conn.transaction().unwrap();

        tx.execute(
            "UPDATE memory_blocks SET last_seq = 10 WHERE id = ?1",
            rusqlite::params!["block-1"],
        )
        .unwrap();
        tx.execute(
            "UPDATE memory_blocks SET last_seq = 20 WHERE id = ?1",
            rusqlite::params!["block-2"],
        )
        .unwrap();

        tx.commit().unwrap();
    }

    let b1 = queries::get_block(&conn, "block-1").unwrap().unwrap();
    let b2 = queries::get_block(&conn, "block-2").unwrap().unwrap();
    assert_eq!(b1.last_seq, 10);
    assert_eq!(b2.last_seq, 20);
}

/// Verifies that when a multi-statement transaction has a successful first
/// mutation but the second mutation fails, BOTH are rolled back.
#[test]
fn raw_transaction_partial_failure_rolls_back_all() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    let result: Result<(), rusqlite::Error> = (|| {
        let tx = conn.transaction()?;

        // Step 1: successfully mutate block-1.
        tx.execute(
            "UPDATE memory_blocks SET last_seq = 99 WHERE id = ?1",
            rusqlite::params!["block-1"],
        )?;

        // Step 2: violate NOT NULL constraint on agents.name to force failure.
        tx.execute(
            "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at) \
             VALUES ('dup', NULL, 'x', 'x', 'x', '{}', '[]', 'active', datetime('now'), datetime('now'))",
            [],
        )?;

        tx.commit()?;
        Ok(())
    })();

    assert!(result.is_err(), "transaction with NULL name should fail");

    // Step 1's mutation must be rolled back.
    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(
        block.last_seq, 0,
        "successful first mutation must be rolled back when second fails"
    );
}

// ============================================================================
// store_update: happy path + error path
// ============================================================================

/// Verifies that `store_update` atomically increments `last_seq` and inserts
/// the update row in a single transaction.
#[test]
fn store_update_happy_path_commits() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    let seq =
        queries::store_update(&mut conn, "block-1", &[1, 2, 3, 4], None, Some("test")).unwrap();
    assert_eq!(seq, 1, "first update should get seq=1");

    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.last_seq, 1);

    let stats = queries::get_pending_update_stats(&conn, "block-1").unwrap();
    assert_eq!(stats.count, 1);
    assert_eq!(stats.total_bytes, 4);
}

/// Verifies that `store_update` rolls back the `last_seq` increment when
/// the update INSERT fails due to a UNIQUE constraint violation.
///
/// This is the genuine rollback test: step 1 (UPDATE last_seq) succeeds,
/// step 2 (INSERT update row) fails, and step 1 must be reversed.
#[test]
fn store_update_rolls_back_seq_increment_on_insert_failure() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    // Pre-insert a row with seq=1 to collide with what store_update will try.
    // store_update does: UPDATE last_seq = last_seq + 1 (0 → 1), then
    // INSERT with seq=1. The UNIQUE index on (block_id, seq) causes the
    // INSERT to fail.
    conn.execute(
        "INSERT INTO memory_block_updates (block_id, seq, update_blob, byte_size, source, created_at)
         VALUES ('block-1', 1, X'FF', 1, 'pre-seeded', datetime('now'))",
        [],
    )
    .unwrap();

    // store_update should fail on the UNIQUE violation.
    let result = queries::store_update(&mut conn, "block-1", &[1, 2, 3], None, None);
    assert!(
        result.is_err(),
        "store_update should fail on UNIQUE violation"
    );

    // The critical check: last_seq must NOT have been incremented.
    // If the transaction rolled back properly, last_seq stays at 0.
    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(
        block.last_seq, 0,
        "last_seq must be rolled back when INSERT fails — transaction atomicity violated"
    );
}

/// Verifies that `store_update` on a nonexistent block fails and does not
/// affect other blocks.
#[test]
fn store_update_nonexistent_block_errors() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    let result = queries::store_update(&mut conn, "nonexistent", &[1, 2, 3], None, None);
    assert!(result.is_err());

    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.last_seq, 0, "unrelated block must be untouched");
}

// ============================================================================
// consolidate_checkpoint: happy path
// ============================================================================

/// Verifies that `consolidate_checkpoint` atomically creates a checkpoint,
/// deletes consolidated updates, and updates the block's snapshot.
#[test]
fn consolidate_checkpoint_happy_path_commits() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    queries::store_update(&mut conn, "block-1", &[10, 20, 30], None, None).unwrap();
    queries::store_update(&mut conn, "block-1", &[40, 50], None, None).unwrap();

    let pre_stats = queries::get_pending_update_stats(&conn, "block-1").unwrap();
    assert_eq!(pre_stats.count, 2);

    queries::consolidate_checkpoint(&mut conn, "block-1", &[99, 98, 97], None, 2).unwrap();

    let post_stats = queries::get_pending_update_stats(&conn, "block-1").unwrap();
    assert_eq!(post_stats.count, 0, "consolidated updates must be deleted");

    let checkpoint = queries::get_latest_checkpoint(&conn, "block-1")
        .unwrap()
        .expect("checkpoint must exist");
    assert_eq!(checkpoint.updates_consolidated, 2);

    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.loro_snapshot, &[99, 98, 97]);
}

// ============================================================================
// update_block_config: happy path + error path
// ============================================================================

/// Verifies that `update_block_config` commits all field changes atomically.
#[test]
fn update_block_config_happy_path_commits() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    queries::update_block_config(
        &mut conn,
        "block-1",
        Some(MemoryPermission::ReadOnly),
        Some(MemoryBlockType::Core),
        Some("Updated description."),
        Some(true),
        Some(8192),
    )
    .unwrap();

    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.permission, MemoryPermission::ReadOnly);
    assert_eq!(block.block_type, MemoryBlockType::Core);
    assert_eq!(block.description, "Updated description.");
    assert!(block.pinned);
    assert_eq!(block.char_limit, 8192);
}

/// Verifies that `update_block_config` on a nonexistent block fails.
#[test]
fn update_block_config_nonexistent_block_errors() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    let result = queries::update_block_config(
        &mut conn,
        "nonexistent",
        Some(MemoryPermission::ReadOnly),
        None,
        None,
        None,
        None,
    );
    assert!(result.is_err());

    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(
        block.permission,
        MemoryPermission::ReadWrite,
        "unrelated block must be untouched"
    );
}

// ============================================================================
// Sequential seq consistency
// ============================================================================

/// Verifies that multiple `store_update` calls produce monotonically increasing
/// sequence numbers.
#[test]
fn store_update_sequential_seq_numbers() {
    let db = open_test_db();
    let mut conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-1");
    insert_test_block(&conn, "block-1", "agent-1", "scratch");

    let seq1 = queries::store_update(&mut conn, "block-1", &[1], None, None).unwrap();
    let seq2 = queries::store_update(&mut conn, "block-1", &[2], None, None).unwrap();
    let seq3 = queries::store_update(&mut conn, "block-1", &[3], None, None).unwrap();

    assert_eq!(seq1, 1);
    assert_eq!(seq2, 2);
    assert_eq!(seq3, 3);

    let block = queries::get_block(&conn, "block-1").unwrap().unwrap();
    assert_eq!(block.last_seq, 3);
}
