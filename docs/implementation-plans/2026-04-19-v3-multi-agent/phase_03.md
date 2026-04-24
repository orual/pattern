# v3-multi-agent Phase 3: Fork isolation and merge

**Goal:** flesh out fork semantics on top of the Phase 2 scaffolding — wire `LoroDoc::fork()` + `LoroDoc::import()` for lightweight isolation, wire `JjAdapter::workspace_add` + namespaced bookmarks + `JjAdapter::merge` for persistent isolation, implement the four resolution paths (`await_result`, `merge_back`, `discard`, `promote`), and land the fork-to-sibling promotion path with capability gating. Add a fork-merge memory-consistency test suite that proves concurrent parent+fork edits converge deterministically.

**Architecture:** fork isolation is a two-axis choice per fork — lightweight (in-memory `LoroDoc::fork()`, zero disk writes, lives and dies with the spawning runtime) or persistent (jj workspace carved from the mount's existing repo, namespaced bookmark `<agent-id>/<task-id>`, survives restart). Resolution is a method on `ForkHandle` that both the Rust SDK caller and the Haskell side can reach; merge semantics differ by isolation mode but promotion is uniform. Draft-persona registration from `promote()` routes through the Phase 2 draft stub; Phase 6 later swaps the stub for the real registry without touching this phase's code.

**Tech Stack:** `loro = 1.10.3` (`LoroDoc::fork()`, `LoroDoc::import()`, `LoroDoc::export_snapshot()`), existing `pattern_memory::jj::JjAdapter` surface (workspace add/forget/update_stale, bookmark set/delete, merge, commit), existing `StructuredDocument` wrapper + `apply_updates` path, `pattern_memory::modes` (InRepo / Standalone / Sidecar — persistent forks require a jj-enabled mode), `proptest` for concurrent-edit property tests, `insta` for merge-outcome snapshots.

**Scope:** 3 of 7. Finishes AC4 completely (both isolation modes + all four resolution paths), completes the AC4.7 / AC4.8 promote-to-sibling pair wired through Phase 2's draft path, confirms AC3 / AC5 still pass after the richer fork shape.

**Codebase verified:** 2026-04-23.

---

## Codebase verification findings

- ✓ `loro = 1.10.3` in `crates/pattern_memory/Cargo.toml:27`. `LoroDoc::fork()` and `LoroDoc::import()` both exist in the crate; `import()` is already called at `crates/pattern_core/src/memory/document.rs:51` (`from_snapshot_with_metadata`), proving the shape is reachable. `fork()` has no existing call site — Phase 3 adds the first.
- ✓ `StructuredDocument` at `crates/pattern_core/src/memory/document.rs` wraps `LoroDoc` (private field `doc`) with `pub fn inner(&self) -> &LoroDoc` at line 378 and `pub fn apply_updates(&self, updates: &[u8]) -> Result<(), DocumentError>` at line 129 (internally calls `self.doc.import(updates)`, wrapping errors as `DocumentError::ImportFailed`).
- ✓ `MemoryCache` at `crates/pattern_memory/src/cache.rs:44` owns `Arc<DashMap<String, CachedBlock>>` plus subscribers. `cache.get(agent_id, label)` at line 263 returns `Option<StructuredDocument>` — the access path for the parent doc that we then fork.
- ✓ `JjAdapter` at `crates/pattern_memory/src/jj/` is production-complete per Plan 1 (Phase 5 `jj_adapter_mutate.rs` tests 2026-04-20):
  - Detection: `JjAdapter::detect() -> JjResult<Option<Self>>` (`adapter.rs:54`).
  - Workspace: `workspace_add(repo_root, new_path)` (`:224`), `workspace_forget(repo_root, name)` (`:245`), `workspace_update_stale(workspace_root)` (`:269`), `workspace_list(repo_root)` (`:156`).
  - Commit / log: `commit(workspace_root, message)` (`:287`), `log(workspace_root, revset)` (`:131`).
  - Bookmarks: `bookmark_set(repo_root, name, revset)` (`:321`), `bookmark_delete(repo_root, name)` (`:340`), `bookmark_list(repo_root)` (`:179`).
  - Merge / restore: `merge(repo_root, source_revset, dest_revset)` (`:366`), `restore(workspace_root, from_rev, paths)` (`:404`).
- ✓ Mount modes at `crates/pattern_memory/src/modes.rs`: `InRepo`, `Standalone`, `Sidecar`. `StorageMode::requires_jj()` (`:87`) distinguishes jj-backed modes. Persistent forks require `requires_jj() == true`. InRepo mode without jj returns a clear "persistent fork not available" error; lightweight forks still work.
- ✓ `.pattern.kdl` `jj enabled=false` section at `crates/pattern_memory/src/config/pattern_kdl.rs:249-257` — toggle is already decoded; persistent fork is gated on both mode AND this toggle (even InRepo can be jj-enabled if the toggle is true, but the mount-mode-to-workspace-location mapping is explicit).
- ✓ `BlockRef` at `crates/pattern_core/src/types/block_ref.rs` (`label`, `block_id`, `agent_id` fields). Already used by Phase 2 `ForkConfig.task_ref`.
- ✗ No existing loro fork+import round-trip test. Phase 3 writes this coverage.
- ✗ No existing bookmark-naming convention enforced in code. Phase 3 introduces `fn fork_bookmark_name(agent: &AgentId, task: Option<&BlockRef>) -> String` producing `<agent>/<task-label>` or `<agent>/<uuid>` when no task ref is supplied.
- ⚠ Phase 2's draft registry is in-memory only. Phase 3's `promote()` registers via the same stub interface; Phase 6 swaps in the real registry. Both AC4.7 and AC4.8 are verifiable in Phase 3 against the stub. End-to-end registry consistency is Phase 6's problem.
- ⚠ Sidecar mode was validated 2026-04-20; confirm the test `jj_adapter_mutate.rs` still passes at Phase 3 execution time. Flag any regression.

### Design decisions locked in

- **Lightweight merge mechanics.** `parent_doc.import(&fork_doc.export_snapshot())` is the merge operation. Loro is a vector-clock CRDT — concurrent edits resolve deterministically by operation order within each site's logical timeline, preserving all ops across both sides. Property tests codify the exact observed behaviour (see Task 2) rather than asserting a specific resolution rule that may shift between loro versions.
- **Persistent merge mechanics.** `JjAdapter::merge(repo_root, fork_revset, parent_revset)` handles the jj side; after jj merges the working copies, we still need to reconcile LoroDoc state. Approach: the fork's workspace maintains its own LoroDoc files on disk (via the existing mount mode); merge reads the fork's on-disk doc snapshots and `import()`s them into the parent's in-memory LoroDocs. jj handles commit-graph convergence; loro handles block-level convergence; the two merges compose.
- **Bookmark name format.** `<agent-id>/<task-label>` when a `BlockRef.label` is available, else `<agent-id>/<short-uuid>`. Task label is sanitized (lowercase, alphanumeric + hyphens).
- **Discard semantics.** Lightweight: drop the forked `LoroDoc` and `SessionContext`; no persisted state. Persistent: `workspace_forget(repo_root, name)` + `bookmark_delete(repo_root, name)` atomically (fallible; if `workspace_forget` fails, we still attempt `bookmark_delete` and surface both errors).
- **Promote capability gate.** `CapabilityFlag::SpawnNewIdentities` is checked on the **spawner's** (parent's) CapabilitySet, not the fork's. The promotion creates a new persona identity under the spawner's authority.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-multi-agent.AC4: Fork spawn and isolation

