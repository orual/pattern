//! Thin delegating wrapper over `Arc<dyn MemoryStore>` with a pending
//! `BlockWrite` buffer.
//!
//! The adapter records memory mutations that handlers report via
//! [`MemoryStoreAdapter::record_write`]. The session drains the buffer at
//! turn close to populate [`pattern_core::types::turn::TurnOutput::block_writes`]
//! and feed Phase 5's pseudo-message emitter.
//!
//! Design choice: the adapter does **not** intercept trait-method calls to
//! auto-record writes. Handlers call `record_write` explicitly because they
//! hold the semantic context (was this a Create or Replace? what was the
//! pre-content?) that the trait layer cannot observe. The adapter is a
//! simple, auditable passthrough plus a pending buffer.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockMetadata, BlockSchema, BlockType, MemoryResult, MemorySearchResult,
    SearchOptions, SharedBlockInfo,
};
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockCreate, BlockWrite};

/// Wraps a concrete `MemoryStore` implementation and intercepts mutations
/// to record `BlockWrite` entries for the current turn. Session drains
/// the pending buffer at turn close; the drained writes populate
/// `TurnOutput.block_writes` and feed Phase 5's pseudo-message emitter.
///
/// The adapter holds the caller's `agent_id` at construction so
/// mutations can be attributed without threading auth context through
/// the `MemoryStore` trait. Author attribution is `Author::Agent(AgentAuthor)`
/// for handler-driven mutations; external paths (partner/scheduler)
/// would wrap their own adapter or use a different path — future work.
pub struct MemoryStoreAdapter {
    inner: Arc<dyn MemoryStore>,
    agent_id: String,
    pending: Arc<Mutex<Vec<BlockWrite>>>,
}

