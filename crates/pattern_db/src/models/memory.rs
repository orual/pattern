//! Memory-related models.

use crate::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A memory block belonging to an agent.
///
/// Memory blocks are stored as Loro CRDT documents, enabling versioning,
/// time-travel, and potential future merging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlock {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// Semantic label: "persona", "human", "scratchpad", etc.
    pub label: String,

    /// Description for the LLM (critical for proper usage)
    pub description: String,

    /// Block type determines context inclusion behavior
    pub block_type: MemoryBlockType,

    /// Character limit for the block
    pub char_limit: i64,

    /// Permission level for this block
    pub permission: MemoryPermission,

    /// Whether this block is pinned (can't be swapped out of context)
    pub pinned: bool,

    /// Loro document snapshot (binary blob)
    pub loro_snapshot: Vec<u8>,

    /// Quick content preview without deserializing Loro
    pub content_preview: Option<String>,

    /// Additional metadata
    pub metadata: Option<Json<serde_json::Value>>,

    /// Embedding model used (if embedded)
    pub embedding_model: Option<String>,

    /// Whether this block is active (false = soft deleted)
    pub is_active: bool,

    /// Loro frontier for version tracking (serialized)
    pub frontier: Option<Vec<u8>>,

    /// Last assigned sequence number for updates
    pub last_seq: i64,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

// Domain enums imported from pattern_core (canonical definitions).
// Re-exported here for backward compatibility with existing `pattern_db::models::*` imports.
pub use pattern_core::types::memory_types::{
    MemoryBlockType, MemoryGate, MemoryOp, MemoryPermission,
};

/// Checkpoint of a memory block (for history/rollback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockCheckpoint {
    /// Auto-incrementing ID
    pub id: i64,

    /// Block this checkpoint belongs to
    pub block_id: String,

    /// Full Loro snapshot at this checkpoint
    pub snapshot: Vec<u8>,

    /// When this checkpoint was created
    pub created_at: DateTime<Utc>,

    /// How many updates were consolidated into this checkpoint
    pub updates_consolidated: i64,

    /// Loro frontier at this checkpoint (for version tracking)
    pub frontier: Option<Vec<u8>>,
}

/// An archival memory entry.
///
/// Separate from blocks - these are individual searchable entries
/// the agent can store/retrieve. Useful for fine-grained memories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivalEntry {
    /// Unique identifier
    pub id: String,

    /// Owning agent ID
    pub agent_id: String,

    /// Content of the entry
    pub content: String,

    /// Optional structured metadata
    pub metadata: Option<Json<serde_json::Value>>,

    /// For chunked large content
    pub chunk_index: i64,

    /// Links chunks together
    pub parent_entry_id: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// Shared block attachment (when blocks are shared between agents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedBlockAttachment {
    /// The shared block
    pub block_id: String,

    /// Agent gaining access
    pub agent_id: String,

    /// Permission level for this attachment (may differ from block's inherent permission)
    pub permission: MemoryPermission,

    /// When the attachment was created
    pub attached_at: DateTime<Utc>,
}

/// An incremental update to a memory block.
///
/// Updates are Loro deltas stored between checkpoints. On read, the checkpoint
/// is loaded and active updates are applied in seq order to reconstruct current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockUpdate {
    /// Auto-incrementing ID
    pub id: i64,

    /// Block this update belongs to
    pub block_id: String,

    /// Sequence number within the block (monotonically increasing)
    pub seq: i64,

    /// Loro update blob (delta)
    pub update_blob: Vec<u8>,

    /// Size of update_blob in bytes (for consolidation decisions)
    pub byte_size: i64,

    /// Source of this update
    pub source: Option<String>,

    /// Loro frontier after this update (for undo support)
    pub frontier: Option<Vec<u8>>,

    /// Whether this update is on the active branch (for undo/redo)
    pub is_active: bool,

    /// When this update was created
    pub created_at: DateTime<Utc>,
}

/// Update source types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateSource {
    /// Update from agent action
    Agent,
    /// Update from sync with another instance
    Sync,
    /// Update from v1->v2 migration
    Migration,
    /// Manual update (user/admin)
    Manual,
}

impl UpdateSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Sync => "sync",
            Self::Migration => "migration",
            Self::Manual => "manual",
        }
    }
}

impl std::fmt::Display for UpdateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Statistics about pending updates for a block.
///
/// Used for consolidation decisions (e.g., consolidate when count > N or bytes > M).
#[derive(Debug, Clone, Default)]
pub struct UpdateStats {
    /// Number of pending updates
    pub count: i64,
    /// Total bytes of all pending updates
    pub total_bytes: i64,
    /// Highest seq number (or 0 if no updates)
    pub max_seq: i64,
}
