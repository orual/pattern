//! V2 Memory System
//!
//! Memory value types, schema definitions, and the `MemoryStore` trait
//! supporting surface. Concrete storage implementations (the LoroDoc-based
//! `MemoryCache` and `SharedBlockManager`) are staged to
//! `rewrite-staging/runtime_subsystems/memory_v2/` pending the Phase 3
//! runtime landing — they depend on removed `crate::db::ConstellationDatabases`
//! plumbing and will return once that plumbing is reassembled in
//! `pattern_runtime`.

mod document;
mod schema;
mod store;
mod types;

use std::fmt::Display;

pub use document::*;
pub use schema::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use store::*;
pub use types::*;

// Re-export search types for convenience.
pub use types::{MemorySearchResult, SearchContentType, SearchMode, SearchOptions};

/// Special agent id used for constellation-level blocks (readable by all
/// agents). Re-homed from the staged `memory/sharing.rs` so that
/// [`crate::types::block_ref::BlockRef`] can reference it without pulling in
/// the staged module.
pub const CONSTELLATION_OWNER: &str = "_constellation_";

/// Default character limit for memory blocks when not specified.
///
/// Re-homed from the staged `memory/cache.rs` so that consumers (e.g. the
/// context composer in Phase 5) can reference the canonical default without
/// reaching into staging.
pub const DEFAULT_MEMORY_CHAR_LIMIT: usize = 5000;

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
