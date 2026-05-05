//! [`MemoryScope`] — policy gate over a [`MemoryStore`] using typed
//! [`Scope`] addresses.
//!
//! # Routing rules
//!
//! Reads (`get_block`, `get_block_metadata`, `get_rendered_content`):
//!
//! | Caller scope | None / CoreOnly | Full |
//! |---|---|---|
//! | `Scope::Local(_)` | hit project; on miss fall back to `Scope::Global(binding.persona_id)`. CoreOnly tags persona docs ReadOnly. | hit project only; no fallback. |
//! | `Scope::Global(_)` | pass through. CoreOnly tags ReadOnly. | return `None` (persona invisible). |
//!
//! Writes (`create_block`, `update_block_metadata`, `delete_block`,
//! `persist_block`, `mark_dirty`, `insert_archival`, `undo_redo`):
//!
//! | Caller scope | None | CoreOnly | Full |
//! |---|---|---|---|
//! | `Scope::Local(_)` | allow | allow | allow |
//! | `Scope::Global(_)` | allow | `IsolationDenied` | `IsolationDenied` |
//!
//! Writes are exact-target — no fallback. Read fallback exists because
//! agent ergonomics value forgiveness; write fallback would mask the
//! "writes go to the wrong place" footgun this redesign was created to
//! eliminate.
//!
//! When `binding.is_passthrough()` (no project mounted), the wrapper is
//! a transparent delegation layer.

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, IsolatePolicy, MemoryError,
    MemoryPermission, MemoryResult, MemorySearchResult, MemorySearchScope, Scope, SearchOptions,
    SharedBlockInfo, UndoRedoDepth, UndoRedoOp,
};
use serde_json::Value as JsonValue;

use super::ScopeBinding;

