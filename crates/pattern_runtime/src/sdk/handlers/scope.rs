//! Scope resolution: maps a [`SearchScope`] + caller identity to the
//! concrete set of agent IDs the caller is permitted to search.
//!
//! This is a pure async function over `dyn MemoryStore` — no handler
//! state, no JIT dependency — so it can be unit-tested in isolation
//! with [`crate::testing::InMemoryMemoryStore`].
//!
//! # Permission policy
//!
//! - `CurrentAgent` → always `[caller]`.
//! - `Agent(target)` → `[target]` if:
//!   (a) target == caller, OR
//!   (b) target has shared ≥1 block with caller.
//!   Otherwise returns a permission-denied error.
//! - `Agents(ids)` → per-id same check; filters out unpermitted without
//!   erroring. Returns error only if the resulting set is empty.
//! - `Constellation` → all constellation agents if caller has the
//!   constellation-wide-search permission (currently: always allowed if
//!   there are agents). Future phases may add a trust-level gate.
//!
//! Group membership no longer gates cross-agent search: the v3-multi-agent
//! `persona_groups` schema (Phase 6) is organisational only, not a
//! coordination/permission mechanism. Cross-agent permission relies on
//! shared blocks; future phases may layer relationship-edge checks
//! (`SupervisorOf` etc.) on top.

use pattern_core::traits::MemoryStore;
use pattern_core::types::SearchScope;
use tidepool_effect::EffectError;

/// Parse a scope string from the Haskell side into a [`SearchScope`].
///
/// Format:
/// - `"current"` → `CurrentAgent`
/// - `"agent:<id>"` → `Agent(id)`
/// - `"agents:<id1>,<id2>,..."` → `Agents(ids)`
/// - `"constellation"` → `Constellation`
pub fn parse_scope(scope: Option<&str>) -> Result<SearchScope, EffectError> {
    match scope {
        None => Ok(SearchScope::CurrentAgent),
        Some("current") => Ok(SearchScope::CurrentAgent),
        Some(s) if s.starts_with("agent:") => {
            let id = s.strip_prefix("agent:").unwrap().to_string();
            if id.is_empty() {
                return Err(EffectError::Handler(
                    "empty agent id in scope 'agent:'".to_string(),
                ));
            }
            Ok(SearchScope::Agent(id.into()))
        }
        Some(s) if s.starts_with("agents:") => {
            let ids: Vec<_> = s
                .strip_prefix("agents:")
                .unwrap()
                .split(',')
                .filter(|id| !id.is_empty())
                .map(|id| id.trim().to_string().into())
                .collect();
            if ids.is_empty() {
                return Err(EffectError::Handler(
                    "empty agent list in scope 'agents:'".to_string(),
                ));
            }
            Ok(SearchScope::Agents(ids))
        }
        Some("constellation") => Ok(SearchScope::Constellation),
        Some(other) => Err(EffectError::Handler(format!(
            "unrecognized scope format: {other:?}; expected 'current', 'agent:<id>', 'agents:<id1>,<id2>', or 'constellation'"
        ))),
    }
}

/// Resolve a [`SearchScope`] to the concrete set of agent IDs the
/// caller is permitted to search.
///
/// Returns `Err(EffectError::Handler)` when the caller lacks permission
/// for any of the requested agents.
pub fn resolve_scope(
    scope: &SearchScope,
    caller: &str,
    store: &dyn MemoryStore,
) -> Result<Vec<String>, EffectError> {
    match scope {
        SearchScope::CurrentAgent => Ok(vec![caller.to_string()]),

        SearchScope::Agent(target) => {
            let target_str = target.as_str();
            if target_str == caller {
                return Ok(vec![caller.to_string()]);
            }
            if check_cross_agent_permission(caller, target_str, store)? {
                Ok(vec![target_str.to_string()])
            } else {
                Err(EffectError::Handler(format!(
                    "permission denied: agent {caller:?} cannot search agent {target_str:?} \
                     (no shared blocks)"
                )))
            }
        }

        SearchScope::Agents(ids) => {
            let mut allowed = Vec::with_capacity(ids.len());
            for id in ids {
                let id_str = id.as_str();
                if id_str == caller {
                    allowed.push(caller.to_string());
                } else if check_cross_agent_permission(caller, id_str, store)? {
                    allowed.push(id_str.to_string());
                }
                // Silently filter out agents the caller cannot access.
            }
            if allowed.is_empty() {
                Err(EffectError::Handler(format!(
                    "permission denied: agent {caller:?} cannot search any of the requested agents"
                )))
            } else {
                Ok(allowed)
            }
        }

        SearchScope::Constellation => {
            let scopes = store
                .list_constellation_scopes()
                .map_err(|e| EffectError::Handler(format!("constellation lookup failed: {e}")))?;
            if scopes.is_empty() {
                Ok(vec![caller.to_string()])
            } else {
                Ok(scopes.into_iter().map(|s| s.id().to_string()).collect())
            }
        }

        // `Schema(kind)` is an orthogonal filter dimension — it restricts by
        // block schema type, not by agent identity. The scope resolver's job
        // is to produce an agent-ID set; schema filtering is applied by the
        // query layer on top of that result. Fall back to `CurrentAgent`
        // semantics so the caller's own data is searched and the query layer
        // applies the schema predicate.
        //
        // The `_ =>` arm also covers any future non-exhaustive variants that
        // may be added to `SearchScope` in later phases.
        _ => Ok(vec![caller.to_string()]),
    }
}