- **v3-multi-agent.AC4.1 Success:** `ctx.spawn.fork(ForkConfig { isolation: Lightweight, .. })` creates a session with a forked LoroDoc; parent and fork can write to their respective memory states independently
- **v3-multi-agent.AC4.2 Success:** `ctx.spawn.fork(ForkConfig { isolation: Persistent, .. })` creates a jj workspace via the jj adapter; fork's memory changes appear in the workspace's working copy
- **v3-multi-agent.AC4.3 Success:** `fork.merge_back()` on a lightweight fork imports the fork's LoroDoc state back into the parent via `LoroDoc::import()`; changes from both parent and fork are merged
- **v3-multi-agent.AC4.4 Success:** `fork.merge_back()` on a persistent fork performs jj merge + loro CRDT merge; parent's working copy reflects the merged state
- **v3-multi-agent.AC4.5 Success:** `fork.discard()` on a lightweight fork drops the forked LoroDoc; no state changes propagate to parent
- **v3-multi-agent.AC4.6 Success:** `fork.discard()` on a persistent fork runs `jj workspace forget` on the fork's workspace; bookmark deleted
- **v3-multi-agent.AC4.7 Success:** `fork.promote(persona_config)` creates a new persona config, registers as Draft in the registry, inherits the fork's memory state
- **v3-multi-agent.AC4.8 Failure:** `fork.promote()` without `SpawnNewIdentities` capability returns `CapabilityError::Denied`
- **v3-multi-agent.AC4.9 Edge:** Concurrent writes in parent and lightweight fork to the same block merge deterministically via loro CRDT semantics (both changes preserved, no data loss)
- **v3-multi-agent.AC4.10 Edge:** Persistent fork bookmark is namespaced as `<agent-id>/<task-id>`; no collision with other forks or bookmarks

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: Lightweight fork — build child context over a forked LoroDoc