/// Policy-routing wrapper around any [`MemoryStore`].
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
    pub fn new(inner: S, binding: ScopeBinding) -> Self {
        Self { inner, binding }
    }

    pub fn binding(&self) -> &ScopeBinding {
        &self.binding
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: MemoryStore> MemoryScope<S> {
    /// The session's persona scope (always `Scope::Global(persona_id)`).
    fn persona_scope(&self) -> Scope {
        Scope::Global(self.binding.persona_id.clone().into())
    }

    /// Deny this write if the policy disallows mutating Global blocks.
    fn check_write(&self, scope: &Scope, operation: &str) -> MemoryResult<()> {
        if self.binding.is_passthrough() {
            return Ok(());
        }
        if scope.is_global() {
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

    /// Read fallback: when the caller asked for Local and the project
    /// scope returned `Ok(None)`, retry under `Scope::Global(persona_id)`
    /// (per policy). Returns the original result on first hit.
    ///
    /// `tag_persona_readonly` mutates the returned doc's permission to
    /// `ReadOnly` when the fallback fires under `CoreOnly`. The
    /// type-erased `T` is one of `StructuredDocument`, `BlockMetadata`,
    /// or `String` (rendered content) — see the helper variants below.
    fn fallback_get<T, F>(
        &self,
        scope: &Scope,
        label: &str,
        primary: MemoryResult<Option<T>>,
        lookup_persona: F,
        on_persona_hit: impl FnOnce(T) -> T,
    ) -> MemoryResult<Option<T>>
    where
        F: FnOnce(&Scope, &str) -> MemoryResult<Option<T>>,
    {
        if self.binding.is_passthrough() {
            return primary;
        }
        // Caller asked for Local: fall back to Global on miss.
        if scope.is_local() {
            match primary {
                Ok(Some(t)) => Ok(Some(t)),
                Ok(None) => match self.binding.policy {
                    IsolatePolicy::None | IsolatePolicy::CoreOnly => {
                        let persona = self.persona_scope();
                        match lookup_persona(&persona, label)? {
                            Some(t) => Ok(Some(on_persona_hit(t))),
                            None => Ok(None),
                        }
                    }
                    IsolatePolicy::Full | _ => Ok(None),
                },
                Err(e) => Err(e),
            }
        } else {
            // Caller asked for Global. Under Full, hide. Under CoreOnly,
            // tag ReadOnly. Under None, pass through.
            match self.binding.policy {
                IsolatePolicy::None => primary,
                IsolatePolicy::CoreOnly => match primary? {
                    Some(t) => Ok(Some(on_persona_hit(t))),
                    None => Ok(None),
                },
                IsolatePolicy::Full | _ => Ok(None),
            }
        }
    }

    /// Should `tag_persona_readonly` apply? Only under CoreOnly when the
    /// hit was at the persona scope.
    fn core_only(&self) -> bool {
        self.binding.policy == IsolatePolicy::CoreOnly
    }
}

impl<S: MemoryStore> MemoryStore for MemoryScope<S> {
    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.check_write(scope, &format!("create_block(label={})", create.label))?;
        self.inner.create_block(scope, create)
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        let primary = self.inner.get_block(scope, label);
        let core_only = self.core_only();
        self.fallback_get(
            scope,
            label,
            primary,
            |s, l| self.inner.get_block(s, l),
            move |mut doc| {
                if core_only {
                    doc.set_permission(MemoryPermission::ReadOnly);
                }
                doc
            },
        )
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        let primary = self.inner.get_block_metadata(scope, label);
        let core_only = self.core_only();
        self.fallback_get(
            scope,
            label,
            primary,
            |s, l| self.inner.get_block_metadata(s, l),
            move |mut meta| {
                if core_only {
                    meta.permission = MemoryPermission::ReadOnly;
                }
                meta
            },
        )
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        // Passthrough binding has no project: only the persona scope is
        // visible. If the caller didn't pin a scope, default to the
        // persona's `Scope::Global` so unmounted sessions don't leak rows
        // owned by other agents in the same DB.
        if self.binding.is_passthrough() {
            let mut f = filter;
            if f.agent_id.is_none() {
                f.agent_id = Some(self.persona_scope().to_db_key());
            }
            return self.inner.list_blocks(f);
        }

        // If the caller pinned a scope explicitly (via `BlockFilter::by_scope`),
        // honour it — they want a single-scope view (e.g. enumerating skills
        // within one scope). Skip the merge.
        if filter.agent_id.is_some() {
            return self.inner.list_blocks(filter);
        }

        // No explicit scope: enumerate every scope visible to this session.
        // Merge project + persona under None/CoreOnly with project winning
        // on label collision; project-only under Full.
        match self.binding.policy {
            IsolatePolicy::None | IsolatePolicy::CoreOnly => {
                let mut results = Vec::new();
                let mut seen = std::collections::HashSet::new();

                if let Some(ref project_id) = self.binding.project_id {
                    let mut f = filter.clone();
                    f.agent_id = Some(Scope::Local(project_id.clone().into()).to_db_key());
                    for meta in self.inner.list_blocks(f)? {
                        seen.insert(meta.label.clone());
                        results.push(meta);
                    }
                }

                let mut f = filter;
                f.agent_id = Some(self.persona_scope().to_db_key());
                for mut meta in self.inner.list_blocks(f)? {
                    if !seen.contains(&meta.label) {
                        if self.core_only() {
                            meta.permission = MemoryPermission::ReadOnly;
                        }
                        results.push(meta);
                    }
                }

                Ok(results)
            }
            IsolatePolicy::Full | _ => {
                if let Some(ref project_id) = self.binding.project_id {
                    let mut f = filter;
                    f.agent_id = Some(Scope::Local(project_id.clone().into()).to_db_key());
                    self.inner.list_blocks(f)
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.inner.commit_write(scope, label)
    }

    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        self.check_write(scope, &format!("create_or_replace_block(label={})", create.label))?;
        self.inner.create_or_replace_block(scope, create)
    }

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.check_write(scope, &format!("delete_block(label={label})"))?;
        self.inner.delete_block(scope, label)
    }

    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        let primary = self.inner.get_rendered_content(scope, label);
        // Rendered content has no permission field to mutate; pass-through identity.
        self.fallback_get(
            scope,
            label,
            primary,
            |s, l| self.inner.get_rendered_content(s, l),
            |s| s,
        )
    }

    fn persist_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.check_write(scope, &format!("persist_block(label={label})"))?;
        self.inner.persist_block(scope, label)
    }

    fn mark_dirty(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        self.check_write(scope, &format!("mark_dirty(label={label})"))?;
        self.inner.mark_dirty(scope, label)
    }

    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        self.check_write(scope, "insert_archival")?;
        self.inner.insert_archival(scope, content, metadata)
    }

    fn search_archival(
        &self,
        scope: &Scope,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        if self.binding.is_passthrough() {
            return self.inner.search_archival(scope, query, limit);
        }

        // Same fallback shape as block reads: project first, persona on miss
        // (under None/CoreOnly), else project-only under Full.
        match self.binding.policy {
            IsolatePolicy::None | IsolatePolicy::CoreOnly => {
                let mut results = Vec::new();
                if scope.is_local() {
                    results.extend(self.inner.search_archival(scope, query, limit)?);
                }
                let remaining = limit.saturating_sub(results.len());
                if remaining > 0 {
                    let persona = self.persona_scope();
                    let target = if scope.is_local() { &persona } else { scope };
                    results.extend(self.inner.search_archival(target, query, remaining)?);
                }
                Ok(results)
            }
            IsolatePolicy::Full | _ => {
                if scope.is_local() {
                    self.inner.search_archival(scope, query, limit)
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        // CLI-only entry point; scope layer does not gate.
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
            IsolatePolicy::None => self.inner.search(query, options, scope),
            IsolatePolicy::CoreOnly | IsolatePolicy::Full | _ => {
                // Restrict to project scope.
                if let Some(ref project_id) = self.binding.project_id {
                    self.inner.search(
                        query,
                        options,
                        MemorySearchScope::Scope(Scope::Local(project_id.clone().into())),
                    )
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn list_shared_blocks(&self, scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        // Shared blocks are a cross-agent concept; pass through.
        self.inner.list_shared_blocks(scope)
    }

    fn get_shared_block(
        &self,
        requester: &Scope,
        owner: &Scope,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // Shared block access is permission-checked by the underlying
        // store via explicit grants from the owner. Pass through.
        self.inner.get_shared_block(requester, owner, label)
    }

    fn update_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        self.check_write(scope, &format!("update_block_metadata(label={label})"))?;
        self.inner.update_block_metadata(scope, label, patch)
    }

    fn undo_redo(&self, scope: &Scope, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        self.check_write(scope, &format!("undo_redo(label={label})"))?;
        self.inner.undo_redo(scope, label, op)
    }

    fn history_depth(&self, scope: &Scope, label: &str) -> MemoryResult<UndoRedoDepth> {
        self.inner.history_depth(scope, label)
    }

    fn has_shared_blocks_with(&self, caller: &Scope, target: &Scope) -> MemoryResult<bool> {
        self.inner.has_shared_blocks_with(caller, target)
    }

    fn list_constellation_scopes(&self) -> MemoryResult<Vec<Scope>> {
        self.inner.list_constellation_scopes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ScopeTestStore;
    use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};

    fn binding_none() -> ScopeBinding {
        ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::None)
    }
    fn binding_core() -> ScopeBinding {
        ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::CoreOnly)
    }
    fn binding_full() -> ScopeBinding {
        ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::Full)
    }

    fn local() -> Scope {
        Scope::Local("project-1".into())
    }
    fn global() -> Scope {
        Scope::Global("persona-1".into())
    }

    // ---- read fallback ----

    #[test]
    fn local_read_falls_back_to_global_under_none() {
        let store = ScopeTestStore::new();
        store.seed(global(), "scratchpad", "persona notes");
        let scope = MemoryScope::new(store, binding_none());

        let content = scope.get_rendered_content(&local(), "scratchpad").unwrap();
        assert_eq!(content.as_deref(), Some("persona notes"));
    }

    #[test]
    fn local_read_hits_local_first_when_present() {
        let store = ScopeTestStore::new();
        store.seed(global(), "notes", "persona version");
        store.seed(local(), "notes", "project version");
        let scope = MemoryScope::new(store, binding_none());

        let content = scope.get_rendered_content(&local(), "notes").unwrap();
        assert_eq!(content.as_deref(), Some("project version"));
    }

    #[test]
    fn local_read_under_full_does_not_fall_back() {
        let store = ScopeTestStore::new();
        store.seed(global(), "scratchpad", "persona notes");
        let scope = MemoryScope::new(store, binding_full());

        let content = scope.get_rendered_content(&local(), "scratchpad").unwrap();
        assert!(content.is_none());
    }

    #[test]
    fn global_read_under_full_returns_none() {
        let store = ScopeTestStore::new();
        store.seed(global(), "scratchpad", "persona notes");
        let scope = MemoryScope::new(store, binding_full());

        let content = scope.get_rendered_content(&global(), "scratchpad").unwrap();
        assert!(content.is_none());
    }

    #[test]
    fn global_read_under_core_only_tags_readonly() {
        let store = ScopeTestStore::new();
        store.seed(global(), "scratchpad", "persona notes");
        let scope = MemoryScope::new(store, binding_core());

        let doc = scope.get_block(&global(), "scratchpad").unwrap().unwrap();
        assert_eq!(doc.metadata().permission, MemoryPermission::ReadOnly);
    }

    #[test]
    fn local_fallback_to_global_under_core_only_tags_readonly() {
        let store = ScopeTestStore::new();
        store.seed(global(), "scratchpad", "persona notes");
        let scope = MemoryScope::new(store, binding_core());

        let doc = scope.get_block(&local(), "scratchpad").unwrap().unwrap();
        assert_eq!(doc.metadata().permission, MemoryPermission::ReadOnly);
    }

    // ---- write enforcement ----

    #[test]
    fn local_writes_allowed_under_all_policies() {
        for policy in [
            IsolatePolicy::None,
            IsolatePolicy::CoreOnly,
            IsolatePolicy::Full,
        ] {
            let store = ScopeTestStore::new();
            let binding =
                ScopeBinding::with_project("persona-1", "project-1", policy);
            let scope = MemoryScope::new(store, binding);

            let result = scope.create_block(
                &local(),
                BlockCreate::new("task-list", MemoryBlockType::Working, BlockSchema::text()),
            );
            assert!(result.is_ok(), "Local write under {policy:?} should be allowed");
        }
    }

    #[test]
    fn global_write_allowed_under_none() {
        let store = ScopeTestStore::new();
        let scope = MemoryScope::new(store, binding_none());

        let result = scope.create_block(
            &global(),
            BlockCreate::new("personal-notes", MemoryBlockType::Core, BlockSchema::text()),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn global_create_denied_under_core_only() {
        let store = ScopeTestStore::new();
        let scope = MemoryScope::new(store, binding_core());

        let err = scope
            .create_block(
                &global(),
                BlockCreate::new("notes", MemoryBlockType::Core, BlockSchema::text()),
            )
            .unwrap_err();
        assert!(matches!(err, MemoryError::IsolationDenied { .. }));
    }

    #[test]
    fn global_update_denied_under_full() {
        let store = ScopeTestStore::new();
        store.seed(global(), "scratchpad", "persona notes");
        let scope = MemoryScope::new(store, binding_full());

        let err = scope
            .update_block_metadata(
                &global(),
                "scratchpad",
                BlockMetadataPatch::default().pinned(true),
            )
            .unwrap_err();
        assert!(matches!(err, MemoryError::IsolationDenied { .. }));
    }

    #[test]
    fn global_mark_dirty_denied_under_core_only() {
        let store = ScopeTestStore::new();
        let scope = MemoryScope::new(store, binding_core());

        let err = scope.mark_dirty(&global(), "any").unwrap_err();
        assert!(matches!(err, MemoryError::IsolationDenied { .. }));
    }

    // ---- list_blocks ----

    #[test]
    fn list_blocks_merges_under_none() {
        let store = ScopeTestStore::new();
        store.seed(global(), "shared-label", "persona version");
        store.seed(local(), "shared-label", "project version");
        store.seed(global(), "persona-only", "p");
        store.seed(local(), "project-only", "j");
        let scope = MemoryScope::new(store, binding_none());

        let blocks = scope.list_blocks(BlockFilter::all()).unwrap();
        let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();

        // shared-label appears once (project wins).
        assert_eq!(labels.iter().filter(|l| **l == "shared-label").count(), 1);
        assert!(labels.contains(&"persona-only"));
        assert!(labels.contains(&"project-only"));
    }

    #[test]
    fn list_blocks_under_full_is_project_only() {
        let store = ScopeTestStore::new();
        store.seed(global(), "persona-block", "p");
        store.seed(local(), "project-block", "j");
        let scope = MemoryScope::new(store, binding_full());

        let blocks = scope.list_blocks(BlockFilter::all()).unwrap();
        let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();

        assert!(labels.contains(&"project-block"));
        assert!(!labels.contains(&"persona-block"));
    }

    // ---- passthrough ----

    #[test]
    fn passthrough_delegates_directly() {
        let store = ScopeTestStore::new();
        store.seed(Scope::Global("agent-1".into()), "notes", "hello");
        let scope = MemoryScope::new(store, ScopeBinding::passthrough("agent-1"));

        let content = scope
            .get_rendered_content(&Scope::Global("agent-1".into()), "notes")
            .unwrap();
        assert_eq!(content.as_deref(), Some("hello"));
    }
}
