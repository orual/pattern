//! Vector search functionality using sqlite-vec.
//!
//! This module provides vector storage and KNN search capabilities for
//! semantic search over memories, messages, and other content.
//!
//! The sqlite-vec extension is registered globally via `sqlite3_auto_extension`
//! in [`ConstellationDb::open`], so all connections automatically have access
//! to vector functions and virtual tables.

use rusqlite::Connection;
use zerocopy::IntoBytes;

use crate::error::{DbError, DbResult};

/// Default embedding dimensions (bge-small-en-v1.5).
/// Configurable per constellation if using different models.
pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 384;

/// Types of content that can have embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Memory block content.
    MemoryBlock,
    /// Message content.
    Message,
    /// Archival entry.
    ArchivalEntry,
    /// File passage.
    FilePassage,
}

impl ContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentType::MemoryBlock => "memory_block",
            ContentType::Message => "message",
            ContentType::ArchivalEntry => "archival_entry",
            ContentType::FilePassage => "file_passage",
        }
    }

    /// Parse from the canonical string form (inverse of [`Self::as_str`]).
    pub fn parse_from_str(s: &str) -> Option<Self> {
        match s {
            "memory_block" => Some(ContentType::MemoryBlock),
            "message" => Some(ContentType::Message),
            "archival_entry" => Some(ContentType::ArchivalEntry),
            "file_passage" => Some(ContentType::FilePassage),
            _ => None,
        }
    }
}

/// Result of a KNN vector search.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    /// The content ID.
    pub content_id: String,
    /// Distance from query vector (lower = more similar).
    pub distance: f32,
    /// Content type.
    pub content_type: ContentType,
    /// Chunk index if applicable.
    pub chunk_index: Option<i32>,
}

/// Statistics about stored embeddings.
#[derive(Debug, Clone, Default)]
pub struct EmbeddingStats {
    pub total_embeddings: u64,
    pub by_content_type: Vec<(ContentType, u64)>,
}

/// Verify that sqlite-vec is loaded and working.
pub fn verify_sqlite_vec(conn: &Connection) -> DbResult<String> {
    let version: String = conn
        .query_row("SELECT vec_version()", [], |r| r.get(0))
        .map_err(|e| DbError::Extension(format!("sqlite-vec not loaded: {e}")))?;
    Ok(version)
}

/// Create the embeddings virtual table if it doesn't exist.
///
/// Virtual tables can't be created via migrations (they use
/// extension-specific syntax), so we create them programmatically.
pub fn ensure_embeddings_table(conn: &Connection, dimensions: usize) -> DbResult<()> {
    let create_sql = format!(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS embeddings USING vec0(
            embedding float[{dimensions}],
            +content_type TEXT NOT NULL,
            +content_id TEXT NOT NULL,
            +chunk_index INTEGER,
            +content_hash TEXT
        )
        "#,
    );

    conn.execute_batch(&create_sql)?;
    tracing::debug!(dimensions, "ensured embeddings virtual table exists");
    Ok(())
}

/// Insert an embedding into the database.
pub fn insert_embedding(
    conn: &Connection,
    content_type: ContentType,
    content_id: &str,
    embedding: &[f32],
    chunk_index: Option<i32>,
    content_hash: Option<&str>,
) -> DbResult<i64> {
    let embedding_bytes = embedding.as_bytes();

    // vec0 virtual tables don't support RETURNING, so use last_insert_rowid().
    conn.execute(
        r#"
        INSERT INTO embeddings (embedding, content_type, content_id, chunk_index, content_hash)
        VALUES (?, ?, ?, ?, ?)
        "#,
        rusqlite::params![
            embedding_bytes,
            content_type.as_str(),
            content_id,
            chunk_index,
            content_hash,
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Delete embeddings for a content item.
pub fn delete_embeddings(
    conn: &Connection,
    content_type: ContentType,
    content_id: &str,
) -> DbResult<usize> {
    let count = conn.execute(
        "DELETE FROM embeddings WHERE content_type = ? AND content_id = ?",
        rusqlite::params![content_type.as_str(), content_id],
    )?;

    Ok(count)
}

/// Update embedding for a content item (delete old, insert new).
pub fn update_embedding(
    conn: &Connection,
    content_type: ContentType,
    content_id: &str,
    embedding: &[f32],
    chunk_index: Option<i32>,
    content_hash: Option<&str>,
) -> DbResult<i64> {
    delete_embeddings(conn, content_type, content_id)?;
    insert_embedding(
        conn,
        content_type,
        content_id,
        embedding,
        chunk_index,
        content_hash,
    )
}

/// Perform KNN search over embeddings.
///
/// Note: vec0 virtual tables don't support WHERE constraints on auxiliary
/// columns during KNN queries. If `content_type_filter` is specified, we
/// fetch more results and filter post-query.
pub fn knn_search(
    conn: &Connection,
    query_embedding: &[f32],
    limit: i64,
    content_type_filter: Option<ContentType>,
) -> DbResult<Vec<VectorSearchResult>> {
    let query_bytes = query_embedding.as_bytes();

    // When filtering by content type, fetch more results to account for
    // post-filtering.
    let fetch_limit = if content_type_filter.is_some() {
        limit * 3
    } else {
        limit
    };

    let mut stmt = conn.prepare(
        r#"
        SELECT content_id, distance, content_type, chunk_index
        FROM embeddings
        WHERE embedding MATCH ? AND k = ?
        ORDER BY distance
        "#,
    )?;

    let rows = stmt.query_map(rusqlite::params![query_bytes, fetch_limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, f32>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i32>>(3)?,
        ))
    })?;

    let mut results: Vec<VectorSearchResult> = rows
        .filter_map(|r| {
            let (content_id, distance, content_type_str, chunk_index) = r.ok()?;
            let ct = ContentType::parse_from_str(&content_type_str)?;
            if let Some(filter_ct) = content_type_filter
                && ct != filter_ct
            {
                return None;
            }
            Some(VectorSearchResult {
                content_id,
                distance,
                content_type: ct,
                chunk_index,
            })
        })
        .collect();

    results.truncate(limit as usize);
    Ok(results)
}

/// Search for similar content within a specific type.
pub fn search_similar(
    conn: &Connection,
    query_embedding: &[f32],
    content_type: ContentType,
    limit: i64,
    max_distance: Option<f32>,
) -> DbResult<Vec<VectorSearchResult>> {
    let mut results = knn_search(conn, query_embedding, limit, Some(content_type))?;

    if let Some(max_dist) = max_distance {
        results.retain(|r| r.distance <= max_dist);
    }

    Ok(results)
}

/// Check if an embedding exists and is up-to-date.
pub fn embedding_is_current(
    conn: &Connection,
    content_type: ContentType,
    content_id: &str,
    current_hash: &str,
) -> DbResult<bool> {
    let result: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM embeddings WHERE content_type = ? AND content_id = ? LIMIT 1",
            rusqlite::params![content_type.as_str(), content_id],
            |row| row.get(0),
        )
        .ok();

    Ok(result.map(|h| h == current_hash).unwrap_or(false))
}

