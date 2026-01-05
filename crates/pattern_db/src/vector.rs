//! Vector search functionality using sqlite-vec.
//!
//! This module provides vector storage and KNN search capabilities for
//! semantic search over memories, messages, and other content.
//!
//! The sqlite-vec extension is registered globally via `sqlite3_auto_extension`
//! before any database connections are opened. This means all connections
//! automatically have access to vector functions and virtual tables.
//!
//! # Why Runtime Queries
//!
//! Unlike the rest of pattern_db, this module uses runtime `sqlx::query_as()`
//! instead of compile-time `sqlx::query_as!()` macros. This is intentional:
//!
//! 1. **Virtual table syntax** - `WHERE embedding MATCH ? AND k = ?` is
//!    sqlite-vec specific, not standard SQL. sqlx's compile-time checker
//!    doesn't understand it.
//!
//! 2. **Table created at runtime** - The `embeddings` virtual table is created
//!    via `ensure_embeddings_table()`, not in migrations. sqlx's offline mode
//!    can't see it.
//!
//! 3. **Dynamic dimensions** - Table definition uses `float[{dimensions}]`
//!    which varies per constellation.
//!
//! 4. **Extension-specific types** - Vector columns and the magic `distance`
//!    column from KNN queries don't map to sqlx-known types.
//!
//! The tradeoff is acceptable: vector queries are isolated here, patterns are
//! simple and stable, and we test at runtime anyway.

use std::ffi::c_char;
use std::sync::Once;

use sqlx::SqlitePool;
use zerocopy::IntoBytes;

use crate::error::{DbError, DbResult};

/// Default embedding dimensions (bge-small-en-v1.5).
/// Configurable per constellation if using different models.
pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 384;

static INIT: Once = Once::new();

/// Initialize sqlite-vec extension globally.
///
/// This registers the extension via `sqlite3_auto_extension`, which means
/// it will be automatically loaded for ALL SQLite connections created after
/// this call. Safe to call multiple times - only runs once.
///
/// # Safety
///
/// This function contains unsafe code to register the C extension. The unsafe
/// block is contained here to keep it in one place. The extension init function
/// is provided by the sqlite-vec crate which bundles and compiles the C source.
pub fn init_sqlite_vec() {
    INIT.call_once(|| {
        unsafe {
            // sqlite-vec exports sqlite3_vec_init with a slightly wrong signature.
            // We transmute to the correct sqlite3_auto_extension callback type.
            // This is the same pattern used in the sqlite-vec docs and confirmed
            // working in sqlx issue #3147.
            let init_fn = sqlite_vec::sqlite3_vec_init as *const ();
            let init_fn: unsafe extern "C" fn(
                *mut libsqlite3_sys::sqlite3,
                *mut *mut c_char,
                *const libsqlite3_sys::sqlite3_api_routines,
            ) -> std::ffi::c_int = std::mem::transmute(init_fn);
            libsqlite3_sys::sqlite3_auto_extension(Some(init_fn));
        }
        tracing::debug!("sqlite-vec extension registered globally");
    });
}

/// Types of content that can have embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Memory block content
    MemoryBlock,
    /// Message content
    Message,
    /// Archival entry
    ArchivalEntry,
    /// File passage
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

    pub fn from_str(s: &str) -> Option<Self> {
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
    /// The content ID
    pub content_id: String,
    /// Distance from query vector (lower = more similar)
    pub distance: f32,
    /// Content type
    pub content_type: ContentType,
    /// Chunk index if applicable
    pub chunk_index: Option<i32>,
}

/// Statistics about stored embeddings.
#[derive(Debug, Clone, Default)]
pub struct EmbeddingStats {
    pub total_embeddings: u64,
    pub by_content_type: Vec<(ContentType, u64)>,
}

/// Verify that sqlite-vec is loaded and working.
pub async fn verify_sqlite_vec(pool: &SqlitePool) -> DbResult<String> {
    let version: (String,) = sqlx::query_as("SELECT vec_version()")
        .fetch_one(pool)
        .await
        .map_err(|e| DbError::Extension(format!("sqlite-vec not loaded: {}", e)))?;
    Ok(version.0)
}

/// Create the embeddings virtual table if it doesn't exist.
///
/// Virtual tables can't be created via sqlx migrations (they use
/// extension-specific syntax), so we create them programmatically.
pub async fn ensure_embeddings_table(pool: &SqlitePool, dimensions: usize) -> DbResult<()> {
    // Create the unified embeddings table using vec0
    // The + prefix on columns makes them "auxiliary" columns stored alongside vectors
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

    sqlx::query(&create_sql).execute(pool).await?;
    tracing::debug!(dimensions, "ensured embeddings virtual table exists");
    Ok(())
}

