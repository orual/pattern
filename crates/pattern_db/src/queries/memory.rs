// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Memory-related database queries.

use chrono::Utc;
use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{
    ArchivalEntry, MemoryBlock, MemoryBlockCheckpoint, MemoryBlockType, MemoryBlockUpdate,
    MemoryPermission, SharedBlockAttachment, UpdateStats,
};

// ============================================================================
// from_row implementations
// ============================================================================

impl MemoryBlock {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            label: row.get("label")?,
            description: row.get("description")?,
            block_type: row.get("block_type")?,
            char_limit: row.get("char_limit")?,
            permission: row.get("permission")?,
            pinned: row.get("pinned")?,
            loro_snapshot: row.get("loro_snapshot")?,
            content_preview: row.get("content_preview")?,
            metadata: row.get("metadata")?,
            embedding_model: row.get("embedding_model")?,
            is_active: row.get("is_active")?,
            frontier: row.get("frontier")?,
            last_seq: row.get("last_seq")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl MemoryBlockCheckpoint {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            block_id: row.get("block_id")?,
            snapshot: row.get("snapshot")?,
            created_at: row.get("created_at")?,
            updates_consolidated: row.get("updates_consolidated")?,
            frontier: row.get("frontier")?,
        })
    }
}

impl ArchivalEntry {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            content: row.get("content")?,
            metadata: row.get("metadata")?,
            chunk_index: row.get("chunk_index")?,
            parent_entry_id: row.get("parent_entry_id")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl SharedBlockAttachment {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            block_id: row.get("block_id")?,
            agent_id: row.get("agent_id")?,
            permission: row.get("permission")?,
            attached_at: row.get("attached_at")?,
        })
    }
}

impl MemoryBlockUpdate {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            block_id: row.get("block_id")?,
            seq: row.get("seq")?,
            update_blob: row.get("update_blob")?,
            byte_size: row.get("byte_size")?,
            source: row.get("source")?,
            frontier: row.get("frontier")?,
            is_active: row.get("is_active")?,
            created_at: row.get("created_at")?,
        })
    }
}

// ============================================================================
// Block queries
// ============================================================================

/// Get a memory block by ID.
pub fn get_block(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<MemoryBlock>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, label, description, block_type, char_limit,
                permission, pinned, loro_snapshot, content_preview, metadata,
                embedding_model, is_active, frontier, last_seq, created_at, updated_at
         FROM memory_blocks WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], MemoryBlock::from_row)
        .optional()?;
    Ok(result)
}

/// Get a memory block by agent ID and label.
pub fn get_block_by_label(
    conn: &rusqlite::Connection,
    agent_id: &str,
    label: &str,
) -> DbResult<Option<MemoryBlock>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, label, description, block_type, char_limit,
                permission, pinned, loro_snapshot, content_preview, metadata,
                embedding_model, is_active, frontier, last_seq, created_at, updated_at
         FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
    )?;
    let result = stmt
        .query_row(rusqlite::params![agent_id, label], MemoryBlock::from_row)
        .optional()?;
    Ok(result)
}

/// List all memory blocks for an agent.
pub fn list_blocks(conn: &rusqlite::Connection, agent_id: &str) -> DbResult<Vec<MemoryBlock>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, label, description, block_type, char_limit,
                permission, pinned, loro_snapshot, content_preview, metadata,
                embedding_model, is_active, frontier, last_seq, created_at, updated_at
         FROM memory_blocks WHERE agent_id = ?1 AND is_active = 1 ORDER BY label",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], MemoryBlock::from_row)?;
    let mut blocks = Vec::new();
    for row in rows {
        blocks.push(row?);
    }
    Ok(blocks)
}

/// List memory blocks by type.
pub fn list_blocks_by_type(
    conn: &rusqlite::Connection,
    agent_id: &str,
    block_type: MemoryBlockType,
) -> DbResult<Vec<MemoryBlock>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, label, description, block_type, char_limit,
                permission, pinned, loro_snapshot, content_preview, metadata,
                embedding_model, is_active, frontier, last_seq, created_at, updated_at
         FROM memory_blocks WHERE agent_id = ?1 AND block_type = ?2 AND is_active = 1 ORDER BY label",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![agent_id, block_type],
        MemoryBlock::from_row,
    )?;
    let mut blocks = Vec::new();
    for row in rows {
        blocks.push(row?);
    }
    Ok(blocks)
}

/// List all memory blocks in the database.
///
/// Used for constellation exports to capture all shared and owned blocks.
/// No agent_id filter - returns every active block.
pub fn list_all_blocks(conn: &rusqlite::Connection) -> DbResult<Vec<MemoryBlock>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, label, description, block_type, char_limit,
                permission, pinned, loro_snapshot, content_preview, metadata,
                embedding_model, is_active, frontier, last_seq, created_at, updated_at
         FROM memory_blocks WHERE is_active = 1 ORDER BY agent_id, label",
    )?;
    let rows = stmt.query_map([], MemoryBlock::from_row)?;
    let mut blocks = Vec::new();
    for row in rows {
        blocks.push(row?);
    }
    Ok(blocks)
}

