//! Metadata types for memory blocks, archival entries, and shared blocks.
//!
//! These types appear in [`crate::traits::MemoryStore`] method return types
//! and are shared across crate boundaries.

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;

use super::{BlockSchema, BlockType};

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
    pub permission: super::MemoryPermission,
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
            permission: super::MemoryPermission::ReadWrite,
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
    pub permission: super::MemoryPermission,
}
