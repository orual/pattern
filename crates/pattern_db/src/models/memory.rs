//! Memory-related models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::Json;

/// A memory block belonging to an agent.
///
/// Memory blocks are stored as Loro CRDT documents, enabling versioning,
/// time-travel, and potential future merging.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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

/// Memory block types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum MemoryBlockType {
    /// Always in context, critical for agent identity
    /// Examples: persona, human, system guidelines
    Core,

    /// Working memory, can be swapped in/out based on relevance
    /// Examples: scratchpad, current_task, session_notes
    Working,

    /// Long-term storage, NOT in context by default
    /// Retrieved via recall/search tools using semantic search
    Archival,

    /// System-maintained logs (read-only to agent)
    /// Recent entries shown in context, older entries searchable
    Log,
}

impl Default for MemoryBlockType {
    fn default() -> Self {
        Self::Working
    }
}

impl MemoryBlockType {
    /// Returns the lowercase string representation matching the database format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Working => "working",
            Self::Archival => "archival",
            Self::Log => "log",
        }
    }
}

/// Permission levels for memory operations.
///
/// Ordered from most restrictive to least restrictive.
/// This determines what operations an agent can perform on a block.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MemoryPermission {
    /// Can only read, no modifications allowed
    ReadOnly,
    /// Requires permission from partner (owner) to write
    Partner,
    /// Requires permission from any human to write
    Human,
    /// Can append to existing content, but not overwrite
    Append,
    /// Can modify content freely (default)
    ReadWrite,
    /// Total control, including delete
    Admin,
}

impl Default for MemoryPermission {
    fn default() -> Self {
        Self::ReadWrite
    }
}

impl MemoryPermission {
    /// Returns the snake_case string representation matching the database format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Partner => "partner",
            Self::Human => "human",
            Self::Append => "append",
            Self::ReadWrite => "read_write",
            Self::Admin => "admin",
        }
    }
}

impl std::fmt::Display for MemoryPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "Read Only"),
            Self::Partner => write!(f, "Requires Partner permission to write"),
            Self::Human => write!(f, "Requires Human permission to write"),
            Self::Append => write!(f, "Append Only"),
            Self::ReadWrite => write!(f, "Read, Append, Write"),
            Self::Admin => write!(f, "Read, Write, Delete"),
        }
    }
}

/// Memory operation types for permission gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOp {
    Read,
    Append,
    Overwrite,
    Delete,
}

/// Result of permission check for a memory operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryGate {
    /// Operation can proceed without additional consent.
    Allow,
    /// Operation may proceed with human/partner consent.
    RequireConsent { reason: String },
    /// Operation is not allowed under current policy.
    Deny { reason: String },
}

impl MemoryGate {
    /// Check whether an operation is allowed under a permission level.
    ///
    /// Policy:
    /// - Read: always allowed
    /// - Append: allowed for Append/ReadWrite/Admin; Human/Partner require consent; ReadOnly denied
    /// - Overwrite: allowed for ReadWrite/Admin; Human/Partner require consent; ReadOnly/Append denied
    /// - Delete: allowed for Admin only; others denied
    pub fn check(op: MemoryOp, perm: MemoryPermission) -> Self {
        match op {
            MemoryOp::Read => Self::Allow,
            MemoryOp::Append => match perm {
                MemoryPermission::Append
                | MemoryPermission::ReadWrite
                | MemoryPermission::Admin => Self::Allow,
                MemoryPermission::Human => Self::RequireConsent {
                    reason: "Requires human approval to append".into(),
                },
                MemoryPermission::Partner => Self::RequireConsent {
                    reason: "Requires partner approval to append".into(),
                },
                MemoryPermission::ReadOnly => Self::Deny {
                    reason: "Block is read-only; appending is not allowed".into(),
                },
            },
            MemoryOp::Overwrite => match perm {
                MemoryPermission::ReadWrite | MemoryPermission::Admin => Self::Allow,
                MemoryPermission::Human => Self::RequireConsent {
                    reason: "Requires human approval to overwrite".into(),
                },
                MemoryPermission::Partner => Self::RequireConsent {
                    reason: "Requires partner approval to overwrite".into(),
                },
                MemoryPermission::Append | MemoryPermission::ReadOnly => Self::Deny {
                    reason: "Insufficient permission (append-only or read-only) for overwrite"
                        .into(),
                },
            },
            MemoryOp::Delete => match perm {
                MemoryPermission::Admin => Self::Allow,
                _ => Self::Deny {
                    reason: "Deleting memory requires admin permission".into(),
                },
            },
        }
    }

    /// Check if the gate allows the operation.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Check if the gate requires consent.
    pub fn requires_consent(&self) -> bool {
        matches!(self, Self::RequireConsent { .. })
    }

    /// Check if the gate denies the operation.
    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }
}

/// Checkpoint of a memory block (for history/rollback).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
/// is loaded and updates are applied in seq order to reconstruct current state.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