/// List all shared block attachments in the database.
///
/// Used for constellation exports to capture all sharing relationships.
pub fn list_all_shared_block_attachments(
    conn: &rusqlite::Connection,
) -> DbResult<Vec<SharedBlockAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT block_id, agent_id, permission, attached_at
         FROM shared_block_agents",
    )?;
    let rows = stmt.query_map([], SharedBlockAttachment::from_row)?;
    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}

/// List memory blocks by label prefix (across all agents).
///
/// Used for system-level operations like restoring DataBlock source tracking
/// after restart. Finds all blocks whose labels start with the given prefix.
pub fn list_blocks_by_label_prefix(
    conn: &rusqlite::Connection,
    prefix: &str,
) -> DbResult<Vec<MemoryBlock>> {
    let pattern = format!("{prefix}%");
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, label, description, block_type, char_limit,
                permission, pinned, loro_snapshot, content_preview, metadata,
                embedding_model, is_active, frontier, last_seq, created_at, updated_at
         FROM memory_blocks WHERE label LIKE ?1 AND is_active = 1 ORDER BY label",
    )?;
    let rows = stmt.query_map(rusqlite::params![pattern], MemoryBlock::from_row)?;
    let mut blocks = Vec::new();
    for row in rows {
        blocks.push(row?);
    }
    Ok(blocks)
}

/// Create a new memory block.
pub fn create_block(conn: &rusqlite::Connection, block: &MemoryBlock) -> DbResult<()> {
    conn.execute(
        "INSERT INTO memory_blocks (id, agent_id, label, description, block_type, char_limit,
                                    permission, pinned, loro_snapshot, content_preview, metadata,
                                    embedding_model, is_active, frontier, last_seq, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
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
        ],
    )?;
    Ok(())
}

/// Create or replace a memory block by (agent_id, label).
///
/// If a block with the same (agent_id, label) exists, replaces it entirely.
/// Used by plugin skill installation where the plugin cache is authoritative.
pub fn create_or_replace_block(conn: &rusqlite::Connection, block: &MemoryBlock) -> DbResult<()> {
    // Delete any existing block with same (agent_id, label) first.
    conn.execute(
        "DELETE FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
        rusqlite::params![block.agent_id, block.label],
    )?;
    create_block(conn, block)
}

/// Create or update a memory block (upsert).
///
/// If a block with the same ID exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
///
/// Note: Callers must ensure no duplicate (agent_id, label) conflicts exist -
/// the importer handles this by tracking imported CIDs.
pub fn upsert_block(conn: &rusqlite::Connection, block: &MemoryBlock) -> DbResult<()> {
    conn.execute(
        "INSERT INTO memory_blocks (id, agent_id, label, description, block_type, char_limit,
                                    permission, pinned, loro_snapshot, content_preview, metadata,
                                    embedding_model, is_active, frontier, last_seq, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(id) DO UPDATE SET
             agent_id = excluded.agent_id,
             label = excluded.label,
             description = excluded.description,
             block_type = excluded.block_type,
             char_limit = excluded.char_limit,
             permission = excluded.permission,
             pinned = excluded.pinned,
             loro_snapshot = excluded.loro_snapshot,
             content_preview = excluded.content_preview,
             metadata = excluded.metadata,
             embedding_model = excluded.embedding_model,
             is_active = excluded.is_active,
             frontier = excluded.frontier,
             last_seq = excluded.last_seq,
             updated_at = excluded.updated_at",
        rusqlite::params![
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
        ],
    )?;
    Ok(())
}

/// Update a memory block's Loro snapshot and preview.
pub fn update_block_content(
    conn: &rusqlite::Connection,
    id: &str,
    loro_snapshot: &[u8],
    content_preview: Option<&str>,
) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks
         SET loro_snapshot = ?1, content_preview = ?2, updated_at = datetime('now')
         WHERE id = ?3",
        rusqlite::params![loro_snapshot, content_preview, id],
    )?;
    Ok(())
}

/// Update only a memory block's content preview without touching the snapshot.
///
/// Used by persist() to update the preview for quick lookups without
/// overwriting any existing snapshot data (e.g., from CAR imports).
pub fn update_block_preview(
    conn: &rusqlite::Connection,
    id: &str,
    content_preview: Option<&str>,
) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks
         SET content_preview = ?1, updated_at = datetime('now')
         WHERE id = ?2",
        rusqlite::params![content_preview, id],
    )?;
    Ok(())
}

