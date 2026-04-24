//! Core memory value types that appear in [`crate::traits::MemoryStore`]
//! signatures.

use std::fmt::Display;

/// Default character limit for memory blocks when not specified.
pub const DEFAULT_MEMORY_CHAR_LIMIT: usize = 5000;

/// Special agent ID for constellation-level blocks (readable by all agents).
pub const CONSTELLATION_OWNER: &str = "_constellation_";

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::BlockSchema;

/// Errors that can occur during document operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DocumentError {
    #[error("failed to import document: {0}")]
    ImportFailed(String),

    #[error("failed to export document: {0}")]
    ExportFailed(String),

    #[error("field not found: {0}")]
    FieldNotFound(String),

    #[error("schema mismatch: expected {expected}, got {actual}")]
    SchemaMismatch { expected: String, actual: String },

    #[error("field '{0}' is read-only and cannot be modified by agent")]
    ReadOnlyField(String),

    #[error("section '{0}' is read-only and cannot be modified by agent")]
    ReadOnlySection(String),

    #[error("operation '{operation}' not supported for schema {schema}")]
    InvalidSchemaForOperation { operation: String, schema: String },

    #[error(
        "permission denied: {operation} requires {required} permission, but block has {actual}"
    )]
    PermissionDenied {
        operation: String,
        required: MemoryPermission,
        actual: MemoryPermission,
    },

    #[error("{0}")]
    Other(String),
}

/// Block types matching pattern_db.
///
/// Only `Core` and `Working` remain after the v3-memory-rework Phase 2.
/// `Archival` rows migrated to the `archival_entries` table; `Log` rows
/// reclassified as `Working` with a `{"kind": "log"}` metadata marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryBlockType {
    Core,
    #[default]
    Working,
}

impl MemoryBlockType {
    /// Returns the lowercase string representation matching the database format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Working => "working",
        }
    }
}

/// Errors from parsing a [`MemoryBlockType`] string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryBlockTypeParseError {
    /// A variant that existed prior to v3-memory-rework Phase 2 but was
    /// removed. Rows must be migrated via `0010_collapse_block_types.sql`.
    #[error(
        "block_type {0:?} was removed in v3-memory-rework; \
         rows must be migrated via migration 0010_collapse_block_types.sql"
    )]
    RemovedVariant(String),

    /// An entirely unknown block type string.
    #[error("unknown block_type {0:?}")]
    Unknown(String),
}

impl std::str::FromStr for MemoryBlockType {
    type Err = MemoryBlockTypeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "core" => Ok(Self::Core),
            "working" => Ok(Self::Working),
            "archival" | "log" => Err(MemoryBlockTypeParseError::RemovedVariant(s.to_owned())),
            other => Err(MemoryBlockTypeParseError::Unknown(other.to_owned())),
        }
    }
}

impl std::fmt::Display for MemoryBlockType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Working => write!(f, "working"),
        }
    }
}

/// Persona isolation policy for project-scoped memory routing.
///
/// Controls how reads and writes are routed when a persona is attached to a
/// project: `None` merges both scopes bidirectionally, `CoreOnly` makes
/// persona core blocks read-only from within the project, and `Full`
/// hides persona block content entirely (only identity metadata is visible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum IsolatePolicy {
    /// Persona + project merged; bidirectional writes.
    None,
    /// Persona core read-only from project; project writes stay project-scoped.
    CoreOnly,
    /// Persona identity only; no persona memory carryover.
    Full,
}

impl Display for IsolatePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::CoreOnly => write!(f, "core-only"),
            Self::Full => write!(f, "full"),
        }
    }
}

// `MemoryError` and `MemoryResult` are defined in `crate::error::memory` and
// re-exported here for backward compatibility with existing import paths.
pub use crate::error::memory::{MemoryError, MemoryResult};

// ========== Consolidation types (v3-memory-rework Phase 3) ==========

