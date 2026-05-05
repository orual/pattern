//! Tiny in-memory [`MemoryStore`] test double.
//!
//! Just enough fidelity to let MemoryHandler round-trip writes / reads
//! in integration tests. Only the handler-touched methods
//! (`create_block`, `get_block`, `get_rendered_content`, `mark_dirty`,
//! `persist_block`, `update_block_metadata`) carry real logic; the remainder
//! either return empty results (list/search families) or `unimplemented!`
//! for operations the handler never invokes.

use std::collections::HashMap;
use std::sync::Mutex;

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::ids::new_id;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, MemoryResult,
    MemorySearchResult, MemorySearchScope, Scope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};
use serde_json::Value as JsonValue;

/// Key used by the in-memory store: `(scope, label)`.
type Key = (Scope, String);

#[derive(Debug)]
struct BlockRecord {
    document: StructuredDocument,
}

#[derive(Debug, Clone)]
struct ArchivalRecord {
    scope: Scope,
    id: String,
    content: String,
    metadata: Option<JsonValue>,
}

#[derive(Debug, Default)]
pub struct InMemoryMemoryStore {
    blocks: Mutex<HashMap<Key, BlockRecord>>,
    archival: Mutex<Vec<ArchivalRecord>>,
}

impl InMemoryMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MemoryStore for InMemoryMemoryStore {
    fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.mark_dirty(scope, label)?;
        self.persist_block(scope, label)
    }

    fn create_or_replace_block(&self, scope: &Scope, create: BlockCreate) -> MemoryResult<StructuredDocument> {
        let _ = self.delete_block(scope, &create.label);
        self.create_block(scope, create)
    }

    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let mut metadata = BlockMetadata::standalone(create.schema.clone());
        // Use the encoded scope key ("global:<id>" / "local:<id>") to match
        // MemoryCache's storage convention and BlockFilter::by_scope semantics.
        metadata.agent_id = scope.to_db_key();
        metadata.label = create.label.clone();
        metadata.description = create.description.clone();
        metadata.block_type = create.block_type;
        metadata.char_limit = create.char_limit;
        metadata.permission = create.permission;
        let doc = StructuredDocument::new_with_metadata(metadata, Some(scope.id().to_string()));
        let mut guard = self.blocks.lock().unwrap();
        guard.insert(
            (scope.clone(), create.label),
            BlockRecord {
                document: doc.clone(),
            },
        );
        Ok(doc)
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(scope.clone(), label.to_string()))
            .map(|r| r.document.clone()))
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(scope.clone(), label.to_string()))
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

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        guard.remove(&(scope.clone(), label.to_string()));
        Ok(())
    }

    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        let guard = self.blocks.lock().unwrap();
        Ok(guard
            .get(&(scope.clone(), label.to_string()))
            .map(|r| r.document.text_content()))
    }

    fn persist_block(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> {
        Ok(())
    }

    fn mark_dirty(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> {
        Ok(())
    }

    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        let id = new_id().to_string();
        let mut guard = self.archival.lock().unwrap();
        guard.push(ArchivalRecord {
            scope: scope.clone(),
            id: id.clone(),
            content: content.to_string(),
            metadata,
        });
        Ok(id)
    }

    fn search_archival(
        &self,
        scope: &Scope,
        query: &str,
        n: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        let guard = self.archival.lock().unwrap();
        let q_lower = query.to_lowercase();
        let mut hits: Vec<ArchivalEntry> = guard
            .iter()
            .filter(|r| &r.scope == scope && r.content.to_lowercase().contains(&q_lower))
            .take(n)
            .map(|r| ArchivalEntry {
                id: r.id.clone(),
                agent_id: r.scope.id().to_string(),
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

    fn list_shared_blocks(&self, _scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        Ok(vec![])
    }

    fn get_shared_block(
        &self,
        _requester: &Scope,
        _owner: &Scope,
        _label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        Ok(None)
    }

    fn update_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        let mut guard = self.blocks.lock().unwrap();
        match guard.get_mut(&(scope.clone(), label.to_string())) {
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
            None => Err(
                pattern_core::types::memory_types::MemoryError::WriteToMissingBlock {
                    scope: scope.clone(),
                    label: label.to_string(),
                    op: "update_block_metadata",
                },
            ),
        }
    }

    fn undo_redo(&self, _scope: &Scope, _l: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
        Ok(false)
    }

    fn history_depth(&self, _scope: &Scope, _l: &str) -> MemoryResult<UndoRedoDepth> {
        Ok(UndoRedoDepth { undo: 0, redo: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::BlockSchema;

    #[test]
    fn create_block_returns_arc_shared_loro_doc() {
        let store = InMemoryMemoryStore::new();

        let create = BlockCreate::new(
            "notes",
            pattern_core::types::memory_types::MemoryBlockType::Working,
            BlockSchema::text(),
        );

        let scope = Scope::global("agent-test");
        let returned = store
            .create_block(&scope, create)
            .expect("create_block should succeed");

        returned
            .set_text("mutated content", false)
            .expect("set_text should succeed");

        let stored = store
            .get_block(&scope, "notes")
            .expect("get_block should succeed")
            .expect("block should exist");

        assert_eq!(
            stored.text_content(),
            "mutated content",
            "mutation on returned doc must propagate to stored doc via Arc-shared LoroDoc"
        );
    }

    /// AC1.1: Local("x") and Global("x") are distinct keyspaces.
    #[test]
    fn local_and_global_blocks_with_same_label_coexist() {
        let store = InMemoryMemoryStore::new();
        let local = Scope::local("pattern");
        let global = Scope::global("pattern");

        store
            .create_block(
                &local,
                BlockCreate::new(
                    "scratchpad",
                    pattern_core::types::memory_types::MemoryBlockType::Working,
                    BlockSchema::text(),
                ),
            )
            .unwrap()
            .set_text("project content", false)
            .unwrap();

        store
            .create_block(
                &global,
                BlockCreate::new(
                    "scratchpad",
                    pattern_core::types::memory_types::MemoryBlockType::Working,
                    BlockSchema::text(),
                ),
            )
            .unwrap()
            .set_text("persona content", false)
            .unwrap();

        let local_doc = store.get_block(&local, "scratchpad").unwrap().unwrap();
        let global_doc = store.get_block(&global, "scratchpad").unwrap().unwrap();

        assert_eq!(local_doc.text_content(), "project content");
        assert_eq!(global_doc.text_content(), "persona content");
    }
}
