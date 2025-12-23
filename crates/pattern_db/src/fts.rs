//! Full-text search functionality using FTS5.
//!
//! This module provides full-text search over messages, memory blocks, and
//! archival entries. FTS5 is built into SQLite, no extension loading required.
//!
//! Unlike sqlite-vec, FTS5 uses standard SQL syntax that sqlx understands,
//! so we can use compile-time checked queries here.
//!
//! # External Content Tables
//!
//! The FTS tables are configured as "external content" tables, meaning they
//! index data from the main tables but don't store a copy of the content.
//! Triggers keep the FTS indexes in sync with the source tables.
//!
//! # FTS5 Query Syntax
//!
//! - Basic search: `word1 word2` (matches documents containing both)
//! - Phrase search: `"exact phrase"`
//! - OR search: `word1 OR word2`
//! - NOT search: `word1 NOT word2`
//! - Prefix search: `prefix*`
//! - Column filter: `column:word` (not used since our tables are single-column)
//!
//! See: https://www.sqlite.org/fts5.html

use sqlx::SqlitePool;

use crate::error::{DbError, DbResult};

/// Result of a full-text search.
#[derive(Debug, Clone)]
pub struct FtsSearchResult {
    /// Rowid of the matching record in the source table
    pub rowid: i64,
    /// Relevance rank (lower is better, typically negative)
    pub rank: f64,
    /// Optional highlighted snippet
    pub snippet: Option<String>,
}

/// FTS match with the original content ID.
#[derive(Debug, Clone)]
pub struct FtsMatch {
    /// The content ID from the source table
    pub id: String,
    /// The matched content
    pub content: String,
    /// Relevance rank (lower is better)
    pub rank: f64,
}