**Verifies:** AC4.1.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` (from Phase 2 Task 8) — flesh out the lightweight path.
- Modify: `crates/pattern_memory/src/cache.rs` — expose `MemoryCache::fork_for_child(&self, parent_agent: &AgentId, child_agent: &AgentId) -> Result<MemoryCache, MemoryError>` that walks the parent's blocks, calls `LoroDoc::fork()` on each, and builds a new cache over the forked docs. Share the underlying DB `Arc` (cheap).
- Modify: `crates/pattern_core/src/memory/document.rs` — add `StructuredDocument::fork(&self, new_metadata: BlockMetadata) -> StructuredDocument` that wraps `LoroDoc::fork()` and clones the metadata.

**Implementation:**

**Verified shape of the types we touch** (check at `crates/pattern_memory/src/types_internal.rs:18` + `crates/pattern_memory/src/cache.rs:44`):
- `CachedBlock` fields: `doc: StructuredDocument`, `last_seq: i64`, `last_persisted_frontier: Option<VersionVector>`, `dirty: bool`, `last_accessed: DateTime<Utc>`. Metadata (id, agent_id, label) is embedded in `doc` and accessed via `doc.id()`, `doc.agent_id()`, `doc.label()`.
- `MemoryCache.blocks`: `Arc<DashMap<String, CachedBlock>>` keyed by **`block_id`** (not label). `cache.get(agent_id: &AgentId, label: &str)` walks the map and filters by both.

Pseudocode accordingly:

```rust
// pattern_core/src/memory/document.rs
impl StructuredDocument {
    /// Fork the underlying LoroDoc + rebuild a StructuredDocument around the
    /// forked doc. Preserves embedded metadata (label, id, schema) — ownership
    /// rewrite happens at the cache level via a separate method.
    pub fn fork(&self) -> Self {
        let forked_doc = self.inner().fork();
        // Reuse the existing from_snapshot_with_metadata-style constructor; see
        // the existing Arc<LoroDoc> wrapping pattern at document.rs:378 for
        // the accessor surface.
        StructuredDocument::from_forked_doc(forked_doc, self.metadata_snapshot())
    }
}