/// Insert an embedding into the database.
pub async fn insert_embedding(
    pool: &SqlitePool,
    content_type: ContentType,
    content_id: &str,
    embedding: &[f32],
    chunk_index: Option<i32>,
    content_hash: Option<&str>,
) -> DbResult<i64> {
    let embedding_bytes = embedding.as_bytes();

    let rowid = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO embeddings (embedding, content_type, content_id, chunk_index, content_hash)
        VALUES (?, ?, ?, ?, ?)
        RETURNING rowid
        "#,
    )
    .bind(embedding_bytes)
    .bind(content_type.as_str())
    .bind(content_id)
    .bind(chunk_index)
    .bind(content_hash)
    .fetch_one(pool)
    .await?;

    Ok(rowid)
}

/// Delete embeddings for a content item.
pub async fn delete_embeddings(
    pool: &SqlitePool,
    content_type: ContentType,
    content_id: &str,
) -> DbResult<u64> {
    let result = sqlx::query("DELETE FROM embeddings WHERE content_type = ? AND content_id = ?")
        .bind(content_type.as_str())
        .bind(content_id)
        .execute(pool)
        .await?;

    Ok(result.rows_affected())
}

/// Update embedding for a content item (delete old, insert new).
pub async fn update_embedding(
    pool: &SqlitePool,
    content_type: ContentType,
    content_id: &str,
    embedding: &[f32],
    chunk_index: Option<i32>,
    content_hash: Option<&str>,
) -> DbResult<i64> {
    delete_embeddings(pool, content_type, content_id).await?;
    insert_embedding(
        pool,
        content_type,
        content_id,
        embedding,
        chunk_index,
        content_hash,
    )
    .await
}

/// Perform KNN search over embeddings.
///
/// Note: vec0 virtual tables don't support WHERE constraints on auxiliary
/// columns during KNN queries. If `content_type_filter` is specified, we
/// fetch more results and filter post-query. This means the actual number
/// of results may be less than `limit` when filtering.
pub async fn knn_search(
    pool: &SqlitePool,
    query_embedding: &[f32],
    limit: i64,
    content_type_filter: Option<ContentType>,
) -> DbResult<Vec<VectorSearchResult>> {
    let query_bytes = query_embedding.as_bytes();

    // When filtering by content type, fetch more results to account for
    // post-filtering. This is a tradeoff - we can't filter during KNN.
    let fetch_limit = if content_type_filter.is_some() {
        limit * 3 // Fetch 3x to have enough after filtering
    } else {
        limit
    };

    let results = sqlx::query_as::<_, (String, f32, String, Option<i32>)>(
        r#"
        SELECT content_id, distance, content_type, chunk_index
        FROM embeddings
        WHERE embedding MATCH ? AND k = ?
        ORDER BY distance
        "#,
    )
    .bind(query_bytes)
    .bind(fetch_limit)
    .fetch_all(pool)
    .await?;

    let mut results: Vec<VectorSearchResult> = results
        .into_iter()
        .filter_map(|(content_id, distance, content_type, chunk_index)| {
            let ct = ContentType::from_str(&content_type)?;
            // Apply content type filter if specified
            if let Some(filter_ct) = content_type_filter {
                if ct != filter_ct {
                    return None;
                }
            }
            Some(VectorSearchResult {
                content_id,
                distance,
                content_type: ct,
                chunk_index,
            })
        })
        .collect();

    // Truncate to requested limit
    results.truncate(limit as usize);
    Ok(results)
}

/// Search for similar content within a specific type.
pub async fn search_similar(
    pool: &SqlitePool,
    query_embedding: &[f32],
    content_type: ContentType,
    limit: i64,
    max_distance: Option<f32>,
) -> DbResult<Vec<VectorSearchResult>> {
    let mut results = knn_search(pool, query_embedding, limit, Some(content_type)).await?;

    // Filter by maximum distance if specified
    if let Some(max_dist) = max_distance {
        results.retain(|r| r.distance <= max_dist);
    }

    Ok(results)
}

/// Check if an embedding exists and is up-to-date.
pub async fn embedding_is_current(
    pool: &SqlitePool,
    content_type: ContentType,
    content_id: &str,
    current_hash: &str,
) -> DbResult<bool> {
    let result: Option<(String,)> = sqlx::query_as(
        "SELECT content_hash FROM embeddings WHERE content_type = ? AND content_id = ? LIMIT 1",
    )
    .bind(content_type.as_str())
    .bind(content_id)
    .fetch_optional(pool)
    .await?;

    Ok(result.map(|(h,)| h == current_hash).unwrap_or(false))
}

