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
//!   (b) target has shared ≥1 block with caller, OR
//!   (c) both are in the same agent_group.
//!   Otherwise returns a permission-denied error.
//! - `Agents(ids)` → per-id same check; filters out unpermitted without
//!   erroring. Returns error only if the resulting set is empty.
//! - `Constellation` → all constellation agents if caller has the
//!   constellation-wide-search permission (currently: always allowed if
//!   there are agents). Future phases may add a trust-level gate.
//!
//! The ordering of checks is: self → shared-blocks → group-membership.
//! This is intentional: shared-blocks is a stronger signal of
//! cooperation than group membership (which may be broad), and
//! short-circuiting on the cheaper self-check avoids unnecessary DB
//! queries.

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
pub async fn resolve_scope(
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
            if check_cross_agent_permission(caller, target_str, store).await? {
                Ok(vec![target_str.to_string()])
            } else {
                Err(EffectError::Handler(format!(
                    "permission denied: agent {caller:?} cannot search agent {target_str:?} \
                     (no shared blocks or group membership)"
                )))
            }
        }

        SearchScope::Agents(ids) => {
            let mut allowed = Vec::with_capacity(ids.len());
            for id in ids {
                let id_str = id.as_str();
                if id_str == caller {
                    allowed.push(caller.to_string());
                } else if check_cross_agent_permission(caller, id_str, store).await? {
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
            let agents = store
                .list_constellation_agent_ids()
                .await
                .map_err(|e| EffectError::Handler(format!("constellation lookup failed: {e}")))?;
            if agents.is_empty() {
                // Fall back to just the caller if the store has no agents listed.
                Ok(vec![caller.to_string()])
            } else {
                Ok(agents)
            }
        }
    }
}

