// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Full-text search functionality using FTS5.
//!
//! This module provides full-text search over messages, memory blocks, and
//! archival entries. FTS5 is built into SQLite, no extension loading required.
//!
//! # External content tables
//!
//! The FTS tables are configured as "external content" tables, meaning they
//! index data from the main tables but don't store a copy of the content.
//! Triggers keep the FTS indexes in sync with the source tables.
//!
//! # FTS5 query syntax
//!
//! - Basic search: `word1 word2` (matches documents containing both)
//! - Phrase search: `"exact phrase"`
//! - OR search: `word1 OR word2`
//! - NOT search: `word1 NOT word2`
//! - Prefix search: `prefix*`
//!
//! See: <https://www.sqlite.org/fts5.html>

use rusqlite::Connection;

use crate::error::{DbError, DbResult};

/// Result of a full-text search.
#[derive(Debug, Clone)]
pub struct FtsSearchResult {
    /// Rowid of the matching record in the source table.
    pub rowid: i64,
    /// Relevance rank (lower is better, typically negative).
    pub rank: f64,
    /// Optional highlighted snippet.
    pub snippet: Option<String>,
}

/// FTS match with the original content ID.
#[derive(Debug, Clone)]
pub struct FtsMatch {
    /// The content ID from the source table.
    pub id: String,
    /// The matched content.
    pub content: String,
    /// Relevance rank (lower is better).
    pub rank: f64,
}