// pattern_memory/src/cache.rs
impl MemoryCache {
    /// Fork every block whose embedded `agent_id` matches `parent_agent`,
    /// producing a new MemoryCache over the forked LoroDocs. Shares the
    /// underlying DB handle (cheap Arc clone).
    pub fn fork_for_child(
        &self,
        parent_agent: &AgentId,
        child_agent: &AgentId,
    ) -> Result<MemoryCache, MemoryError> {
        let child = MemoryCache::new(self.db.clone());
        for entry in self.blocks.iter() {
            let (block_id, cached) = (entry.key().clone(), entry.value());
            if cached.doc.agent_id() != parent_agent.as_str() { continue; }
            // Fork the document and rewrite ownership. Requires a helper on
            // StructuredDocument to re-tag the agent_id on the forked doc;
            // add it alongside `fork()` (`fn retag_owner(&mut self, new_owner: &AgentId)`).
            let mut forked_doc = cached.doc.fork();
            forked_doc.retag_owner(child_agent);
            child.insert_cached_block(block_id, CachedBlock {
                doc: forked_doc,
                last_seq: cached.last_seq,
                last_persisted_frontier: cached.last_persisted_frontier.clone(),
                dirty: false, // fork starts clean — parent's pending writes do not transfer
                last_accessed: chrono::Utc::now(),
            });
        }
        Ok(child)
    }
}
```

Add helper methods needed for the above (they're all trivial plumbing over existing fields):
- `StructuredDocument::from_forked_doc(doc: LoroDoc, metadata_snapshot: BlockMetadata) -> Self`
- `StructuredDocument::metadata_snapshot(&self) -> BlockMetadata`
- `StructuredDocument::retag_owner(&mut self, new_owner: &AgentId)`
- `MemoryCache::insert_cached_block(&self, block_id: String, block: CachedBlock)` — pub(crate), used only by `fork_for_child`.

**Pre-task sanity check:** before writing code, open `types_internal.rs` and `cache.rs` and confirm field names match. If the struct has evolved since 2026-04-23, adjust the names in the pseudocode. Do NOT write against an assumed shape.

(Names illustrative — match the existing style of `MemoryCache` fields. Restrict visibility with `pub(crate)` as appropriate.)

In `spawn/fork.rs`, the lightweight arm now:
1. Looks up the parent's `MemoryCache` via `parent.memory_cache()` (add accessor on `SessionContext`).
2. Calls `fork_for_child(parent_agent, child_agent)`.
3. Builds a child `MemoryStoreAdapter` over the forked cache.
4. Spawns an EvalWorker with the forked adapter + parent's `include_paths`.
5. Returns a `ForkHandle { child_id, isolation: Lightweight, .. }` tracking the child's `Arc<CancelState>`, the child session handle, and (for merge-back) the child's `MemoryCache`.

**Testing:**
- Integration (`crates/pattern_runtime/tests/fork_lightweight.rs`): parent has block `notes` with content `"initial"`; spawn a lightweight fork; fork writes `"fork-change"` to `notes`; parent writes `"parent-change"` to `notes`. Assert both observe their own writes; no cross-contamination (AC4.1).
- Unit: `StructuredDocument::fork` produces a doc whose state matches the source at fork-time but diverges under writes.
- Unit: `MemoryCache::fork_for_child` only forks blocks owned by `parent_agent` (skips shared-block references owned elsewhere).

**Verification:**
`cargo nextest run -p pattern-memory cache::fork && cargo nextest run -p pattern-runtime fork_lightweight`

**Commit:** `[pattern-memory] [pattern-runtime] lightweight fork via LoroDoc::fork + forked MemoryCache`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Lightweight merge_back

**Verifies:** AC4.3, AC4.9.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` — `ForkHandle::merge_back_lightweight(&self) -> Result<MergeReport, ForkError>`.
- Modify: `crates/pattern_core/src/memory/document.rs` — ensure `apply_updates` takes a snapshot exported from another LoroDoc (confirm the snapshot format roundtrips across fork/import).
- Create: `crates/pattern_runtime/src/spawn/merge.rs` — `pub struct MergeReport { blocks_merged: u32, blocks_conflicted: u32, conflicts: Vec<ConflictSummary> }`. Conflicts here are informational, not failures (CRDT guarantees convergence); the summary just lets the caller see which blocks received concurrent edits.

**Implementation:**

```rust
impl ForkHandle {
    pub fn merge_back_lightweight(&self) -> Result<MergeReport, ForkError> {
        let ForkIsolationState::Lightweight { child_cache, .. } = &self.isolation_state else {
            return Err(ForkError::WrongIsolation);
        };
        let parent_cache = self.parent_cache_handle.upgrade()
            .ok_or(ForkError::ParentDropped)?;
        let mut report = MergeReport::default();
        for entry in child_cache.blocks.iter() {
            let label = entry.key();
            let child_doc = &entry.value().document;
            let snapshot = child_doc.inner().export_snapshot();
            if let Some(parent_doc) = parent_cache.get_doc_mut(self.parent_agent(), label) {
                parent_doc.apply_updates(&snapshot)?;
                report.blocks_merged += 1;
            } else {
                // Block doesn't exist on parent — create it with forked state
                parent_cache.insert_from_snapshot(label.clone(), snapshot)?;
                report.blocks_merged += 1;
            }
        }
        Ok(report)
    }
}
```

Concurrent edit behaviour: when both parent and child wrote to `notes` between fork and merge, `parent.import(child_snapshot)` applies the child's ops on top of the parent's. Loro's vector-clock CRDT semantics preserve all operations across both timelines and produce a deterministic merge result independent of import order. Tests snapshot the observed output so regressions against future loro upgrades are visible.

**Testing:**
- Integration: "concurrent edits diamond":
  1. Parent writes `"hello"` to `notes`.
  2. Fork lightweight.
  3. Parent writes `"hello world"`.
  4. Fork writes `"hello fork"`.
  5. `fork.merge_back()`.
  6. Assert the final parent `notes` contains the merged state per loro semantics — record the exact outcome via `insta` snapshot so regressions are visible.
- proptest (AC4.9): generate sequences of random text-insert operations on both sides; after merge, assert the final state is independent of which side's ops applied first (the merge is commutative up to loro's resolution rules). Use `proptest::collection::vec` for the operation traces; keep traces bounded to avoid long test runs.
- Unit: `MergeReport` counts are accurate.