/// Check whether `caller` has cross-agent permission to access
/// `target`'s data. Checks shared-blocks first (stronger signal),
/// then group membership.
async fn check_cross_agent_permission(
    caller: &str,
    target: &str,
    store: &dyn MemoryStore,
) -> Result<bool, EffectError> {
    // Check shared blocks.
    let shared = store
        .has_shared_blocks_with(caller, target)
        .await
        .map_err(|e| EffectError::Handler(format!("shared-block check failed: {e}")))?;
    if shared {
        return Ok(true);
    }

    // Check group membership.
    let in_group = store
        .shares_group_with(caller, target)
        .await
        .map_err(|e| EffectError::Handler(format!("group-membership check failed: {e}")))?;
    Ok(in_group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use pattern_core::memory::*;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::block::BlockCreate;
    use serde_json::Value as JsonValue;

    /// Test double for scope resolution. Tracks shared-blocks and group
    /// membership relationships without needing real DB queries.
    #[derive(Debug, Default)]
    struct ScopeTestStore {
        /// (caller, target) pairs where target has shared blocks with caller.
        shared_blocks: Mutex<HashSet<(String, String)>>,
        /// (a, b) pairs where a and b share a group.
        shared_groups: Mutex<HashSet<(String, String)>>,
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

        fn add_group_membership(&self, a: &str, b: &str) {
            let mut groups = self.shared_groups.lock().unwrap();
            groups.insert((a.to_string(), b.to_string()));
            groups.insert((b.to_string(), a.to_string()));
        }

        fn set_constellation_agents(&self, agents: Vec<&str>) {
            *self.constellation_agents.lock().unwrap() =
                agents.into_iter().map(String::from).collect();
        }
    }

    #[async_trait]
    impl MemoryStore for ScopeTestStore {
        // Scope resolution only uses the three new methods; everything
        // else can panic.

        async fn has_shared_blocks_with(&self, caller: &str, target: &str) -> MemoryResult<bool> {
            Ok(self
                .shared_blocks
                .lock()
                .unwrap()
                .contains(&(caller.to_string(), target.to_string())))
        }

        async fn shares_group_with(&self, caller: &str, target: &str) -> MemoryResult<bool> {
            Ok(self
                .shared_groups
                .lock()
                .unwrap()
                .contains(&(caller.to_string(), target.to_string())))
        }

        async fn list_constellation_agent_ids(&self) -> MemoryResult<Vec<String>> {
            Ok(self.constellation_agents.lock().unwrap().clone())
        }

        // ---- Stubs for the rest of MemoryStore ----

        async fn create_block(&self, _: &str, _: BlockCreate) -> MemoryResult<StructuredDocument> {
            panic!("not used in scope tests")
        }
        async fn get_block(&self, _: &str, _: &str) -> MemoryResult<Option<StructuredDocument>> {
            panic!("not used in scope tests")
        }
        async fn get_block_metadata(
            &self,
            _: &str,
            _: &str,
        ) -> MemoryResult<Option<BlockMetadata>> {
            panic!()
        }
        async fn list_blocks(&self, _: &str) -> MemoryResult<Vec<BlockMetadata>> {
            panic!()
        }
        async fn list_blocks_by_type(
            &self,
            _: &str,
            _: BlockType,
        ) -> MemoryResult<Vec<BlockMetadata>> {
            panic!()
        }
        async fn list_all_blocks_by_label_prefix(
            &self,
            _: &str,
        ) -> MemoryResult<Vec<BlockMetadata>> {
            panic!()
        }
        async fn delete_block(&self, _: &str, _: &str) -> MemoryResult<()> {
            panic!()
        }
        async fn get_rendered_content(&self, _: &str, _: &str) -> MemoryResult<Option<String>> {
            panic!()
        }
        async fn persist_block(&self, _: &str, _: &str) -> MemoryResult<()> {
            panic!()
        }
        fn mark_dirty(&self, _: &str, _: &str) {
            panic!()
        }
        async fn insert_archival(
            &self,
            _: &str,
            _: &str,
            _: Option<JsonValue>,
        ) -> MemoryResult<String> {
            panic!()
        }
        async fn search_archival(
            &self,
            _: &str,
            _: &str,
            _: usize,
        ) -> MemoryResult<Vec<ArchivalEntry>> {
            panic!()
        }
        async fn delete_archival(&self, _: &str) -> MemoryResult<()> {
            panic!()
        }
        async fn search(
            &self,
            _: &str,
            _: &str,
            _: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            panic!()
        }
        async fn search_all(
            &self,
            _: &str,
            _: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            panic!()
        }
        async fn list_shared_blocks(&self, _: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
            panic!()
        }
        async fn get_shared_block(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            panic!()
        }
        async fn set_block_pinned(&self, _: &str, _: &str, _: bool) -> MemoryResult<()> {
            panic!()
        }
        async fn set_block_type(&self, _: &str, _: &str, _: BlockType) -> MemoryResult<()> {
            panic!()
        }
        async fn update_block_schema(&self, _: &str, _: &str, _: BlockSchema) -> MemoryResult<()> {
            panic!()
        }
        async fn update_block_description(&self, _: &str, _: &str, _: &str) -> MemoryResult<()> {
            panic!()
        }
        async fn undo_block(&self, _: &str, _: &str) -> MemoryResult<bool> {
            panic!()
        }
        async fn redo_block(&self, _: &str, _: &str) -> MemoryResult<bool> {
            panic!()
        }
        async fn undo_depth(&self, _: &str, _: &str) -> MemoryResult<usize> {
            panic!()
        }
        async fn redo_depth(&self, _: &str, _: &str) -> MemoryResult<usize> {
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

    #[tokio::test]
    async fn resolve_current_agent_always_returns_caller() {
        let store = ScopeTestStore::new();
        let result = resolve_scope(&SearchScope::CurrentAgent, "alice", &store)
            .await
            .unwrap();
        assert_eq!(result, vec!["alice"]);
    }

    #[tokio::test]
    async fn resolve_agent_self_always_allowed() {
        let store = ScopeTestStore::new();
        let result = resolve_scope(&SearchScope::Agent("alice".into()), "alice", &store)
            .await
            .unwrap();
        assert_eq!(result, vec!["alice"]);
    }

    #[tokio::test]
    async fn resolve_agent_denied_without_relationship() {
        let store = ScopeTestStore::new();
        let err = resolve_scope(&SearchScope::Agent("bob".into()), "alice", &store)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("permission denied"), "got: {err}");
    }

    #[tokio::test]
    async fn resolve_agent_allowed_via_shared_blocks() {
        let store = ScopeTestStore::new();
        store.add_shared_blocks("alice", "bob");
        let result = resolve_scope(&SearchScope::Agent("bob".into()), "alice", &store)
            .await
            .unwrap();
        assert_eq!(result, vec!["bob"]);
    }

    #[tokio::test]
    async fn resolve_agent_allowed_via_group_membership() {
        let store = ScopeTestStore::new();
        store.add_group_membership("alice", "bob");
        let result = resolve_scope(&SearchScope::Agent("bob".into()), "alice", &store)
            .await
            .unwrap();
        assert_eq!(result, vec!["bob"]);
    }

    #[tokio::test]
    async fn resolve_agents_filters_unpermitted() {
        let store = ScopeTestStore::new();
        store.add_shared_blocks("alice", "bob");
        // charlie has no relationship with alice.
        let result = resolve_scope(
            &SearchScope::Agents(vec!["bob".into(), "charlie".into()]),
            "alice",
            &store,
        )
        .await
        .unwrap();
        assert_eq!(result, vec!["bob"]);
    }

    #[tokio::test]
    async fn resolve_agents_all_denied_errors() {
        let store = ScopeTestStore::new();
        let err = resolve_scope(
            &SearchScope::Agents(vec!["bob".into(), "charlie".into()]),
            "alice",
            &store,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("permission denied"), "got: {err}");
    }

    #[tokio::test]
    async fn resolve_agents_includes_self() {
        let store = ScopeTestStore::new();
        // alice is always allowed.
        let result = resolve_scope(
            &SearchScope::Agents(vec!["alice".into(), "bob".into()]),
            "alice",
            &store,
        )
        .await
        .unwrap();
        assert_eq!(result, vec!["alice"]);
    }

    #[tokio::test]
    async fn resolve_constellation_returns_all_agents() {
        let store = ScopeTestStore::new();
        store.set_constellation_agents(vec!["alice", "bob", "charlie"]);
        let result = resolve_scope(&SearchScope::Constellation, "alice", &store)
            .await
            .unwrap();
        assert_eq!(result, vec!["alice", "bob", "charlie"]);
    }

    #[tokio::test]
    async fn resolve_constellation_empty_falls_back_to_caller() {
        let store = ScopeTestStore::new();
        let result = resolve_scope(&SearchScope::Constellation, "alice", &store)
            .await
            .unwrap();
        assert_eq!(result, vec!["alice"]);
    }
}
