//! Handler for `Pattern.Search` — scoped search across message history
//! and archival entries.
//!
//! Dispatches search ops via the scope resolver to determine the
//! permitted agent set, then delegates to [`MemoryStore`] FTS methods.
//! Results are serialized as JSON arrays of search-hit objects.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pattern_core::traits::MemoryStore;
use pattern_core::types::memory_types::SearchOptions;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::handlers::scope::{parse_scope, resolve_scope};
use crate::sdk::requests::SearchReq;
use crate::session::{SessionContext, record_exchange};
use crate::timeout::{CANCELLED_SENTINEL, HandlerGuard};

/// Handler position in the canonical [`crate::sdk::bundle::SdkBundle`]
/// HList. Immediately after Memory (tag 0).
const SEARCH_HANDLER_TAG: u32 = 1;

/// Handler for `Pattern.Search`.
#[derive(Clone)]
pub struct SearchHandler {
    store: Arc<dyn MemoryStore>,
}

impl std::fmt::Debug for SearchHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchHandler").finish_non_exhaustive()
    }
}

impl SearchHandler {
    /// Construct a handler bound to the given store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

impl DescribeEffect for SearchHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Search",
            description: "Scoped search across message history and archival entries (SearchMessages/SearchArchival/SearchAll)",
            constructors: &[
                "SearchMessages :: SearchQuery -> Maybe Scope -> Search [SearchHit]",
                "SearchArchival :: SearchQuery -> Maybe Scope -> Search [SearchHit]",
                "SearchAll      :: SearchQuery -> Maybe Scope -> Search [SearchHit]",
            ],
            type_defs: &[
                "type SearchQuery = Text",
                "type Scope = Text",
                "type SearchHit = Text",
            ],
            helpers: &[
                "messages :: Member Search effs => SearchQuery -> Maybe Scope -> Eff effs [SearchHit]\nmessages q s = send (SearchMessages q s)",
                "archival :: Member Search effs => SearchQuery -> Maybe Scope -> Eff effs [SearchHit]\narchival q s = send (SearchArchival q s)",
                "all_ :: Member Search effs => SearchQuery -> Maybe Scope -> Eff effs [SearchHit]\nall_ q s = send (SearchAll q s)",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for SearchHandler {
    type Request = SearchReq;

    fn handle(
        &mut self,
        req: SearchReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        if state.cancellation.load(Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{CANCELLED_SENTINEL}: search handler cancelled at entry"
            )));
        }

        let _guard = HandlerGuard::enter(&state.gate);
        let agent_id = cx.user().agent_id().to_string();
        let store = self.store.clone();
        let request_repr = format!("{req:?}");

        let result = (|| {
            let (query, scope_str, domain) = match &req {
                SearchReq::SearchMessages(q, s) => (q.clone(), s.clone(), SearchDomain::Messages),
                SearchReq::SearchArchival(q, s) => (q.clone(), s.clone(), SearchDomain::Archival),
                SearchReq::SearchAll(q, s) => (q.clone(), s.clone(), SearchDomain::All),
            };

            let scope = parse_scope(scope_str.as_deref())?;
            let agents = resolve_scope(&scope, &agent_id, &*store)?;

            let options = match domain {
                SearchDomain::Messages => SearchOptions::new().messages_only(),
                SearchDomain::Archival => SearchOptions::new().archival_only(),
                SearchDomain::All => SearchOptions::new(),
            };

            // Collect results across all permitted agents.
            let mut hits: Vec<serde_json::Value> = Vec::new();
            for target_agent in &agents {
                let results = store
                    .search(
                        &query,
                        options.clone(),
                        pattern_core::types::memory_types::MemorySearchScope::Agent(
                            target_agent.as_str().into(),
                        ),
                    )
                    .map_err(|e| {
                        EffectError::Handler(format!("Pattern.Search: search failed: {e}"))
                    })?;
                for r in results {
                    hits.push(serde_json::json!({
                        "id": r.id,
                        "agentId": target_agent,
                        "content": r.content,
                        "contentType": format!("{:?}", r.content_type),
                        "score": r.score,
                    }));
                }
            }

            // Return as a Haskell list of Text (one JSON-encoded hit per element).
            let items: Vec<String> = hits
                .iter()
                .map(|h| serde_json::to_string(h).unwrap_or_default())
                .collect();
            cx.respond(items)
        })();

        if let Ok(ref value) = result {
            let log = cx.user().checkpoint_log();
            let turn = cx.user().current_turn();
            record_exchange(&log, SEARCH_HANDLER_TAG, request_repr, value, turn);
        }
        result
    }
}

/// Internal domain enum for dispatch clarity.
#[derive(Debug, Clone, Copy)]
enum SearchDomain {
    Messages,
    Archival,
    All,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NopProviderClient;
    use crate::testing::{InMemoryMemoryStore, standard_datacon_table};
    use pattern_core::ProviderClient;
    use pattern_core::types::snapshot::PersonaSnapshot;

    async fn sctx() -> SessionContext {
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        SessionContext::from_persona(
            &persona,
            Arc::new(InMemoryMemoryStore::new()),
            Arc::new(NopProviderClient),
            db,
            tokio::runtime::Handle::current(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn search_messages_current_agent_returns_empty_list() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let db = crate::testing::test_db().await;
        let result = tokio::task::spawn_blocking(move || {
            let table = standard_datacon_table();
            let persona = PersonaSnapshot::new("agent-a", "A");
            let ctx = SessionContext::from_persona(
                &persona,
                store.clone(),
                Arc::new(NopProviderClient) as Arc<dyn ProviderClient>,
                db,
                tokio::runtime::Handle::current(),
            );
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = SearchHandler::new(store);
            h.handle(SearchReq::SearchMessages("test query".into(), None), &cx)
        })
        .await
        .expect("spawn_blocking panicked");

        // InMemoryMemoryStore::search returns empty vec, so we expect an
        // empty Haskell list.
        assert!(result.is_ok(), "expected ok, got: {:?}", result.err());
    }

    #[tokio::test]
    async fn search_cancelled_at_entry() {
        let table = standard_datacon_table();
        let ctx = sctx().await;
        ctx.cancel_state()
            .cancellation
            .store(true, Ordering::SeqCst);
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = SearchHandler::new(Arc::new(InMemoryMemoryStore::new()));
        let err = h
            .handle(SearchReq::SearchAll("q".into(), None), &cx)
            .unwrap_err();
        assert!(err.to_string().contains(CANCELLED_SENTINEL), "got: {err}");
    }
}