**Verification:**
`cargo nextest run -p pattern-runtime fork_merge_lightweight`

**Commit:** `[pattern-runtime] lightweight fork merge_back via LoroDoc::import`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Lightweight discard

**Verifies:** AC4.5.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` — `ForkHandle::discard(self) -> Result<(), ForkError>`.

**Implementation:**
`discard` takes `self` by value (consumes the handle), drops the child's `SessionContext` (which drops the child's `MemoryCache` and EvalWorker), never calls `import` on the parent. Since the child's LoroDoc is never exported back, parent state is unchanged.

For symmetry with `merge_back`, `discard` also marks the child's `CancelState::cancellation = true` first (so any in-flight child turn observes the cancel and exits promptly).

**Testing:**
- Integration: parent writes `"parent-only"` to `notes`; fork; fork writes `"fork-only"`; `fork.discard()`; parent reads `"parent-only"` (fork's write dropped).
- Unit: calling `discard` twice returns an error the second time (`ForkError::AlreadyResolved`) instead of panicking.

**Verification:**
`cargo nextest run -p pattern-runtime fork_discard`

**Commit:** `[pattern-runtime] lightweight fork discard drops child state`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->

<!-- START_TASK_4 -->
### Task 4: Persistent fork — jj workspace creation + namespaced bookmark

**Verifies:** AC4.2, AC4.10.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` — persistent arm.
- Create: `crates/pattern_memory/src/jj/fork_bookmark.rs` — `pub fn fork_bookmark_name(agent: &AgentId, task: Option<&BlockRef>) -> String` with sanitization.
- Modify: `crates/pattern_memory/src/modes.rs` — add a helper that resolves "workspace location for fork `X`" given the active mode (Standalone / Sidecar / InRepo-with-jj).

**Implementation:**

```rust
pub fn fork_bookmark_name(agent: &AgentId, task: Option<&BlockRef>) -> String {
    let slug = task
        .map(|t| sanitize_slug(&t.label))
        .unwrap_or_else(|| format!("anon-{}", short_uuid()));
    format!("{}/{}", sanitize_slug(agent), slug)
}

fn sanitize_slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
```

Persistent fork dispatch:
1. Check `mount_config.mode.requires_jj() || mount_config.jj.enabled` — else return `ForkError::PersistentNotAvailable { mode }`.
2. Resolve workspace path (mount-mode dependent; e.g. Standalone → `~/.pattern/projects/<id>/workspaces/<bookmark>`; Sidecar → `.pattern/shared/workspaces/<bookmark>`).
3. `JjAdapter::workspace_add(repo_root, &new_workspace_path)`.
4. `JjAdapter::bookmark_set(repo_root, &bookmark_name, "@")` — pin bookmark at current working-copy revset for the new workspace.
5. Build a child `MemoryCache` whose on-disk root is the new workspace; load blocks from disk (same code path mount init already uses).
6. Build a child `SessionContext` rooted at this cache, spawn EvalWorker, wrap in `ForkHandle { isolation: Persistent { workspace_path, bookmark_name }, .. }`.

If any step after `workspace_add` fails, attempt `workspace_forget` + `bookmark_delete` as cleanup and return the original error (do NOT leak a partial workspace). Log cleanup failures at warn.

**Testing:**
- Integration gated on a jj-enabled temp mount (`crates/pattern_runtime/tests/fork_persistent.rs`):
  - Create a temp mount in Standalone mode with `jj enabled=true`.
  - Spawn persistent fork; assert a new workspace exists (`JjAdapter::workspace_list` shows it), bookmark `<agent>/<task-label>` exists (`bookmark_list`), fork can write blocks that appear in its workspace's files on disk (AC4.2).
  - Assert bookmark format matches `<agent>/<task>` (AC4.10).
  - Spawn a second fork with the same task — bookmark names collide; second attempt returns `ForkError::BookmarkConflict` with a clear "use a different task ref or discard the existing fork" message.
- Integration: InRepo mode with jj disabled → `ForkError::PersistentNotAvailable { mode: InRepo }`.
- Unit: `fork_bookmark_name` sanitization — spaces, capitals, special chars all normalised.

**Verification:**
`cargo nextest run -p pattern-runtime fork_persistent -- --nocapture`

**Commit:** `[pattern-memory] [pattern-runtime] persistent fork via jj workspace + namespaced bookmark`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Persistent merge_back — jj merge + loro import

