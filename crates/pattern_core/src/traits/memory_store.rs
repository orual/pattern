//! MemoryStore trait — abstraction for memory-block storage operations.
//!
//! This trait is the interface that tools (context, recall, search) use to
//! read and write memory blocks. It abstracts over storage implementations
//! (cache-backed, direct DB, in-memory stub, etc.). The canonical
//! implementation lives in `pattern_memory::MemoryCache`. Supporting
//! value types ([`crate::types::memory_types::BlockMetadata`],
//! [`crate::types::memory_types::ArchivalEntry`],
//! [`crate::types::memory_types::SharedBlockInfo`]) live in
//! `crate::types::memory_types`.

use core::fmt;

use serde_json::Value as JsonValue;

use crate::memory::StructuredDocument;
use crate::types::block::BlockCreate;
use crate::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, MemoryResult,
    MemorySearchResult, MemorySearchScope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};

/// Storage-agnostic contract for reading and writing memory blocks.
///
/// Implementations persist [`StructuredDocument`] instances keyed by
/// `(agent_id, label)` and expose search, archival, and shared-block
/// operations. All methods are synchronous — the underlying storage is
/// rusqlite (Phase 2 port).
///
/// # Method surface consolidation (v3-memory-rework Phase 3, 2026-04-19)
///
/// Reduced from 28 methods to 19 via five consolidations:
/// - `list_blocks`, `list_blocks_by_type`, `list_all_blocks_by_label_prefix`
///   -> [`list_blocks(BlockFilter)`](MemoryStore::list_blocks)
/// - `set_block_pinned`, `set_block_type`, `update_block_schema`,
///   `update_block_description`
///   -> [`update_block_metadata(BlockMetadataPatch)`](MemoryStore::update_block_metadata)
/// - `undo_block`, `redo_block`
///   -> [`undo_redo(UndoRedoOp)`](MemoryStore::undo_redo)
/// - `undo_depth`, `redo_depth`
///   -> [`history_depth`](MemoryStore::history_depth)
/// - `search`, `search_all`
///   -> [`search(MemorySearchScope)`](MemoryStore::search)
///
/// All method signatures are sync (no `#[async_trait]`). The trait
/// contract is driven by rusqlite under the hood (see pattern_db).
///
/// `delete_archival` is retained as a trait method for human-operator
/// tooling (CLI curation, TUI); it is NOT reachable via any agent-facing
/// SDK effect (see v3-memory-rework Phase 3 SDK removal).
pub trait MemoryStore: Send + Sync + fmt::Debug + 'static {
    // ========== Block CRUD ==========

    /// Create a new memory block, returning the document ready for editing.
    ///
    /// The returned document includes all metadata and is already cached.
    /// Construction parameters are bundled in [`BlockCreate`] to prevent
    /// positional-argument transposition across the six scalar fields.
    fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument>;

    /// Get a block's document for reading/writing.
    fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    /// Get block metadata without loading the document.
    fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>>;

    /// List blocks matching the given filter.
    ///
    /// Replaces the pre-Phase-3 `list_blocks`, `list_blocks_by_type`, and
    /// `list_all_blocks_by_label_prefix` methods. Use [`BlockFilter`]
    /// factory methods to construct common filter shapes:
    ///
    /// - `BlockFilter::by_agent(id)` — all blocks for one agent.
    /// - `BlockFilter::by_type(id, bt)` — blocks of a specific type.
    /// - `BlockFilter::by_prefix(pfx)` — label prefix scan (all agents).
    /// - `BlockFilter::all()` — everything.
    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>>;

    /// Delete (deactivate) a block.
    fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    // ========== Content Operations ==========

    /// Get rendered content for context (respects schema).
    fn get_rendered_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>>;

    /// Persist any pending changes for a block.
    fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    /// Mark block as dirty (has unpersisted changes).
    fn mark_dirty(&self, agent_id: &str, label: &str);

    // ========== Archival Operations ==========

    /// Insert an archival entry (separate from blocks).
    ///
    /// Returns the entry id.
    fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String>;

    /// Search archival memory.
    fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>>;

    /// Delete an archival entry.
    ///
    /// Retained for human-operator tooling (CLI, TUI). Not reachable via
    /// any agent-facing SDK effect.
    fn delete_archival(&self, id: &str) -> MemoryResult<()>;

    // ========== Search Operations ==========

    /// Search across memory content, scoped by [`MemorySearchScope`].
    ///
    /// Replaces the pre-Phase-3 `search` (agent-scoped) and `search_all`
    /// (constellation-scoped) methods.
    fn search(
        &self,
        query: &str,
        options: SearchOptions,
        scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    // ========== Shared Block Operations ==========

    /// List blocks shared with this agent (not owned by, but accessible to).
    fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>>;

    /// Get a shared block by owner and label (checks permission).
    fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    // ========== Block Configuration ==========

    /// Apply a metadata patch to a block.
    ///
    /// Replaces the pre-Phase-3 `set_block_pinned`, `set_block_type`,
    /// `update_block_schema`, and `update_block_description` methods.
    /// Each `Some(...)` field in the patch is applied; `None` fields
    /// leave the stored value unchanged.
    fn update_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()>;

    // ========== Undo/Redo Operations ==========

    /// Undo or redo the last persisted change to a block.
    ///
    /// Replaces the pre-Phase-3 separate `undo_block` and `redo_block`
    /// methods. Returns `true` if the operation was performed, `false`
    /// if no history is available in that direction.
    fn undo_redo(&self, agent_id: &str, label: &str, op: UndoRedoOp) -> MemoryResult<bool>;

    /// Get the number of available undo and redo steps for a block.
    ///
    /// Replaces the pre-Phase-3 separate `undo_depth` and `redo_depth`
    /// methods.
    fn history_depth(&self, agent_id: &str, label: &str) -> MemoryResult<UndoRedoDepth>;

    // ========== Scope Resolution Helpers ==========
    //
    // These methods support the scope resolver in `pattern_runtime`.
    // Default implementations return conservative answers (no permission,
    // no agents). Implementations backed by pattern_db override these
    // with real DB queries.

    /// Check whether `target` has shared at least one block with `caller`.
    ///
    /// Used by the scope resolver to determine cross-agent search
    /// permission: sharing a block is treated as a signal that two agents
    /// cooperate.
    fn has_shared_blocks_with(&self, _caller: &str, _target: &str) -> MemoryResult<bool> {
        Ok(false)
    }

    /// Check whether `caller` and `target` are members of the same
    /// agent group.
    fn shares_group_with(&self, _caller: &str, _target: &str) -> MemoryResult<bool> {
        Ok(false)
    }

    /// List all agent IDs in the constellation. Used for
    /// `MemorySearchScope::Constellation` resolution.
    fn list_constellation_agent_ids(&self) -> MemoryResult<Vec<String>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryStore;

    // Verify the trait is object-safe (dyn-compatible).
    fn _assert_object_safe(_: &dyn MemoryStore) {}
}
