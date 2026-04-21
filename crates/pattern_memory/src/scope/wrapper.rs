//! [`MemoryScope`] — a policy-routing wrapper around any [`MemoryStore`].
//!
//! Sits between the caller (SessionContext / adapter) and the underlying
//! store (MemoryCache). Every trait method is intercepted and routed based
//! on the [`IsolatePolicy`] in the [`ScopeBinding`].
//!
//! # Routing rules
//!
//! **Read path** (`get_block`, `get_block_metadata`, `get_rendered_content`,
//! `list_blocks`, `search`):
//!
//! - `None`: check project scope first (if `project_id` is set); fall back to
//!   persona scope. Project wins on label collision.
//! - `CoreOnly`: same as `None` for reads, but persona results are returned
//!   with their permission set to `ReadOnly`.
//! - `Full`: project scope only. Persona blocks are invisible.
//!
//! **Write path** (`create_block`, `update_block_metadata`, `delete_block`,
//! `persist_block`, `mark_dirty`):
//!
//! - Default write target is the agent_id the caller passes in. The scope
//!   layer only *denies* writes — it does not silently redirect.
//! - Under `CoreOnly` or `Full`, writes targeting the `persona_id` return
//!   `MemoryError::IsolationDenied`.
//! - Under `None`, all writes pass through (bidirectional).
//!
//! **Explicit persona write** (`write_to_persona` SDK effect):
//!
//! The SDK handler calls the store with `agent_id = persona_id` directly.
//! Under `None` this passes through. Under `CoreOnly`/`Full` the scope
//! layer returns `IsolationDenied` — the SDK handler converts that to an
//! effect error.

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, IsolatePolicy, MemoryError,
    MemoryResult, MemorySearchResult, MemorySearchScope, SearchOptions, SharedBlockInfo,
    UndoRedoDepth, UndoRedoOp,
};
use serde_json::Value as JsonValue;

use super::ScopeBinding;

/// Policy-routing wrapper around any [`MemoryStore`].
///
/// Generic over `S` so it can wrap `MemoryCache`, `InMemoryMemoryStore`, or
/// any other test double. The wrapper is transparent when the binding has no
/// project scope (passthrough mode).
pub struct MemoryScope<S> {
    inner: S,
    binding: ScopeBinding,
}

impl<S: std::fmt::Debug> std::fmt::Debug for MemoryScope<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryScope")
            .field("inner", &self.inner)
            .field("binding", &self.binding)
            .finish()
    }
}

impl<S> MemoryScope<S> {
    /// Wrap a store with the given scope binding.
    pub fn new(inner: S, binding: ScopeBinding) -> Self {
        Self { inner, binding }
    }

    /// Access the scope binding.
    pub fn binding(&self) -> &ScopeBinding {
        &self.binding
    }

    /// Access the inner store.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: MemoryStore> MemoryScope<S> {
    /// Check whether a write targeting `agent_id` is denied by the current
    /// isolation policy.
    fn deny_persona_write(&self, agent_id: &str, operation: &str) -> MemoryResult<()> {
        if self.binding.is_passthrough() {
            return Ok(());
        }
        if agent_id == self.binding.persona_id {
            match self.binding.policy {
                IsolatePolicy::None => Ok(()),
                IsolatePolicy::CoreOnly | IsolatePolicy::Full | _ => {
                    Err(MemoryError::IsolationDenied {
                        operation: operation.to_string(),
                        policy: self.binding.policy,
                    })
                }
            }
        } else {
            Ok(())
        }
    }

    /// Get a block with fallback semantics per policy.
    ///
    /// For `None` and `CoreOnly`: check project first, fall back to persona.
    /// For `Full`: project only.
    fn get_block_routed(
        &self,
        label: &str,
        mark_persona_readonly: bool,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // No project scope → passthrough to inner with whatever agent_id
        // the caller originally wanted. But this method is called from
        // the trait impl which passes a specific agent_id — we need to
        // check both project and persona.
        if let Some(project_id) = &self.binding.project_id {
            // Check project scope first (project wins on collision).
            if let Some(doc) = self.inner.get_block(project_id, label)? {
                return Ok(Some(doc));
            }
        }

        match self.binding.policy {
            IsolatePolicy::None | IsolatePolicy::CoreOnly => {
                // Fall through to persona.
                match self.inner.get_block(&self.binding.persona_id, label)? {
                    Some(mut doc) if mark_persona_readonly => {
                        doc.set_permission(pattern_db::models::MemoryPermission::ReadOnly);
                        Ok(Some(doc))
                    }
                    other => Ok(other),
                }
            }
            // Full and any future unknown policies hide persona blocks.
            IsolatePolicy::Full | _ => Ok(None),
        }
    }
}