**Verifies:** AC4.4.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` — persistent merge.

**Implementation:**

```rust
fn merge_back_persistent(&self) -> Result<MergeReport, ForkError> {
    let ForkIsolationState::Persistent { workspace_path, bookmark_name, child_cache, .. } = &self.isolation_state else {
        return Err(ForkError::WrongIsolation);
    };
    let adapter = JjAdapter::detect()?.ok_or(ForkError::JjUnavailable)?;
    let repo_root = self.mount.repo_root();

    // 1. Commit any outstanding child writes so the merge operates on a clean working copy.
    adapter.commit(workspace_path, &format!("fork merge_back from {}", bookmark_name))?;

    // 2. Merge the bookmark into the parent's current revset.
    adapter.merge(repo_root, bookmark_name, "@")?;

    // 3. Reconcile loro state: read fork's block snapshots from disk, apply_updates into parent's in-memory cache.
    let mut report = MergeReport::default();
    for entry in child_cache.blocks.iter() {
        let label = entry.key();
        let snapshot = entry.value().document.inner().export_snapshot();
        let parent_doc = self.parent_cache.get_doc_mut(self.parent_agent(), label)?;
        parent_doc.apply_updates(&snapshot)?;
        report.blocks_merged += 1;
    }
    Ok(report)
}
```

If step 2 fails (jj merge conflict), return the jj error unchanged and do NOT attempt step 3 — the tree is in a conflicted state the user must resolve. Document that jj conflicts are NOT automatically resolvable here; the persistent fork remains until `discard` or manual jj intervention.

**Testing:**
- Integration (temp Standalone mount): parent writes `A`, fork persists, fork writes `B`, parent writes `C`, `fork.merge_back()` — assert jj shows a merge commit and parent in-memory state reflects both `B` and `C` per loro CRDT merge.
- Integration: intentionally conflicting jj-level writes (e.g. fork renames the block config KDL, parent edits it) → `merge_back` returns a `ForkError::JjConflict` carrying the failing revset(s). Fork remains operable.

**Verification:**
`cargo nextest run -p pattern-runtime fork_merge_persistent -- --nocapture`

**Commit:** `[pattern-runtime] persistent fork merge_back composes jj merge + loro import`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Persistent discard — workspace_forget + bookmark_delete

**Verifies:** AC4.6.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` — persistent discard.

**Implementation:**

```rust
fn discard_persistent(self) -> Result<(), ForkError> {
    let ForkIsolationState::Persistent { workspace_path, bookmark_name, .. } = &self.isolation_state else {
        return Err(ForkError::WrongIsolation);
    };
    let adapter = JjAdapter::detect()?.ok_or(ForkError::JjUnavailable)?;
    let repo_root = self.mount.repo_root();

    // Cancel child session first so no in-flight writes race the delete.
    self.cancel_state().cancellation.store(true, Ordering::SeqCst);

    // Best-effort cleanup: run both, collect errors.
    let mut errs = Vec::new();
    if let Err(e) = adapter.workspace_forget(repo_root, workspace_path_name(workspace_path)) {
        errs.push(e.into());
    }
    if let Err(e) = adapter.bookmark_delete(repo_root, bookmark_name) {
        errs.push(e.into());
    }
    if errs.is_empty() { Ok(()) } else { Err(ForkError::DiscardCleanup(errs)) }
}
```

Design note: if `workspace_forget` fails but `bookmark_delete` succeeds (or vice-versa), the caller sees `DiscardCleanup` with both error slots so they can diagnose. This is non-panicking partial-failure handling consistent with defense-in-depth guidance.

**Testing:**
- Integration: spawn persistent fork; `fork.discard()`; confirm `workspace_list` no longer shows the workspace and `bookmark_list` no longer shows the bookmark.
- Integration: simulate failing `workspace_forget` (lock-file present or similar) and assert `DiscardCleanup` carries the error while `bookmark_delete` still ran.

**Verification:**
`cargo nextest run -p pattern-runtime fork_discard_persistent -- --nocapture`

**Commit:** `[pattern-runtime] persistent fork discard runs workspace_forget + bookmark_delete`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 7-8) -->

<!-- START_TASK_7 -->
### Task 7: `fork.promote(persona_config)` — capability gate + draft creation

