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

use serde_json::Value as JsonValue;

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockCreate, BlockWrite};
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, MemoryResult,
    MemorySearchResult, MemorySearchScope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};
use pattern_core::types::message::MessageAttachment;

/// Wraps a concrete `MemoryStore` implementation and intercepts mutations
/// to record `BlockWrite` entries for the current turn. Session drains
/// the pending buffer at turn close; the drained writes populate
/// `TurnOutput.block_writes` and feed Phase 5's pseudo-message emitter.
///
/// The adapter holds the caller's `agent_id` at construction so
/// mutations can be attributed without threading auth context through
/// the `MemoryStore` trait.
pub struct MemoryStoreAdapter {
    inner: Arc<dyn MemoryStore>,
    agent_id: String,
    pending: Arc<Mutex<Vec<BlockWrite>>>,
    /// Pending [`MessageAttachment`]s queued by handlers (e.g. plugin
    /// auto-install events emitting `SkillAvailable`). Drained at turn
    /// close into the wire turn's tool_result_msg or assistant_msg
    /// `attachments` vec, then persisted via the splice machinery.
    /// Write-once: once attached, never updated (cache-stable).
    pending_attachments: Arc<Mutex<Vec<MessageAttachment>>>,
}

impl MemoryStoreAdapter {
    /// Construct an adapter wrapping the given store, attributing
    /// mutations to `agent_id`.
    pub fn new(inner: Arc<dyn MemoryStore>, agent_id: impl Into<String>) -> Self {
        Self {
            inner,
            agent_id: agent_id.into(),
            pending: Arc::new(Mutex::new(Vec::new())),
            pending_attachments: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Handlers call this after a successful mutation to record the write.
    pub fn record_write(&self, write: BlockWrite) {
        self.pending.lock().unwrap().push(write);
    }

    /// Drain pending writes. Session calls at turn close.
    pub fn drain_pending(&self) -> Vec<BlockWrite> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    /// Handlers call this to queue a [`MessageAttachment`] for the current
    /// wire turn. The session drains the buffer at turn close and attaches
    /// each entry onto the appropriate message in
    /// [`pattern_core::types::turn::TurnOutput::messages`] (preferring the
    /// last message — typically a `tool_result` for handler-originated
    /// events). The splice machinery in `compose_request_for_turn` then
    /// renders attachments as a single grouped `<system-reminder>` block
    /// onto the wire on subsequent compose cycles.
    ///
    /// Write-once contract: once attached to a Message, the attachment is
    /// never updated. This keeps wire bytes stable across turns and the
    /// cache warm.
    pub fn record_attachment(&self, attachment: MessageAttachment) {
        self.pending_attachments.lock().unwrap().push(attachment);
    }

    /// Drain pending attachments. Session calls at turn close.
    pub fn drain_pending_attachments(&self) -> Vec<MessageAttachment> {
        std::mem::take(&mut *self.pending_attachments.lock().unwrap())
    }

    /// Agent id this adapter attributes mutations to.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Access the underlying store.
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
            .field(
                "pending_attachments_count",
                &self
                    .pending_attachments
                    .lock()
                    .map(|v| v.len())
                    .unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

// Delegate all MemoryStore methods to inner. No write-interception at
// this level — handlers know the semantic context of each mutation and
// call record_write() themselves.
impl MemoryStore for MemoryStoreAdapter {
    fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.inner.create_block(agent_id, create)
    }

    fn get_block(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        self.inner.get_block(agent_id, label)
    }

    fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        self.inner.get_block_metadata(agent_id, label)
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        self.inner.list_blocks(filter)
    }

    fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.inner.delete_block(agent_id, label)
    }

    fn get_rendered_content(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        self.inner.get_rendered_content(agent_id, label)
    }

    fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.inner.persist_block(agent_id, label)
    }

    fn mark_dirty(&self, agent_id: &str, label: &str) {
        self.inner.mark_dirty(agent_id, label);
    }

    fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        self.inner.insert_archival(agent_id, content, metadata)
    }

    fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        self.inner.search_archival(agent_id, query, limit)
    }

    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        self.inner.delete_archival(id)
    }

    fn search(
        &self,
        query: &str,
        options: SearchOptions,
        scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        self.inner.search(query, options, scope)
    }

    fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        self.inner.list_shared_blocks(agent_id)
    }

    fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        self.inner
            .get_shared_block(requester_agent_id, owner_agent_id, label)
    }

    fn update_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        self.inner.update_block_metadata(agent_id, label, patch)
    }

    fn undo_redo(&self, agent_id: &str, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        self.inner.undo_redo(agent_id, label, op)
    }

    fn history_depth(&self, agent_id: &str, label: &str) -> MemoryResult<UndoRedoDepth> {
        self.inner.history_depth(agent_id, label)
    }

    fn has_shared_blocks_with(&self, caller: &str, target: &str) -> MemoryResult<bool> {
        self.inner.has_shared_blocks_with(caller, target)
    }

    fn shares_group_with(&self, caller: &str, target: &str) -> MemoryResult<bool> {
        self.inner.shares_group_with(caller, target)
    }

    fn list_constellation_agent_ids(&self) -> MemoryResult<Vec<String>> {
        self.inner.list_constellation_agent_ids()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::InMemoryMemoryStore;
    use pattern_core::types::block::BlockWriteKind;
    use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
    use pattern_core::types::origin::{AgentAuthor, Author};
    use smol_str::SmolStr;

    fn make_block_write(handle: &str, kind: BlockWriteKind) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_test_01"),
            block_type: MemoryBlockType::Working,
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

    #[test]
    fn adapter_delegates_create_block() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter = MemoryStoreAdapter::new(store, "agent-a");

        let create = BlockCreate::new("notes", MemoryBlockType::Working, BlockSchema::text());
        let doc = adapter.create_block("agent-a", create).unwrap();
        assert_eq!(doc.metadata().label, "notes");

        // Verify read-through also works.
        let fetched = adapter.get_block("agent-a", "notes").unwrap();
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

    #[test]
    fn record_attachment_and_drain_roundtrip() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter = MemoryStoreAdapter::new(store, "agent-a");

        adapter.record_attachment(MessageAttachment::Custom {
            content: "hello".to_string(),
        });
        adapter.record_attachment(MessageAttachment::SkillAvailable {
            handle: SmolStr::new("skill-1"),
            name: "demo".to_string(),
            trust_tier: pattern_core::types::memory_types::SkillTrustTier::ProjectLocal,
            description: None,
            keywords: vec![],
        });

        let drained = adapter.drain_pending_attachments();
        assert_eq!(drained.len(), 2);
        assert!(matches!(drained[0], MessageAttachment::Custom { .. }));
        assert!(matches!(
            drained[1],
            MessageAttachment::SkillAvailable { .. }
        ));

        // Subsequent drain returns empty.
        assert!(adapter.drain_pending_attachments().is_empty());
    }

    #[test]
    fn attachment_buffer_isolated_per_adapter() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter_a = MemoryStoreAdapter::new(store.clone(), "agent-a");
        let adapter_b = MemoryStoreAdapter::new(store, "agent-b");

        adapter_a.record_attachment(MessageAttachment::Custom {
            content: "from a".to_string(),
        });
        adapter_b.record_attachment(MessageAttachment::Custom {
            content: "from b".to_string(),
        });

        let a = adapter_a.drain_pending_attachments();
        let b = adapter_b.drain_pending_attachments();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        if let MessageAttachment::Custom { content } = &a[0] {
            assert_eq!(content, "from a");
        } else {
            panic!("expected Custom");
        }
    }
}
