//! Supporting value types for the [`MemoryStore`] trait.
//!
//! The `MemoryStore` trait itself lives in [`crate::traits::memory_store`];
//! this file keeps the metadata / archival / shared-block value types that
//! storage implementations and consumers share. Concrete `MemoryStore`
//! implementations (e.g. `MemoryCache`) continue to live in this crate and
//! implement the trait at `crate::traits::MemoryStore`.

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;

use crate::memory::{BlockSchema, BlockType};

// Re-export the trait so downstream consumers that imported
// `crate::memory::store::MemoryStore` before the relocation still compile.
pub use crate::traits::memory_store::MemoryStore;

/// Block metadata (without loading the full document).
#[derive(Debug, Clone)]
pub struct BlockMetadata {
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub description: String,
    pub block_type: BlockType,
    pub schema: BlockSchema,
    pub char_limit: usize,
    pub permission: pattern_db::models::MemoryPermission,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BlockMetadata {
    /// Create standalone metadata for testing or documents not backed by DB.
    pub fn standalone(schema: BlockSchema) -> Self {
        let now = Utc::now();
        Self {
            id: String::new(),
            agent_id: String::new(),
            label: String::new(),
            description: String::new(),
            block_type: BlockType::Working,
            schema,
            char_limit: 0,
            permission: pattern_db::models::MemoryPermission::ReadWrite,
            pinned: false,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Archival entry (for search results).
#[derive(Debug, Clone)]
pub struct ArchivalEntry {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub metadata: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
}

/// Information about a block shared with an agent.
#[derive(Debug, Clone)]
pub struct SharedBlockInfo {
    pub block_id: String,
    pub owner_agent_id: String,
    /// The display name of the owning agent (if available).
    pub owner_agent_name: Option<String>,
    pub label: String,
    pub description: String,
    pub block_type: BlockType,
    pub permission: pattern_db::models::MemoryPermission,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Just verify the trait is object-safe.
    fn _assert_object_safe(_: &dyn MemoryStore) {}
}
