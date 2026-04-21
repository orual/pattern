//! Shared test helpers for `pattern_memory` tests.
//!
//! Gated behind `#[cfg(any(test, feature = "test-support"))]` so that none of
//! this code reaches production builds. Enable the `test-support` feature in
//! downstream crates that need `ScopeTestStore` in their own integration tests.

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, BlockSchema, MemoryResult,
    MemorySearchResult, MemorySearchScope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};
use serde_json::Value as JsonValue;

/// Minimal in-memory [`MemoryStore`] for scope policy tests.
///
/// Stores blocks in a `HashMap<(agent_id, label), (StructuredDocument, rendered_content)>`
/// and archival entries in a `Vec<ArchivalEntry>`. All operations are
/// synchronous and use `std::sync::Mutex` for interior mutability, so the
/// store is `Send + Sync` without `async`.
///
/// Use [`ScopeTestStore::seed`] to pre-populate blocks and
/// [`ScopeTestStore::seed_archival`] to pre-populate archival entries.
///
/// `search_archival` returns all entries for the given `agent_id`, up to
/// `limit` — the query argument is deliberately ignored because these tests
/// exercise scope routing, not full-text search semantics.
#[derive(Debug, Default)]
pub struct ScopeTestStore {
    blocks:
        std::sync::Mutex<std::collections::HashMap<(String, String), (StructuredDocument, String)>>,
    archival: std::sync::Mutex<Vec<ArchivalEntry>>,
}

impl ScopeTestStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a core/working block directly into the store.
    ///
    /// Creates a standalone text block with `agent_id` and `label` set, with
    /// the rendered content initialised to `content`.
    pub fn seed(&self, agent_id: &str, label: &str, content: &str) {
        let mut meta = BlockMetadata::standalone(BlockSchema::text());
        meta.agent_id = agent_id.to_string();
        meta.label = label.to_string();
        let doc = StructuredDocument::new_with_metadata(meta, None);
        doc.set_text(content, false).unwrap();
        self.blocks.lock().unwrap().insert(
            (agent_id.to_string(), label.to_string()),
            (doc, content.to_string()),
        );
    }

    /// Seed an archival entry directly, bypassing `insert_archival`.
    ///
    /// Useful when the test needs to set `agent_id` precisely (e.g. to
    /// pre-populate entries for the persona agent before creating the scope).
    pub fn seed_archival(&self, agent_id: &str, id: &str, content: &str) {
        self.archival
            .lock()
            .unwrap()
            .push(ArchivalEntry {
                id: id.to_string(),
                agent_id: agent_id.to_string(),
                content: content.to_string(),
                metadata: None,
                created_at: chrono::Utc::now(),
            });
    }
}

impl MemoryStore for ScopeTestStore {
    fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let mut meta = BlockMetadata::standalone(create.schema.clone());
        meta.agent_id = agent_id.to_string();
        meta.label = create.label.clone();
        meta.block_type = create.block_type;
        let doc = StructuredDocument::new_with_metadata(meta, None);
        self.blocks.lock().unwrap().insert(
            (agent_id.to_string(), create.label.clone()),
            (doc.clone(), String::new()),
        );
        Ok(doc)
    }

    fn get_block(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|(doc, _)| doc.clone()))
    }

    fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|(doc, _)| doc.metadata().clone()))
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        let mut results = Vec::new();
        for ((aid, _), (doc, _)) in guard.iter() {
            if let Some(ref fa) = filter.agent_id
                && aid != fa
            {
                continue;
            }
            let meta = doc.metadata().clone();
            if let Some(ref bt) = filter.block_type
                && &meta.block_type != bt
            {
                continue;
            }
            if let Some(ref pfx) = filter.label_prefix
                && !meta.label.starts_with(pfx.as_str())
            {
                continue;
            }
            results.push(meta);
        }
        Ok(results)
    }

    fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.blocks
            .lock()
            .unwrap()
            .remove(&(agent_id.to_string(), label.to_string()));
        Ok(())
    }

    fn get_rendered_content(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|(_, content)| content.clone()))
    }

    fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
        Ok(())
    }

    fn mark_dirty(&self, _agent_id: &str, _label: &str) {}

    fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        let id = format!("archival-{}", self.archival.lock().unwrap().len());
        self.archival.lock().unwrap().push(ArchivalEntry {
            id: id.clone(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            metadata,
            created_at: chrono::Utc::now(),
        });
        Ok(id)
    }

    fn search_archival(
        &self,
        agent_id: &str,
        _query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        // Naive stub: return all entries for the given agent_id, up to limit.
        // The query parameter is deliberately ignored — these tests cover
        // scope routing only, not full-text search semantics.
        let guard = self.archival.lock().unwrap();
        let results: Vec<_> = guard
            .iter()
            .filter(|e| e.agent_id == agent_id)
            .take(limit)
            .cloned()
            .collect();
        Ok(results)
    }

    fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
        Ok(())
    }

    fn search(
        &self,
        _query: &str,
        _options: SearchOptions,
        _scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        Ok(vec![])
    }

    fn list_shared_blocks(&self, _agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        Ok(vec![])
    }

    fn get_shared_block(
        &self,
        _requester: &str,
        _owner: &str,
        _label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        Ok(None)
    }

    fn update_block_metadata(
        &self,
        _agent_id: &str,
        _label: &str,
        _patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        Ok(())
    }

    fn undo_redo(&self, _agent_id: &str, _label: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
        Ok(false)
    }

    fn history_depth(&self, _agent_id: &str, _label: &str) -> MemoryResult<UndoRedoDepth> {
        Ok(UndoRedoDepth { undo: 0, redo: 0 })
    }
}
