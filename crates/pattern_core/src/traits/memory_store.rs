//! MemoryStore trait — abstraction for memory-block storage operations.
//!
//! This trait is the interface that tools (context, recall, search) use to
//! read and write memory blocks. It abstracts over storage implementations
//! (cache-backed, direct DB, in-memory stub, etc.). The canonical
//! implementation lives in `crate::memory` alongside the supporting
//! value types ([`crate::memory::BlockMetadata`], [`crate::memory::ArchivalEntry`],
//! [`crate::memory::SharedBlockInfo`]).
//!
//! The trait is relocated here unchanged from its pre-v3 location in
//! `crate::memory::store`. No method signatures were added, removed, or
//! renamed. Supporting types remain in `crate::memory::*` so storage impls
//! need not import from `traits::`.
//!
//! # Example dummy impl (AC1.3)
//!
//! See the trait-level doctest below for the full shape.

use core::fmt;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::memory::{
    ArchivalEntry, BlockMetadata, BlockSchema, BlockType, MemoryResult, MemorySearchResult,
    SearchOptions, SharedBlockInfo, StructuredDocument,
};
use crate::types::block::BlockCreate;

/// Storage-agnostic contract for reading and writing memory blocks.
///
/// Implementations persist [`StructuredDocument`] instances keyed by
/// `(agent_id, label)` and expose search, archival, and shared-block
/// operations. All methods are `async` except the synchronous `mark_dirty`
/// helper, which is a cheap metadata toggle.
///
/// # Example
///
/// ```no_run
/// use async_trait::async_trait;
/// use serde_json::Value as JsonValue;
/// use pattern_core::memory::{
///     ArchivalEntry, BlockMetadata, BlockSchema, BlockType, MemoryResult,
///     MemorySearchResult, SearchOptions, SharedBlockInfo, StructuredDocument,
/// };
/// use pattern_core::traits::MemoryStore;
/// use pattern_core::types::block::BlockCreate;
///
/// #[derive(Debug)]
/// struct Dummy;
///
/// #[async_trait]
/// impl MemoryStore for Dummy {
///     async fn create_block(
///         &self,
///         _agent_id: &str,
///         _create: BlockCreate,
///     ) -> MemoryResult<StructuredDocument> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn get_block(&self, _a: &str, _l: &str) -> MemoryResult<Option<StructuredDocument>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn get_block_metadata(&self, _a: &str, _l: &str) -> MemoryResult<Option<BlockMetadata>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn list_blocks(&self, _a: &str) -> MemoryResult<Vec<BlockMetadata>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn list_blocks_by_type(
///         &self,
///         _a: &str,
///         _t: BlockType,
///     ) -> MemoryResult<Vec<BlockMetadata>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn list_all_blocks_by_label_prefix(
///         &self,
///         _p: &str,
///     ) -> MemoryResult<Vec<BlockMetadata>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn delete_block(&self, _a: &str, _l: &str) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn get_rendered_content(
///         &self,
///         _a: &str,
///         _l: &str,
///     ) -> MemoryResult<Option<String>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn persist_block(&self, _a: &str, _l: &str) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn mark_dirty(&self, _a: &str, _l: &str) {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn insert_archival(
///         &self,
///         _a: &str,
///         _c: &str,
///         _m: Option<JsonValue>,
///     ) -> MemoryResult<String> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn search_archival(
///         &self,
///         _a: &str,
///         _q: &str,
///         _n: usize,
///     ) -> MemoryResult<Vec<ArchivalEntry>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn search(
///         &self,
///         _a: &str,
///         _q: &str,
///         _o: SearchOptions,
///     ) -> MemoryResult<Vec<MemorySearchResult>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn search_all(
///         &self,
///         _q: &str,
///         _o: SearchOptions,
///     ) -> MemoryResult<Vec<MemorySearchResult>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn list_shared_blocks(
///         &self,
///         _a: &str,
///     ) -> MemoryResult<Vec<SharedBlockInfo>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn get_shared_block(
///         &self,
///         _r: &str,
///         _o: &str,
///         _l: &str,
///     ) -> MemoryResult<Option<StructuredDocument>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn set_block_pinned(
///         &self,
///         _a: &str,
///         _l: &str,
///         _p: bool,
///     ) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn set_block_type(
///         &self,
///         _a: &str,
///         _l: &str,
///         _t: BlockType,
///     ) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn update_block_schema(
///         &self,
///         _a: &str,
///         _l: &str,
///         _s: BlockSchema,
///     ) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn update_block_description(
///         &self,
///         _a: &str,
///         _l: &str,
///         _d: &str,
///     ) -> MemoryResult<()> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn undo_block(&self, _a: &str, _l: &str) -> MemoryResult<bool> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn redo_block(&self, _a: &str, _l: &str) -> MemoryResult<bool> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn undo_depth(&self, _a: &str, _l: &str) -> MemoryResult<usize> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn redo_depth(&self, _a: &str, _l: &str) -> MemoryResult<usize> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
/// ```
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    // ========== Block CRUD ==========

    /// Create a new memory block, returning the document ready for editing.
    ///
    /// The returned document includes all metadata and is already cached.
    /// Construction parameters are bundled in [`BlockCreate`] to prevent
    /// positional-argument transposition across the six scalar fields.
    async fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument>;

    /// Get a block's document for reading/writing.
    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    /// Get block metadata without loading the document.
    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>>;

    /// List all blocks for an agent.
    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>>;

    /// List blocks by type.
    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>>;

    /// List blocks by label prefix (across all agents).
    ///
    /// System-level operation for restoring DataBlock source tracking after
    /// restart. Finds all active blocks whose labels start with the given
    /// prefix. Not for use in agent tool calls — use agent-scoped methods
    /// instead.
    async fn list_all_blocks_by_label_prefix(
        &self,
        prefix: &str,
    ) -> MemoryResult<Vec<BlockMetadata>>;

    /// Delete (deactivate) a block.
    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    // ========== Content Operations ==========

    /// Get rendered content for context (respects schema).
    async fn get_rendered_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>>;

    /// Persist any pending changes for a block.
    async fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    /// Mark block as dirty (has unpersisted changes).
    fn mark_dirty(&self, agent_id: &str, label: &str);

    // ========== Archival Operations ==========

    /// Insert an archival entry (separate from blocks).
    ///
    /// Returns the entry id.
    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String>;

    /// Search archival memory.
    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>>;

    /// Delete an archival entry.
    async fn delete_archival(&self, id: &str) -> MemoryResult<()>;

    // ========== Search Operations ==========

    /// Search across memory content for a specific agent.
    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    /// Search across ALL agents in the constellation.
    ///
    /// Used for constellation-wide search scope.
    async fn search_all(
        &self,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    // ========== Shared Block Operations ==========

    /// List blocks shared with this agent (not owned by, but accessible to).
    async fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>>;

    /// Get a shared block by owner and label (checks permission).
    async fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    // ========== Block Configuration ==========

    /// Set the pinned flag on a block.
    ///
    /// Pinned blocks are always loaded into agent context while subscribed.
    /// Unpinned (ephemeral) blocks only load when referenced by a
    /// notification.
    async fn set_block_pinned(&self, agent_id: &str, label: &str, pinned: bool)
    -> MemoryResult<()>;

    /// Change a block's type.
    ///
    /// Used primarily for archiving blocks (Working -> Archival). Core blocks
    /// cannot be archived.
    async fn set_block_type(
        &self,
        agent_id: &str,
        label: &str,
        block_type: BlockType,
    ) -> MemoryResult<()>;

    /// Update a block's schema settings.
    ///
    /// Used to modify schema properties like viewport (Text) or display_limit
    /// (Log). The schema variant must match the existing block's schema
    /// variant (can't change Text to Map). Returns error if schema types are
    /// incompatible.
    async fn update_block_schema(
        &self,
        agent_id: &str,
        label: &str,
        schema: BlockSchema,
    ) -> MemoryResult<()>;

    /// Update a block's human-readable description.
    ///
    /// Returns `MemoryError::NotFound` if the block does not exist.
    async fn update_block_description(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
    ) -> MemoryResult<()>;

    // ========== Undo/Redo Operations ==========

    /// Undo the last persisted change to a block.
    ///
    /// Marks the most recent active update as inactive, effectively undoing
    /// it. Returns true if undo was performed, false if no history available.
    async fn undo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool>;

    /// Redo a previously undone change to a block.
    ///
    /// Reactivates the first inactive update after the current active branch.
    /// Returns true if redo was performed, false if nothing to redo.
    async fn redo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool>;

    /// Get the number of available undo steps for a block.
    ///
    /// Returns the count of active updates that can be undone.
    async fn undo_depth(&self, agent_id: &str, label: &str) -> MemoryResult<usize>;

    /// Get the number of available redo steps for a block.
    ///
    /// Returns the count of inactive updates that can be redone.
    async fn redo_depth(&self, agent_id: &str, label: &str) -> MemoryResult<usize>;
}
