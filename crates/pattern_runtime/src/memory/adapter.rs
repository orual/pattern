//! Thin delegating wrapper over `Arc<dyn MemoryStore>` with a pending
//! `BlockWrite` buffer.

use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockCreate, BlockWrite};
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, MemoryResult,
    MemorySearchResult, MemorySearchScope, Scope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};
use pattern_core::types::message::MessageAttachment;

/// Wraps a concrete `MemoryStore` implementation and intercepts mutations
/// to record `BlockWrite` entries for the current turn.
pub struct MemoryStoreAdapter {
    inner: Arc<dyn MemoryStore>,
    agent_id: String,
    pending: Arc<Mutex<Vec<BlockWrite>>>,
    pending_attachments: Arc<Mutex<Vec<MessageAttachment>>>,
}

impl MemoryStoreAdapter {
    pub fn new(inner: Arc<dyn MemoryStore>, agent_id: impl Into<String>) -> Self {
        Self {
            inner,
            agent_id: agent_id.into(),
            pending: Arc::new(Mutex::new(Vec::new())),
            pending_attachments: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn record_write(&self, write: BlockWrite) {
        self.pending.lock().unwrap().push(write);
    }

    pub fn drain_pending(&self) -> Vec<BlockWrite> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    pub fn record_attachment(&self, attachment: MessageAttachment) {
        self.pending_attachments.lock().unwrap().push(attachment);
    }

    pub fn drain_pending_attachments(&self) -> Vec<MessageAttachment> {
        std::mem::take(&mut *self.pending_attachments.lock().unwrap())
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

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

impl MemoryStore for MemoryStoreAdapter {
    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.inner.create_block(scope, create)
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        self.inner.get_block(scope, label)
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        self.inner.get_block_metadata(scope, label)
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        self.inner.list_blocks(filter)
    }

    fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.inner.commit_write(scope, label)
    }

    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: pattern_core::types::block::BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.inner.create_or_replace_block(scope, create)
    }

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.inner.delete_block(scope, label)
    }

    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        self.inner.get_rendered_content(scope, label)
    }

    fn persist_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.inner.persist_block(scope, label)
    }

    fn mark_dirty(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.inner.mark_dirty(scope, label)
    }

    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        self.inner.insert_archival(scope, content, metadata)
    }

    fn search_archival(
        &self,
        scope: &Scope,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        self.inner.search_archival(scope, query, limit)
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

    fn list_shared_blocks(&self, scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        self.inner.list_shared_blocks(scope)
    }

    fn get_shared_block(
        &self,
        requester: &Scope,
        owner: &Scope,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        self.inner.get_shared_block(requester, owner, label)
    }

    fn update_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        self.inner.update_block_metadata(scope, label, patch)
    }

    fn undo_redo(&self, scope: &Scope, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        self.inner.undo_redo(scope, label, op)
    }

    fn history_depth(&self, scope: &Scope, label: &str) -> MemoryResult<UndoRedoDepth> {
        self.inner.history_depth(scope, label)
    }

    fn has_shared_blocks_with(&self, caller: &Scope, target: &Scope) -> MemoryResult<bool> {
        self.inner.has_shared_blocks_with(caller, target)
    }

    fn list_constellation_scopes(&self) -> MemoryResult<Vec<Scope>> {
        self.inner.list_constellation_scopes()
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

        let again = adapter.drain_pending();
        assert!(again.is_empty());
    }

    #[test]
    fn adapter_delegates_create_block() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let adapter = MemoryStoreAdapter::new(store, "agent-a");

        let create = BlockCreate::new("notes", MemoryBlockType::Working, BlockSchema::text());
        let scope = Scope::global("agent-a");
        let doc = adapter.create_block(&scope, create).unwrap();
        assert_eq!(doc.metadata().label, "notes");

        let fetched = adapter.get_block(&scope, "notes").unwrap();
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
