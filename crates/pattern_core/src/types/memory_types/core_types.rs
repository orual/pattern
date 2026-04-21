//! Core memory value types that appear in [`crate::traits::MemoryStore`]
//! signatures.

use std::fmt::Display;

/// Default character limit for memory blocks when not specified.
pub const DEFAULT_MEMORY_CHAR_LIMIT: usize = 5000;

/// Special agent ID for constellation-level blocks (readable by all agents).
pub const CONSTELLATION_OWNER: &str = "_constellation_";

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
        required: pattern_db::models::MemoryPermission,
        actual: pattern_db::models::MemoryPermission,
    },

    #[error("{0}")]
    Other(String),
}

/// Block types matching pattern_db.
///
/// Only `Core` and `Working` remain after the v3-memory-rework Phase 2.
/// `Archival` rows migrated to the `archival_entries` table; `Log` rows
/// reclassified as `Working` with a `{"kind": "log"}` metadata marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockType {
    Core,
    Working,
}

/// Errors from parsing a [`BlockType`] string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockTypeParseError {
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

impl std::str::FromStr for BlockType {
    type Err = BlockTypeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "core" => Ok(Self::Core),
            "working" => Ok(Self::Working),
            "archival" | "log" => Err(BlockTypeParseError::RemovedVariant(s.to_owned())),
            other => Err(BlockTypeParseError::Unknown(other.to_owned())),
        }
    }
}

impl std::fmt::Display for BlockType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Working => write!(f, "working"),
        }
    }
}

impl From<pattern_db::models::MemoryBlockType> for BlockType {
    fn from(t: pattern_db::models::MemoryBlockType) -> Self {
        match t {
            pattern_db::models::MemoryBlockType::Core => BlockType::Core,
            pattern_db::models::MemoryBlockType::Working => BlockType::Working,
            // Future-proofing: non-exhaustive requires a catch-all.
            _ => BlockType::Working,
        }
    }
}

impl From<BlockType> for pattern_db::models::MemoryBlockType {
    fn from(t: BlockType) -> Self {
        match t {
            BlockType::Core => pattern_db::models::MemoryBlockType::Core,
            BlockType::Working => pattern_db::models::MemoryBlockType::Working,
        }
    }
}

/// Error type for memory operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    #[error("block not found: {agent_id}/{label}")]
    NotFound { agent_id: String, label: String },

    #[error("block is read-only: {0}")]
    ReadOnly(String),

    #[error(
        "permission denied for block '{block_label}': required {required:?}, actual {actual:?}"
    )]
    PermissionDenied {
        block_label: String,
        required: pattern_db::models::MemoryPermission,
        actual: pattern_db::models::MemoryPermission,
    },

    #[error("database error: {0}")]
    Database(#[from] pattern_db::DbError),

    #[error("loro error: {0}")]
    Loro(String),

    #[error("document error: {0}")]
    Document(#[from] DocumentError),

    #[error("memory operation failed: {0}")]
    Other(String),
}

pub type MemoryResult<T> = Result<T, MemoryError>;

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

impl From<MemoryPermission> for pattern_db::models::MemoryPermission {
    fn from(p: MemoryPermission) -> Self {
        match p {
            MemoryPermission::ReadOnly => pattern_db::models::MemoryPermission::ReadOnly,
            MemoryPermission::Partner => pattern_db::models::MemoryPermission::Partner,
            MemoryPermission::Human => pattern_db::models::MemoryPermission::Human,
            MemoryPermission::Append => pattern_db::models::MemoryPermission::Append,
            MemoryPermission::ReadWrite => pattern_db::models::MemoryPermission::ReadWrite,
            MemoryPermission::Admin => pattern_db::models::MemoryPermission::Admin,
        }
    }
}

impl From<pattern_db::models::MemoryPermission> for MemoryPermission {
    fn from(p: pattern_db::models::MemoryPermission) -> Self {
        match p {
            pattern_db::models::MemoryPermission::ReadOnly => MemoryPermission::ReadOnly,
            pattern_db::models::MemoryPermission::Partner => MemoryPermission::Partner,
            pattern_db::models::MemoryPermission::Human => MemoryPermission::Human,
            pattern_db::models::MemoryPermission::Append => MemoryPermission::Append,
            pattern_db::models::MemoryPermission::ReadWrite => MemoryPermission::ReadWrite,
            pattern_db::models::MemoryPermission::Admin => MemoryPermission::Admin,
        }
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
