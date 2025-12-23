//! Memory-related database queries.

use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{
    ArchivalEntry, MemoryBlock, MemoryBlockCheckpoint, MemoryBlockType, MemoryPermission,
};

/// Get a memory block by ID.
pub async fn get_block(pool: &SqlitePool, id: &str) -> DbResult<Option<MemoryBlock>> {
    let block = sqlx::query_as!(
        MemoryBlock,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            label as "label!",
            description as "description!",
            block_type as "block_type!: MemoryBlockType",
            char_limit as "char_limit!",
            permission as "permission!: MemoryPermission",
            pinned as "pinned!: bool",
            loro_snapshot as "loro_snapshot!",
            content_preview,
            metadata as "metadata: _",
            embedding_model,
            is_active as "is_active!: bool",
            frontier,
            last_seq as "last_seq!",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM memory_blocks WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(block)
}

/// Get a memory block by agent ID and label.
pub async fn get_block_by_label(
    pool: &SqlitePool,
    agent_id: &str,
    label: &str,
) -> DbResult<Option<MemoryBlock>> {
    let block = sqlx::query_as!(
        MemoryBlock,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            label as "label!",
            description as "description!",
            block_type as "block_type!: MemoryBlockType",
            char_limit as "char_limit!",
            permission as "permission!: MemoryPermission",
            pinned as "pinned!: bool",
            loro_snapshot as "loro_snapshot!",
            content_preview,
            metadata as "metadata: _",
            embedding_model,
            is_active as "is_active!: bool",
            frontier,
            last_seq as "last_seq!",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM memory_blocks WHERE agent_id = ? AND label = ?
        "#,
        agent_id,
        label
    )
    .fetch_optional(pool)
    .await?;
    Ok(block)
}

/// List all memory blocks for an agent.
pub async fn list_blocks(pool: &SqlitePool, agent_id: &str) -> DbResult<Vec<MemoryBlock>> {
    let blocks = sqlx::query_as!(
        MemoryBlock,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            label as "label!",
            description as "description!",
            block_type as "block_type!: MemoryBlockType",
            char_limit as "char_limit!",
            permission as "permission!: MemoryPermission",
            pinned as "pinned!: bool",
            loro_snapshot as "loro_snapshot!",
            content_preview,
            metadata as "metadata: _",
            embedding_model,
            is_active as "is_active!: bool",
            frontier,
            last_seq as "last_seq!",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM memory_blocks WHERE agent_id = ? AND is_active = 1 ORDER BY label
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(blocks)
}

/// List memory blocks by type.
pub async fn list_blocks_by_type(
    pool: &SqlitePool,
    agent_id: &str,
    block_type: MemoryBlockType,
) -> DbResult<Vec<MemoryBlock>> {
    let blocks = sqlx::query_as!(
        MemoryBlock,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            label as "label!",
            description as "description!",
            block_type as "block_type!: MemoryBlockType",
            char_limit as "char_limit!",
            permission as "permission!: MemoryPermission",
            pinned as "pinned!: bool",
            loro_snapshot as "loro_snapshot!",
            content_preview,
            metadata as "metadata: _",
            embedding_model,
            is_active as "is_active!: bool",
            frontier,
            last_seq as "last_seq!",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM memory_blocks WHERE agent_id = ? AND block_type = ? AND is_active = 1 ORDER BY label
        "#,
        agent_id,
        block_type
    )
    .fetch_all(pool)
    .await?;
    Ok(blocks)
}

/// Create a new memory block.
pub async fn create_block(pool: &SqlitePool, block: &MemoryBlock) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO memory_blocks (id, agent_id, label, description, block_type, char_limit,
                                   permission, pinned, loro_snapshot, content_preview, metadata,
                                   embedding_model, is_active, frontier, last_seq, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        block.id,
        block.agent_id,
        block.label,
        block.description,
        block.block_type,
        block.char_limit,
        block.permission,
        block.pinned,
        block.loro_snapshot,
        block.content_preview,
        block.metadata,
        block.embedding_model,
        block.is_active,
        block.frontier,
        block.last_seq,
        block.created_at,
        block.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a memory block's Loro snapshot and preview.
pub async fn update_block_content(
    pool: &SqlitePool,
    id: &str,
    loro_snapshot: &[u8],
    content_preview: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE memory_blocks
        SET loro_snapshot = ?, content_preview = ?, updated_at = datetime('now')
        WHERE id = ?
        "#,
        loro_snapshot,
        content_preview,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Soft-delete a memory block.
pub async fn deactivate_block(pool: &SqlitePool, id: &str) -> DbResult<()> {
    sqlx::query!(
        "UPDATE memory_blocks SET is_active = 0, updated_at = datetime('now') WHERE id = ?",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Create a checkpoint for a memory block.
pub async fn create_checkpoint(
    pool: &SqlitePool,
    checkpoint: &MemoryBlockCheckpoint,
) -> DbResult<i64> {
    let result = sqlx::query!(
        r#"
        INSERT INTO memory_block_checkpoints (block_id, snapshot, created_at, updates_consolidated, frontier)
        VALUES (?, ?, ?, ?, ?)
        "#,
        checkpoint.block_id,
        checkpoint.snapshot,
        checkpoint.created_at,
        checkpoint.updates_consolidated,
        checkpoint.frontier,
    )
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Get the latest checkpoint for a block.
pub async fn get_latest_checkpoint(
    pool: &SqlitePool,
    block_id: &str,
) -> DbResult<Option<MemoryBlockCheckpoint>> {
    let checkpoint = sqlx::query_as!(
        MemoryBlockCheckpoint,
        r#"
        SELECT
            id as "id!",
            block_id as "block_id!",
            snapshot as "snapshot!",
            created_at as "created_at!: _",
            updates_consolidated as "updates_consolidated!",
            frontier
        FROM memory_block_checkpoints WHERE block_id = ? ORDER BY created_at DESC LIMIT 1
        "#,
        block_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(checkpoint)
}

/// Get an archival entry by ID.
pub async fn get_archival_entry(pool: &SqlitePool, id: &str) -> DbResult<Option<ArchivalEntry>> {
    let entry = sqlx::query_as!(
        ArchivalEntry,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            content as "content!",
            metadata as "metadata: _",
            chunk_index as "chunk_index!",
            parent_entry_id,
            created_at as "created_at!: _"
        FROM archival_entries WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(entry)
}

/// List archival entries for an agent.
pub async fn list_archival_entries(
    pool: &SqlitePool,
    agent_id: &str,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<ArchivalEntry>> {
    let entries = sqlx::query_as!(
        ArchivalEntry,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            content as "content!",
            metadata as "metadata: _",
            chunk_index as "chunk_index!",
            parent_entry_id,
            created_at as "created_at!: _"
        FROM archival_entries WHERE agent_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?
        "#,
        agent_id,
        limit,
        offset
    )
    .fetch_all(pool)
    .await?;
    Ok(entries)
}

/// Create a new archival entry.
pub async fn create_archival_entry(pool: &SqlitePool, entry: &ArchivalEntry) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO archival_entries (id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        entry.id,
        entry.agent_id,
        entry.content,
        entry.metadata,
        entry.chunk_index,
        entry.parent_entry_id,
        entry.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete an archival entry.
pub async fn delete_archival_entry(pool: &SqlitePool, id: &str) -> DbResult<()> {
    sqlx::query!("DELETE FROM archival_entries WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Count archival entries for an agent.
pub async fn count_archival_entries(pool: &SqlitePool, agent_id: &str) -> DbResult<i64> {
    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM archival_entries WHERE agent_id = ?",
        agent_id
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

// ============================================================================
// Memory Block Updates (Delta Storage)
// ============================================================================

use crate::models::{MemoryBlockUpdate, UpdateStats};
use chrono::Utc;

/// Store a new incremental update for a memory block.
///
/// Atomically assigns the next sequence number and persists the update.
/// Returns the assigned sequence number.
pub async fn store_update(
    pool: &SqlitePool,
    block_id: &str,
    update_blob: &[u8],
    source: Option<&str>,
) -> DbResult<i64> {
    let now = Utc::now();
    let byte_size = update_blob.len() as i64;

    // Use a transaction to atomically increment last_seq and insert
    let mut tx = pool.begin().await?;

    // Get and increment the sequence number
    let row = sqlx::query!(
        "UPDATE memory_blocks SET last_seq = last_seq + 1, updated_at = ? WHERE id = ? RETURNING last_seq",
        now,
        block_id
    )
    .fetch_one(&mut *tx)
    .await?;

    let seq = row.last_seq;

    // Insert the update
    sqlx::query!(
        r#"
        INSERT INTO memory_block_updates (block_id, seq, update_blob, byte_size, source, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
        block_id,
        seq,
        update_blob,
        byte_size,
        source,
        now,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(seq)
}

/// Get the latest checkpoint and all pending updates for a block.
///
/// Used for full reconstruction on cache miss.
pub async fn get_checkpoint_and_updates(
    pool: &SqlitePool,
    block_id: &str,
) -> DbResult<(Option<MemoryBlockCheckpoint>, Vec<MemoryBlockUpdate>)> {
    // Get latest checkpoint
    let checkpoint = get_latest_checkpoint(pool, block_id).await?;

    // Get all updates (or updates since checkpoint if we have one)
    let updates = if let Some(ref cp) = checkpoint {
        // Get updates created after the checkpoint
        sqlx::query_as!(
            MemoryBlockUpdate,
            r#"
            SELECT
                id as "id!",
                block_id as "block_id!",
                seq as "seq!",
                update_blob as "update_blob!",
                byte_size as "byte_size!",
                source,
                created_at as "created_at!: _"
            FROM memory_block_updates
            WHERE block_id = ? AND created_at > ?
            ORDER BY seq ASC
            "#,
            block_id,
            cp.created_at
        )
        .fetch_all(pool)
        .await?
    } else {
        // No checkpoint, get all updates
        sqlx::query_as!(
            MemoryBlockUpdate,
            r#"
            SELECT
                id as "id!",
                block_id as "block_id!",
                seq as "seq!",
                update_blob as "update_blob!",
                byte_size as "byte_size!",
                source,
                created_at as "created_at!: _"
            FROM memory_block_updates
            WHERE block_id = ?
            ORDER BY seq ASC
            "#,
            block_id
        )
        .fetch_all(pool)
        .await?
    };

    Ok((checkpoint, updates))
}

/// Get updates after a given sequence number.
///
/// Used for cache refresh when we already have some state.
pub async fn get_updates_since(
    pool: &SqlitePool,
    block_id: &str,
    after_seq: i64,
) -> DbResult<Vec<MemoryBlockUpdate>> {
    let updates = sqlx::query_as!(
        MemoryBlockUpdate,
        r#"
        SELECT
            id as "id!",
            block_id as "block_id!",
            seq as "seq!",
            update_blob as "update_blob!",
            byte_size as "byte_size!",
            source,
            created_at as "created_at!: _"
        FROM memory_block_updates
        WHERE block_id = ? AND seq > ?
        ORDER BY seq ASC
        "#,
        block_id,
        after_seq
    )
    .fetch_all(pool)
    .await?;
    Ok(updates)
}

/// Check if there are updates after a given sequence number.
///
/// Lightweight check without fetching update data.
pub async fn has_updates_since(
    pool: &SqlitePool,
    block_id: &str,
    after_seq: i64,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "SELECT EXISTS(SELECT 1 FROM memory_block_updates WHERE block_id = ? AND seq > ?) as has_updates",
        block_id,
        after_seq
    )
    .fetch_one(pool)
    .await?;
    Ok(result.has_updates != 0)
}

/// Atomically consolidate updates into a new checkpoint.
///
/// Creates a new checkpoint with the merged state and deletes updates up to `up_to_seq`.
/// Updates arriving during the merge (with seq > up_to_seq) are preserved.
pub async fn consolidate_checkpoint(
    pool: &SqlitePool,
    block_id: &str,
    new_snapshot: &[u8],
    new_frontier: Option<&[u8]>,
    up_to_seq: i64,
) -> DbResult<()> {
    let now = Utc::now();

    let mut tx = pool.begin().await?;

    // Count updates being consolidated
    let count_result = sqlx::query!(
        "SELECT COUNT(*) as count FROM memory_block_updates WHERE block_id = ? AND seq <= ?",
        block_id,
        up_to_seq
    )
    .fetch_one(&mut *tx)
    .await?;
    let updates_consolidated = count_result.count;

    // Create new checkpoint
    sqlx::query!(
        r#"
        INSERT INTO memory_block_checkpoints (block_id, snapshot, created_at, updates_consolidated, frontier)
        VALUES (?, ?, ?, ?, ?)
        "#,
        block_id,
        new_snapshot,
        now,
        updates_consolidated,
        new_frontier,
    )
    .execute(&mut *tx)
    .await?;

    // Delete consolidated updates
    sqlx::query!(
        "DELETE FROM memory_block_updates WHERE block_id = ? AND seq <= ?",
        block_id,
        up_to_seq
    )
    .execute(&mut *tx)
    .await?;

    // Update the block's loro_snapshot and frontier
    sqlx::query!(
        r#"
        UPDATE memory_blocks
        SET loro_snapshot = ?, frontier = ?, updated_at = ?
        WHERE id = ?
        "#,
        new_snapshot,
        new_frontier,
        now,
        block_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Get statistics about pending updates for consolidation decisions.
pub async fn get_pending_update_stats(pool: &SqlitePool, block_id: &str) -> DbResult<UpdateStats> {
    let result = sqlx::query!(
        r#"
        SELECT
            COUNT(*) as count,
            COALESCE(SUM(byte_size), 0) as total_bytes,
            COALESCE(MAX(seq), 0) as max_seq
        FROM memory_block_updates
        WHERE block_id = ?
        "#,
        block_id
    )
    .fetch_one(pool)
    .await?;

    Ok(UpdateStats {
        count: result.count,
        total_bytes: result.total_bytes,
        max_seq: result.max_seq,
    })
}

/// Update a block's frontier without creating an update record.
///
/// Used when applying updates from external sources where we just need to track version.
pub async fn update_block_frontier(
    pool: &SqlitePool,
    block_id: &str,
    frontier: &[u8],
) -> DbResult<()> {
    let now = Utc::now();
    sqlx::query!(
        "UPDATE memory_blocks SET frontier = ?, updated_at = ? WHERE id = ?",
        frontier,
        now,
        block_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a lightweight view of a block for cache lookups.
///
/// Returns just the ID and last_seq without loading the full snapshot.
pub async fn get_block_version_info(
    pool: &SqlitePool,
    block_id: &str,
) -> DbResult<Option<(String, i64)>> {
    let result = sqlx::query!(
        r#"SELECT id as "id!", last_seq FROM memory_blocks WHERE id = ?"#,
        block_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result.map(|r| (r.id, r.last_seq)))
}