/// Filter predicate for [`crate::traits::MemoryStore::list_blocks`].
///
/// Replaces the pre-Phase-3 `list_blocks`, `list_blocks_by_type`, and
/// `list_all_blocks_by_label_prefix` methods with a single entry point.
/// Each `Some(...)` field narrows the results; `None` fields impose no
/// constraint.
///
/// # Examples
///
/// ```
/// use pattern_core::types::memory_types::BlockFilter;
///
/// // All blocks for a single agent.
/// let f = BlockFilter::by_agent("agent-1");
/// assert!(f.agent_id.is_some());
/// assert!(f.block_type.is_none());
///
/// // Only Core blocks for an agent.
/// let f = BlockFilter::by_type("agent-1", pattern_core::types::memory_types::MemoryBlockType::Core);
/// assert_eq!(f.block_type, Some(pattern_core::types::memory_types::MemoryBlockType::Core));
///
/// // Constellation-wide label prefix scan.
/// let f = BlockFilter::by_prefix("ds:");
/// assert!(f.agent_id.is_none());
/// assert_eq!(f.label_prefix.as_deref(), Some("ds:"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BlockFilter {
    /// If set, only blocks owned by this agent are returned.
    /// If `None`, blocks from every agent are returned (use for
    /// constellation-wide listings).
    pub agent_id: Option<String>,
    /// If set, only blocks with this type are returned.
    pub block_type: Option<MemoryBlockType>,
    /// If set, only blocks whose label starts with this prefix
    /// are returned.
    pub label_prefix: Option<String>,
}

impl BlockFilter {
    /// Filter to a single agent's blocks.
    pub fn by_agent(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: Some(agent_id.into()),
            ..Self::default()
        }
    }

    /// Filter to a single agent's blocks of a specific type.
    pub fn by_type(agent_id: impl Into<String>, block_type: MemoryBlockType) -> Self {
        Self {
            agent_id: Some(agent_id.into()),
            block_type: Some(block_type),
            ..Self::default()
        }
    }

    /// Filter by label prefix across all agents.
    pub fn by_prefix(prefix: impl Into<String>) -> Self {
        Self {
            label_prefix: Some(prefix.into()),
            ..Self::default()
        }
    }

    /// No filter — returns all blocks.
    pub fn all() -> Self {
        Self::default()
    }
}

/// Sparse patch for [`crate::traits::MemoryStore::update_block_metadata`].
///
/// Each `Some(...)` field is applied; `None` fields leave the stored
/// value unchanged. Replaces the pre-Phase-3 `set_block_pinned`,
/// `set_block_type`, `update_block_schema`, and `update_block_description`
/// methods.
///
/// Uses builder-style chaining for ergonomic construction:
///
/// ```
/// use pattern_core::types::memory_types::{BlockMetadataPatch, MemoryBlockType};
///
/// let patch = BlockMetadataPatch::default()
///     .pinned(true)
///     .block_type(MemoryBlockType::Working);
///
/// assert_eq!(patch.pinned, Some(true));
/// assert!(!patch.is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct BlockMetadataPatch {
    /// If set, update the block's pinned flag.
    pub pinned: Option<bool>,
    /// If set, change the block's type.
    pub block_type: Option<MemoryBlockType>,
    /// If set, update the block's schema.
    pub schema: Option<BlockSchema>,
    /// If set, update the block's human-readable description.
    pub description: Option<String>,
}

impl BlockMetadataPatch {
    /// Set the pinned flag.
    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = Some(pinned);
        self
    }

    /// Set the block type.
    pub fn block_type(mut self, bt: MemoryBlockType) -> Self {
        self.block_type = Some(bt);
        self
    }

    /// Set the block schema.
    pub fn schema(mut self, sch: BlockSchema) -> Self {
        self.schema = Some(sch);
        self
    }

    /// Set the block description.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    /// Returns `true` if no fields are set (the patch would be a no-op).
    pub fn is_empty(&self) -> bool {
        self.pinned.is_none()
            && self.block_type.is_none()
            && self.schema.is_none()
            && self.description.is_none()
    }
}

/// Direction for [`crate::traits::MemoryStore::undo_redo`].
///
/// Replaces the pre-Phase-3 separate `undo_block` and `redo_block`
/// methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum UndoRedoOp {
    /// Undo the last persisted change.
    Undo,
    /// Redo a previously undone change.
    Redo,
}

/// Combined undo/redo depth returned by
/// [`crate::traits::MemoryStore::history_depth`].
///
/// Replaces the pre-Phase-3 separate `undo_depth` and `redo_depth`
/// methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndoRedoDepth {
    /// Number of available undo steps.
    pub undo: usize,
    /// Number of available redo steps.
    pub redo: usize,
}

/// Permission levels for memory operations (most to least restrictive)
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPermission {
    /// Can only read, no modifications allowed
    ReadOnly,
    /// Requires permission from partner (owner)
    Partner,
    /// Requires permission from any human
    Human,
    /// Can append to existing content
    Append,
    /// Can modify content freely
    #[default]
    ReadWrite,
    /// Total control, can delete
    Admin,
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

impl Display for MemoryPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryPermission::ReadOnly => write!(f, "Read Only"),
            MemoryPermission::Partner => write!(f, "Requires Partner permission to write"),
            MemoryPermission::Human => write!(f, "Requires Human permission to write"),
            MemoryPermission::Append => write!(f, "Append Only"),
            MemoryPermission::ReadWrite => write!(f, "Read, Append, Write"),
            MemoryPermission::Admin => write!(f, "Read, Write, Delete"),
        }
    }
}