/// Update a memory block's permission.
pub fn update_block_permission(
    conn: &rusqlite::Connection,
    id: &str,
    permission: MemoryPermission,
) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks SET permission = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![permission, id],
    )?;
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
pub fn update_block_config(
    conn: &mut rusqlite::Connection,
    id: &str,
    permission: Option<MemoryPermission>,
    block_type: Option<MemoryBlockType>,
    description: Option<&str>,
    pinned: Option<bool>,
    char_limit: Option<i64>,
) -> DbResult<()> {
    // Use a transaction to ensure atomicity between fetch and update.
    let tx = conn.transaction()?;

    // Fetch current values to use as defaults for unspecified fields.
    let current = {
        let mut stmt = tx.prepare(
            "SELECT id, agent_id, label, description, block_type, char_limit,
                    permission, pinned, loro_snapshot, content_preview, metadata,
                    embedding_model, is_active, frontier, last_seq, created_at, updated_at
             FROM memory_blocks WHERE id = ?1",
        )?;
        stmt.query_row(rusqlite::params![id], MemoryBlock::from_row)
            .optional()?
    };

    let Some(current) = current else {
        return Err(crate::error::DbError::not_found("memory block", id));
    };

    // Use provided values or fall back to current values.
    let perm = permission.unwrap_or(current.permission);
    let btype = block_type.unwrap_or(current.block_type);
    let desc = description.unwrap_or(&current.description);
    let pin = pinned.unwrap_or(current.pinned);
    let limit = char_limit.unwrap_or(current.char_limit);

    tx.execute(
        "UPDATE memory_blocks
         SET permission = ?1, block_type = ?2, description = ?3, pinned = ?4, char_limit = ?5, updated_at = datetime('now')
         WHERE id = ?6",
        rusqlite::params![perm, btype, desc, pin, limit, id],
    )?;

    tx.commit()?;
    Ok(())
}

/// Update a memory block's pinned flag.
///
/// Pinned blocks are always loaded into agent context while subscribed.
/// Unpinned (ephemeral) blocks only load when referenced by a notification.
pub fn update_block_pinned(conn: &rusqlite::Connection, id: &str, pinned: bool) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks SET pinned = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![pinned, id],
    )?;
    Ok(())
}

/// Rename a memory block by updating its label.
///
/// Note: This only updates the label in the database. The caller is responsible
/// for ensuring no other block with the same label exists for this agent.
pub fn update_block_label(conn: &rusqlite::Connection, id: &str, new_label: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks SET label = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![new_label, id],
    )?;
    Ok(())
}

/// Update a memory block's type.
///
/// Used for archiving blocks (changing Working -> Archival).
pub fn update_block_type(
    conn: &rusqlite::Connection,
    id: &str,
    block_type: MemoryBlockType,
) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks SET block_type = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![block_type, id],
    )?;
    Ok(())
}

/// Update a memory block's metadata.
///
/// Used for schema updates (e.g., changing viewport settings on Text blocks).
pub fn update_block_metadata(
    conn: &rusqlite::Connection,
    id: &str,
    metadata: &serde_json::Value,
) -> DbResult<()> {
    let metadata_str = serde_json::to_string(metadata)?;
    conn.execute(
        "UPDATE memory_blocks SET metadata = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![metadata_str, id],
    )?;
    Ok(())
}

/// Soft-delete a memory block.
pub fn deactivate_block(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE memory_blocks SET is_active = 0, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Reactivate a soft-deleted memory block in place, replacing all metadata
/// fields with values from `block` while preserving the row's primary key.
///
/// Used by `MemoryCache::create_block` when a `BlockCreate` request targets
/// a label whose previous block was soft-deleted: rather than failing with
/// a UNIQUE conflict on `(agent_id, label)`, we reuse the existing row,
/// flip `is_active` back to true, and overwrite metadata + content. This
/// makes `Memory.delete` followed by `Memory.create` with the same label
/// idempotent from the caller's perspective.
///
/// Returns the number of rows updated (0 if `id` doesn't exist or was
/// already active — caller should check via `get_block_by_label` first).
pub fn reactivate_block(
    conn: &rusqlite::Connection,
    id: &str,
    block: &MemoryBlock,
) -> DbResult<usize> {
    let updated = conn.execute(
        "UPDATE memory_blocks SET
            agent_id = ?2,
            label = ?3,
            description = ?4,
            block_type = ?5,
            char_limit = ?6,
            permission = ?7,
            pinned = ?8,
            loro_snapshot = ?9,
            content_preview = ?10,
            metadata = ?11,
            embedding_model = ?12,
            is_active = 1,
            frontier = ?13,
            last_seq = ?14,
            updated_at = ?15
         WHERE id = ?1",
        rusqlite::params![
            id,
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
            block.frontier,
            block.last_seq,
            block.updated_at,
        ],
    )?;
    Ok(updated)
}


/// Create a checkpoint for a memory block.
pub fn create_checkpoint(
    conn: &rusqlite::Connection,
    checkpoint: &MemoryBlockCheckpoint,
) -> DbResult<i64> {
    conn.execute(
        "INSERT INTO memory_block_checkpoints (block_id, snapshot, created_at, updates_consolidated, frontier)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            checkpoint.block_id,
            checkpoint.snapshot,
            checkpoint.created_at,
            checkpoint.updates_consolidated,
            checkpoint.frontier,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get the latest checkpoint for a block.
pub fn get_latest_checkpoint(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<Option<MemoryBlockCheckpoint>> {
    let mut stmt = conn.prepare(
        "SELECT id, block_id, snapshot, created_at, updates_consolidated, frontier
         FROM memory_block_checkpoints WHERE block_id = ?1 ORDER BY created_at DESC LIMIT 1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![block_id], MemoryBlockCheckpoint::from_row)
        .optional()?;
    Ok(result)
}

/// Get an archival entry by ID.
pub fn get_archival_entry(
    conn: &rusqlite::Connection,
    id: &str,
) -> DbResult<Option<ArchivalEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at
         FROM archival_entries WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], ArchivalEntry::from_row)
        .optional()?;
    Ok(result)
}

/// List archival entries for an agent.
pub fn list_archival_entries(
    conn: &rusqlite::Connection,
    agent_id: &str,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<ArchivalEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at
         FROM archival_entries WHERE agent_id = ?1 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![agent_id, limit, offset],
        ArchivalEntry::from_row,
    )?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Create a new archival entry.
pub fn create_archival_entry(conn: &rusqlite::Connection, entry: &ArchivalEntry) -> DbResult<()> {
    conn.execute(
        "INSERT INTO archival_entries (id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            entry.id,
            entry.agent_id,
            entry.content,
            entry.metadata,
            entry.chunk_index,
            entry.parent_entry_id,
            entry.created_at,
        ],
    )?;
    Ok(())
}