**Verifies:** AC4.7, AC4.8.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/fork.rs` — `ForkHandle::promote(self, cfg: PersonaConfig) -> Result<PersonaId, ForkError>`.
- Modify: `crates/pattern_runtime/src/spawn/draft.rs` (Phase 2 Task 7) — extend the stub to accept an initial `MemoryCache` state so the draft persona inherits the fork's memory.

**Implementation:**

```rust
impl ForkHandle {
    pub fn promote(self, cfg: PersonaConfig) -> Result<PersonaId, ForkError> {
        let spawner_caps = self.spawner_capabilities();
        if !spawner_caps.has_flag(CapabilityFlag::SpawnNewIdentities) {
            return Err(ForkError::CapabilityDenied(CapabilityError::Denied {
                category: EffectCategory::Spawn,
            }));
        }
        let persona_id = PersonaId::from(cfg.name.clone());

        // Extract the fork's memory state before consuming the handle.
        let fork_cache = match self.isolation_state {
            ForkIsolationState::Lightweight { child_cache, .. } => child_cache,
            ForkIsolationState::Persistent { child_cache, workspace_path, bookmark_name, .. } => {
                // Commit the fork's state as a final revset so the promoted persona
                // can pick up a clean persistent view later.
                let adapter = JjAdapter::detect()?.ok_or(ForkError::JjUnavailable)?;
                adapter.commit(&workspace_path, &format!("fork promote: {}", persona_id))?;
                // Note: we do NOT delete the bookmark here; the promoted persona
                // inherits it and assumes ownership.
                child_cache
            }
        };

        let draft_registry = self.runtime.draft_registry();
        draft_registry.register_draft(DraftPersona {
            id: persona_id.clone(),
            config: cfg,
            seed_cache: Some(fork_cache),
            created_at: jiff::Timestamp::now(),
        })?;
        Ok(persona_id)
    }
}
```

Capability check on the **spawner** (not the fork): the fork itself is short-lived and may have narrower capabilities, but the authority to create a new persona identity belongs to the spawner. Store `spawner_capabilities` on `ForkHandle` at spawn time (a snapshot, not a reference — the spawner's caps at fork creation are what matter).

`DraftPersona` gains an optional `seed_cache` field so when Phase 6's real registry later opens the draft, it populates initial memory from this cache.

**Testing:**
- Integration (lightweight): parent with `SpawnNewIdentities` flag. Fork, fork writes a block, `fork.promote(cfg)` → returns `PersonaId`. Assert draft registry entry exists with `seed_cache.is_some()` and the cached block is present.
- AC4.8: parent WITHOUT flag → `ForkError::CapabilityDenied`.
- Integration (persistent): same as above but over a Standalone mount; assert the jj log shows a `"fork promote:"` commit.

**Verification:**
`cargo nextest run -p pattern-runtime fork_promote`

**Commit:** `[pattern-runtime] implement fork.promote with SpawnNewIdentities gate + draft seed memory`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: `ctx.spawn.fork` Haskell surface — return a structured `ForkHandle`

**Verifies:** AC4 surface reachable from agent programs.

**Files:**
- Modify: `crates/pattern_runtime/haskell/Pattern/Spawn.hs` — from Phase 2 Task 9, `fork :: ForkConfig -> Eff effs ForkHandle`. Phase 3 fleshes out the `ForkHandle` Haskell type to carry an opaque id plus helpers: `awaitResult`, `mergeBack`, `discard`, `promote`.
- Modify: `crates/pattern_runtime/src/sdk/requests/spawn.rs` — add `SpawnReq::ForkOp { id: SpawnId, op: ForkOpKind }` with `ForkOpKind::{AwaitResult, MergeBack, Discard, Promote(PersonaConfig)}`.
- Create: `crates/pattern_runtime/src/spawn/fork_registry.rs` — `ForkRegistry` struct wrapping `DashMap<SpawnId, ForkHandle>` with CRUD methods (`insert`, `get`, `remove`). This is a distinct type from Phase 2 Task 3's `SpawnRegistry` (which is for ephemeral/fork child-session lifetime). Forks live in `ForkRegistry` so they survive past the spawner's turn and remain addressable by id for subsequent `ForkOp`s.
- Modify: `crates/pattern_runtime/src/session.rs` — add `fork_registry: Arc<ForkRegistry>` field to `SessionContext`, initialised empty at session open. Expose via `HasForkRegistry` trait.
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — dispatch each variant to the `ForkRegistry`. Operations consume the handle (remove from map) on `Discard`/`Promote`/`AwaitResult`; `MergeBack` preserves the handle so the caller can still `Discard` later.

**ForkRegistry ownership note.** The registry lives on each spawner's `SessionContext`, not on the daemon. Forks are scoped to the session that created them; when that session closes, the registry drops and all outstanding `ForkHandle`s are discarded (same Drop semantics as Phase 2's `SpawnRegistry`, but on a different collection). Phase 2 Task 8 constructed `ForkHandle` but stashed it locally in the handler — Phase 3 Task 8 moves it into the registry so subsequent `ForkOp` calls can reach it by id.

**Implementation:**

Lookup in the handler:

```rust
SpawnReq::ForkOp { id, op } => {
    let registry = cx.user().fork_registry();
    match op {
        ForkOpKind::AwaitResult => {
            let handle = registry.remove(&id)?;
            let reply = handle.await_result().await?;
            Ok(Value::from_step_reply(&reply))
        }
        ForkOpKind::MergeBack => {
            let handle = registry.get(&id)?;
            let report = handle.merge_back()?;
            Ok(Value::from_merge_report(&report))
        }
        ForkOpKind::Discard => {
            let handle = registry.remove(&id)?;
            handle.discard()?;
            Ok(Value::unit())
        }
        ForkOpKind::Promote(cfg) => {
            let handle = registry.remove(&id)?;
            let pid = handle.promote(cfg)?;
            Ok(Value::persona_id(pid))
        }
    }
}
```

Haskell side mirrors:

```haskell
data ForkHandle = ForkHandle { forkId :: SpawnId }

