//! FTS5 BM25 scoring regression tests with insta snapshots.
//!
//! These tests seed a canonical corpus and snapshot the BM25 scores
//! so that any change to FTS behavior is immediately visible.

use pattern_db::ConstellationDb;
use pattern_db::fts::{search_memory_blocks, search_messages};

/// Seed a canonical corpus of messages and memory blocks.
fn insert_canonical_corpus(conn: &rusqlite::Connection) {
    // Create a test agent.
    conn.execute(
        r#"
        INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
        VALUES ('agent_fts', 'fts_agent', 'test', 'test', 'test', '{}', '[]', 'active', datetime('now'), datetime('now'))
        "#,
        [],
    )
    .unwrap();

    // Messages.
    let messages = [
        ("msg_01", "memory blocks are fundamental to agent cognition"),
        ("msg_02", "the weather today is sunny with a chance of rain"),
        ("msg_03", "ADHD executive function support through structured routines"),
        ("msg_04", "memory consolidation happens during sleep cycles"),
        ("msg_05", "blocks of code should be well documented"),
        ("msg_06", "the agent's working memory holds current context"),
        ("msg_07", "archival memory stores long-term knowledge"),
        ("msg_08", "full text search uses BM25 ranking algorithm"),
        ("msg_09", "sqlite FTS5 provides efficient text indexing"),
        ("msg_10", "pattern matching in functional programming"),
    ];

    for (id, preview) in &messages {
        conn.execute(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content_json, content_preview, is_archived, created_at)
            VALUES (?1, 'agent_fts', ?1, 'user', '{}', ?2, 0, datetime('now'))
            "#,
            rusqlite::params![id, preview],
        )
        .unwrap();
    }

    // Memory blocks with Loro snapshot placeholder.
    let blocks = [
        ("blk_01", "persona", "agent personality and identity"),
        ("blk_02", "scratchpad", "working notes and current task tracking"),
        ("blk_03", "human", "information about the human partner"),
        ("blk_04", "system", "system configuration and guidelines"),
        ("blk_05", "project_notes", "project-specific memory blocks and context"),
    ];

    for (id, label, preview) in &blocks {
        conn.execute(
            r#"
            INSERT INTO memory_blocks (id, agent_id, label, description, block_type, char_limit, permission, pinned, loro_snapshot, content_preview, is_active, created_at, updated_at)
            VALUES (?1, 'agent_fts', ?2, ?3, 'core', 5000, 'read_write', 0, X'00', ?3, 1, datetime('now'), datetime('now'))
            "#,
            rusqlite::params![id, label, preview],
        )
        .unwrap();
    }
}

#[test]
fn bm25_message_scoring_snapshot() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();
    insert_canonical_corpus(&conn);

    let results = search_messages(&conn, "memory blocks", None, 10).unwrap();
    let snapshot: Vec<(String, f64)> = results
        .iter()
        .map(|r| (r.id.clone(), (r.rank * 1000.0).round() / 1000.0))
        .collect();

    insta::assert_yaml_snapshot!("bm25_message_memory_blocks", snapshot);
}

#[test]
fn bm25_memory_block_scoring_snapshot() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();
    insert_canonical_corpus(&conn);

    let results = search_memory_blocks(&conn, "memory", None, 10).unwrap();
    let snapshot: Vec<(String, f64)> = results
        .iter()
        .map(|r| (r.id.clone(), (r.rank * 1000.0).round() / 1000.0))
        .collect();

    insta::assert_yaml_snapshot!("bm25_memory_block_search", snapshot);
}

#[test]
fn bm25_agent_filter_snapshot() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();
    insert_canonical_corpus(&conn);

    // With agent filter.
    let results = search_messages(&conn, "memory", Some("agent_fts"), 10).unwrap();
    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();

    insta::assert_yaml_snapshot!("bm25_agent_filter_ids", ids);

    // With non-existent agent -- should return empty.
    let results = search_messages(&conn, "memory", Some("no_such_agent"), 10).unwrap();
    assert!(results.is_empty());
}
