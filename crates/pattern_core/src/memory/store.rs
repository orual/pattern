//! MemoryStore trait - abstraction for memory operations
//!
//! This is the interface that tools (context, recall, search) will use.
//! It abstracts over the storage implementation (cache-backed, direct DB, etc.)

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;

use crate::memory::{
    BlockSchema, BlockType, MemoryResult, MemorySearchResult, SearchOptions, StructuredDocument,
};

/// Trait for memory storage operations
///
/// Abstracts over the storage implementation (cache-backed, direct DB, etc.)
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    // ========== Block CRUD ==========

    /// Create a new memory block
    async fn create_block(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
        block_type: BlockType,
        schema: BlockSchema,
        char_limit: usize,
    ) -> MemoryResult<String>; // Returns block ID

    /// Get a block's document for reading/writing
    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    /// Get block metadata without loading document
    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>>;

    /// List all blocks for an agent
    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>>;

    /// List blocks by type
    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>>;

    /// Delete (deactivate) a block
    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    // ========== Content Operations ==========

    /// Get rendered content for context (respects schema)
    async fn get_rendered_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>>;

    /// Persist any pending changes for a block
    async fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    /// Mark block as dirty (has unpersisted changes)
    fn mark_dirty(&self, agent_id: &str, label: &str);

    // ========== Convenience Content Operations ==========

    /// Update the entire text content of a block (for PlainText schema blocks)
    async fn update_block_text(
        &self,
        agent_id: &str,
        label: &str,
        new_content: &str,
    ) -> MemoryResult<()>;

    /// Append text to a block's content
    async fn append_to_block(&self, agent_id: &str, label: &str, content: &str)
    -> MemoryResult<()>;

    /// Replace first occurrence of text in a block's content
    /// Returns true if replacement was made, false if old text not found
    async fn replace_in_block(
        &self,
        agent_id: &str,
        label: &str,
        old: &str,
        new: &str,
    ) -> MemoryResult<bool>;

    // ========== Archival Operations ==========

    /// Insert an archival entry (separate from blocks)
    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String>; // Returns entry ID

    /// Search archival memory
    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>>;

    /// Delete archival entry
    async fn delete_archival(&self, id: &str) -> MemoryResult<()>;

    // ========== Search Operations ==========

    /// Search across memory content for a specific agent
    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    /// Search across ALL agents in the constellation
    /// Used for constellation-wide search scope
    async fn search_all(
        &self,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    // ========== Shared Block Operations ==========

    /// List blocks shared with this agent (not owned by, but accessible to)
    async fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>>;

    /// Get a shared block by owner and label (checks permission)
    async fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    // ========== Block Configuration ==========

    /// Set the pinned flag on a block
    ///
    /// Pinned blocks are always loaded into agent context while subscribed.
    /// Unpinned (ephemeral) blocks only load when referenced by a notification.
    async fn set_block_pinned(&self, agent_id: &str, label: &str, pinned: bool)
    -> MemoryResult<()>;

    /// Change a block's type
    ///
    /// Used primarily for archiving blocks (Working -> Archival).
    /// Core blocks cannot be archived.
    async fn set_block_type(
        &self,
        agent_id: &str,
        label: &str,
        block_type: BlockType,
    ) -> MemoryResult<()>;
}

/// Block metadata (without loading the full document)
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

/// Archival entry (for search results)
#[derive(Debug, Clone)]
pub struct ArchivalEntry {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub metadata: Option<JsonValue>,
    pub created_at: DateTime<Utc>,
}

/// Information about a block shared with an agent
#[derive(Debug, Clone)]
pub struct SharedBlockInfo {
    pub block_id: String,
    pub owner_agent_id: String,
    /// The display name of the owning agent (if available)
    pub owner_agent_name: Option<String>,
    pub label: String,
    pub description: String,
    pub block_type: BlockType,
    pub permission: pattern_db::models::MemoryPermission,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Just verify the trait is object-safe
    fn _assert_object_safe(_: &dyn MemoryStore) {}
}