/// Search messages using full-text search.
///
/// Returns messages matching the FTS5 query, ordered by relevance.
/// The query uses FTS5 syntax (see module docs).
pub async fn search_messages(
    pool: &SqlitePool,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    // Note: We use runtime query here because we need to join with the source
    // table to get the full content and filter by agent_id.
    //
    // FTS5's MATCH is supported by sqlx since PR #396 (June 2020), but the
    // bm25() ranking function and complex joins are easier with runtime queries.
    let results = if let Some(agent_id) = agent_id {
        sqlx::query_as::<_, (String, Option<String>, f64)>(
            r#"
            SELECT m.id, m.content, bm25(messages_fts) as rank
            FROM messages_fts
            JOIN messages m ON messages_fts.rowid = m.rowid
            WHERE messages_fts MATCH ?
              AND m.agent_id = ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(agent_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, (String, Option<String>, f64)>(
            r#"
            SELECT m.id, m.content, bm25(messages_fts) as rank
            FROM messages_fts
            JOIN messages m ON messages_fts.rowid = m.rowid
            WHERE messages_fts MATCH ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(limit)
        .fetch_all(pool)
        .await?
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
///
/// Searches the content_preview field of memory blocks.
pub async fn search_memory_blocks(
    pool: &SqlitePool,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    let results = if let Some(agent_id) = agent_id {
        sqlx::query_as::<_, (String, Option<String>, f64)>(
            r#"
            SELECT mb.id, mb.content_preview, bm25(memory_blocks_fts) as rank
            FROM memory_blocks_fts
            JOIN memory_blocks mb ON memory_blocks_fts.rowid = mb.rowid
            WHERE memory_blocks_fts MATCH ?
              AND mb.agent_id = ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(agent_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, (String, Option<String>, f64)>(
            r#"
            SELECT mb.id, mb.content_preview, bm25(memory_blocks_fts) as rank
            FROM memory_blocks_fts
            JOIN memory_blocks mb ON memory_blocks_fts.rowid = mb.rowid
            WHERE memory_blocks_fts MATCH ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(limit)
        .fetch_all(pool)
        .await?
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
pub async fn search_archival(
    pool: &SqlitePool,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    let results = if let Some(agent_id) = agent_id {
        sqlx::query_as::<_, (String, String, f64)>(
            r#"
            SELECT ae.id, ae.content, bm25(archival_fts) as rank
            FROM archival_fts
            JOIN archival_entries ae ON archival_fts.rowid = ae.rowid
            WHERE archival_fts MATCH ?
              AND ae.agent_id = ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(agent_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, (String, String, f64)>(
            r#"
            SELECT ae.id, ae.content, bm25(archival_fts) as rank
            FROM archival_fts
            JOIN archival_entries ae ON archival_fts.rowid = ae.rowid
            WHERE archival_fts MATCH ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };

    Ok(results
        .into_iter()
        .map(|(id, content, rank)| FtsMatch { id, content, rank })
        .collect())
}

/// Search across all content types.
///
/// Performs separate searches on messages, memory blocks, and archival entries,
/// then merges results by rank.
pub async fn search_all(
    pool: &SqlitePool,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<(FtsMatch, FtsContentType)>> {
    // Search each type concurrently
    let (messages, blocks, archival) = tokio::try_join!(
        search_messages(pool, query, agent_id, limit),
        search_memory_blocks(pool, query, agent_id, limit),
        search_archival(pool, query, agent_id, limit),
    )?;

    // Merge and sort by rank
    let mut all: Vec<(FtsMatch, FtsContentType)> = messages
        .into_iter()
        .map(|m| (m, FtsContentType::Message))
        .chain(blocks.into_iter().map(|m| (m, FtsContentType::MemoryBlock)))
        .chain(
            archival
                .into_iter()
                .map(|m| (m, FtsContentType::ArchivalEntry)),
        )
        .collect();

    // Sort by rank (lower is better)
    all.sort_by(|a, b| {
        a.0.rank
            .partial_cmp(&b.0.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Truncate to limit
    all.truncate(limit as usize);

    Ok(all)
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
///
/// Use this after bulk imports or if the index gets out of sync.
pub async fn rebuild_messages_fts(pool: &SqlitePool) -> DbResult<()> {
    // FTS5 rebuild command
    sqlx::query("INSERT INTO messages_fts(messages_fts) VALUES('rebuild')")
        .execute(pool)
        .await?;
    Ok(())
}

/// Rebuild the FTS index for memory blocks.
pub async fn rebuild_memory_blocks_fts(pool: &SqlitePool) -> DbResult<()> {
    sqlx::query("INSERT INTO memory_blocks_fts(memory_blocks_fts) VALUES('rebuild')")
        .execute(pool)
        .await?;
    Ok(())
}

/// Rebuild the FTS index for archival entries.
pub async fn rebuild_archival_fts(pool: &SqlitePool) -> DbResult<()> {
    sqlx::query("INSERT INTO archival_fts(archival_fts) VALUES('rebuild')")
        .execute(pool)
        .await?;
    Ok(())
}

/// Rebuild all FTS indexes.
pub async fn rebuild_all_fts(pool: &SqlitePool) -> DbResult<()> {
    tokio::try_join!(
        rebuild_messages_fts(pool),
        rebuild_memory_blocks_fts(pool),
        rebuild_archival_fts(pool),
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
pub async fn get_fts_stats(pool: &SqlitePool) -> DbResult<FtsStats> {
    // Count indexed rows in each FTS table
    let messages: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages_fts")
        .fetch_one(pool)
        .await?;

    let memory_blocks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memory_blocks_fts")
        .fetch_one(pool)
        .await?;

    let archival: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM archival_fts")
        .fetch_one(pool)
        .await?;

    Ok(FtsStats {
        messages_indexed: messages.0 as u64,
        memory_blocks_indexed: memory_blocks.0 as u64,
        archival_entries_indexed: archival.0 as u64,
    })
}

/// Validate FTS query syntax.
///
/// Returns an error if the query contains invalid FTS5 syntax.
pub fn validate_fts_query(query: &str) -> DbResult<()> {
    // Basic validation - FTS5 will give better errors at runtime,
    // but we can catch obvious issues early.

    // Empty queries are invalid
    if query.trim().is_empty() {
        return Err(DbError::invalid_data("FTS query cannot be empty"));
    }

    // Unbalanced quotes
    let quote_count = query.chars().filter(|c| *c == '"').count();
    if quote_count % 2 != 0 {
        return Err(DbError::invalid_data("Unbalanced quotes in FTS query"));
    }

    // Unbalanced parentheses
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
    async fn create_test_agent(pool: &SqlitePool, id: &str) {
        sqlx::query(
            r#"
            INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
            VALUES (?, ?, 'anthropic', 'claude-3', 'test prompt', '{}', '[]', 'active', datetime('now'), datetime('now'))
            "#,
        )
        .bind(id)
        .bind(format!("{}_name", id))
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn test_validate_fts_query() {
        // Valid queries
        assert!(validate_fts_query("hello world").is_ok());
        assert!(validate_fts_query("\"exact phrase\"").is_ok());
        assert!(validate_fts_query("hello OR world").is_ok());
        assert!(validate_fts_query("prefix*").is_ok());
        assert!(validate_fts_query("(hello OR world) AND foo").is_ok());

        // Invalid queries
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

    #[tokio::test]
    async fn test_fts_tables_exist() {
        let db = ConstellationDb::open_in_memory().await.unwrap();

        // FTS tables should be created by migration
        let stats = get_fts_stats(db.pool()).await.unwrap();
        assert_eq!(stats.messages_indexed, 0);
        assert_eq!(stats.memory_blocks_indexed, 0);
        assert_eq!(stats.archival_entries_indexed, 0);
    }

    #[tokio::test]
    async fn test_fts_message_search() {
        let db = ConstellationDb::open_in_memory().await.unwrap();

        // Create agent first (foreign key constraint)
        create_test_agent(db.pool(), "agent_1").await;

        // Insert test messages
        sqlx::query(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content, is_archived, created_at)
            VALUES ('msg_1', 'agent_1', '1', 'user', 'hello world this is a test message', false, datetime('now'))
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content, is_archived, created_at)
            VALUES ('msg_2', 'agent_1', '2', 'assistant', 'goodbye cruel world', false, datetime('now'))
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        // Search for "hello" - should find msg_1
        let results = search_messages(db.pool(), "hello", None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "msg_1");
        assert!(results[0].content.contains("hello"));

        // Search for "world" - should find both
        let results = search_messages(db.pool(), "world", None, 10).await.unwrap();
        assert_eq!(results.len(), 2);

        // Search with agent filter
        let results = search_messages(db.pool(), "world", Some("agent_1"), 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = search_messages(db.pool(), "world", Some("agent_other"), 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_fts_rebuild() {
        let db = ConstellationDb::open_in_memory().await.unwrap();

        // Create agent first
        create_test_agent(db.pool(), "agent_1").await;

        // Insert a message
        sqlx::query(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content, is_archived, created_at)
            VALUES ('msg_rebuild', 'agent_1', '1', 'user', 'rebuild test message', false, datetime('now'))
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        // Rebuild should not error
        rebuild_messages_fts(db.pool()).await.unwrap();

        // Should still be searchable
        let results = search_messages(db.pool(), "rebuild", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_fts_phrase_search() {
        let db = ConstellationDb::open_in_memory().await.unwrap();

        // Create agent first
        create_test_agent(db.pool(), "agent_1").await;

        sqlx::query(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content, is_archived, created_at)
            VALUES ('msg_phrase', 'agent_1', '1', 'user', 'the quick brown fox jumps over the lazy dog', false, datetime('now'))
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        // Exact phrase search
        let results = search_messages(db.pool(), "\"quick brown fox\"", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // Non-matching phrase
        let results = search_messages(db.pool(), "\"brown quick fox\"", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_fts_prefix_search() {
        let db = ConstellationDb::open_in_memory().await.unwrap();

        // Create agent first
        create_test_agent(db.pool(), "agent_1").await;

        sqlx::query(
            r#"
            INSERT INTO messages (id, agent_id, position, role, content, is_archived, created_at)
            VALUES ('msg_prefix', 'agent_1', '1', 'user', 'programming is fun', false, datetime('now'))
            "#,
        )
        .execute(db.pool())
        .await
        .unwrap();

        // Prefix search
        let results = search_messages(db.pool(), "prog*", None, 10).await.unwrap();
        assert_eq!(results.len(), 1);

        let results = search_messages(db.pool(), "program*", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        let results = search_messages(db.pool(), "xyz*", None, 10).await.unwrap();
        assert_eq!(results.len(), 0);
    }
}
