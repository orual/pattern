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
    MemorySearchResult, MemorySearchScope, Scope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};

/// Storage-agnostic contract for reading and writing memory blocks.
///
/// Implementations persist [`StructuredDocument`] instances keyed by
/// `(scope, label)` and expose search, archival, and shared-block
/// operations. All methods are synchronous — the underlying storage is
/// rusqlite (Phase 2 port).
///
/// # Scope semantics (Phase 1 redesign, 2026-04-30)
///
/// Each block lives in exactly one [`Scope`]:
///
/// - [`Scope::Local`] — project-scoped block, shared across all agents
///   in a project mount.
/// - [`Scope::Global`] — persona-scoped block, follows the persona
///   across mounts.
///
/// `Local("x")` and `Global("x")` are distinct keyspaces — the prior
/// collision bug (project named "pattern" vs. persona named "@pattern"
/// sharing a single keyspace) is resolved by the type system.
pub trait MemoryStore: Send + Sync + fmt::Debug + 'static {
    /// Cross-block memory event broadcast for observers (MemorySync, etc).
    /// Concrete impls that emit raw loro update bytes + origin info return
    /// `Some(&observer)`; impls that don't support cross-block observation
    /// (in-memory test stubs, future plugin-side proxies whose observability
    /// is upstream-driven) default to `None`.
    fn observer(&self) -> Option<&crate::observer::MemoryObserver> { None }

    /// Externally-applied loro update bytes need to drive the same persistence
    /// pipeline as local agent edits (disk render + FTS5 + embedding) — but
    /// `LoroDoc::subscribe_local_update` doesn't fire on imports, so the
    /// existing local-edit closure doesn't catch them. Concrete impls with
    /// worker-backed persistence override this method to push a CommitEvent on
    /// their per-block channel manually after a successful import. Defaults to
    /// `Ok(())` — no-op for impls without workers (in-memory tests, the future
    /// plugin-side proxy whose persistence is upstream-driven, etc).
    fn push_external_commit(
        &self,
        _scope: &crate::types::memory_types::Scope,
        _label: &str,
        _update_bytes: Vec<u8>,
    ) -> MemoryResult<()> {
        Ok(())
    }

    // ========== Block CRUD ==========

    /// Create a new memory block, returning the document ready for editing.
    ///
    /// The returned document includes all metadata and is already cached.
    /// Construction parameters are bundled in [`BlockCreate`] to prevent
    /// positional-argument transposition across the six scalar fields.
    fn create_block(&self, scope: &Scope, create: BlockCreate)
    -> MemoryResult<StructuredDocument>;

    /// Get a block's document for reading/writing.
    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>>;

    /// Get block metadata without loading the document.
    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>>;

    /// List blocks matching the given filter.
    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>>;

    /// Delete (deactivate) a block.
    /// Create or replace a block (system-level upsert).
    /// Removes any existing block with the same label first.
    /// Implementors must provide an atomic delete+create.
    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument>;

    /// Commit a block write: mark dirty, persist to DB, and trigger file sync.
    /// This is the correct way to flush mutations to a block. Callers should
    /// NOT call mark_dirty + persist_block separately.
    fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()>;

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()>;

    // ========== Content Operations ==========

    /// Get rendered content for context (respects schema).
    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>>;

    /// Persist any pending changes for a block.
    fn persist_block(&self, scope: &Scope, label: &str) -> MemoryResult<()>;

    /// Mark block as dirty (has unpersisted changes).
    ///
    /// Returns `Err(MemoryError::WriteToMissingBlock)` when the
    /// `(scope, label)` pair does not match any cached block — failing
    /// loud rather than silently no-opping. Pre-Phase-1 callers relied
    /// on the `mark_dirty` no-op behavior to get persistence "for free"
    /// after a block mutation; the new contract makes mis-routed writes
    /// surface immediately.
    fn mark_dirty(&self, scope: &Scope, label: &str) -> MemoryResult<()>;

    // ========== Archival Operations ==========

    /// Insert an archival entry (separate from blocks).
    ///
    /// Returns the entry id.
    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String>;

    /// Search archival memory.
    fn search_archival(
        &self,
        scope: &Scope,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>>;

    /// Delete an archival entry.
    fn delete_archival(&self, id: &str) -> MemoryResult<()>;

    // ========== Search Operations ==========

    /// Search across memory content, scoped by [`MemorySearchScope`].
    fn search(
        &self,
        query: &str,
        options: SearchOptions,
        scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    // ========== Shared Block Operations ==========

    /// List blocks shared with this scope (not owned by, but accessible to).
    fn list_shared_blocks(&self, scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>>;

    /// Get a shared block by owner and label (checks permission).
    fn get_shared_block(
        &self,
        requester: &Scope,
        owner: &Scope,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    // ========== Block Configuration ==========

    /// Apply a metadata patch to a block.
    fn update_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()>;

    // ========== Undo/Redo Operations ==========

    /// Undo or redo the last persisted change to a block.
    fn undo_redo(&self, scope: &Scope, label: &str, op: UndoRedoOp) -> MemoryResult<bool>;

    /// Get the number of available undo and redo steps for a block.
    fn history_depth(&self, scope: &Scope, label: &str) -> MemoryResult<UndoRedoDepth>;

    // ========== Scope Resolution Helpers ==========

    /// Check whether `target` has shared at least one block with `caller`.
    fn has_shared_blocks_with(&self, _caller: &Scope, _target: &Scope) -> MemoryResult<bool> {
        Ok(false)
    }

    /// List all scopes in the constellation. Used for
    /// `MemorySearchScope::Constellation` resolution.
    fn list_constellation_scopes(&self) -> MemoryResult<Vec<Scope>> {
        Ok(vec![])
    }
}

// Blanket delegation for `Arc<dyn MemoryStore>` so wrappers like
// `MemoryScope<Arc<dyn MemoryStore>>` can satisfy the `S: MemoryStore`
// bound without a newtype shim.
impl MemoryStore for std::sync::Arc<dyn MemoryStore> {
    fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        (**self).commit_write(scope, label)
    }

    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        (**self).create_or_replace_block(scope, create)
    }

    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        (**self).create_block(scope, create)
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        (**self).get_block(scope, label)
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        (**self).get_block_metadata(scope, label)
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        (**self).list_blocks(filter)
    }

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        (**self).delete_block(scope, label)
    }

    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        (**self).get_rendered_content(scope, label)
    }

    fn persist_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        (**self).persist_block(scope, label)
    }

    fn mark_dirty(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        (**self).mark_dirty(scope, label)
    }

    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        (**self).insert_archival(scope, content, metadata)
    }

    fn search_archival(
        &self,
        scope: &Scope,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        (**self).search_archival(scope, query, limit)
    }

    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        (**self).delete_archival(id)
    }

    fn search(
        &self,
        query: &str,
        options: SearchOptions,
        scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        (**self).search(query, options, scope)
    }

    fn list_shared_blocks(&self, scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        (**self).list_shared_blocks(scope)
    }

    fn get_shared_block(
        &self,
        requester: &Scope,
        owner: &Scope,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        (**self).get_shared_block(requester, owner, label)
    }

    fn update_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        (**self).update_block_metadata(scope, label, patch)
    }

    fn undo_redo(&self, scope: &Scope, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        (**self).undo_redo(scope, label, op)
    }

    fn history_depth(&self, scope: &Scope, label: &str) -> MemoryResult<UndoRedoDepth> {
        (**self).history_depth(scope, label)
    }

    fn has_shared_blocks_with(&self, caller: &Scope, target: &Scope) -> MemoryResult<bool> {
        (**self).has_shared_blocks_with(caller, target)
    }

    fn list_constellation_scopes(&self) -> MemoryResult<Vec<Scope>> {
        (**self).list_constellation_scopes()
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryStore;

    // Verify the trait is object-safe (dyn-compatible).
    fn _assert_object_safe(_: &dyn MemoryStore) {}

    // Verify Arc<dyn MemoryStore> also implements MemoryStore.
    fn _assert_arc_impl(_: &std::sync::Arc<dyn MemoryStore>) {}
}