/// Get embedding statistics.
pub fn get_embedding_stats(conn: &Connection) -> DbResult<EmbeddingStats> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))?;

    let mut stmt =
        conn.prepare("SELECT content_type, COUNT(*) FROM embeddings GROUP BY content_type")?;
    let by_type: Vec<(ContentType, u64)> = stmt
        .query_map([], |row| {
            let ct_str: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((ct_str, count))
        })?
        .filter_map(|r| {
            let (ct_str, count) = r.ok()?;
            ContentType::parse_from_str(&ct_str).map(|ct| (ct, count as u64))
        })
        .collect();

    Ok(EmbeddingStats {
        total_embeddings: total as u64,
        by_content_type: by_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConstellationDb;

    #[test]
    fn test_content_type_roundtrip() {
        for ct in [
            ContentType::MemoryBlock,
            ContentType::Message,
            ContentType::ArchivalEntry,
            ContentType::FilePassage,
        ] {
            let s = ct.as_str();
            assert_eq!(ContentType::parse_from_str(s), Some(ct));
        }
    }

    #[test]
    fn test_content_type_unknown() {
        assert_eq!(ContentType::parse_from_str("unknown"), None);
    }

    #[test]
    fn test_sqlite_vec_loaded() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        let version = verify_sqlite_vec(&conn).unwrap();
        assert!(!version.is_empty());
        assert!(
            version.starts_with("v"),
            "version should start with 'v': {version}",
        );
    }

    #[test]
    fn test_embeddings_table_creation() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();

        ensure_embeddings_table(&conn, 384).unwrap();
        // Should be idempotent.
        ensure_embeddings_table(&conn, 384).unwrap();
    }

    #[test]
    fn test_embedding_insert_and_search() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();
        ensure_embeddings_table(&conn, 4).unwrap();

        let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        let rowid = insert_embedding(
            &conn,
            ContentType::Message,
            "msg_123",
            &embedding,
            None,
            Some("abc123"),
        )
        .unwrap();
        assert!(rowid >= 0);

        let embedding2 = vec![0.9f32, 0.1, 0.0, 0.0];
        insert_embedding(
            &conn,
            ContentType::Message,
            "msg_456",
            &embedding2,
            None,
            None,
        )
        .unwrap();

        let embedding3 = vec![0.0f32, 0.0, 1.0, 0.0];
        insert_embedding(
            &conn,
            ContentType::MemoryBlock,
            "block_789",
            &embedding3,
            Some(0),
            None,
        )
        .unwrap();

        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = knn_search(&conn, &query, 3, None).unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].content_id, "msg_123");
        assert!(results[0].distance < 0.01);
        assert_eq!(results[1].content_id, "msg_456");

        // Search with content type filter.
        let results = knn_search(&conn, &query, 3, Some(ContentType::Message)).unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|r| r.content_type == ContentType::Message)
        );
    }

    #[test]
    fn test_embedding_delete() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();
        ensure_embeddings_table(&conn, 4).unwrap();

        let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        insert_embedding(
            &conn,
            ContentType::Message,
            "msg_delete_me",
            &embedding,
            None,
            None,
        )
        .unwrap();

        let deleted = delete_embeddings(&conn, ContentType::Message, "msg_delete_me").unwrap();
        assert_eq!(deleted, 1);

        let results = knn_search(&conn, &embedding, 10, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_embedding_stats() {
        let db = ConstellationDb::open_in_memory().unwrap();
        let conn = db.get().unwrap();
        ensure_embeddings_table(&conn, 4).unwrap();

        let stats = get_embedding_stats(&conn).unwrap();
        assert_eq!(stats.total_embeddings, 0);

        let emb = vec![1.0f32, 0.0, 0.0, 0.0];
        insert_embedding(&conn, ContentType::Message, "m1", &emb, None, None).unwrap();
        insert_embedding(&conn, ContentType::Message, "m2", &emb, None, None).unwrap();
        insert_embedding(&conn, ContentType::MemoryBlock, "b1", &emb, None, None).unwrap();

        let stats = get_embedding_stats(&conn).unwrap();
        assert_eq!(stats.total_embeddings, 3);
        assert_eq!(stats.by_content_type.len(), 2);
    }
}