impl<S: MemoryStore> MemoryStore for MemoryScope<S> {
    fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.deny_persona_write(agent_id, &format!("create_block(label={})", create.label))?;
        self.inner.create_block(agent_id, create)
    }

    fn get_block(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        if self.binding.is_passthrough() {
            return self.inner.get_block(agent_id, label);
        }

        let mark_readonly = self.binding.policy == IsolatePolicy::CoreOnly;
        self.get_block_routed(label, mark_readonly)
    }

    fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        if self.binding.is_passthrough() {
            return self.inner.get_block_metadata(agent_id, label);
        }

        // Same routing as get_block but for metadata.
        if let Some(project_id) = &self.binding.project_id
            && let Some(meta) = self.inner.get_block_metadata(project_id, label)?
        {
            return Ok(Some(meta));
        }

        match self.binding.policy {
            IsolatePolicy::None | IsolatePolicy::CoreOnly => self
                .inner
                .get_block_metadata(&self.binding.persona_id, label),
            IsolatePolicy::Full | _ => Ok(None),
        }
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        if self.binding.is_passthrough() {
            return self.inner.list_blocks(filter);
        }

        match self.binding.policy {
            IsolatePolicy::None | IsolatePolicy::CoreOnly => {
                // Merge persona + project blocks. Project wins on label collision.
                let mut results = Vec::new();
                let mut seen_labels = std::collections::HashSet::new();

                // Project blocks first.
                if let Some(project_id) = &self.binding.project_id {
                    let mut project_filter = filter.clone();
                    project_filter.agent_id = Some(project_id.clone());
                    for meta in self.inner.list_blocks(project_filter)? {
                        seen_labels.insert(meta.label.clone());
                        results.push(meta);
                    }
                }

                // Persona blocks (skip labels already seen from project).
                let mut persona_filter = filter;
                persona_filter.agent_id = Some(self.binding.persona_id.clone());
                for meta in self.inner.list_blocks(persona_filter)? {
                    if !seen_labels.contains(&meta.label) {
                        results.push(meta);
                    }
                }

                Ok(results)
            }
            IsolatePolicy::Full | _ => {
                // Project only.
                if let Some(project_id) = &self.binding.project_id {
                    let mut project_filter = filter;
                    project_filter.agent_id = Some(project_id.clone());
                    self.inner.list_blocks(project_filter)
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.deny_persona_write(agent_id, &format!("delete_block(label={label})"))?;
        self.inner.delete_block(agent_id, label)
    }

    fn get_rendered_content(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        if self.binding.is_passthrough() {
            return self.inner.get_rendered_content(agent_id, label);
        }

        // Same routing logic as get_block: project first, then persona.
        if let Some(project_id) = &self.binding.project_id
            && let Some(content) = self.inner.get_rendered_content(project_id, label)?
        {
            return Ok(Some(content));
        }

        match self.binding.policy {
            IsolatePolicy::None | IsolatePolicy::CoreOnly => self
                .inner
                .get_rendered_content(&self.binding.persona_id, label),
            IsolatePolicy::Full | _ => Ok(None),
        }
    }

    fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        self.deny_persona_write(agent_id, &format!("persist_block(label={label})"))?;
        self.inner.persist_block(agent_id, label)
    }

    fn mark_dirty(&self, agent_id: &str, label: &str) {
        // mark_dirty does not return a Result, so we cannot deny here.
        // However, a write that was denied at create/update time will never
        // reach mark_dirty for the persona scope. We still delegate to the
        // inner store — if someone calls mark_dirty on a persona block under
        // CoreOnly/Full, the subsequent persist_block will be denied.
        self.inner.mark_dirty(agent_id, label);
    }

    fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        self.deny_persona_write(agent_id, "insert_archival")?;
        self.inner.insert_archival(agent_id, content, metadata)
    }

    fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        if self.binding.is_passthrough() {
            return self.inner.search_archival(agent_id, query, limit);
        }

        // Archival search follows the same read policy: merge under None,
        // project-only under CoreOnly/Full.
        match self.binding.policy {
            IsolatePolicy::None => {
                // Merge persona + project archival results.
                let mut results = Vec::new();
                if let Some(project_id) = &self.binding.project_id {
                    results.extend(self.inner.search_archival(project_id, query, limit)?);
                }
                let remaining = limit.saturating_sub(results.len());
                if remaining > 0 {
                    results.extend(self.inner.search_archival(
                        &self.binding.persona_id,
                        query,
                        remaining,
                    )?);
                }
                Ok(results)
            }
            IsolatePolicy::CoreOnly | IsolatePolicy::Full | _ => {
                // Project only for archival search.
                if let Some(project_id) = &self.binding.project_id {
                    self.inner.search_archival(project_id, query, limit)
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        // Archival deletion is by entry id, not agent_id. We cannot
        // determine ownership from the id alone, so we delegate directly.
        // This method is only reachable via human-operator tooling (CLI),
        // not agent effects, so the isolation boundary is less critical.
        self.inner.delete_archival(id)
    }

    fn search(
        &self,
        query: &str,
        options: SearchOptions,
        scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        if self.binding.is_passthrough() {
            return self.inner.search(query, options, scope);
        }

        match self.binding.policy {
            IsolatePolicy::None => {
                // Let the search through with the original scope. The
                // underlying store handles merging across agents.
                self.inner.search(query, options, scope)
            }
            IsolatePolicy::CoreOnly | IsolatePolicy::Full | _ => {
                // Restrict search to project scope.
                if let Some(project_id) = &self.binding.project_id {
                    self.inner.search(
                        query,
                        options,
                        MemorySearchScope::Agent(project_id.clone().into()),
                    )
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        // Shared blocks are a cross-agent concept. Delegate directly.
        self.inner.list_shared_blocks(agent_id)
    }

    fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // Shared block access is already permission-checked by the store.
        // The scope layer does not add additional restrictions — shared
        // blocks are an explicit grant from the owner.
        self.inner
            .get_shared_block(requester_agent_id, owner_agent_id, label)
    }

    fn update_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        self.deny_persona_write(agent_id, &format!("update_block_metadata(label={label})"))?;
        self.inner.update_block_metadata(agent_id, label, patch)
    }

    fn undo_redo(&self, agent_id: &str, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        // Undo/redo is a write operation.
        self.deny_persona_write(agent_id, &format!("undo_redo(label={label})"))?;
        self.inner.undo_redo(agent_id, label, op)
    }

    fn history_depth(&self, agent_id: &str, label: &str) -> MemoryResult<UndoRedoDepth> {
        // Read-only operation, delegate directly.
        self.inner.history_depth(agent_id, label)
    }

    fn has_shared_blocks_with(&self, caller: &str, target: &str) -> MemoryResult<bool> {
        self.inner.has_shared_blocks_with(caller, target)
    }

    fn shares_group_with(&self, caller: &str, target: &str) -> MemoryResult<bool> {
        self.inner.shares_group_with(caller, target)
    }

    fn list_constellation_agent_ids(&self) -> MemoryResult<Vec<String>> {
        self.inner.list_constellation_agent_ids()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ScopeTestStore;
    use pattern_core::types::memory_types::{BlockSchema, BlockType};

    // ---- AC12.1: IsolatePolicy::None merges both scopes ----

    #[test]
    fn none_policy_reads_merge_persona_and_project() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "scratchpad", "persona notes");
        store.seed("project-1", "readme", "project readme");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::None),
        );

        // Both blocks are visible.
        let scratch = scope
            .get_rendered_content("persona-1", "scratchpad")
            .unwrap();
        assert_eq!(scratch.as_deref(), Some("persona notes"));

        let readme = scope.get_rendered_content("project-1", "readme").unwrap();
        assert_eq!(readme.as_deref(), Some("project readme"));
    }

    #[test]
    fn none_policy_project_wins_on_label_collision() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "notes", "persona version");
        store.seed("project-1", "notes", "project version");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::None),
        );

        // Project wins on collision.
        let notes = scope.get_rendered_content("any", "notes").unwrap();
        assert_eq!(notes.as_deref(), Some("project version"));
    }

    // ---- AC12.2: IsolatePolicy::CoreOnly — persona read-only ----

    #[test]
    fn core_only_persona_blocks_marked_readonly() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "scratchpad", "persona notes");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::CoreOnly),
        );

        let doc = scope.get_block("any", "scratchpad").unwrap().unwrap();
        assert_eq!(
            doc.metadata().permission,
            pattern_db::models::MemoryPermission::ReadOnly
        );
    }

    #[test]
    fn core_only_denies_persona_write() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "scratchpad", "persona notes");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::CoreOnly),
        );

        let result = scope.update_block_metadata(
            "persona-1",
            "scratchpad",
            BlockMetadataPatch::default().pinned(true),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, MemoryError::IsolationDenied { .. }),
            "expected IsolationDenied, got: {err:?}"
        );
    }

    // ---- AC12.3: IsolatePolicy::Full — persona invisible ----

    #[test]
    fn full_policy_persona_blocks_invisible() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "scratchpad", "persona notes");
        store.seed("project-1", "readme", "project readme");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::Full),
        );

        // Persona block invisible.
        let scratch = scope.get_rendered_content("any", "scratchpad").unwrap();
        assert!(scratch.is_none());

        // Project block visible.
        let readme = scope.get_rendered_content("any", "readme").unwrap();
        assert_eq!(readme.as_deref(), Some("project readme"));
    }

    #[test]
    fn full_policy_denies_persona_write() {
        let store = ScopeTestStore::new();

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::Full),
        );

        let result = scope.create_block(
            "persona-1",
            BlockCreate::new("new-block", BlockType::Working, BlockSchema::text()),
        );
        assert!(matches!(
            result.unwrap_err(),
            MemoryError::IsolationDenied { .. }
        ));
    }

    // ---- AC12.6: Default writes go to project scope ----

    #[test]
    fn none_policy_write_to_project_succeeds() {
        let store = ScopeTestStore::new();

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::None),
        );

        // Write to project-1 (not persona-1) succeeds under None.
        let result = scope.create_block(
            "project-1",
            BlockCreate::new("task-list", BlockType::Working, BlockSchema::text()),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn none_policy_write_to_persona_succeeds() {
        let store = ScopeTestStore::new();

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::None),
        );

        // Under None, writes to persona are also allowed (bidirectional).
        let result = scope.create_block(
            "persona-1",
            BlockCreate::new("personal-notes", BlockType::Core, BlockSchema::text()),
        );
        assert!(result.is_ok());
    }

    // ---- Passthrough (no project) ----

    #[test]
    fn passthrough_delegates_directly() {
        let store = ScopeTestStore::new();
        store.seed("agent-1", "notes", "hello");

        let scope = MemoryScope::new(store, ScopeBinding::passthrough("agent-1"));

        let content = scope.get_rendered_content("agent-1", "notes").unwrap();
        assert_eq!(content.as_deref(), Some("hello"));
    }

    // ---- list_blocks merging ----

    #[test]
    fn none_policy_list_blocks_merges_deduplicating_by_label() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "shared-label", "persona version");
        store.seed("project-1", "shared-label", "project version");
        store.seed("persona-1", "persona-only", "only in persona");
        store.seed("project-1", "project-only", "only in project");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::None),
        );

        let blocks = scope.list_blocks(BlockFilter::all()).unwrap();
        let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();

        // shared-label appears only once (from project).
        assert_eq!(labels.iter().filter(|l| **l == "shared-label").count(), 1);
        // Both unique labels present.
        assert!(labels.contains(&"persona-only"));
        assert!(labels.contains(&"project-only"));
    }

    #[test]
    fn full_policy_list_blocks_project_only() {
        let store = ScopeTestStore::new();
        store.seed("persona-1", "persona-block", "content");
        store.seed("project-1", "project-block", "content");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::Full),
        );

        let blocks = scope.list_blocks(BlockFilter::all()).unwrap();
        let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();

        assert!(labels.contains(&"project-block"));
        assert!(!labels.contains(&"persona-block"));
    }
}