awaitResult :: Member Spawn effs => ForkHandle -> Eff effs StepReply
awaitResult h = Freer.send (ForkOp (forkId h) AwaitResult)

mergeBack :: Member Spawn effs => ForkHandle -> Eff effs MergeReport
mergeBack h = Freer.send (ForkOp (forkId h) MergeBack)

discard :: Member Spawn effs => ForkHandle -> Eff effs ()
discard h = Freer.send (ForkOp (forkId h) Discard)

promote :: Member Spawn effs => ForkHandle -> PersonaConfig -> Eff effs PersonaId
promote h cfg = Freer.send (ForkOp (forkId h) (Promote cfg))
```

**Testing:**
- Integration: agent program spawns a lightweight fork, writes a block via `Memory.put`, calls `Spawn.mergeBack forkHandle`, then `Memory.get` in the parent — observes the fork's write.
- Integration: agent program calls `Spawn.discard forkHandle`, then `Memory.get` — does NOT observe the fork's write.
- Integration: agent program calls `Spawn.promote forkHandle cfg` — returns a `PersonaId`; draft registry stub shows the new entry.

**Verification:**
`cargo nextest run -p pattern-runtime fork_sdk_surface`

**Commit:** `[pattern-runtime] surface fork resolution ops in Pattern.Spawn`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] `StructuredDocument::fork()` and `MemoryCache::fork_for_child` wrap loro's fork API correctly.
- [ ] Lightweight forks branch and merge cleanly; `discard()` drops child state without propagation.
- [ ] Persistent forks create namespaced jj workspaces/bookmarks; merge composes jj + loro import; discard runs `workspace_forget` + `bookmark_delete` atomically.
- [ ] `fork.promote(cfg)` seeds draft-registry state with fork memory and gates on `SpawnNewIdentities`.
- [ ] Fork handle ops reachable from Haskell via `Spawn.awaitResult / mergeBack / discard / promote`.
- [ ] proptest coverage for loro concurrent-edit convergence (AC4.9).
- [ ] Bookmark sanitization + collision detection (AC4.10).
- [ ] All existing tests still green.

---

## Notes for executor

- **No JSON-over-string** for `SpawnReq` payloads — implement `FromCore` for `PersonaConfig` / `ForkOpKind` directly if the derive doesn't cover them (same principle as Phase 2 Task 2).
- **Registry stub semantics.** Phase 3 exercises the stub draft registry from Phase 2; do not attempt to land Phase 6's real registry here. If a test seems to need registry behaviour beyond the stub, flag it — we may have mis-scoped.
- **Jj unavailable in CI?** Some CI images lack `jj` CLI. The persistent-fork test file is gated on `JjAdapter::detect()?.is_some()`; the lightweight tests are not. Do NOT stub out persistent tests — fix the CI image or document the gating clearly (Nix devshell provides `jj` today).
- **Sidecar mode** was validated 2026-04-20 — re-run the `jj_adapter_mutate.rs` suite at the start of Phase 3 as a smoke check; if it regressed, fix that first.
- Commit style per project: `[pattern-memory]`, `[pattern-runtime]`, cross-crate `[pattern-memory] [pattern-runtime]`.
