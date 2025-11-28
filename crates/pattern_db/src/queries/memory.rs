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
                                   embedding_model, is_active, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        INSERT INTO memory_block_checkpoints (block_id, snapshot, created_at, updates_consolidated)
        VALUES (?, ?, ?, ?)
        "#,
        checkpoint.block_id,
        checkpoint.snapshot,
        checkpoint.created_at,
        checkpoint.updates_consolidated,
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
            updates_consolidated as "updates_consolidated!"
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
