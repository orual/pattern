//! Tiny in-memory [`MemoryStore`] test double.
//!
//! Just enough fidelity to let MemoryHandler round-trip writes / reads
//! in integration tests. Only the handler-touched methods
//! (`create_block`, `get_block`, `get_rendered_content`, `mark_dirty`,
//! `persist_block`, `update_block_metadata`) carry real logic; the remainder
//! either return empty results (list/search families) or `unimplemented!`
//! for operations the handler never invokes.
//!
//! This double is deliberately minimal: it backs tests, not production
//! behaviour. If a future integration test needs one of the currently
//! `unimplemented!` methods, implement it here; do not add the real
//! pattern_core `MemoryCache` as a dependency — that would reintroduce
//! the pattern_runtime -> pattern_core-concrete coupling Phase 2
//! forbids (trait-object dispatch only).

use std::collections::HashMap;
use std::sync::Mutex;

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::ids::new_id;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, MemoryResult,
    MemorySearchResult, MemorySearchScope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};
use serde_json::Value as JsonValue;

/// Key used by the in-memory store: `(agent_id, label)` — the shape the
/// `MemoryStore` trait operates on.
type Key = (String, String);

/// Internal bookkeeping for one block.
#[derive(Debug)]
struct BlockRecord {
    document: StructuredDocument,
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

impl MemoryStore for InMemoryMemoryStore {
    fn create_block(
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
        // Honor the caller-supplied permission instead of leaving the default.
        metadata.permission = create.permission;
        let doc = StructuredDocument::new_with_metadata(metadata, Some(agent_id.to_string()));
        let mut guard = self.blocks.lock().unwrap();
        guard.insert(
            (agent_id.to_string(), create.label),
            BlockRecord {
                document: doc.clone(),
            },
        );
        Ok(doc)
    }

    fn get_block(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|r| r.document.clone()))
    }

    fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|r| r.document.metadata().clone()))
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        let mut results: Vec<BlockMetadata> = guard
            .values()
            .map(|r| r.document.metadata().clone())
            .collect();

        if let Some(ref agent) = filter.agent_id {
            results.retain(|m| m.agent_id == *agent);
        }
        if let Some(bt) = filter.block_type {
            results.retain(|m| m.block_type == bt);
        }
        if let Some(ref prefix) = filter.label_prefix {
            results.retain(|m| m.label.starts_with(prefix.as_str()));
        }
        Ok(results)
    }

    fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        guard.remove(&(agent_id.to_string(), label.to_string()));
        Ok(())
    }

    fn get_rendered_content(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(agent_id.to_string(), label.to_string()))
            .map(|r| r.document.text_content()))
    }

    fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
        // No-op: writes land directly via StructuredDocument::set_text.
        Ok(())
    }

    fn mark_dirty(&self, _agent_id: &str, _label: &str) {
        // No-op: the double has no "dirty" bookkeeping.
    }

    fn insert_archival(
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

    fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        n: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
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
                created_at: Default::default(),
            })
            .collect();
        hits.reverse();
        Ok(hits)
    }

    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        let mut guard = self.archival.lock().unwrap();
        guard.retain(|r| r.id != id);
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

    fn list_shared_blocks(&self, _a: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        Ok(vec![])
    }

    fn get_shared_block(
        &self,
        _r: &str,
        _o: &str,
        _l: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        Ok(None)
    }

    fn update_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        match guard.get_mut(&(agent_id.to_string(), label.to_string())) {
            Some(r) => {
                if let Some(pinned) = patch.pinned {
                    r.document.metadata_mut().pinned = pinned;
                }
                if let Some(bt) = patch.block_type {
                    r.document.metadata_mut().block_type = bt;
                }
                if let Some(ref schema) = patch.schema {
                    r.document.metadata_mut().schema = schema.clone();
                }
                if let Some(ref description) = patch.description {
                    r.document.metadata_mut().description = description.clone();
                }
                Ok(())
            }
            None => Err(pattern_core::types::memory_types::MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            }),
        }
    }

    fn undo_redo(&self, _a: &str, _l: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
        Ok(false)
    }

    fn history_depth(&self, _a: &str, _l: &str) -> MemoryResult<UndoRedoDepth> {
        Ok(UndoRedoDepth { undo: 0, redo: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::BlockSchema;

    /// Verify that `create_block` returns a doc whose internal `LoroDoc` is
    /// Arc-shared with the copy stored in the map.
    #[test]
    fn create_block_returns_arc_shared_loro_doc() {
        let store = InMemoryMemoryStore::new();

        let create = BlockCreate::new(
            "notes",
            pattern_core::types::memory_types::MemoryBlockType::Working,
            BlockSchema::text(),
        );

        let returned = store
            .create_block("agent-test", create)
            .expect("create_block should succeed");

        // Mutate content via the returned handle.
        returned
            .set_text("mutated content", false)
            .expect("set_text should succeed");

        // Re-read from the map — mutation must be visible.
        let stored = store
            .get_block("agent-test", "notes")
            .expect("get_block should succeed")
            .expect("block should exist");

        assert_eq!(
            stored.text_content(),
            "mutated content",
            "mutation on returned doc must propagate to stored doc via Arc-shared LoroDoc"
        );
    }
}
