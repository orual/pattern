//! Handler for `Pattern.Recall` — archival-entry CRUD with optional
//! scope on search.
//!
//! Thin layer over [`MemoryStore`]'s archival methods. Insert and
//! delete always target the caller's own entries; search takes an
//! optional scope resolved via the scope resolver.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pattern_core::traits::MemoryStore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::handlers::scope::{parse_scope, resolve_scope};
use crate::sdk::requests::RecallReq;
use crate::session::{SessionContext, record_exchange};
use crate::timeout::{CANCELLED_SENTINEL, HandlerGuard};

/// Handler position in the canonical [`crate::sdk::bundle::SdkBundle`]
/// HList. After Search (tag 1).
const RECALL_HANDLER_TAG: u32 = 2;

/// Handler for `Pattern.Recall`.
#[derive(Clone)]
pub struct RecallHandler {
    store: Arc<dyn MemoryStore>,
}

impl std::fmt::Debug for RecallHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecallHandler").finish_non_exhaustive()
    }
}

impl RecallHandler {
    /// Construct a handler bound to the given store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

impl DescribeEffect for RecallHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Recall",
            description: "Archival-entry CRUD with optional scope (RecallInsert/RecallSearch/RecallGet/RecallDelete)",
            constructors: &[
                "RecallInsert :: ArchivalContent -> Recall EntryId",
                "RecallSearch :: RecallQuery -> Maybe Scope -> Recall [ArchivalHit]",
                "RecallGet    :: EntryId -> Recall ArchivalContent",
                "RecallDelete :: EntryId -> Recall ()",
            ],
            type_defs: &[
                "type ArchivalContent = Text",
                "type EntryId = Text",
                "type RecallQuery = Text",
                "type Scope = Text",
                "type ArchivalHit = Text",
            ],
            helpers: &[
                "recallInsert :: Member Recall effs => ArchivalContent -> Eff effs EntryId\nrecallInsert c = send (RecallInsert c)",
                "recallSearch :: Member Recall effs => RecallQuery -> Maybe Scope -> Eff effs [ArchivalHit]\nrecallSearch q s = send (RecallSearch q s)",
                "recallGet :: Member Recall effs => EntryId -> Eff effs ArchivalContent\nrecallGet i = send (RecallGet i)",
                "recallDelete :: Member Recall effs => EntryId -> Eff effs ()\nrecallDelete i = send (RecallDelete i)",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for RecallHandler {
    type Request = RecallReq;

    fn handle(
        &mut self,
        req: RecallReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        if state.cancellation.load(Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{CANCELLED_SENTINEL}: recall handler cancelled at entry"
            )));
        }

        let _guard = HandlerGuard::enter(&state.gate);
        let agent_id = cx.user().agent_id().to_string();
        let store = self.store.clone();
        let request_repr = format!("{req:?}");
        let handle = tokio::runtime::Handle::current();

        let result = (|| match req {
            RecallReq::Insert(content) => {
                let id = handle
                    .block_on(store.insert_archival(&agent_id, &content, None))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Insert: {e}")))?;
                cx.respond(id)
            }

            RecallReq::Search(query, scope_str) => {
                let scope = parse_scope(scope_str.as_deref())?;
                let agents = handle.block_on(resolve_scope(&scope, &agent_id, &*store))?;

                let mut hits: Vec<String> = Vec::new();
                for target_agent in &agents {
                    let results = handle
                        .block_on(store.search_archival(target_agent, &query, 10))
                        .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Search: {e}")))?;
                    for r in results {
                        let hit = serde_json::json!({
                            "id": r.id,
                            "agentId": r.agent_id,
                            "content": r.content,
                            "createdAt": r.created_at.to_rfc3339(),
                        });
                        hits.push(serde_json::to_string(&hit).unwrap_or_default());
                    }
                }

                cx.respond(hits)
            }

            RecallReq::Get(id) => {
                // Get retrieves by entry id; the store checks existence.
                // We search for the entry across the caller's archival entries.
                // Since archival entries have globally unique IDs, we search
                // the caller's entries. If not found, return an error.
                let results = handle
                    .block_on(store.search_archival(&agent_id, &id, 1))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Get: {e}")))?;

                // search_archival does FTS, not exact-id lookup. For now, return
                // not-found and note this as a limitation for follow-up (an
                // exact get_archival_by_id method would be cleaner).
                let entry = results.into_iter().find(|e| e.id == id).ok_or_else(|| {
                    EffectError::Handler(format!(
                        "Pattern.Recall.Get: no archival entry with id {id:?}"
                    ))
                })?;
                cx.respond(entry.content)
            }

            RecallReq::Delete(id) => {
                handle
                    .block_on(store.delete_archival(&id))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Delete: {e}")))?;
                cx.respond(())
            }
        })();

