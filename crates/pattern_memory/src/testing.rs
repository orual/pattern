//! Shared test helpers for `pattern_memory` tests.

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, BlockSchema, MemoryResult,
    MemorySearchResult, MemorySearchScope, Scope, SearchOptions, SharedBlockInfo, SkillMetadata,
    UndoRedoDepth, UndoRedoOp,
};
use serde_json::Value as JsonValue;

/// Minimal in-memory [`MemoryStore`] for scope policy tests.
#[derive(Debug, Default)]
pub struct ScopeTestStore {
    blocks:
        std::sync::Mutex<std::collections::HashMap<(Scope, String), (StructuredDocument, String)>>,
    archival: std::sync::Mutex<Vec<(Scope, ArchivalEntry)>>,
}

impl ScopeTestStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a core/working block directly into the store.
    pub fn seed(&self, scope: Scope, label: &str, content: &str) {
        let mut meta = BlockMetadata::standalone(BlockSchema::text());
        meta.agent_id = scope.id().to_string();
        meta.label = label.to_string();
        let doc = StructuredDocument::new_with_metadata(meta, None);
        doc.set_text(content, false).unwrap();
        self.blocks.lock().unwrap().insert(
            (scope, label.to_string()),
            (doc, content.to_string()),
        );
    }

    /// Seed a Skill block directly into the store.
    pub fn seed_skill(&self, scope: Scope, label: &str, metadata: SkillMetadata, body: &str) {
        let schema = BlockSchema::Skill {
            expected_keys: vec![],
        };
        let mut meta = BlockMetadata::standalone(schema);
        meta.agent_id = scope.id().to_string();
        meta.label = label.to_string();
        let doc = StructuredDocument::new_with_metadata(meta, None);

        let skill_file = crate::fs::markdown_skill::parse::SkillFile {
            metadata: metadata.clone(),
            extras: loro::LoroValue::Map(Default::default()),
            body: body.to_string(),
        };
        crate::fs::markdown_skill::write_skill_to_loro_doc(&skill_file, doc.inner())
            .expect("seed_skill: write_skill_to_loro_doc failed");
        doc.inner().commit();

        let rendered = crate::fs::markdown_skill::emit(&metadata, &skill_file.extras, body)
            .expect("seed_skill: emit failed");

        self.blocks
            .lock()
            .unwrap()
            .insert((scope, label.to_string()), (doc, rendered));
    }

    /// Seed an archival entry directly, bypassing `insert_archival`.
    pub fn seed_archival(&self, scope: Scope, id: &str, content: &str) {
        self.archival.lock().unwrap().push((
            scope.clone(),
            ArchivalEntry {
                id: id.to_string(),
                agent_id: scope.id().to_string(),
                content: content.to_string(),
                metadata: None,
                created_at: chrono::Utc::now(),
            },
        ));
    }
}

impl MemoryStore for ScopeTestStore {
    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let _ = self.delete_block(scope, &create.label);
        self.create_block(scope, create)
    }

    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let mut meta = BlockMetadata::standalone(create.schema.clone());
        meta.agent_id = scope.id().to_string();
        meta.label = create.label.clone();
        meta.block_type = create.block_type;
        let doc = StructuredDocument::new_with_metadata(meta, None);
        self.blocks.lock().unwrap().insert(
            (scope.clone(), create.label.clone()),
            (doc.clone(), String::new()),
        );
        Ok(doc)
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&(scope.clone(), label.to_string()))
            .map(|(doc, _)| doc.clone()))
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&(scope.clone(), label.to_string()))
            .map(|(doc, _)| doc.metadata().clone()))
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        let guard = self.blocks.lock().unwrap();
        let mut results = Vec::new();
        for ((scope, _), (doc, _)) in guard.iter() {
            if let Some(ref fa) = filter.agent_id
                && &scope.to_db_key() != fa
            {
                // Filter is a Scope-encoded db_key (`local:<id>` /
                // `global:<id>`). Construct via `BlockFilter::by_scope`.
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

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.blocks
            .lock()
            .unwrap()
            .remove(&(scope.clone(), label.to_string()));
        Ok(())
    }

    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        Ok(self
            .blocks
            .lock()
            .unwrap()
            .get(&(scope.clone(), label.to_string()))
            .map(|(_, content)| content.clone()))
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
        let id = format!("archival-{}", self.archival.lock().unwrap().len());
        self.archival.lock().unwrap().push((
            scope.clone(),
            ArchivalEntry {
                id: id.clone(),
                agent_id: scope.id().to_string(),
                content: content.to_string(),
                metadata,
                created_at: chrono::Utc::now(),
            },
        ));
        Ok(id)
    }

    fn search_archival(
        &self,
        scope: &Scope,
        _query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        let guard = self.archival.lock().unwrap();
        let results: Vec<_> = guard
            .iter()
            .filter(|(s, _)| s == scope)
            .map(|(_, e)| e.clone())
            .take(limit)
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
        _scope: &Scope,
        _label: &str,
        _patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        Ok(())
    }

    fn undo_redo(&self, _scope: &Scope, _label: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
        Ok(false)
    }

    fn history_depth(&self, _scope: &Scope, _label: &str) -> MemoryResult<UndoRedoDepth> {
        Ok(UndoRedoDepth { undo: 0, redo: 0 })
    }
}