/// Check whether `caller` has cross-agent permission to access
/// `target`'s data. Currently driven by shared-blocks alone; future
/// phases may layer relationship-edge checks (`SupervisorOf` etc.).
fn check_cross_agent_permission(
    caller: &str,
    target: &str,
    store: &dyn MemoryStore,
) -> Result<bool, EffectError> {
    use pattern_core::types::memory_types::Scope;
    let caller_scope = Scope::Global(caller.into());
    let target_scope = Scope::Global(target.into());
    store
        .has_shared_blocks_with(&caller_scope, &target_scope)
        .map_err(|e| EffectError::Handler(format!("shared-block check failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    use pattern_core::memory::StructuredDocument;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::*;
    use serde_json::Value as JsonValue;

    /// Test double for scope resolution. Tracks shared-blocks
    /// relationships without needing real DB queries.
    #[derive(Debug, Default)]
    struct ScopeTestStore {
        /// (caller, target) pairs where target has shared blocks with caller.
        shared_blocks: Mutex<HashSet<(String, String)>>,
        /// All agent ids in the constellation.
        constellation_agents: Mutex<Vec<String>>,
    }

    impl ScopeTestStore {
        fn new() -> Self {
            Self::default()
        }

        fn add_shared_blocks(&self, caller: &str, target: &str) {
            self.shared_blocks
                .lock()
                .unwrap()
                .insert((caller.to_string(), target.to_string()));
        }

        fn set_constellation_agents(&self, agents: Vec<&str>) {
            *self.constellation_agents.lock().unwrap() =
                agents.into_iter().map(String::from).collect();
        }
    }

    impl MemoryStore for ScopeTestStore {
        fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()> { self.mark_dirty(scope, label)?; self.persist_block(scope, label) }
        fn create_or_replace_block(&self, scope: &Scope, create: BlockCreate) -> MemoryResult<StructuredDocument> { self.create_block(scope, create) }
        fn has_shared_blocks_with(&self, caller: &Scope, target: &Scope) -> MemoryResult<bool> {
            Ok(self
                .shared_blocks
                .lock()
                .unwrap()
                .contains(&(caller.id().to_string(), target.id().to_string())))
        }

        fn list_constellation_scopes(&self) -> MemoryResult<Vec<Scope>> {
            Ok(self
                .constellation_agents
                .lock()
                .unwrap()
                .iter()
                .map(|s| Scope::Global(s.clone().into()))
                .collect())
        }

        fn create_block(&self, _: &Scope, _: BlockCreate) -> MemoryResult<StructuredDocument> {
            panic!("not used in scope tests")
        }
        fn get_block(&self, _: &Scope, _: &str) -> MemoryResult<Option<StructuredDocument>> {
            panic!("not used in scope tests")
        }
        fn get_block_metadata(&self, _: &Scope, _: &str) -> MemoryResult<Option<BlockMetadata>> {
            panic!()
        }
        fn list_blocks(&self, _: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
            panic!()
        }
        fn delete_block(&self, _: &Scope, _: &str) -> MemoryResult<()> {
            panic!()
        }
        fn get_rendered_content(&self, _: &Scope, _: &str) -> MemoryResult<Option<String>> {
            panic!()
        }
        fn persist_block(&self, _: &Scope, _: &str) -> MemoryResult<()> {
            panic!()
        }
        fn mark_dirty(&self, _: &Scope, _: &str) -> MemoryResult<()> {
            panic!()
        }
        fn insert_archival(
            &self,
            _: &Scope,
            _: &str,
            _: Option<JsonValue>,
        ) -> MemoryResult<String> {
            panic!()
        }
        fn search_archival(
            &self,
            _: &Scope,
            _: &str,
            _: usize,
        ) -> MemoryResult<Vec<ArchivalEntry>> {
            panic!()
        }
        fn delete_archival(&self, _: &str) -> MemoryResult<()> {
            panic!()
        }
        fn search(
            &self,
            _: &str,
            _: SearchOptions,
            _: MemorySearchScope,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            panic!()
        }
        fn list_shared_blocks(&self, _: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
            panic!()
        }
        fn get_shared_block(
            &self,
            _: &Scope,
            _: &Scope,
            _: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            panic!()
        }
        fn update_block_metadata(
            &self,
            _: &Scope,
            _: &str,
            _: BlockMetadataPatch,
        ) -> MemoryResult<()> {
            panic!()
        }
        fn undo_redo(&self, _: &Scope, _: &str, _: UndoRedoOp) -> MemoryResult<bool> {
            panic!()
        }
        fn history_depth(&self, _: &Scope, _: &str) -> MemoryResult<UndoRedoDepth> {
            panic!()
        }
    }

    // ---- parse_scope tests ----

    #[test]
    fn parse_scope_none_is_current_agent() {
        assert_eq!(parse_scope(None).unwrap(), SearchScope::CurrentAgent);
    }

    #[test]
    fn parse_scope_current() {
        assert_eq!(
            parse_scope(Some("current")).unwrap(),
            SearchScope::CurrentAgent
        );
    }

    #[test]
    fn parse_scope_single_agent() {
        assert_eq!(
            parse_scope(Some("agent:abc-123")).unwrap(),
            SearchScope::Agent("abc-123".into())
        );
    }

    #[test]
    fn parse_scope_multiple_agents() {
        let scope = parse_scope(Some("agents:a,b,c")).unwrap();
        match scope {
            SearchScope::Agents(ids) => {
                assert_eq!(ids.len(), 3);
                assert_eq!(ids[0].as_str(), "a");
                assert_eq!(ids[1].as_str(), "b");
                assert_eq!(ids[2].as_str(), "c");
            }
            _ => panic!("expected Agents variant"),
        }
    }

    #[test]
    fn parse_scope_constellation() {
        assert_eq!(
            parse_scope(Some("constellation")).unwrap(),
            SearchScope::Constellation
        );
    }

    #[test]
    fn parse_scope_unknown_format_errors() {
        let err = parse_scope(Some("garbage")).unwrap_err();
        assert!(
            err.to_string().contains("unrecognized scope format"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_scope_empty_agent_id_errors() {
        let err = parse_scope(Some("agent:")).unwrap_err();
        assert!(err.to_string().contains("empty agent id"), "got: {err}");
    }

    // ---- resolve_scope tests ----

    #[test]
    fn resolve_current_agent_always_returns_caller() {
        let store = ScopeTestStore::new();
        let result = resolve_scope(&SearchScope::CurrentAgent, "alice", &store).unwrap();
        assert_eq!(result, vec!["alice"]);
    }

    #[test]
    fn resolve_agent_self_always_allowed() {
        let store = ScopeTestStore::new();
        let result = resolve_scope(&SearchScope::Agent("alice".into()), "alice", &store).unwrap();
        assert_eq!(result, vec!["alice"]);
    }

    #[test]
    fn resolve_agent_denied_without_relationship() {
        let store = ScopeTestStore::new();
        let err = resolve_scope(&SearchScope::Agent("bob".into()), "alice", &store).unwrap_err();
        assert!(err.to_string().contains("permission denied"), "got: {err}");
    }

    #[test]
    fn resolve_agent_allowed_via_shared_blocks() {
        let store = ScopeTestStore::new();
        store.add_shared_blocks("alice", "bob");
        let result = resolve_scope(&SearchScope::Agent("bob".into()), "alice", &store).unwrap();
        assert_eq!(result, vec!["bob"]);
    }

    #[test]
    fn resolve_agents_filters_unpermitted() {
        let store = ScopeTestStore::new();
        store.add_shared_blocks("alice", "bob");
        // charlie has no relationship with alice.
        let result = resolve_scope(
            &SearchScope::Agents(vec!["bob".into(), "charlie".into()]),
            "alice",
            &store,
        )
        .unwrap();
        assert_eq!(result, vec!["bob"]);
    }

    #[test]
    fn resolve_agents_all_denied_errors() {
        let store = ScopeTestStore::new();
        let err = resolve_scope(
            &SearchScope::Agents(vec!["bob".into(), "charlie".into()]),
            "alice",
            &store,
        )
        .unwrap_err();
        assert!(err.to_string().contains("permission denied"), "got: {err}");
    }

    #[test]
    fn resolve_agents_includes_self() {
        let store = ScopeTestStore::new();
        // alice is always allowed.
        let result = resolve_scope(
            &SearchScope::Agents(vec!["alice".into(), "bob".into()]),
            "alice",
            &store,
        )
        .unwrap();
        assert_eq!(result, vec!["alice"]);
    }

    #[test]
    fn resolve_constellation_returns_all_agents() {
        let store = ScopeTestStore::new();
        store.set_constellation_agents(vec!["alice", "bob", "charlie"]);
        let result = resolve_scope(&SearchScope::Constellation, "alice", &store).unwrap();
        assert_eq!(result, vec!["alice", "bob", "charlie"]);
    }

    #[test]
    fn resolve_constellation_empty_falls_back_to_caller() {
        let store = ScopeTestStore::new();
        let result = resolve_scope(&SearchScope::Constellation, "alice", &store).unwrap();
        assert_eq!(result, vec!["alice"]);
    }
}