/// Create or update an archival entry (upsert).
///
/// If an entry with the same ID exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
pub fn upsert_archival_entry(conn: &rusqlite::Connection, entry: &ArchivalEntry) -> DbResult<()> {
    conn.execute(
        "INSERT INTO archival_entries (id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
             agent_id = excluded.agent_id,
             content = excluded.content,
             metadata = excluded.metadata,
             chunk_index = excluded.chunk_index,
             parent_entry_id = excluded.parent_entry_id",
        rusqlite::params![
            entry.id,
            entry.agent_id,
            entry.content,
            entry.metadata,
            entry.chunk_index,
            entry.parent_entry_id,
            entry.created_at,
        ],
    )?;
    Ok(())
}

/// Delete an archival entry.
pub fn delete_archival_entry(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    conn.execute(
        "DELETE FROM archival_entries WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Count archival entries for an agent.
pub fn count_archival_entries(conn: &rusqlite::Connection, agent_id: &str) -> DbResult<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM archival_entries WHERE agent_id = ?1",
        rusqlite::params![agent_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

// ============================================================================
// Memory Block Updates (Delta Storage)
// ============================================================================

/// Store a new incremental update for a memory block.
///
/// Atomically assigns the next sequence number and persists the update.
/// The `frontier` parameter stores the Loro version vector after this update,
/// enabling precise undo to any historical state.
/// Returns the assigned sequence number.
pub fn store_update(
    conn: &mut rusqlite::Connection,
    block_id: &str,
    update_blob: &[u8],
    frontier: Option<&[u8]>,
    source: Option<&str>,
) -> DbResult<i64> {
    let now = Utc::now();
    let byte_size = update_blob.len() as i64;

    // Use a transaction to atomically increment last_seq and insert.
    let tx = conn.transaction()?;

    // Get and increment the sequence number.
    let seq: i64 = tx.query_row(
        "UPDATE memory_blocks SET last_seq = last_seq + 1, updated_at = ?1 WHERE id = ?2 RETURNING last_seq",
        rusqlite::params![now, block_id],
        |r| r.get(0),
    )?;

    // Insert the update.
    tx.execute(
        "INSERT INTO memory_block_updates (block_id, seq, update_blob, byte_size, source, frontier, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![block_id, seq, update_blob, byte_size, source, frontier, now],
    )?;

    tx.commit()?;
    Ok(seq)
}

/// Get the latest checkpoint and all pending updates for a block.
///
/// Used for full reconstruction on cache miss.
pub fn get_checkpoint_and_updates(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<(Option<MemoryBlockCheckpoint>, Vec<MemoryBlockUpdate>)> {
    // Get latest checkpoint.
    let checkpoint = get_latest_checkpoint(conn, block_id)?;

    // Get all active updates (or updates since checkpoint if we have one).
    let updates = if let Some(ref cp) = checkpoint {
        // Get active updates created after the checkpoint.
        let mut stmt = conn.prepare(
            "SELECT id, block_id, seq, update_blob, byte_size, source, frontier, is_active, created_at
             FROM memory_block_updates
             WHERE block_id = ?1 AND created_at > ?2 AND is_active = 1
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![block_id, cp.created_at],
            MemoryBlockUpdate::from_row,
        )?;
        let mut updates = Vec::new();
        for row in rows {
            updates.push(row?);
        }
        updates
    } else {
        // No checkpoint, get all active updates.
        let mut stmt = conn.prepare(
            "SELECT id, block_id, seq, update_blob, byte_size, source, frontier, is_active, created_at
             FROM memory_block_updates
             WHERE block_id = ?1 AND is_active = 1
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![block_id], MemoryBlockUpdate::from_row)?;
        let mut updates = Vec::new();
        for row in rows {
            updates.push(row?);
        }
        updates
    };

    Ok((checkpoint, updates))
}

/// Get active updates after a given sequence number.
///
/// Used for cache refresh when we already have some state.
pub fn get_updates_since(
    conn: &rusqlite::Connection,
    block_id: &str,
    after_seq: i64,
) -> DbResult<Vec<MemoryBlockUpdate>> {
    let mut stmt = conn.prepare(
        "SELECT id, block_id, seq, update_blob, byte_size, source, frontier, is_active, created_at
         FROM memory_block_updates
         WHERE block_id = ?1 AND seq > ?2 AND is_active = 1
         ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![block_id, after_seq],
        MemoryBlockUpdate::from_row,
    )?;
    let mut updates = Vec::new();
    for row in rows {
        updates.push(row?);
    }
    Ok(updates)
}

/// Check if there are updates after a given sequence number.
///
/// Lightweight check without fetching update data.
pub fn has_updates_since(
    conn: &rusqlite::Connection,
    block_id: &str,
    after_seq: i64,
) -> DbResult<bool> {
    let has: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_block_updates WHERE block_id = ?1 AND seq > ?2)",
        rusqlite::params![block_id, after_seq],
        |r| r.get(0),
    )?;
    Ok(has)
}

/// Atomically consolidate updates into a new checkpoint.
///
/// Creates a new checkpoint with the merged state and deletes updates up to `up_to_seq`.
/// Updates arriving during the merge (with seq > up_to_seq) are preserved.
pub fn consolidate_checkpoint(
    conn: &mut rusqlite::Connection,
    block_id: &str,
    new_snapshot: &[u8],
    new_frontier: Option<&[u8]>,
    up_to_seq: i64,
) -> DbResult<()> {
    let now = Utc::now();

    let tx = conn.transaction()?;

    // Count updates being consolidated.
    let updates_consolidated: i64 = tx.query_row(
        "SELECT COUNT(*) FROM memory_block_updates WHERE block_id = ?1 AND seq <= ?2",
        rusqlite::params![block_id, up_to_seq],
        |r| r.get(0),
    )?;

    // Create new checkpoint.
    tx.execute(
        "INSERT INTO memory_block_checkpoints (block_id, snapshot, created_at, updates_consolidated, frontier)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![block_id, new_snapshot, now, updates_consolidated, new_frontier],
    )?;

    // Delete consolidated updates.
    tx.execute(
        "DELETE FROM memory_block_updates WHERE block_id = ?1 AND seq <= ?2",
        rusqlite::params![block_id, up_to_seq],
    )?;

    // Update the block's loro_snapshot and frontier.
    tx.execute(
        "UPDATE memory_blocks
         SET loro_snapshot = ?1, frontier = ?2, updated_at = ?3
         WHERE id = ?4",
        rusqlite::params![new_snapshot, new_frontier, now, block_id],
    )?;

    tx.commit()?;
    Ok(())
}

/// Get statistics about pending updates for consolidation decisions.
pub fn get_pending_update_stats(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<UpdateStats> {
    let result = conn.query_row(
        "SELECT
             COUNT(*) as count,
             COALESCE(SUM(byte_size), 0) as total_bytes,
             COALESCE(MAX(seq), 0) as max_seq
         FROM memory_block_updates
         WHERE block_id = ?1",
        rusqlite::params![block_id],
        |row| {
            Ok(UpdateStats {
                count: row.get(0)?,
                total_bytes: row.get(1)?,
                max_seq: row.get(2)?,
            })
        },
    )?;
    Ok(result)
}

// ============================================================================
// Undo Support Queries
// ============================================================================

/// Get the most recent active update for a block.
///
/// Returns None if no active updates exist.
pub fn get_latest_update(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<Option<MemoryBlockUpdate>> {
    let mut stmt = conn.prepare(
        "SELECT id, block_id, seq, update_blob, byte_size, source, frontier, is_active, created_at
         FROM memory_block_updates
         WHERE block_id = ?1 AND is_active = 1
         ORDER BY seq DESC
         LIMIT 1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![block_id], MemoryBlockUpdate::from_row)
        .optional()?;
    Ok(result)
}

/// Get checkpoint and active updates up to (inclusive) a sequence number.
///
/// Used for reconstructing document state at a specific point in history.
/// Returns the latest checkpoint that precedes the target seq, plus all
/// active updates from checkpoint up to and including target_seq.
pub fn get_checkpoint_and_updates_until(
    conn: &rusqlite::Connection,
    block_id: &str,
    max_seq: i64,
) -> DbResult<(Option<MemoryBlockCheckpoint>, Vec<MemoryBlockUpdate>)> {
    // Get latest checkpoint.
    let checkpoint = get_latest_checkpoint(conn, block_id)?;

    // Get active updates up to max_seq (from checkpoint if exists, otherwise from beginning).
    let updates = if let Some(ref cp) = checkpoint {
        let mut stmt = conn.prepare(
            "SELECT id, block_id, seq, update_blob, byte_size, source, frontier, is_active, created_at
             FROM memory_block_updates
             WHERE block_id = ?1 AND created_at > ?2 AND seq <= ?3 AND is_active = 1
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![block_id, cp.created_at, max_seq],
            MemoryBlockUpdate::from_row,
        )?;
        let mut updates = Vec::new();
        for row in rows {
            updates.push(row?);
        }
        updates
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, block_id, seq, update_blob, byte_size, source, frontier, is_active, created_at
             FROM memory_block_updates
             WHERE block_id = ?1 AND seq <= ?2 AND is_active = 1
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![block_id, max_seq],
            MemoryBlockUpdate::from_row,
        )?;
        let mut updates = Vec::new();
        for row in rows {
            updates.push(row?);
        }
        updates
    };

    Ok((checkpoint, updates))
}

/// Deactivate the latest active update for a block (undo).
///
/// Marks the most recent active update as inactive, effectively undoing it.
/// Returns the seq of the deactivated update, or None if no active updates.
pub fn deactivate_latest_update(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<Option<i64>> {
    // Find the latest active update.
    let latest: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, seq FROM memory_block_updates
             WHERE block_id = ?1 AND is_active = 1
             ORDER BY seq DESC
             LIMIT 1",
            rusqlite::params![block_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let Some((id, seq)) = latest else {
        return Ok(None);
    };

    // Mark it as inactive.
    conn.execute(
        "UPDATE memory_block_updates SET is_active = 0 WHERE id = ?1",
        rusqlite::params![id],
    )?;

    Ok(Some(seq))
}

/// Reactivate the next inactive update for a block (redo).
///
/// Finds the first inactive update after the current active branch and reactivates it.
/// Returns the seq of the reactivated update, or None if nothing to redo.
pub fn reactivate_next_update(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<Option<i64>> {
    // Get the max active seq (or 0 if none).
    let max_active_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM memory_block_updates
         WHERE block_id = ?1 AND is_active = 1",
        rusqlite::params![block_id],
        |r| r.get(0),
    )?;

    // Find the first inactive update after max_active_seq.
    let next_inactive: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, seq FROM memory_block_updates
             WHERE block_id = ?1 AND is_active = 0 AND seq > ?2
             ORDER BY seq ASC
             LIMIT 1",
            rusqlite::params![block_id, max_active_seq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let Some((id, seq)) = next_inactive else {
        return Ok(None);
    };

    // Mark it as active.
    conn.execute(
        "UPDATE memory_block_updates SET is_active = 1 WHERE id = ?1",
        rusqlite::params![id],
    )?;

    Ok(Some(seq))
}

/// Count available undo steps for a block.
///
/// Returns the number of active updates that can be undone.
pub fn count_undo_steps(conn: &rusqlite::Connection, block_id: &str) -> DbResult<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_block_updates WHERE block_id = ?1 AND is_active = 1",
        rusqlite::params![block_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Count available redo steps for a block.
///
/// Returns the number of inactive updates after the active branch that can be redone.
pub fn count_redo_steps(conn: &rusqlite::Connection, block_id: &str) -> DbResult<i64> {
    // Get max active seq.
    let max_active_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM memory_block_updates
         WHERE block_id = ?1 AND is_active = 1",
        rusqlite::params![block_id],
        |r| r.get(0),
    )?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_block_updates WHERE block_id = ?1 AND is_active = 0 AND seq > ?2",
        rusqlite::params![block_id, max_active_seq],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Reset a block's last_seq to a specific value.
///
/// Used after undo to sync the sequence counter with the actual update history.
pub fn reset_block_last_seq(
    conn: &rusqlite::Connection,
    block_id: &str,
    new_seq: i64,
) -> DbResult<()> {
    let now = Utc::now();
    conn.execute(
        "UPDATE memory_blocks SET last_seq = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![new_seq, now, block_id],
    )?;
    Ok(())
}

/// Update a block's frontier without creating an update record.
///
/// Used when applying updates from external sources where we just need to track version.
pub fn update_block_frontier(
    conn: &rusqlite::Connection,
    block_id: &str,
    frontier: &[u8],
) -> DbResult<()> {
    let now = Utc::now();
    conn.execute(
        "UPDATE memory_blocks SET frontier = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![frontier, now, block_id],
    )?;
    Ok(())
}

/// Get a lightweight view of a block for cache lookups.
///
/// Returns just the ID and last_seq without loading the full snapshot.
pub fn get_block_version_info(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<Option<(String, i64)>> {
    let result = conn
        .query_row(
            "SELECT id, last_seq FROM memory_blocks WHERE id = ?1",
            rusqlite::params![block_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    Ok(result)
}

// ============================================================================
// Shared Block Management
// ============================================================================

/// Create a shared block attachment.
///
/// Grants an agent access to a block with specific permissions.
/// If the attachment already exists, updates the permission and timestamp.
pub fn create_shared_block_attachment(
    conn: &rusqlite::Connection,
    block_id: &str,
    agent_id: &str,
    permission: MemoryPermission,
) -> DbResult<()> {
    let now = Utc::now();
    conn.execute(
        "INSERT INTO shared_block_agents (block_id, agent_id, permission, attached_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(block_id, agent_id) DO UPDATE SET
             permission = excluded.permission,
             attached_at = excluded.attached_at",
        rusqlite::params![block_id, agent_id, permission, now],
    )?;
    Ok(())
}

/// Delete a shared block attachment.
///
/// Removes an agent's access to a shared block.
pub fn delete_shared_block_attachment(
    conn: &rusqlite::Connection,
    block_id: &str,
    agent_id: &str,
) -> DbResult<()> {
    conn.execute(
        "DELETE FROM shared_block_agents WHERE block_id = ?1 AND agent_id = ?2",
        rusqlite::params![block_id, agent_id],
    )?;
    Ok(())
}

/// List all agents a block is shared with.
///
/// Returns all shared attachments for a given block.
pub fn list_block_shared_agents(
    conn: &rusqlite::Connection,
    block_id: &str,
) -> DbResult<Vec<SharedBlockAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT block_id, agent_id, permission, attached_at
         FROM shared_block_agents WHERE block_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![block_id], SharedBlockAttachment::from_row)?;
    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}

/// List all blocks shared with an agent.
///
/// Returns all shared attachments for a given agent.
pub fn list_agent_shared_blocks(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<SharedBlockAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT block_id, agent_id, permission, attached_at
         FROM shared_block_agents WHERE agent_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], SharedBlockAttachment::from_row)?;
    let mut attachments = Vec::new();
    for row in rows {
        attachments.push(row?);
    }
    Ok(attachments)
}

/// Get a specific shared attachment.
///
/// Checks if an agent has access to a specific block and returns the attachment details.
pub fn get_shared_block_attachment(
    conn: &rusqlite::Connection,
    block_id: &str,
    agent_id: &str,
) -> DbResult<Option<SharedBlockAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT block_id, agent_id, permission, attached_at
         FROM shared_block_agents WHERE block_id = ?1 AND agent_id = ?2",
    )?;
    let result = stmt
        .query_row(
            rusqlite::params![block_id, agent_id],
            SharedBlockAttachment::from_row,
        )
        .optional()?;
    Ok(result)
}

/// Check if a requester has access to a specific block and return the permission level.
///
/// This is an efficient single-query check that handles both owned and shared blocks.
/// Returns (block_id, effective_permission):
/// - If the requester owns the block: returns the block's inherent permission
/// - If the requester has shared access: returns the shared permission
/// - If no access: returns None
pub fn check_block_access(
    conn: &rusqlite::Connection,
    requester_agent_id: &str,
    owner_agent_id: &str,
    label: &str,
) -> DbResult<Option<(String, MemoryPermission)>> {
    // First check if requester owns the block.
    if requester_agent_id == owner_agent_id {
        // Owned block - get inherent permission.
        let block = get_block_by_label(conn, owner_agent_id, label)?;
        return Ok(block.map(|b| (b.id, b.permission)));
    }

    // Check for shared access.
    // Join to ensure the block exists and is active.
    let result: Option<(String, MemoryPermission)> = conn
        .query_row(
            "SELECT mb.id, sba.permission
             FROM shared_block_agents sba
             INNER JOIN memory_blocks mb ON sba.block_id = mb.id
             WHERE sba.agent_id = ?1
               AND mb.agent_id = ?2
               AND mb.label = ?3
               AND mb.is_active = 1",
            rusqlite::params![requester_agent_id, owner_agent_id, label],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    Ok(result)
}

/// Get all shared blocks for an agent with full block data.
///
/// Returns tuples of (MemoryBlock, MemoryPermission, Option<owner_name>) where the permission
/// is from the shared_block_agents table. Only returns active blocks.
/// The owner_name is looked up from the agents table (may be None if agent doesn't exist).
pub fn get_shared_blocks(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<(MemoryBlock, MemoryPermission, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT
             mb.id, mb.agent_id, a.name AS agent_name,
             mb.label, mb.description, mb.block_type, mb.char_limit,
             mb.permission, mb.pinned, mb.loro_snapshot, mb.content_preview,
             mb.metadata, mb.embedding_model, mb.is_active, mb.frontier,
             mb.last_seq, mb.created_at, mb.updated_at,
             sba.permission AS attachment_permission
         FROM shared_block_agents sba
         INNER JOIN memory_blocks mb ON sba.block_id = mb.id
         LEFT JOIN agents a ON mb.agent_id = a.id
         WHERE sba.agent_id = ?1 AND mb.is_active = 1
         ORDER BY mb.label",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], |row| {
        let block = MemoryBlock {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            label: row.get("label")?,
            description: row.get("description")?,
            block_type: row.get("block_type")?,
            char_limit: row.get("char_limit")?,
            permission: row.get("permission")?,
            pinned: row.get("pinned")?,
            loro_snapshot: row.get("loro_snapshot")?,
            content_preview: row.get("content_preview")?,
            metadata: row.get("metadata")?,
            embedding_model: row.get("embedding_model")?,
            is_active: row.get("is_active")?,
            frontier: row.get("frontier")?,
            last_seq: row.get("last_seq")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        };
        let attachment_permission: MemoryPermission = row.get("attachment_permission")?;
        let agent_name: Option<String> = row.get("agent_name")?;
        Ok((block, attachment_permission, agent_name))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConstellationDb;
    use crate::models::Agent;

    fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().unwrap()
    }

    fn create_test_agent(conn: &rusqlite::Connection, id: &str, name: &str) {
        let agent = Agent {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "Test prompt".to_string(),
            config: crate::Json(serde_json::json!({})),
            enabled_tools: crate::Json(vec![]),
            tool_rules: None,
            status: crate::models::AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        crate::queries::create_agent(conn, &agent).unwrap();
    }

    fn create_test_block(conn: &rusqlite::Connection, id: &str, agent_id: &str) {
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
        create_block(conn, &block).unwrap();
    }

    #[test]
    fn test_create_and_get_shared_attachment() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent1", "Agent 1");
        create_test_agent(&conn, "agent2", "Agent 2");
        create_test_block(&conn, "block1", "agent1");

        create_shared_block_attachment(&conn, "block1", "agent2", MemoryPermission::ReadOnly)
            .unwrap();

        let attachment = get_shared_block_attachment(&conn, "block1", "agent2").unwrap();
        assert!(attachment.is_some());
        let att = attachment.unwrap();
        assert_eq!(att.block_id, "block1");
        assert_eq!(att.agent_id, "agent2");
        assert_eq!(att.permission, MemoryPermission::ReadOnly);
    }

    #[test]
    fn test_delete_shared_attachment() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent1", "Agent 1");
        create_test_agent(&conn, "agent2", "Agent 2");
        create_test_block(&conn, "block1", "agent1");

        create_shared_block_attachment(&conn, "block1", "agent2", MemoryPermission::ReadOnly)
            .unwrap();
        delete_shared_block_attachment(&conn, "block1", "agent2").unwrap();

        let attachment = get_shared_block_attachment(&conn, "block1", "agent2").unwrap();
        assert!(attachment.is_none());
    }

    #[test]
    fn test_list_block_shared_agents() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent1", "Agent 1");
        create_test_agent(&conn, "agent2", "Agent 2");
        create_test_agent(&conn, "agent3", "Agent 3");
        create_test_block(&conn, "block1", "agent1");

        create_shared_block_attachment(&conn, "block1", "agent2", MemoryPermission::ReadOnly)
            .unwrap();
        create_shared_block_attachment(&conn, "block1", "agent3", MemoryPermission::ReadWrite)
            .unwrap();

        let mut agents = list_block_shared_agents(&conn, "block1").unwrap();
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].agent_id, "agent2");
        assert_eq!(agents[0].permission, MemoryPermission::ReadOnly);
        assert_eq!(agents[1].agent_id, "agent3");
        assert_eq!(agents[1].permission, MemoryPermission::ReadWrite);
    }

    #[test]
    fn test_list_agent_shared_blocks() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        create_test_agent(&conn, "agent1", "Agent 1");
        create_test_agent(&conn, "agent2", "Agent 2");
        create_test_agent(&conn, "agent3", "Agent 3");
        create_test_block(&conn, "block1", "agent1");
        create_test_block(&conn, "block2", "agent2");

        create_shared_block_attachment(&conn, "block1", "agent3", MemoryPermission::ReadOnly)
            .unwrap();
        create_shared_block_attachment(&conn, "block2", "agent3", MemoryPermission::ReadWrite)
            .unwrap();

        let mut blocks = list_agent_shared_blocks(&conn, "agent3").unwrap();
        blocks.sort_by(|a, b| a.block_id.cmp(&b.block_id));

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_id, "block1");
        assert_eq!(blocks[0].permission, MemoryPermission::ReadOnly);
        assert_eq!(blocks[1].block_id, "block2");
        assert_eq!(blocks[1].permission, MemoryPermission::ReadWrite);
    }

    #[test]
    fn test_update_block_config() {
        let db = setup_test_db();
        let mut conn = db.get().unwrap();

        create_test_agent(&conn, "test-agent", "Test Agent");
        create_test_block(&conn, "test-block", "test-agent");

        update_block_config(
            &mut conn,
            "test-block",
            Some(MemoryPermission::ReadOnly),
            Some(MemoryBlockType::Core),
            Some("Updated description"),
            Some(true),
            Some(8192),
        )
        .unwrap();

        let block = get_block(&conn, "test-block").unwrap().unwrap();
        assert_eq!(block.permission, MemoryPermission::ReadOnly);
        assert_eq!(block.block_type, MemoryBlockType::Core);
        assert_eq!(block.description, "Updated description");
        assert!(block.pinned);
        assert_eq!(block.char_limit, 8192);
    }

    #[test]
    fn test_update_block_config_partial() {
        let db = setup_test_db();
        let mut conn = db.get().unwrap();

        create_test_agent(&conn, "test-agent", "Test Agent");
        create_test_block(&conn, "test-block", "test-agent");

        let original = get_block(&conn, "test-block").unwrap().unwrap();

        update_block_config(&mut conn, "test-block", None, None, None, Some(true), None).unwrap();

        let block = get_block(&conn, "test-block").unwrap().unwrap();
        assert_eq!(block.permission, original.permission);
        assert_eq!(block.block_type, original.block_type);
        assert_eq!(block.description, original.description);
        assert!(block.pinned);
        assert_eq!(block.char_limit, original.char_limit);
    }
}
