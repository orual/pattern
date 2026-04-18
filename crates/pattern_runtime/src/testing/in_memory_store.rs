//! Tiny in-memory [`MemoryStore`] test double.
//!
//! Just enough fidelity to let MemoryHandler round-trip writes / reads
//! in Phase 3 integration tests. Only the handler-touched methods
//! (`create_block`, `get_block`, `get_rendered_content`, `mark_dirty`,
//! `persist_block`, `set_block_type`) carry real logic; the remainder
//! either return empty results (list/search families) or `unimplemented!`
//! for operations Phase 3's handler never invokes.
//!
//! This double is deliberately minimal: it backs tests, not production
//! behaviour. If a future integration test needs one of the currently
//! `unimplemented!` methods, implement it here; do not add the real
//! pattern_core `MemoryCache` as a dependency — that would reintroduce
//! the pattern_runtime → pattern_core-concrete coupling Phase 2
//! forbids (trait-object dispatch only).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use pattern_core::memory::{
    ArchivalEntry, BlockMetadata, BlockSchema, BlockType, MemoryResult, MemorySearchResult,
    SearchOptions, SharedBlockInfo, StructuredDocument,
};
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::ids::new_id;
use serde_json::Value as JsonValue;

/// Key used by the in-memory store: `(agent_id, label)` — the shape the
/// `MemoryStore` trait operates on.
type Key = (String, String);

/// Internal bookkeeping for one block.
#[derive(Debug)]
struct BlockRecord {
    document: StructuredDocument,
    block_type: BlockType,
}

/// An archival entry indexed by id. `(agent_id, id, content, metadata)`.
/// Minimal — no FTS; `search_archival` walks all entries for substring
/// matches.
#[derive(Debug, Clone)]
struct ArchivalRecord {
    agent_id: String,
    id: String,
    content: String,
    metadata: Option<JsonValue>,
}

/// In-memory MemoryStore double. Cloneable via `Arc`; internal state is
/// `Mutex<HashMap<_, _>>`.
#[derive(Debug, Default)]
pub struct InMemoryMemoryStore {
    blocks: Mutex<HashMap<Key, BlockRecord>>,
    archival: Mutex<Vec<ArchivalRecord>>,
}

impl InMemoryMemoryStore {
    /// Fresh empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let mut metadata = BlockMetadata::standalone(create.schema.clone());
        metadata.agent_id = agent_id.to_string();
        metadata.label = create.label.clone();
        metadata.description = create.description.clone();
        metadata.block_type = create.block_type;
        metadata.char_limit = create.char_limit;
        let doc = StructuredDocument::new_with_metadata(metadata, Some(agent_id.to_string()));
        let mut guard = self.blocks.lock().unwrap();
        guard.insert(
            (agent_id.to_string(), create.label),
            BlockRecord {
                document: doc.clone(),
                block_type: create.block_type,
            },
        );
        Ok(doc)
    }

    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|r| r.document.clone()))
    }

    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|r| r.document.metadata().clone()))
    }

    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .iter()
            .filter(|((a, _), _)| a == agent_id)
            .map(|(_, r)| r.document.metadata().clone())
            .collect())
    }

    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .iter()
            .filter(|((a, _), r)| a == agent_id && r.block_type == block_type)
            .map(|(_, r)| r.document.metadata().clone())
            .collect())
    }

    async fn list_all_blocks_by_label_prefix(
        &self,
        prefix: &str,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .iter()
            .filter(|((_, l), _)| l.starts_with(prefix))
            .map(|(_, r)| r.document.metadata().clone())
            .collect())
    }

    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        guard.remove(&(agent_id.to_string(), label.to_string()));
        Ok(())
    }

    async fn get_rendered_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|r| r.document.text_content()))
    }

    async fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
        // No-op: writes land directly via StructuredDocument::set_text,
        // which mutates the Arc-shared LoroDoc. Nothing to flush.
        Ok(())
    }

    fn mark_dirty(&self, _agent_id: &str, _label: &str) {
        // No-op: the double has no "dirty" bookkeeping.
    }

    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        let id = new_id().to_string();
        let mut guard = self.archival.lock().unwrap();
        guard.push(ArchivalRecord {
            agent_id: agent_id.to_string(),
            id: id.clone(),
            content: content.to_string(),
            metadata,
        });
        Ok(id)
    }
    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        n: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        // Naive substring scan. No FTS/BM25 — just case-insensitive
        // contains(). Good enough for test fidelity; real store uses
        // pattern_db's FTS5 index.
        let guard = self.archival.lock().unwrap();
        let q_lower = query.to_lowercase();
        let mut hits: Vec<ArchivalEntry> = guard
            .iter()
            .filter(|r| r.agent_id == agent_id && r.content.to_lowercase().contains(&q_lower))
            .take(n)
            .map(|r| ArchivalEntry {
                id: r.id.clone(),
                agent_id: r.agent_id.clone(),
                content: r.content.clone(),
                metadata: r.metadata.clone(),
                // Default is epoch; stub doesn't track real timestamps.
                created_at: Default::default(),
            })
            .collect();
        // Keep most-recent-first (insertion order is append; reverse gives recency).
        hits.reverse();
        Ok(hits)
    }
    async fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        let mut guard = self.archival.lock().unwrap();
        guard.retain(|r| r.id != id);
        Ok(())
    }
    async fn search(
        &self,
        _a: &str,
        _q: &str,
        _o: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        Ok(vec![])
    }
    async fn search_all(
        &self,
        _q: &str,
        _o: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        Ok(vec![])
    }
    async fn list_shared_blocks(&self, _a: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        Ok(vec![])
    }
    async fn get_shared_block(
        &self,
        _r: &str,
        _o: &str,
        _l: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        Ok(None)
    }
    async fn set_block_pinned(
        &self,
        agent_id: &str,
        label: &str,
        pinned: bool,
    ) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        if let Some(r) = guard.get_mut(&(agent_id.to_string(), label.to_string())) {
            // StructuredDocument's metadata is Arc-shared with the cached
            // document — mutating here propagates to every holder of the
            // Arc (matching the real cache's live-share semantics).
            r.document.metadata_mut().pinned = pinned;
        }
        Ok(())
    }
    async fn set_block_type(
        &self,
        agent_id: &str,
        label: &str,
        block_type: BlockType,
    ) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        if let Some(r) = guard.get_mut(&(agent_id.to_string(), label.to_string())) {
            r.block_type = block_type;
        }
        Ok(())
    }
    async fn update_block_schema(
        &self,
        agent_id: &str,
        label: &str,
        schema: BlockSchema,
    ) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        if let Some(r) = guard.get_mut(&(agent_id.to_string(), label.to_string())) {
            r.document.metadata_mut().schema = schema;
        }
        Ok(())
    }
    async fn update_block_description(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
    ) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        match guard.get_mut(&(agent_id.to_string(), label.to_string())) {
            Some(r) => {
                r.document.metadata_mut().description = description.to_string();
                Ok(())
            }
            None => Err(pattern_core::memory::MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            }),
        }
    }
    async fn undo_block(&self, _a: &str, _l: &str) -> MemoryResult<bool> {
        Ok(false)
    }
    async fn redo_block(&self, _a: &str, _l: &str) -> MemoryResult<bool> {
        Ok(false)
    }
    async fn undo_depth(&self, _a: &str, _l: &str) -> MemoryResult<usize> {
        Ok(0)
    }
    async fn redo_depth(&self, _a: &str, _l: &str) -> MemoryResult<usize> {
        Ok(0)
    }
}
