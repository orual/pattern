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

/// List all memory blocks in the database.
///
/// Used for constellation exports to capture all shared and owned blocks.
/// No agent_id filter - returns every active block.
pub async fn list_all_blocks(pool: &SqlitePool) -> DbResult<Vec<MemoryBlock>> {
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
        FROM memory_blocks WHERE is_active = 1 ORDER BY agent_id, label
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(blocks)
}

/// List all shared block attachments in the database.
///
/// Used for constellation exports to capture all sharing relationships.
pub async fn list_all_shared_block_attachments(
    pool: &SqlitePool,
) -> DbResult<Vec<SharedBlockAttachment>> {
    let attachments = sqlx::query_as!(
        SharedBlockAttachment,
        r#"
        SELECT
            block_id as "block_id!",
            agent_id as "agent_id!",
            permission as "permission!: MemoryPermission",
            attached_at as "attached_at!: _"
        FROM shared_block_agents
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(attachments)
}

/// List memory blocks by label prefix (across all agents).
///
/// Used for system-level operations like restoring DataBlock source tracking
/// after restart. Finds all blocks whose labels start with the given prefix.
pub async fn list_blocks_by_label_prefix(
    pool: &SqlitePool,
    prefix: &str,
) -> DbResult<Vec<MemoryBlock>> {
    let pattern = format!("{}%", prefix);
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
        FROM memory_blocks WHERE label LIKE ? AND is_active = 1 ORDER BY label
        "#,
        pattern
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

/// Update a memory block's permission.
pub async fn update_block_permission(
    pool: &SqlitePool,
    id: &str,
    permission: MemoryPermission,
) -> DbResult<()> {
    let perm_str = permission.as_str();
    sqlx::query(
        "UPDATE memory_blocks SET permission = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(perm_str)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update configuration metadata for a memory block without touching content.
///
/// This is used for config file merges where the TOML can update config fields
/// but the database owns the content. Only fields provided as Some will be updated;
/// None values leave the field unchanged.
///
/// Fields:
/// - `permission`: Access permission level for the block
/// - `block_type`: Type classification (core, working, archival, log)
/// - `description`: Human/LLM-readable description of the block's purpose
/// - `pinned`: Whether the block is always loaded into context
/// - `char_limit`: Maximum character limit for block content
pub async fn update_block_config(
    pool: &SqlitePool,
    id: &str,
    permission: Option<MemoryPermission>,
    block_type: Option<MemoryBlockType>,
    description: Option<&str>,
    pinned: Option<bool>,
    char_limit: Option<i64>,
) -> DbResult<()> {
    // Use a transaction to ensure atomicity between fetch and update.
    let mut tx = pool.begin().await?;

    // Fetch current values to use as defaults for unspecified fields.
    let current = sqlx::query_as!(
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
    .fetch_optional(&mut *tx)
    .await?;

    let Some(current) = current else {
        return Err(crate::error::DbError::not_found("memory block", id));
    };

    // Use provided values or fall back to current values.
    let perm = permission.unwrap_or(current.permission);
    let btype = block_type.unwrap_or(current.block_type);
    let desc = description.unwrap_or(&current.description);
    let pin = pinned.unwrap_or(current.pinned);
    let limit = char_limit.unwrap_or(current.char_limit);

    sqlx::query!(
        r#"
        UPDATE memory_blocks
        SET permission = ?, block_type = ?, description = ?, pinned = ?, char_limit = ?, updated_at = datetime('now')
        WHERE id = ?
        "#,
        perm,
        btype,
        desc,
        pin,
        limit,
        id
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Update a memory block's pinned flag.
///
/// Pinned blocks are always loaded into agent context while subscribed.
/// Unpinned (ephemeral) blocks only load when referenced by a notification.
pub async fn update_block_pinned(pool: &SqlitePool, id: &str, pinned: bool) -> DbResult<()> {
    sqlx::query!(
        "UPDATE memory_blocks SET pinned = ?, updated_at = datetime('now') WHERE id = ?",
        pinned,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Rename a memory block by updating its label.
///
/// Note: This only updates the label in the database. The caller is responsible
/// for ensuring no other block with the same label exists for this agent.
pub async fn update_block_label(pool: &SqlitePool, id: &str, new_label: &str) -> DbResult<()> {
    sqlx::query!(
        "UPDATE memory_blocks SET label = ?, updated_at = datetime('now') WHERE id = ?",
        new_label,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a memory block's type.
///
/// Used for archiving blocks (changing Working -> Archival).
pub async fn update_block_type(
    pool: &SqlitePool,
    id: &str,
    block_type: MemoryBlockType,
) -> DbResult<()> {
    let type_str = block_type.as_str();
    sqlx::query(
        "UPDATE memory_blocks SET block_type = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(type_str)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a memory block's metadata.
///
/// Used for schema updates (e.g., changing viewport settings on Text blocks).
pub async fn update_block_metadata(
    pool: &SqlitePool,
    id: &str,
    metadata: &serde_json::Value,
) -> DbResult<()> {
    let metadata_str = serde_json::to_string(metadata)?;
    sqlx::query("UPDATE memory_blocks SET metadata = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(metadata_str)
        .bind(id)
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
/// The `frontier` parameter stores the Loro version vector after this update,
/// enabling precise undo to any historical state.
/// Returns the assigned sequence number.
pub async fn store_update(
    pool: &SqlitePool,
    block_id: &str,
    update_blob: &[u8],
    frontier: Option<&[u8]>,
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
        INSERT INTO memory_block_updates (block_id, seq, update_blob, byte_size, source, frontier, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        block_id,
        seq,
        update_blob,
        byte_size,
        source,
        frontier,
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

    // Get all active updates (or updates since checkpoint if we have one)
    let updates = if let Some(ref cp) = checkpoint {
        // Get active updates created after the checkpoint
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
                frontier,
                is_active as "is_active!: bool",
                created_at as "created_at!: _"
            FROM memory_block_updates
            WHERE block_id = ? AND created_at > ? AND is_active = 1
            ORDER BY seq ASC
            "#,
            block_id,
            cp.created_at
        )
        .fetch_all(pool)
        .await?
    } else {
        // No checkpoint, get all active updates
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
                frontier,
                is_active as "is_active!: bool",
                created_at as "created_at!: _"
            FROM memory_block_updates
            WHERE block_id = ? AND is_active = 1
            ORDER BY seq ASC
            "#,
            block_id
        )
        .fetch_all(pool)
        .await?
    };

    Ok((checkpoint, updates))
}

/// Get active updates after a given sequence number.
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
            frontier,
            is_active as "is_active!: bool",
            created_at as "created_at!: _"
        FROM memory_block_updates
        WHERE block_id = ? AND seq > ? AND is_active = 1
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

// ============================================================================
// Undo Support Queries
// ============================================================================

/// Get the most recent active update for a block.
///
/// Returns None if no active updates exist.
pub async fn get_latest_update(
    pool: &SqlitePool,
    block_id: &str,
) -> DbResult<Option<MemoryBlockUpdate>> {
    let update = sqlx::query_as!(
        MemoryBlockUpdate,
        r#"
        SELECT
            id as "id!",
            block_id as "block_id!",
            seq as "seq!",
            update_blob as "update_blob!",
            byte_size as "byte_size!",
            source,
            frontier,
            is_active as "is_active!: bool",
            created_at as "created_at!: _"
        FROM memory_block_updates
        WHERE block_id = ? AND is_active = 1
        ORDER BY seq DESC
        LIMIT 1
        "#,
        block_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(update)
}

/// Get checkpoint and active updates up to (inclusive) a sequence number.
///
/// Used for reconstructing document state at a specific point in history.
/// Returns the latest checkpoint that precedes the target seq, plus all
/// active updates from checkpoint up to and including target_seq.
pub async fn get_checkpoint_and_updates_until(
    pool: &SqlitePool,
    block_id: &str,
    max_seq: i64,
) -> DbResult<(Option<MemoryBlockCheckpoint>, Vec<MemoryBlockUpdate>)> {
    // Get latest checkpoint
    let checkpoint = get_latest_checkpoint(pool, block_id).await?;

    // Get active updates up to max_seq (from checkpoint if exists, otherwise from beginning)
    let updates = if let Some(ref cp) = checkpoint {
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
                frontier,
                is_active as "is_active!: bool",
                created_at as "created_at!: _"
            FROM memory_block_updates
            WHERE block_id = ? AND created_at > ? AND seq <= ? AND is_active = 1
            ORDER BY seq ASC
            "#,
            block_id,
            cp.created_at,
            max_seq
        )
        .fetch_all(pool)
        .await?
    } else {
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
                frontier,
                is_active as "is_active!: bool",
                created_at as "created_at!: _"
            FROM memory_block_updates
            WHERE block_id = ? AND seq <= ? AND is_active = 1
            ORDER BY seq ASC
            "#,
            block_id,
            max_seq
        )
        .fetch_all(pool)
        .await?
    };

    Ok((checkpoint, updates))
}

/// Deactivate the latest active update for a block (undo).
///
/// Marks the most recent active update as inactive, effectively undoing it.
/// Returns the seq of the deactivated update, or None if no active updates.
pub async fn deactivate_latest_update(pool: &SqlitePool, block_id: &str) -> DbResult<Option<i64>> {
    // Find the latest active update
    let latest = sqlx::query!(
        r#"
        SELECT id, seq FROM memory_block_updates
        WHERE block_id = ? AND is_active = 1
        ORDER BY seq DESC
        LIMIT 1
        "#,
        block_id
    )
    .fetch_optional(pool)
    .await?;

    let Some(row) = latest else {
        return Ok(None);
    };

    // Mark it as inactive
    sqlx::query!(
        "UPDATE memory_block_updates SET is_active = 0 WHERE id = ?",
        row.id
    )
    .execute(pool)
    .await?;

    Ok(Some(row.seq))
}

/// Reactivate the next inactive update for a block (redo).
///
/// Finds the first inactive update after the current active branch and reactivates it.
/// Returns the seq of the reactivated update, or None if nothing to redo.
pub async fn reactivate_next_update(pool: &SqlitePool, block_id: &str) -> DbResult<Option<i64>> {
    // Get the max active seq (or 0 if none)
    let max_active = sqlx::query!(
        r#"
        SELECT COALESCE(MAX(seq), 0) as max_seq
        FROM memory_block_updates
        WHERE block_id = ? AND is_active = 1
        "#,
        block_id
    )
    .fetch_one(pool)
    .await?;

    let max_active_seq = max_active.max_seq;

    // Find the first inactive update after max_active_seq
    let next_inactive = sqlx::query!(
        r#"
        SELECT id, seq FROM memory_block_updates
        WHERE block_id = ? AND is_active = 0 AND seq > ?
        ORDER BY seq ASC
        LIMIT 1
        "#,
        block_id,
        max_active_seq
    )
    .fetch_optional(pool)
    .await?;

    let Some(row) = next_inactive else {
        return Ok(None);
    };

    // Mark it as active
    sqlx::query!(
        "UPDATE memory_block_updates SET is_active = 1 WHERE id = ?",
        row.id
    )
    .execute(pool)
    .await?;

    Ok(Some(row.seq))
}

/// Count available undo steps for a block.
///
/// Returns the number of active updates that can be undone.
pub async fn count_undo_steps(pool: &SqlitePool, block_id: &str) -> DbResult<i64> {
    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM memory_block_updates WHERE block_id = ? AND is_active = 1",
        block_id
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

/// Count available redo steps for a block.
///
/// Returns the number of inactive updates after the active branch that can be redone.
pub async fn count_redo_steps(pool: &SqlitePool, block_id: &str) -> DbResult<i64> {
    // Get max active seq
    let max_active = sqlx::query!(
        r#"
        SELECT COALESCE(MAX(seq), 0) as max_seq
        FROM memory_block_updates
        WHERE block_id = ? AND is_active = 1
        "#,
        block_id
    )
    .fetch_one(pool)
    .await?;

    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM memory_block_updates WHERE block_id = ? AND is_active = 0 AND seq > ?",
        block_id,
        max_active.max_seq
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

/// Reset a block's last_seq to a specific value.
///
/// Used after undo to sync the sequence counter with the actual update history.
pub async fn reset_block_last_seq(pool: &SqlitePool, block_id: &str, new_seq: i64) -> DbResult<()> {
    let now = Utc::now();
    sqlx::query!(
        "UPDATE memory_blocks SET last_seq = ?, updated_at = ? WHERE id = ?",
        new_seq,
        now,
        block_id
    )
    .execute(pool)
    .await?;
    Ok(())
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

// ============================================================================
// Shared Block Management
// ============================================================================

use crate::models::SharedBlockAttachment;

/// Create a shared block attachment.
///
/// Grants an agent access to a block with specific permissions.
/// If the attachment already exists, updates the permission and timestamp.
pub async fn create_shared_block_attachment(
    pool: &SqlitePool,
    block_id: &str,
    agent_id: &str,
    permission: MemoryPermission,
) -> DbResult<()> {
    let now = Utc::now();
    sqlx::query!(
        r#"
        INSERT INTO shared_block_agents (block_id, agent_id, permission, attached_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(block_id, agent_id) DO UPDATE SET
            permission = excluded.permission,
            attached_at = excluded.attached_at
        "#,
        block_id,
        agent_id,
        permission,
        now,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a shared block attachment.
///
/// Removes an agent's access to a shared block.
pub async fn delete_shared_block_attachment(
    pool: &SqlitePool,
    block_id: &str,
    agent_id: &str,
) -> DbResult<()> {
    sqlx::query!(
        "DELETE FROM shared_block_agents WHERE block_id = ? AND agent_id = ?",
        block_id,
        agent_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// List all agents a block is shared with.
///
/// Returns all shared attachments for a given block.
pub async fn list_block_shared_agents(
    pool: &SqlitePool,
    block_id: &str,
) -> DbResult<Vec<SharedBlockAttachment>> {
    let attachments = sqlx::query_as!(
        SharedBlockAttachment,
        r#"
        SELECT
            block_id as "block_id!",
            agent_id as "agent_id!",
            permission as "permission!: MemoryPermission",
            attached_at as "attached_at!: _"
        FROM shared_block_agents WHERE block_id = ?
        "#,
        block_id
    )
    .fetch_all(pool)
    .await?;
    Ok(attachments)
}

/// List all blocks shared with an agent.
///
/// Returns all shared attachments for a given agent.
pub async fn list_agent_shared_blocks(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<SharedBlockAttachment>> {
    let attachments = sqlx::query_as!(
        SharedBlockAttachment,
        r#"
        SELECT
            block_id as "block_id!",
            agent_id as "agent_id!",
            permission as "permission!: MemoryPermission",
            attached_at as "attached_at!: _"
        FROM shared_block_agents WHERE agent_id = ?
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(attachments)
}

/// Get a specific shared attachment.
///
/// Checks if an agent has access to a specific block and returns the attachment details.
pub async fn get_shared_block_attachment(
    pool: &SqlitePool,
    block_id: &str,
    agent_id: &str,
) -> DbResult<Option<SharedBlockAttachment>> {
    let attachment = sqlx::query_as!(
        SharedBlockAttachment,
        r#"
        SELECT
            block_id as "block_id!",
            agent_id as "agent_id!",
            permission as "permission!: MemoryPermission",
            attached_at as "attached_at!: _"
        FROM shared_block_agents WHERE block_id = ? AND agent_id = ?
        "#,
        block_id,
        agent_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(attachment)
}

/// Helper struct for the JOIN result in get_shared_blocks.
struct SharedBlockRow {
    id: String,
    agent_id: String,
    agent_name: Option<String>,
    label: String,
    description: String,
    block_type: MemoryBlockType,
    char_limit: i64,
    permission: MemoryPermission,
    pinned: bool,
    loro_snapshot: Vec<u8>,
    content_preview: Option<String>,
    metadata: Option<sqlx::types::Json<serde_json::Value>>,
    embedding_model: Option<String>,
    is_active: bool,
    frontier: Option<Vec<u8>>,
    last_seq: i64,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    attachment_permission: MemoryPermission,
}

/// Check if a requester has access to a specific block and return the permission level.
///
/// This is an efficient single-query check that handles both owned and shared blocks.
/// Returns (block_id, effective_permission):
/// - If the requester owns the block: returns the block's inherent permission
/// - If the requester has shared access: returns the shared permission
/// - If no access: returns None
pub async fn check_block_access(
    pool: &SqlitePool,
    requester_agent_id: &str,
    owner_agent_id: &str,
    label: &str,
) -> DbResult<Option<(String, MemoryPermission)>> {
    // First check if requester owns the block
    if requester_agent_id == owner_agent_id {
        // Owned block - get inherent permission
        let block = get_block_by_label(pool, owner_agent_id, label).await?;
        return Ok(block.map(|b| (b.id, b.permission)));
    }

    // Check for shared access
    // Join to ensure the block exists and is active
    let result = sqlx::query!(
        r#"
        SELECT mb.id as "id!", sba.permission as "permission!: MemoryPermission"
        FROM shared_block_agents sba
        INNER JOIN memory_blocks mb ON sba.block_id = mb.id
        WHERE sba.agent_id = ?
          AND mb.agent_id = ?
          AND mb.label = ?
          AND mb.is_active = 1
        "#,
        requester_agent_id,
        owner_agent_id,
        label
    )
    .fetch_optional(pool)
    .await?;

    Ok(result.map(|r| (r.id, r.permission)))
}

/// Get all shared blocks for an agent with full block data.
///
/// Returns tuples of (MemoryBlock, MemoryPermission, Option<owner_name>) where the permission
/// is from the shared_block_agents table. Only returns active blocks.
/// The owner_name is looked up from the agents table (may be None if agent doesn't exist).
pub async fn get_shared_blocks(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<(MemoryBlock, MemoryPermission, Option<String>)>> {
    let rows = sqlx::query_as!(
        SharedBlockRow,
        r#"
        SELECT
            mb.id as "id!",
            mb.agent_id as "agent_id!",
            a.name as "agent_name",
            mb.label as "label!",
            mb.description as "description!",
            mb.block_type as "block_type!: MemoryBlockType",
            mb.char_limit as "char_limit!",
            mb.permission as "permission!: MemoryPermission",
            mb.pinned as "pinned!: bool",
            mb.loro_snapshot as "loro_snapshot!",
            mb.content_preview,
            mb.metadata as "metadata: _",
            mb.embedding_model,
            mb.is_active as "is_active!: bool",
            mb.frontier,
            mb.last_seq as "last_seq!",
            mb.created_at as "created_at!: _",
            mb.updated_at as "updated_at!: _",
            sba.permission as "attachment_permission!: MemoryPermission"
        FROM shared_block_agents sba
        INNER JOIN memory_blocks mb ON sba.block_id = mb.id
        LEFT JOIN agents a ON mb.agent_id = a.id
        WHERE sba.agent_id = ? AND mb.is_active = 1
        ORDER BY mb.label
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let block = MemoryBlock {
                id: r.id,
                agent_id: r.agent_id,
                label: r.label,
                description: r.description,
                block_type: r.block_type,
                char_limit: r.char_limit,
                permission: r.permission,
                pinned: r.pinned,
                loro_snapshot: r.loro_snapshot,
                content_preview: r.content_preview,
                metadata: r.metadata,
                embedding_model: r.embedding_model,
                is_active: r.is_active,
                frontier: r.frontier,
                last_seq: r.last_seq,
                created_at: r.created_at,
                updated_at: r.updated_at,
            };
            (block, r.attachment_permission, r.agent_name)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConstellationDb;
    use crate::models::Agent;

    async fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().await.unwrap()
    }

    async fn create_test_agent(db: &ConstellationDb, id: &str, name: &str) {
        use sqlx::types::Json;
        let agent = Agent {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "Test prompt".to_string(),
            config: Json(serde_json::json!({})),
            enabled_tools: Json(vec![]),
            tool_rules: None,
            status: crate::models::AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        crate::queries::create_agent(db.pool(), &agent)
            .await
            .unwrap();
    }

    async fn create_test_block(db: &ConstellationDb, id: &str, agent_id: &str) {
        let block = MemoryBlock {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            label: "test".to_string(),
            description: "Test block".to_string(),
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
        create_block(db.pool(), &block).await.unwrap();
    }

    #[tokio::test]
    async fn test_create_and_get_shared_attachment() {
        let db = setup_test_db().await;

        // Create test agents
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;

        // Create a test block
        create_test_block(&db, "block1", "agent1").await;

        // Create shared attachment
        create_shared_block_attachment(db.pool(), "block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();

        // Get the attachment
        let attachment = get_shared_block_attachment(db.pool(), "block1", "agent2")
            .await
            .unwrap();

        assert!(attachment.is_some());
        let att = attachment.unwrap();
        assert_eq!(att.block_id, "block1");
        assert_eq!(att.agent_id, "agent2");
        assert_eq!(att.permission, MemoryPermission::ReadOnly);
    }

    #[tokio::test]
    async fn test_delete_shared_attachment() {
        let db = setup_test_db().await;

        // Create test agents
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;

        // Create block and attachment
        create_test_block(&db, "block1", "agent1").await;
        create_shared_block_attachment(db.pool(), "block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();

        // Delete the attachment
        delete_shared_block_attachment(db.pool(), "block1", "agent2")
            .await
            .unwrap();

        // Verify it's gone
        let attachment = get_shared_block_attachment(db.pool(), "block1", "agent2")
            .await
            .unwrap();
        assert!(attachment.is_none());
    }

    #[tokio::test]
    async fn test_list_block_shared_agents() {
        let db = setup_test_db().await;

        // Create test agents
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;
        create_test_agent(&db, "agent3", "Agent 3").await;

        // Create block and share with multiple agents
        create_test_block(&db, "block1", "agent1").await;
        create_shared_block_attachment(db.pool(), "block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();
        create_shared_block_attachment(db.pool(), "block1", "agent3", MemoryPermission::ReadWrite)
            .await
            .unwrap();

        // List shared agents
        let mut agents = list_block_shared_agents(db.pool(), "block1").await.unwrap();
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].agent_id, "agent2");
        assert_eq!(agents[0].permission, MemoryPermission::ReadOnly);
        assert_eq!(agents[1].agent_id, "agent3");
        assert_eq!(agents[1].permission, MemoryPermission::ReadWrite);
    }

    #[tokio::test]
    async fn test_list_agent_shared_blocks() {
        let db = setup_test_db().await;

        // Create test agents
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;
        create_test_agent(&db, "agent3", "Agent 3").await;

        // Create multiple blocks and share with same agent
        create_test_block(&db, "block1", "agent1").await;
        create_test_block(&db, "block2", "agent2").await;

        create_shared_block_attachment(db.pool(), "block1", "agent3", MemoryPermission::ReadOnly)
            .await
            .unwrap();
        create_shared_block_attachment(db.pool(), "block2", "agent3", MemoryPermission::ReadWrite)
            .await
            .unwrap();

        // List blocks shared with agent3
        let mut blocks = list_agent_shared_blocks(db.pool(), "agent3").await.unwrap();
        blocks.sort_by(|a, b| a.block_id.cmp(&b.block_id));

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_id, "block1");
        assert_eq!(blocks[0].permission, MemoryPermission::ReadOnly);
        assert_eq!(blocks[1].block_id, "block2");
        assert_eq!(blocks[1].permission, MemoryPermission::ReadWrite);
    }

    #[tokio::test]
    async fn test_update_block_config() {
        let db = setup_test_db().await;

        // Create test agent (required FK).
        create_test_agent(&db, "test-agent", "Test Agent").await;

        // Create a block.
        create_test_block(&db, "test-block", "test-agent").await;

        // Update config fields.
        update_block_config(
            db.pool(),
            "test-block",
            Some(MemoryPermission::ReadOnly),
            Some(MemoryBlockType::Core),
            Some("Updated description"),
            Some(true), // pinned
            Some(8192), // char_limit
        )
        .await
        .unwrap();

        // Verify.
        let block = get_block(db.pool(), "test-block").await.unwrap().unwrap();
        assert_eq!(block.permission, MemoryPermission::ReadOnly);
        assert_eq!(block.block_type, MemoryBlockType::Core);
        assert_eq!(block.description, "Updated description");
        assert!(block.pinned);
        assert_eq!(block.char_limit, 8192);
    }

    #[tokio::test]
    async fn test_update_block_config_partial() {
        let db = setup_test_db().await;

        // Create test agent (required FK).
        create_test_agent(&db, "test-agent", "Test Agent").await;

        // Create a block.
        create_test_block(&db, "test-block", "test-agent").await;

        // Get original values.
        let original = get_block(db.pool(), "test-block").await.unwrap().unwrap();

        // Update only pinned field.
        update_block_config(
            db.pool(),
            "test-block",
            None,       // permission unchanged
            None,       // block_type unchanged
            None,       // description unchanged
            Some(true), // pinned = true
            None,       // char_limit unchanged
        )
        .await
        .unwrap();

        // Verify only pinned changed.
        let block = get_block(db.pool(), "test-block").await.unwrap().unwrap();
        assert_eq!(block.permission, original.permission);
        assert_eq!(block.block_type, original.block_type);
        assert_eq!(block.description, original.description);
        assert!(block.pinned); // This changed.
        assert_eq!(block.char_limit, original.char_limit);
    }
}