impl std::str::FromStr for MemoryPermission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "read_only" | "readonly" => Ok(Self::ReadOnly),
            "partner" => Ok(Self::Partner),
            "human" => Ok(Self::Human),
            "append" => Ok(Self::Append),
            "read_write" | "readwrite" => Ok(Self::ReadWrite),
            "admin" => Ok(Self::Admin),
            _ => Err(format!(
                "unknown permission '{}', expected: read_only, partner, human, append, read_write, admin",
                s
            )),
        }
    }
}

/// Memory operation types for permission gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOp {
    /// Read data from a block.
    Read,
    /// Append to existing content.
    Append,
    /// Replace content entirely.
    Overwrite,
    /// Delete a block.
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
    /// - Read: always allowed.
    /// - Append: allowed for Append/ReadWrite/Admin; Human/Partner require consent; ReadOnly denied.
    /// - Overwrite: allowed for ReadWrite/Admin; Human/Partner require consent; ReadOnly/Append denied.
    /// - Delete: allowed for Admin only; others denied.
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

/// Type of memory storage
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// Always in context, cannot be swapped out
    #[default]
    Core,
    /// Active working memory, can be swapped
    Working,
    /// Long-term storage, searchable on demand
    Archival,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::Core => write!(f, "core"),
            MemoryType::Working => write!(f, "working"),
            MemoryType::Archival => write!(f, "recall"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- BlockFilter tests ----

    #[test]
    fn block_filter_all_is_default() {
        let f = BlockFilter::all();
        assert_eq!(f, BlockFilter::default());
        assert!(f.agent_id.is_none());
        assert!(f.block_type.is_none());
        assert!(f.label_prefix.is_none());
    }

    #[test]
    fn block_filter_by_agent() {
        let f = BlockFilter::by_agent("agent-1");
        assert_eq!(f.agent_id.as_deref(), Some("agent-1"));
        assert!(f.block_type.is_none());
        assert!(f.label_prefix.is_none());
    }

    #[test]
    fn block_filter_by_type() {
        let f = BlockFilter::by_type("agent-1", MemoryBlockType::Core);
        assert_eq!(f.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(f.block_type, Some(MemoryBlockType::Core));
        assert!(f.label_prefix.is_none());
    }

    #[test]
    fn block_filter_by_prefix() {
        let f = BlockFilter::by_prefix("ds:");
        assert!(f.agent_id.is_none());
        assert!(f.block_type.is_none());
        assert_eq!(f.label_prefix.as_deref(), Some("ds:"));
    }

    // ---- BlockMetadataPatch tests ----

    #[test]
    fn patch_empty_by_default() {
        let p = BlockMetadataPatch::default();
        assert!(p.is_empty());
    }

    #[test]
    fn patch_builder_chaining() {
        let p = BlockMetadataPatch::default()
            .pinned(true)
            .block_type(MemoryBlockType::Working)
            .description("test description");
        assert_eq!(p.pinned, Some(true));
        assert_eq!(p.block_type, Some(MemoryBlockType::Working));
        assert_eq!(p.description.as_deref(), Some("test description"));
        assert!(p.schema.is_none());
        assert!(!p.is_empty());
    }

    #[test]
    fn patch_single_field_not_empty() {
        let p = BlockMetadataPatch::default().pinned(false);
        assert!(!p.is_empty());
    }

    #[test]
    fn patch_schema_field() {
        let p = BlockMetadataPatch::default().schema(BlockSchema::text());
        assert!(p.schema.is_some());
        assert!(!p.is_empty());
    }

    // ---- UndoRedoOp tests ----

    #[test]
    fn undo_redo_op_variants() {
        assert_ne!(UndoRedoOp::Undo, UndoRedoOp::Redo);
        // Verify Copy.
        let op = UndoRedoOp::Undo;
        let op2 = op;
        assert_eq!(op, op2);
    }

    // ---- UndoRedoDepth tests ----

    #[test]
    fn undo_redo_depth_fields() {
        let d = UndoRedoDepth { undo: 3, redo: 1 };
        assert_eq!(d.undo, 3);
        assert_eq!(d.redo, 1);
        // Verify Copy.
        let d2 = d;
        assert_eq!(d, d2);
    }
}