/// Search messages using full-text search.
///
/// Returns messages matching the FTS5 query, ordered by relevance.
/// Messages always live in the `msg` schema (ATTACHed database).
pub fn search_messages(
    conn: &Connection,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    // Use unqualified table names: SQLite's schema search order finds
    // messages_fts and messages in the attached `msg` schema automatically.
    let results = if let Some(agent_id) = agent_id {
        let mut stmt = conn.prepare(
            r#"
            SELECT m.id, m.content_preview, bm25(messages_fts) as rank
            FROM messages_fts
            JOIN messages m ON messages_fts.rowid = m.rowid
            WHERE messages_fts MATCH ?1
              AND m.agent_id = ?2
            ORDER BY rank
            LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![query, agent_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            r#"
            SELECT m.id, m.content_preview, bm25(messages_fts) as rank
            FROM messages_fts
            JOIN messages m ON messages_fts.rowid = m.rowid
            WHERE messages_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(results
        .into_iter()
        .map(|(id, content, rank)| FtsMatch {
            id,
            content: content.unwrap_or_default(),
            rank,
        })
        .collect())
}

/// Search memory blocks using full-text search.
pub fn search_memory_blocks(
    conn: &Connection,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    let results = if let Some(agent_id) = agent_id {
        let mut stmt = conn.prepare(
            r#"
            SELECT mb.id, mb.content_preview, bm25(memory_blocks_fts) as rank
            FROM memory_blocks_fts
            JOIN memory_blocks mb ON memory_blocks_fts.rowid = mb.rowid
            WHERE memory_blocks_fts MATCH ?1
              AND mb.agent_id = ?2
            ORDER BY rank
            LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![query, agent_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            r#"
            SELECT mb.id, mb.content_preview, bm25(memory_blocks_fts) as rank
            FROM memory_blocks_fts
            JOIN memory_blocks mb ON memory_blocks_fts.rowid = mb.rowid
            WHERE memory_blocks_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(results
        .into_iter()
        .map(|(id, content, rank)| FtsMatch {
            id,
            content: content.unwrap_or_default(),
            rank,
        })
        .collect())
}

/// Search archival entries using full-text search.
pub fn search_archival(
    conn: &Connection,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    let results = if let Some(agent_id) = agent_id {
        let mut stmt = conn.prepare(
            r#"
            SELECT ae.id, ae.content, bm25(archival_fts) as rank
            FROM archival_fts
            JOIN archival_entries ae ON archival_fts.rowid = ae.rowid
            WHERE archival_fts MATCH ?1
              AND ae.agent_id = ?2
            ORDER BY rank
            LIMIT ?3
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![query, agent_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        let mut stmt = conn.prepare(
            r#"
            SELECT ae.id, ae.content, bm25(archival_fts) as rank
            FROM archival_fts
            JOIN archival_entries ae ON archival_fts.rowid = ae.rowid
            WHERE archival_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(results
        .into_iter()
        .map(|(id, content, rank)| FtsMatch { id, content, rank })
        .collect())
}

/// Content types for FTS search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsContentType {
    Message,
    MemoryBlock,
    ArchivalEntry,
}

impl FtsContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FtsContentType::Message => "message",
            FtsContentType::MemoryBlock => "memory_block",
            FtsContentType::ArchivalEntry => "archival_entry",
        }
    }
}

/// Rebuild the FTS index for messages.
pub fn rebuild_messages_fts(conn: &Connection) -> DbResult<()> {
    // Use unqualified name: SQLite searches temp -> main -> attached schemas.
    conn.execute(
        "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
        [],
    )?;
    Ok(())
}

/// Rebuild the FTS index for memory blocks.
pub fn rebuild_memory_blocks_fts(conn: &Connection) -> DbResult<()> {
    conn.execute(
        "INSERT INTO memory_blocks_fts(memory_blocks_fts) VALUES('rebuild')",
        [],
    )?;
    Ok(())
}

/// Rebuild the FTS index for archival entries.
pub fn rebuild_archival_fts(conn: &Connection) -> DbResult<()> {
    conn.execute(
        "INSERT INTO archival_fts(archival_fts) VALUES('rebuild')",
        [],
    )?;
    Ok(())
}

/// Get FTS index statistics.
#[derive(Debug, Clone, Default)]
pub struct FtsStats {
    pub messages_indexed: u64,
    pub memory_blocks_indexed: u64,
    pub archival_entries_indexed: u64,
}

/// Get statistics about FTS indexes.
pub fn get_fts_stats(conn: &Connection) -> DbResult<FtsStats> {
    // Use unqualified name: SQLite searches temp -> main -> attached schemas.
    let messages: i64 = conn.query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))?;

    let memory_blocks: i64 =
        conn.query_row("SELECT COUNT(*) FROM memory_blocks_fts", [], |r| r.get(0))?;

    let archival: i64 = conn.query_row("SELECT COUNT(*) FROM archival_fts", [], |r| r.get(0))?;

    Ok(FtsStats {
        messages_indexed: messages as u64,
        memory_blocks_indexed: memory_blocks as u64,
        archival_entries_indexed: archival as u64,
    })
}

/// Validate FTS query syntax.
///
/// Returns an error if the query contains invalid FTS5 syntax.
pub fn validate_fts_query(query: &str) -> DbResult<()> {
    if query.trim().is_empty() {
        return Err(DbError::invalid_data("FTS query cannot be empty"));
    }

    let quote_count = query.chars().filter(|c| *c == '"').count();
    if quote_count % 2 != 0 {
        return Err(DbError::invalid_data("Unbalanced quotes in FTS query"));
    }

    let open_parens = query.chars().filter(|c| *c == '(').count();
    let close_parens = query.chars().filter(|c| *c == ')').count();
    if open_parens != close_parens {
        return Err(DbError::invalid_data("Unbalanced parentheses in FTS query"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConstellationDb;

    /// Helper to create a test agent for foreign key constraints.
    fn create_test_agent(conn: &Connection, id: &str) {
        conn.execute(
            r#"
            INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
            VALUES (?1, ?2, 'anthropic', 'claude-3', 'test prompt', '{}', '[]', 'active', datetime('now'), datetime('now'))
            "#,
            rusqlite::params![id, format!("{id}_name")],
        )
        .unwrap();
    }

    #[test]
    fn test_validate_fts_query() {
        assert!(validate_fts_query("hello world").is_ok());
        assert!(validate_fts_query("\"exact phrase\"").is_ok());
        assert!(validate_fts_query("hello OR world").is_ok());
        assert!(validate_fts_query("prefix*").is_ok());
        assert!(validate_fts_query("(hello OR world) AND foo").is_ok());

        assert!(validate_fts_query("").is_err());
        assert!(validate_fts_query("   ").is_err());
        assert!(validate_fts_query("\"unbalanced").is_err());
        assert!(validate_fts_query("(unbalanced").is_err());
    }

    #[test]
    fn test_fts_content_type() {
        assert_eq!(FtsContentType::Message.as_str(), "message");
        assert_eq!(FtsContentType::MemoryBlock.as_str(), "memory_block");
        assert_eq!(FtsContentType::ArchivalEntry.as_str(), "archival_entry");
    }

    #[test]
    fn test_fts_tables_exist() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        let stats = get_fts_stats(&conn).unwrap();
        assert_eq!(stats.messages_indexed, 0);
        assert_eq!(stats.memory_blocks_indexed, 0);
        assert_eq!(stats.archival_entries_indexed, 0);
    }

    #[test]
    fn test_fts_message_search() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent_1");

        conn.execute(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content_json, content_preview, is_archived, created_at)
            VALUES ('msg_1', 'agent_1', '1', 'user', '{}', 'hello world this is a test message', 0, datetime('now'))
            "#,
            [],
        )
        .unwrap();

        conn.execute(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content_json, content_preview, is_archived, created_at)
            VALUES ('msg_2', 'agent_1', '2', 'assistant', '{}', 'goodbye cruel world', 0, datetime('now'))
            "#,
            [],
        )
        .unwrap();

        let results = search_messages(&conn, "hello", None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "msg_1");
        assert!(results[0].content.contains("hello"));

        let results = search_messages(&conn, "world", None, 10).unwrap();
        assert_eq!(results.len(), 2);

        let results = search_messages(&conn, "world", Some("agent_1"), 10).unwrap();
        assert_eq!(results.len(), 2);

        let results = search_messages(&conn, "world", Some("agent_other"), 10).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_fts_rebuild() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent_1");

        conn.execute(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content_json, content_preview, is_archived, created_at)
            VALUES ('msg_rebuild', 'agent_1', '1', 'user', '{}', 'rebuild test message', 0, datetime('now'))
            "#,
            [],
        )
        .unwrap();

        rebuild_messages_fts(&conn).unwrap();

        let results = search_messages(&conn, "rebuild", None, 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_fts_phrase_search() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent_1");

        conn.execute(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content_json, content_preview, is_archived, created_at)
            VALUES ('msg_phrase', 'agent_1', '1', 'user', '{}', 'the quick brown fox jumps over the lazy dog', 0, datetime('now'))
            "#,
            [],
        )
        .unwrap();

        let results = search_messages(&conn, "\"quick brown fox\"", None, 10).unwrap();
        assert_eq!(results.len(), 1);

        let results = search_messages(&conn, "\"brown quick fox\"", None, 10).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_fts_prefix_search() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent_1");

        conn.execute(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content_json, content_preview, is_archived, created_at)
            VALUES ('msg_prefix', 'agent_1', '1', 'user', '{}', 'programming is fun', 0, datetime('now'))
            "#,
            [],
        )
        .unwrap();

        let results = search_messages(&conn, "prog*", None, 10).unwrap();
        assert_eq!(results.len(), 1);

        let results = search_messages(&conn, "program*", None, 10).unwrap();
        assert_eq!(results.len(), 1);

        let results = search_messages(&conn, "xyz*", None, 10).unwrap();
        assert_eq!(results.len(), 0);
    }
}
