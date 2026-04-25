//! Cross-schema JOIN tests for pattern-db.
//!
//! Verifies that pooled connections correctly expose both the `main` (memory.db)
//! schema and the `msg` (messages.db) schema via ATTACH, and that cross-schema
//! JOINs produce correct results.

use chrono::Utc;
use jiff::Timestamp;
use pattern_db::{
    ConstellationDb,
    models::{
        Agent, AgentStatus, MemoryBlock, MemoryBlockType, MemoryPermission, Message, MessageRole,
    },
    queries,
};

// ============================================================================
// Helpers
// ============================================================================

fn open_test_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().unwrap()
}

fn insert_test_agent(conn: &rusqlite::Connection, id: &str, name: &str) -> Agent {
    let agent = Agent {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        model_provider: "test".to_string(),
        model_name: "test-model".to_string(),
        system_prompt: "Test prompt.".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: AgentStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    queries::create_agent(conn, &agent).unwrap();
    agent
}

fn insert_test_block(
    conn: &rusqlite::Connection,
    id: &str,
    agent_id: &str,
    label: &str,
) -> MemoryBlock {
    let block = MemoryBlock {
        id: id.to_string(),
        agent_id: agent_id.to_string(),
        label: label.to_string(),
        description: "Cross-db test block.".to_string(),
        block_type: MemoryBlockType::Core,
        char_limit: 5000,
        permission: MemoryPermission::ReadWrite,
        pinned: false,
        loro_snapshot: vec![],
        content_preview: Some("block content preview".to_string()),
        metadata: None,
        embedding_model: None,
        is_active: true,
        frontier: None,
        last_seq: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    queries::create_block(conn, &block).unwrap();
    block
}

fn insert_test_message(
    conn: &rusqlite::Connection,
    id: &str,
    agent_id: &str,
    content: &str,
) -> Message {
    let msg = Message {
        id: id.to_string(),
        agent_id: agent_id.to_string(),
        // Use a simple sortable string as position for testing.
        position: format!("{:020}", id.len()),
        batch_id: None,
        sequence_in_batch: None,
        role: MessageRole::User,
        content_json: pattern_db::Json(serde_json::json!({ "text": content })),
        content_preview: Some(content.to_string()),
        batch_type: None,
        source: Some("test".to_string()),
        source_metadata: None,
        attachments_json: None,
        origin_json: None,
        is_archived: false,
        is_deleted: false,
        created_at: Timestamp::now(),
    };
    queries::create_message(conn, &msg).unwrap();
    msg
}

// ============================================================================
// AC2.10: cross-schema JOIN returns correct rows
// ============================================================================

/// Inserts a memory_block in the main schema and a message in the `msg` schema,
/// both owned by the same agent.  Runs a cross-schema JOIN to verify that both
/// schemas are simultaneously accessible on a pooled connection.
#[test]
fn cross_schema_join_returns_matching_rows() {
    let db = open_test_db();
    let conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-a", "Agent Alpha");
    let block = insert_test_block(&conn, "block-a", "agent-a", "persona");
    let msg = insert_test_message(&conn, "msg-001", "agent-a", "hello world");

    // Cross-schema JOIN: memory_blocks (main) ⋈ messages (msg schema).
    let result: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT mb.id, mb.label, m.id
             FROM memory_blocks mb
             JOIN msg.messages m ON mb.agent_id = m.agent_id
             WHERE mb.agent_id = ?1",
        )
        .unwrap()
        .query_map(rusqlite::params!["agent-a"], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(result.len(), 1, "join should produce exactly one row");
    let (block_id, label, message_id) = &result[0];
    assert_eq!(block_id, &block.id);
    assert_eq!(label, &block.label);
    assert_eq!(message_id, &msg.id);
}

/// Verifies that the cross-schema join correctly excludes agents that do not
/// have matching rows in both schemas.
#[test]
fn cross_schema_join_excludes_non_matching_agents() {
    let db = open_test_db();
    let conn = db.get().unwrap();

    // Agent A has both a block and a message.
    insert_test_agent(&conn, "agent-a", "Agent Alpha");
    insert_test_block(&conn, "block-a", "agent-a", "persona");
    insert_test_message(&conn, "msg-001", "agent-a", "hello");

    // Agent B has only a block (no message).
    insert_test_agent(&conn, "agent-b", "Agent Beta");
    insert_test_block(&conn, "block-b", "agent-b", "notes");

    // INNER JOIN should only return agent-a's row.
    let result: Vec<String> = conn
        .prepare(
            "SELECT mb.agent_id
             FROM memory_blocks mb
             JOIN msg.messages m ON mb.agent_id = m.agent_id",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0], "agent-a");
}

/// Verifies that multiple messages per agent produce the expected number of
/// joined rows (one per block-message combination).
#[test]
fn cross_schema_join_multiple_messages() {
    let db = open_test_db();
    let conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-a", "Agent Alpha");
    insert_test_block(&conn, "block-a", "agent-a", "persona");
    insert_test_message(&conn, "msg-001", "agent-a", "first message");
    insert_test_message(&conn, "msg-002", "agent-a", "second message");
    insert_test_message(&conn, "msg-003", "agent-a", "third message");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*)
             FROM memory_blocks mb
             JOIN msg.messages m ON mb.agent_id = m.agent_id
             WHERE mb.agent_id = ?1",
            rusqlite::params!["agent-a"],
            |r| r.get(0),
        )
        .unwrap();

    // 1 block × 3 messages = 3 joined rows.
    assert_eq!(count, 3);
}

/// Verifies that the `msg` schema is accessible via unqualified name resolution
/// (SQLite searches temp → main → attached schemas in that order).
/// Messages in the attached database should resolve without the `msg.` prefix
/// because the connection was initialised with ATTACH.
#[test]
fn unqualified_messages_resolves_to_msg_schema() {
    let db = open_test_db();
    let conn = db.get().unwrap();

    insert_test_agent(&conn, "agent-a", "Agent Alpha");
    insert_test_message(&conn, "msg-001", "agent-a", "hello");

    // Unqualified `messages` table — resolves to msg.messages via schema search.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE agent_id = ?1",
            rusqlite::params!["agent-a"],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(count, 1);
}