impl MemoryStoreAdapter {
    /// Construct an adapter wrapping the given store, attributing
    /// mutations to `agent_id`.
    pub fn new(inner: Arc<dyn MemoryStore>, agent_id: impl Into<String>) -> Self {
        Self {
            inner,
            agent_id: agent_id.into(),
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Handlers call this after a successful mutation to record the write.
    /// The `BlockWrite` should carry pre-write state (`previous_rendered_content`,
    /// `previous_content_hash`) when available; handler-level code knows best
    /// what pre-state it had access to.
    pub fn record_write(&self, write: BlockWrite) {
        self.pending.lock().unwrap().push(write);
    }

    /// Drain pending writes. Session calls at turn close.
    pub fn drain_pending(&self) -> Vec<BlockWrite> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    /// Agent id this adapter attributes mutations to.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Access the underlying store. Used when callers need the trait
    /// object directly (e.g. for operations that don't go through the
    /// adapter's delegated methods).
    pub fn inner(&self) -> &Arc<dyn MemoryStore> {
        &self.inner
    }
}

impl std::fmt::Debug for MemoryStoreAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStoreAdapter")
            .field("agent_id", &self.agent_id)
            .field(
                "pending_count",
                &self.pending.lock().map(|v| v.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

// Delegate all MemoryStore methods to inner. No write-interception at
// this level — handlers know the semantic context of each mutation and
// call record_write() themselves.
#[async_trait]
impl MemoryStore for MemoryStoreAdapter {
    async fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.inner.create_block(agent_id, create).await
    }

    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        self.inner.get_block(agent_id, label).await
    }

    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        self.inner.get_block_metadata(agent_id, label).await
    }

    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
        self.inner.list_blocks(agent_id).await
    }

    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        self.inner.list_blocks_by_type(agent_id, block_type).await
    }

    async fn list_all_blocks_by_label_prefix(
        &self,
        prefix: &str,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        self.inner.list_all_blocks_by_label_prefix(prefix).await
    }

    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.inner.delete_block(agent_id, label).await
    }

    async fn get_rendered_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>> {
        self.inner.get_rendered_content(agent_id, label).await
    }

    async fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.inner.persist_block(agent_id, label).await
    }

    fn mark_dirty(&self, agent_id: &str, label: &str) {
        self.inner.mark_dirty(agent_id, label);
    }

    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        self.inner
            .insert_archival(agent_id, content, metadata)
            .await
    }

    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        self.inner.search_archival(agent_id, query, limit).await
    }

    async fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        self.inner.delete_archival(id).await
    }

    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        self.inner.search(agent_id, query, options).await
    }

    async fn search_all(
        &self,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        self.inner.search_all(query, options).await
    }

    async fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        self.inner.list_shared_blocks(agent_id).await
    }

    async fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        self.inner
            .get_shared_block(requester_agent_id, owner_agent_id, label)
            .await
    }

    async fn set_block_pinned(
        &self,
        agent_id: &str,
        label: &str,
        pinned: bool,
    ) -> MemoryResult<()> {
        self.inner.set_block_pinned(agent_id, label, pinned).await
    }

    async fn set_block_type(
        &self,
        agent_id: &str,
        label: &str,
        block_type: BlockType,
    ) -> MemoryResult<()> {
        self.inner.set_block_type(agent_id, label, block_type).await
    }

    async fn update_block_schema(
        &self,
        agent_id: &str,
        label: &str,
        schema: BlockSchema,
    ) -> MemoryResult<()> {
        self.inner
            .update_block_schema(agent_id, label, schema)
            .await
    }

    async fn update_block_description(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
    ) -> MemoryResult<()> {
        self.inner
            .update_block_description(agent_id, label, description)
            .await
    }

    async fn undo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool> {
        self.inner.undo_block(agent_id, label).await
    }

    async fn redo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool> {
        self.inner.redo_block(agent_id, label).await
    }

    async fn undo_depth(&self, agent_id: &str, label: &str) -> MemoryResult<usize> {
        self.inner.undo_depth(agent_id, label).await
    }

    async fn redo_depth(&self, agent_id: &str, label: &str) -> MemoryResult<usize> {
        self.inner.redo_depth(agent_id, label).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::InMemoryMemoryStore;
    use pattern_core::types::memory_types::BlockType;
    use pattern_core::types::block::BlockWriteKind;
    use pattern_core::types::origin::{AgentAuthor, Author};
    use smol_str::SmolStr;

    fn make_block_write(handle: &str, kind: BlockWriteKind) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_test_01"),
            block_type: BlockType::Working,
            rendered_content: format!("content for {handle}"),
            kind,
            previous_content_hash: None,
            previous_rendered_content: None,
            at: jiff::Timestamp::now(),
            author: Author::Agent(AgentAuthor {
                agent_id: SmolStr::new("test-agent"),
            }),
        }
    }

    #[test]
    fn record_write_and_drain_roundtrip() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter = MemoryStoreAdapter::new(store, "agent-a");

        adapter.record_write(make_block_write("block1", BlockWriteKind::Created));
        adapter.record_write(make_block_write("block2", BlockWriteKind::Updated));
        adapter.record_write(make_block_write("block3", BlockWriteKind::Appended));

        let drained = adapter.drain_pending();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].handle.as_str(), "block1");
        assert_eq!(drained[1].handle.as_str(), "block2");
        assert_eq!(drained[2].handle.as_str(), "block3");

        // Subsequent drain returns empty.
        let again = adapter.drain_pending();
        assert!(again.is_empty());
    }

    #[tokio::test]
    async fn adapter_delegates_create_block() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter = MemoryStoreAdapter::new(store, "agent-a");

        let create = BlockCreate::new("notes", BlockType::Working, BlockSchema::text());
        let doc = adapter.create_block("agent-a", create).await.unwrap();
        assert_eq!(doc.metadata().label, "notes");

        // Verify read-through also works.
        let fetched = adapter.get_block("agent-a", "notes").await.unwrap();
        assert!(fetched.is_some());
    }

    #[test]
    fn pending_buffer_isolated_per_adapter() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter_a = MemoryStoreAdapter::new(store.clone(), "agent-a");
        let adapter_b = MemoryStoreAdapter::new(store, "agent-b");

        adapter_a.record_write(make_block_write("block-a", BlockWriteKind::Created));
        adapter_b.record_write(make_block_write("block-b", BlockWriteKind::Created));

        let a_writes = adapter_a.drain_pending();
        let b_writes = adapter_b.drain_pending();
        assert_eq!(a_writes.len(), 1);
        assert_eq!(a_writes[0].handle.as_str(), "block-a");
        assert_eq!(b_writes.len(), 1);
        assert_eq!(b_writes[0].handle.as_str(), "block-b");
    }

    #[test]
    fn debug_impl_does_not_panic() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter = MemoryStoreAdapter::new(store, "agent-a");
        let debug_str = format!("{adapter:?}");
        assert!(debug_str.contains("agent-a"));
    }
}