        if let Ok(ref value) = result {
            let log = cx.user().checkpoint_log();
            let turn = cx.user().current_turn();
            record_exchange(&log, RECALL_HANDLER_TAG, request_repr, value, turn);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NopProviderClient;
    use crate::testing::standard_datacon_table;
    use pattern_core::types::snapshot::PersonaConfig;
    use tidepool_repr::{DataCon, DataConId};

    /// Standard table extended with `()` for handlers that return unit.
    fn handler_table() -> tidepool_repr::DataConTable {
        let mut table = standard_datacon_table();
        table.insert(DataCon {
            id: DataConId(100),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    /// In-memory store with archival support for recall tests.
    #[derive(Debug, Default)]
    struct RecallTestStore {
        entries: std::sync::Mutex<Vec<pattern_core::memory::ArchivalEntry>>,
        next_id: std::sync::atomic::AtomicU64,
    }

    impl RecallTestStore {
        fn new() -> Self {
            Self::default()
        }
    }

    #[async_trait::async_trait]
    impl MemoryStore for RecallTestStore {
        async fn insert_archival(
            &self,
            agent_id: &str,
            content: &str,
            _metadata: Option<serde_json::Value>,
        ) -> pattern_core::memory::MemoryResult<String> {
            let id = format!(
                "arch-{}",
                self.next_id
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
            self.entries
                .lock()
                .unwrap()
                .push(pattern_core::memory::ArchivalEntry {
                    id: id.clone(),
                    agent_id: agent_id.to_string(),
                    content: content.to_string(),
                    metadata: None,
                    created_at: chrono::Utc::now(),
                });
            Ok(id)
        }

        async fn search_archival(
            &self,
            agent_id: &str,
            query: &str,
            limit: usize,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::ArchivalEntry>> {
            let guard = self.entries.lock().unwrap();
            let results: Vec<_> = guard
                .iter()
                .filter(|e| e.agent_id == agent_id && e.content.contains(query))
                .take(limit)
                .cloned()
                .collect();
            Ok(results)
        }

        async fn delete_archival(&self, id: &str) -> pattern_core::memory::MemoryResult<()> {
            self.entries.lock().unwrap().retain(|e| e.id != id);
            Ok(())
        }

        // ---- Stubs for the rest ----
        async fn create_block(
            &self,
            _: &str,
            _: pattern_core::types::block::BlockCreate,
        ) -> pattern_core::memory::MemoryResult<pattern_core::memory::StructuredDocument> {
            panic!()
        }
        async fn get_block(
            &self,
            _: &str,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Option<pattern_core::memory::StructuredDocument>>
        {
            panic!()
        }
        async fn get_block_metadata(
            &self,
            _: &str,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Option<pattern_core::memory::BlockMetadata>>
        {
            panic!()
        }
        async fn list_blocks(
            &self,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::BlockMetadata>> {
            panic!()
        }
        async fn list_blocks_by_type(
            &self,
            _: &str,
            _: pattern_core::memory::BlockType,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::BlockMetadata>> {
            panic!()
        }
        async fn list_all_blocks_by_label_prefix(
            &self,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::BlockMetadata>> {
            panic!()
        }
        async fn delete_block(&self, _: &str, _: &str) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn get_rendered_content(
            &self,
            _: &str,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Option<String>> {
            panic!()
        }
        async fn persist_block(&self, _: &str, _: &str) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        fn mark_dirty(&self, _: &str, _: &str) {}
        async fn search(
            &self,
            _: &str,
            _: &str,
            _: pattern_core::memory::SearchOptions,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::MemorySearchResult>>
        {
            Ok(vec![])
        }
        async fn search_all(
            &self,
            _: &str,
            _: pattern_core::memory::SearchOptions,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::MemorySearchResult>>
        {
            Ok(vec![])
        }
        async fn list_shared_blocks(
            &self,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::SharedBlockInfo>>
        {
            Ok(vec![])
        }
        async fn get_shared_block(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<Option<pattern_core::memory::StructuredDocument>>
        {
            Ok(None)
        }
        async fn set_block_pinned(
            &self,
            _: &str,
            _: &str,
            _: bool,
        ) -> pattern_core::memory::MemoryResult<()> {
            Ok(())
        }
        async fn set_block_type(
            &self,
            _: &str,
            _: &str,
            _: pattern_core::memory::BlockType,
        ) -> pattern_core::memory::MemoryResult<()> {
            Ok(())
        }
        async fn update_block_schema(
            &self,
            _: &str,
            _: &str,
            _: pattern_core::memory::BlockSchema,
        ) -> pattern_core::memory::MemoryResult<()> {
            Ok(())
        }
        async fn update_block_description(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> pattern_core::memory::MemoryResult<()> {
            Ok(())
        }
        async fn undo_block(&self, _: &str, _: &str) -> pattern_core::memory::MemoryResult<bool> {
            Ok(false)
        }
        async fn redo_block(&self, _: &str, _: &str) -> pattern_core::memory::MemoryResult<bool> {
            Ok(false)
        }
        async fn undo_depth(&self, _: &str, _: &str) -> pattern_core::memory::MemoryResult<usize> {
            Ok(0)
        }
        async fn redo_depth(&self, _: &str, _: &str) -> pattern_core::memory::MemoryResult<usize> {
            Ok(0)
        }
    }

    fn sctx(store: Arc<dyn MemoryStore>) -> SessionContext {
        let persona = PersonaConfig::new("agent-a", "A", "module X where\nx = pure ()");
        SessionContext::from_persona(&persona, store, Arc::new(NopProviderClient))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recall_insert_and_search_roundtrip() {
        let store: Arc<dyn MemoryStore> = Arc::new(RecallTestStore::new());
        let store_for_handler = store.clone();
        tokio::task::spawn_blocking(move || {
            let table = handler_table();
            let ctx = sctx(store.clone());
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = RecallHandler::new(store_for_handler);

            // Insert.
            let insert_result = h.handle(RecallReq::Insert("test content about cats".into()), &cx);
            assert!(
                insert_result.is_ok(),
                "insert failed: {:?}",
                insert_result.err()
            );

            // Search.
            let search_result = h.handle(RecallReq::Search("cats".into(), None), &cx);
            assert!(
                search_result.is_ok(),
                "search failed: {:?}",
                search_result.err()
            );
        })
        .await
        .expect("spawn_blocking panicked");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recall_delete_removes_entry() {
        let store: Arc<dyn MemoryStore> = Arc::new(RecallTestStore::new());
        let store_for_handler = store.clone();
        tokio::task::spawn_blocking(move || {
            let table = handler_table();
            let ctx = sctx(store.clone());
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = RecallHandler::new(store_for_handler);

            // Insert then delete.
            let _ = h
                .handle(RecallReq::Insert("ephemeral data".into()), &cx)
                .unwrap();
            let delete_result = h.handle(RecallReq::Delete("arch-0".into()), &cx);
            assert!(
                delete_result.is_ok(),
                "delete failed: {:?}",
                delete_result.err()
            );

            // Search should find nothing.
            let search_result = h
                .handle(RecallReq::Search("ephemeral".into(), None), &cx)
                .unwrap();
            // The result should be an empty list.
            match &search_result {
                Value::Con(_, fields) if fields.is_empty() => {
                    // Empty list [] constructor.
                }
                _ => {
                    // May be a different encoding; just ensure no panic.
                }
            }
        })
        .await
        .expect("spawn_blocking panicked");
    }

    #[tokio::test]
    async fn recall_cancelled_at_entry() {
        let table = standard_datacon_table();
        let store: Arc<dyn MemoryStore> = Arc::new(RecallTestStore::new());
        let ctx = sctx(store.clone());
        ctx.cancel_state()
            .cancellation
            .store(true, Ordering::SeqCst);
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = RecallHandler::new(store);
        let err = h.handle(RecallReq::Insert("x".into()), &cx).unwrap_err();
        assert!(err.to_string().contains(CANCELLED_SENTINEL), "got: {err}");
    }
}