/// Get embedding statistics.
pub async fn get_embedding_stats(pool: &SqlitePool) -> DbResult<EmbeddingStats> {
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM embeddings")
        .fetch_one(pool)
        .await?;

    let by_type: Vec<(String, i64)> =
        sqlx::query_as("SELECT content_type, COUNT(*) FROM embeddings GROUP BY content_type")
            .fetch_all(pool)
            .await?;

    Ok(EmbeddingStats {
        total_embeddings: total.0 as u64,
        by_content_type: by_type
            .into_iter()
            .filter_map(|(ct, count)| ContentType::from_str(&ct).map(|t| (t, count as u64)))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_type_roundtrip() {
        for ct in [
            ContentType::MemoryBlock,
            ContentType::Message,
            ContentType::ArchivalEntry,
            ContentType::FilePassage,
        ] {
            let s = ct.as_str();
            assert_eq!(ContentType::from_str(s), Some(ct));
        }
    }

    #[test]
    fn test_content_type_unknown() {
        assert_eq!(ContentType::from_str("unknown"), None);
    }

    #[test]
    fn test_init_sqlite_vec_idempotent() {
        // Should be safe to call multiple times
        init_sqlite_vec();
        init_sqlite_vec();
        init_sqlite_vec();
    }

    #[tokio::test]
    async fn test_sqlite_vec_loaded() {
        // Open a connection (which registers sqlite-vec)
        let db = crate::ConstellationDb::open_in_memory().await.unwrap();

        // Verify sqlite-vec is available
        let version = verify_sqlite_vec(db.pool()).await.unwrap();
        assert!(!version.is_empty());
        assert!(
            version.starts_with("v"),
            "version should start with 'v': {}",
            version
        );
    }

    #[tokio::test]
    async fn test_embeddings_table_creation() {
        let db = crate::ConstellationDb::open_in_memory().await.unwrap();

        // Create the embeddings table
        ensure_embeddings_table(db.pool(), 384).await.unwrap();

        // Should be idempotent
        ensure_embeddings_table(db.pool(), 384).await.unwrap();
    }

    #[tokio::test]
    async fn test_embedding_insert_and_search() {
        let db = crate::ConstellationDb::open_in_memory().await.unwrap();
        ensure_embeddings_table(db.pool(), 4).await.unwrap();

        // Insert a test embedding
        let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        let rowid = insert_embedding(
            db.pool(),
            ContentType::Message,
            "msg_123",
            &embedding,
            None,
            Some("abc123"),
        )
        .await
        .unwrap();
        // vec0 rowids start at 0
        assert!(rowid >= 0);

        // Insert another
        let embedding2 = vec![0.9f32, 0.1, 0.0, 0.0]; // Similar to first
        insert_embedding(
            db.pool(),
            ContentType::Message,
            "msg_456",
            &embedding2,
            None,
            None,
        )
        .await
        .unwrap();

        // Insert a dissimilar one
        let embedding3 = vec![0.0f32, 0.0, 1.0, 0.0];
        insert_embedding(
            db.pool(),
            ContentType::MemoryBlock,
            "block_789",
            &embedding3,
            Some(0),
            None,
        )
        .await
        .unwrap();

        // Search for similar to first embedding
        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = knn_search(db.pool(), &query, 3, None).await.unwrap();

        assert_eq!(results.len(), 3);
        // First result should be exact match
        assert_eq!(results[0].content_id, "msg_123");
        assert!(results[0].distance < 0.01);
        // Second should be similar
        assert_eq!(results[1].content_id, "msg_456");

        // Search with content type filter
        let results = knn_search(db.pool(), &query, 3, Some(ContentType::Message))
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|r| r.content_type == ContentType::Message)
        );
    }

    #[tokio::test]
    async fn test_embedding_delete() {
        let db = crate::ConstellationDb::open_in_memory().await.unwrap();
        ensure_embeddings_table(db.pool(), 4).await.unwrap();

        let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        insert_embedding(
            db.pool(),
            ContentType::Message,
            "msg_delete_me",
            &embedding,
            None,
            None,
        )
        .await
        .unwrap();

        let deleted = delete_embeddings(db.pool(), ContentType::Message, "msg_delete_me")
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        // Should find nothing now
        let results = knn_search(db.pool(), &embedding, 10, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_embedding_stats() {
        let db = crate::ConstellationDb::open_in_memory().await.unwrap();
        ensure_embeddings_table(db.pool(), 4).await.unwrap();

        // Initially empty
        let stats = get_embedding_stats(db.pool()).await.unwrap();
        assert_eq!(stats.total_embeddings, 0);

        // Add some embeddings
        let emb = vec![1.0f32, 0.0, 0.0, 0.0];
        insert_embedding(db.pool(), ContentType::Message, "m1", &emb, None, None)
            .await
            .unwrap();
        insert_embedding(db.pool(), ContentType::Message, "m2", &emb, None, None)
            .await
            .unwrap();
        insert_embedding(db.pool(), ContentType::MemoryBlock, "b1", &emb, None, None)
            .await
            .unwrap();

        let stats = get_embedding_stats(db.pool()).await.unwrap();
        assert_eq!(stats.total_embeddings, 3);
        assert_eq!(stats.by_content_type.len(), 2);
    }
}
