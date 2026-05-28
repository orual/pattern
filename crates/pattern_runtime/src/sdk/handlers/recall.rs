// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Handler for `Pattern.Recall` — archival-entry CRUD with optional
//! scope on search.
//!
//! Thin layer over [`MemoryStore`]'s archival methods. Insert and
//! delete always target the caller's own entries; search takes an
//! optional scope resolved via the scope resolver.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pattern_core::hooks::HookEvent;
use pattern_core::traits::MemoryStore;
use pattern_core::types::memory_types::Scope;
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
            description: "Archival-entry CRUD with optional scope (RecallInsert/RecallSearch)",
            constructors: std::borrow::Cow::Borrowed(&[
                "RecallInsert :: ArchivalContent -> Recall EntryId",
                "RecallSearch :: RecallQuery -> Maybe Scope -> Recall [ArchivalHit]",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type ArchivalContent = Text",
                "type EntryId = Text",
                "type RecallQuery = Text",
                "type Scope = Text",
                "type ArchivalHit = Text",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "insert :: Member Recall effs => ArchivalContent -> Eff effs EntryId\ninsert c = send (RecallInsert c)",
                "search :: Member Recall effs => RecallQuery -> Maybe Scope -> Eff effs [ArchivalHit]\nsearch q s = send (RecallSearch q s)",
            ]),
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

        // Effect-class runtime guard.
        // RecallInsert=MutateInternal/Enforce; RecallSearch=Observe/Skip;
        // RecallGet=Observe/Enforce.
        let constructor_name = match &req {
            RecallReq::Insert(_) => "RecallInsert",
            RecallReq::Search(_, _) => "RecallSearch",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Recall",
            constructor_name,
        )?;

        let agent_id = cx.user().agent_id().to_string();
        let session_scope = cx.user().default_scope().clone();
        let store = self.store.clone();
        let request_repr = format!("{req:?}");

        let result = (|| match req {
            RecallReq::Insert(content) => {
                let id = store
                    .insert_archival(&session_scope, &content, None)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Insert: {e}")))?;
                cx.user().hook_bridge().emit(HookEvent::notification(
                    pattern_core::hooks::tags::RECALL_INSERTED,
                    serde_json::json!({ "entry_id": id }),
                ));
                cx.respond(id)
            }

            RecallReq::Search(query, scope_str) => {
                let scope = parse_scope(scope_str.as_deref())?;
                let agents = resolve_scope(&scope, &agent_id, &*store)?;

                let mut hits: Vec<String> = Vec::new();
                for target_agent in &agents {
                    // Cross-agent recall is persona-scoped (Global).
                    let target_scope = Scope::Local(target_agent.clone().into());
                    let results = store
                        .search_archival(&target_scope, &query, 10)
                        .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Search: {e}")))?;
                    for r in results {
                        let hit = serde_json::json!({
                            "id": r.id,
                            "agentId": r.agent_id,
                            "content": r.content,
                            "createdAt": r.created_at.to_string(),
                        });
                        hits.push(serde_json::to_string(&hit).unwrap_or_default());
                    }

                    // TODO: gate this properly
                    let target_scope = Scope::Global(target_agent.clone().into());
                    let results = store
                        .search_archival(&target_scope, &query, 10)
                        .map_err(|e| EffectError::Handler(format!("Pattern.Recall.Search: {e}")))?;
                    for r in results {
                        let hit = serde_json::json!({
                            "id": r.id,
                            "agentId": r.agent_id,
                            "content": r.content,
                            "createdAt": r.created_at.to_string(),
                        });
                        hits.push(serde_json::to_string(&hit).unwrap_or_default());
                    }
                }

                cx.user().hook_bridge().emit(HookEvent::notification(
                    pattern_core::hooks::tags::RECALL_SEARCH,
                    serde_json::json!({ "query": query, "results": hits }),
                ));
                cx.respond(hits)
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
    use pattern_core::types::snapshot::PersonaSnapshot;
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
        entries: std::sync::Mutex<Vec<pattern_core::types::memory_types::ArchivalEntry>>,
        next_id: std::sync::atomic::AtomicU64,
    }

    impl RecallTestStore {
        fn new() -> Self {
            Self::default()
        }
    }

    impl MemoryStore for RecallTestStore {
        fn create_or_replace_block(
            &self,
            scope: &pattern_core::types::memory_types::Scope,
            create: pattern_core::types::block::BlockCreate,
        ) -> pattern_core::types::memory_types::MemoryResult<pattern_core::memory::StructuredDocument>
        {
            self.create_block(scope, create)
        }
        fn insert_archival(
            &self,
            scope: &pattern_core::types::memory_types::Scope,
            content: &str,
            _metadata: Option<serde_json::Value>,
        ) -> pattern_core::types::memory_types::MemoryResult<String> {
            let id = format!(
                "arch-{}",
                self.next_id
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
            self.entries
                .lock()
                .unwrap()
                .push(pattern_core::types::memory_types::ArchivalEntry {
                    id: id.clone(),
                    agent_id: scope.id().to_string(),
                    content: content.to_string(),
                    metadata: None,
                    created_at: jiff::Timestamp::now(),
                });
            Ok(id)
        }
        fn search_archival(
            &self,
            scope: &pattern_core::types::memory_types::Scope,
            query: &str,
            limit: usize,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::ArchivalEntry>,
        > {
            let guard = self.entries.lock().unwrap();
            Ok(guard
                .iter()
                .filter(|e| e.agent_id == scope.id() && e.content.contains(query))
                .take(limit)
                .cloned()
                .collect())
        }
        fn delete_archival(&self, id: &str) -> pattern_core::types::memory_types::MemoryResult<()> {
            self.entries.lock().unwrap().retain(|e| e.id != id);
            Ok(())
        }
        // ---- Stubs ----
        fn create_block(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: pattern_core::types::block::BlockCreate,
        ) -> pattern_core::types::memory_types::MemoryResult<pattern_core::memory::StructuredDocument>
        {
            panic!()
        }
        fn get_block(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Option<pattern_core::memory::StructuredDocument>,
        > {
            panic!()
        }
        fn get_block_metadata(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Option<pattern_core::types::memory_types::BlockMetadata>,
        > {
            panic!()
        }
        fn list_blocks(
            &self,
            _: pattern_core::types::memory_types::BlockFilter,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::BlockMetadata>,
        > {
            panic!()
        }
        fn delete_block(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            panic!()
        }
        fn get_rendered_content(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<Option<String>> {
            panic!()
        }
        fn persist_block(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            panic!()
        }
        fn mark_dirty(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            Ok(())
        }
        fn search(
            &self,
            _: &str,
            _: pattern_core::types::memory_types::SearchOptions,
            _: pattern_core::types::memory_types::MemorySearchScope,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::MemorySearchResult>,
        > {
            Ok(vec![])
        }
        fn list_shared_blocks(
            &self,
            _: &pattern_core::types::memory_types::Scope,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::SharedBlockInfo>,
        > {
            Ok(vec![])
        }
        fn get_shared_block(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Option<pattern_core::memory::StructuredDocument>,
        > {
            Ok(None)
        }
        fn update_block_metadata(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
            _: pattern_core::types::memory_types::BlockMetadataPatch,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            Ok(())
        }
        fn undo_redo(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
            _: pattern_core::types::memory_types::UndoRedoOp,
        ) -> pattern_core::types::memory_types::MemoryResult<bool> {
            Ok(false)
        }
        fn history_depth(
            &self,
            _: &pattern_core::types::memory_types::Scope,
            _: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            pattern_core::types::memory_types::UndoRedoDepth,
        > {
            Ok(pattern_core::types::memory_types::UndoRedoDepth { undo: 0, redo: 0 })
        }
        fn commit_write(
            &self,
            _scope: &pattern_core::types::memory_types::Scope,
            _label: &str,
        ) -> pattern_core::MemoryResult<()> {
            panic!()
        }
    }

    fn sctx(store: Arc<dyn MemoryStore>, db: Arc<pattern_db::ConstellationDb>) -> SessionContext {
        let persona = PersonaSnapshot::new("agent-a", "A");
        SessionContext::from_persona(
            &persona,
            store,
            Arc::new(NopProviderClient),
            db,
            tokio::runtime::Handle::current(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recall_insert_and_search_roundtrip() {
        let store: Arc<dyn MemoryStore> = Arc::new(RecallTestStore::new());
        let store_for_handler = store.clone();
        let db = crate::testing::test_db().await;
        tokio::task::spawn_blocking(move || {
            let table = handler_table();
            let ctx = sctx(store.clone(), db);
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

    // recall_delete_removes_entry test removed: RecallReq::Delete
    // variant was removed in v3-memory-rework Phase 3, AC4.9.
    // MemoryStore::delete_archival is retained for human-operator
    // tooling but is no longer reachable via the SDK.

    #[tokio::test]
    async fn recall_cancelled_at_entry() {
        let table = standard_datacon_table();
        let store: Arc<dyn MemoryStore> = Arc::new(RecallTestStore::new());
        let db = crate::testing::test_db().await;
        let ctx = sctx(store.clone(), db);
        ctx.cancel_state()
            .cancellation
            .store(true, Ordering::SeqCst);
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = RecallHandler::new(store);
        let err = h.handle(RecallReq::Insert("x".into()), &cx).unwrap_err();
        assert!(err.to_string().contains(CANCELLED_SENTINEL), "got: {err}");
    }
}
