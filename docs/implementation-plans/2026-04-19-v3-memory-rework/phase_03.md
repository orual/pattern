# Pattern v3 Memory Rework — Phase 3 Implementation Plan

**Goal:** Desync the `MemoryStore` trait (remove `#[async_trait]`, all methods become sync `fn`), audit the surface from 28 → 19 methods via five consolidations, simplify the eval worker to a plain OS thread driven by `std::sync::mpsc` + `tokio::sync::oneshot`, eliminate all `Handle::current().block_on` sites in the memory/recall/search handlers, migrate async callsites to `tokio::task::spawn_blocking` for DB-hitting operations, and remove `Pattern.Recall.delete` from the agent-facing SDK surface (the Rust `MemoryStore::delete_archival` method stays for human-ops tooling).

**Architecture:** The MemoryStore trait goes sync because the underlying storage is now sync (Phase 2's rusqlite port made every DB operation genuinely synchronous). The eval worker hosts Tidepool's Haskell evaluator on an OS thread; previously it span up a per-session multi-thread tokio runtime and bridged via `block_in_place` + `Handle::current().block_on` so sync handler code could drive async MemoryStore calls. With MemoryStore sync, the runtime-within-runtime pattern becomes unnecessary: the eval worker intake channel becomes `std::sync::mpsc::Receiver<EvalRequest>`, the worker thread invokes handlers directly against the sync MemoryStore surface, and replies go back to the async caller via `tokio::sync::oneshot` (which works cross-thread because oneshot is Send). The async orchestrator's view of `Session::step` is unchanged — still `async fn`, still returns a `StepReply` — it just no longer needs to nest runtimes. `async_trait` stays in pattern_core for the other 8 genuinely-async traits (ProviderClient, EmbeddingProvider, DataStream, Endpoint, EndpointRegistry, AgentRuntime, Session, SourceManager).

**Tech Stack:** Pure stdlib (`std::sync::mpsc`, `std::thread`) for the intake side; tokio primitives (`tokio::task::spawn_blocking`, `tokio::sync::oneshot`, `Arc<...>`) for the async orchestrator side; `trybuild` as a new dev-dep for the compile-fail test.

**Scope:** Phase 3 of 8.

**Codebase verified:** 2026-04-19 (codebase-investigator agent a1038335a55c6b7ab). Note: the investigator reported `pattern_memory` crate "not found" — expected (Phase 1 has not been executed yet). Plan assumes Phase 1 and Phase 2 completed before Phase 3 runs; file paths below reflect the post-Phase-1 layout (`MemoryCache` at `pattern_memory/src/cache.rs`, not the current pre-Phase-1 `pattern_core/src/memory/cache.rs`).

**Execution posture:** Autonomous subagent delegation is appropriate for the bulk of this phase — mechanical sync conversion + call-site rewiring. No gates requiring human sign-off. Main executor reviews the final diff before Phase 4.

**Library-first audit for eval worker internals:** Per the global "never reinvent the wheel" rule, before writing any thread/channel/cancel machinery the implementor surveys what's already usable: stdlib channels, `crossbeam-channel`, `tokio_util::sync::CancellationToken`, and any sync-thread-pool / supervisor crates. For the eval worker specifically the conclusion is pre-baked into this plan — see Task 5's rationale block for why stdlib primitives are the right pick here. Phase 4's subscriber workers have a different requirement profile (select/multiplex + bounded backpressure) and will pick `crossbeam-channel` + `tokio_util::sync::CancellationToken` — that decision is made in Phase 4, not here.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC4: MemoryStore sync-ification + surface audit

- **v3-memory-rework.AC4.1 Success:** `MemoryStore` trait has no `#[async_trait]` decorator
- **v3-memory-rework.AC4.2 Success:** Trait has 19 methods (audited down from 28); consolidation detail captured in trait-method doc comments. (Design target was ~18 as approximate; actual arithmetic lands at 19. Documented in trait docs + this plan; design plan updated to reflect actual count post-Phase-3.)
- **v3-memory-rework.AC4.3 Success:** `list_blocks(BlockFilter)` replaces the three previous variants; every filter combination works
- **v3-memory-rework.AC4.4 Success:** `update_block_metadata(id, BlockMetadataPatch)` correctly updates specified fields and leaves others untouched
- **v3-memory-rework.AC4.5 Success:** `undo_redo(label, UndoRedoOp)` + `history_depth(label)` produce equivalent behavior to the four removed methods
- **v3-memory-rework.AC4.6 Success:** `search(SearchScope)` correctly scopes to persona / project / constellation
- **v3-memory-rework.AC4.7 Success:** All existing MemoryCache impl tests pass against the new sync trait surface
- **v3-memory-rework.AC4.8 Failure:** `async_trait` dep is not removed from pattern_core (other 8 traits still use it); `cargo check -p pattern_core` still imports `async_trait`
- **v3-memory-rework.AC4.9 Edge:** `MemoryStore::delete_archival` method is retained in the trait but is not reachable via any agent SDK effect. Verified via a `trybuild` compile-fail test at `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs` that attempts to construct the removed SDK request variant and confirms the compile error. The Haskell-side `Pattern.Recall.delete` symbol is removed from the Recall SDK module; existing agent programs invoking it fail at Tidepool compile-time with a 'symbol not found' diagnostic.

### v3-memory-rework.AC5: Eval worker simplification + async callsite migration

- **v3-memory-rework.AC5.1 Success:** `eval_worker.rs` no longer constructs a per-session `tokio::runtime::Builder::new_multi_thread()`
- **v3-memory-rework.AC5.2 Success:** Worker thread is spawned via `std::thread::spawn` with `std::sync::mpsc::channel` for request intake
- **v3-memory-rework.AC5.3 Success:** Zero `Handle::current().block_on(...)` call sites remain in memory, recall, search, or scope effect handlers
- **v3-memory-rework.AC5.4 Success:** Session::step caller-visible signature unchanged (still `async`)
- **v3-memory-rework.AC5.5 Success:** Pre-existing `spawn_blocking`-related search bug is resolved (regression test passes)
- **v3-memory-rework.AC5.6 Success:** Async callsites that invoke `MemoryStore` DB operations use `tokio::task::spawn_blocking`; cheap sync operations (metadata reads from in-memory caches) call directly
- **v3-memory-rework.AC5.7 Failure:** Running a stream of 100 eval requests against the sync worker completes without `cannot start a runtime from within a runtime` panics
- **v3-memory-rework.AC5.8 Edge:** On eval worker thread panic, a user-visible error surfaces; session becomes unusable (does not silently deadlock)

---

## Codebase verification findings

Key realities that shape the task breakdown:

- ✓ 28 MemoryStore methods confirmed, matching design. Full enumeration in investigator report; of note, `mark_dirty` is already sync (`fn(agent_id, label)`), the other 27 are async. Three methods (`has_shared_blocks_with`, `shares_group_with`, `list_constellation_agent_ids`) have default impls; the rest are required.
- ✗ **Design claims 28 → ~18 consolidation**; actual arithmetic lands at **19**:
  - 3 `list_*` merge into 1 (`list_blocks(BlockFilter)`) → −2
  - 4 metadata setters merge into 1 (`update_block_metadata(BlockMetadataPatch)`) → −3
  - 2 `undo_*`/`redo_*` merge into 1 (`undo_redo(UndoRedoOp)`) → −1
  - 2 `*_depth` merge into 1 (`history_depth(label) -> UndoRedoDepth`) → −1
  - 2 `search`/`search_all` merge into 1 (`search(SearchScope)`) → −1
  - Net: 28 − 8 = **19**. Plan targets 19 and updates the design plan's "~18" reference post-phase. AC4.2 also reworded to "19" (see above).
- ✗ **Design's `ctx.memory.archive.delete` effect naming is wrong**. The actual surface lives in the **Recall** module: `RecallReq::Delete` variant at `crates/pattern_runtime/src/sdk/handlers/recall.rs:146` and Haskell `Pattern.Recall.delete` at `crates/pattern_runtime/haskell/Pattern/Recall.hs:51-52`. The plan below uses the real names.
- ✓ `eval_worker.rs` at `crates/pattern_runtime/src/agent_loop/eval_worker.rs`: multi-thread tokio runtime at lines 148-152 with 2 worker threads; worker thread spawned at lines 140-184 with 256 MiB stack; intake via `tokio::sync::mpsc::UnboundedSender<EvalRequest>`; reply via `tokio::sync::oneshot::Sender<ToolOutcome>` owned inside `EvalRequest`; `block_in_place` wrapper at lines 174-176 around `run_eval()`. Phase 3 swaps intake to `std::sync::mpsc` (unbounded, same semantics) and removes the per-session tokio runtime + `block_in_place` wrapping.
- ✓ `Handle::current().block_on` sites verified:
  - `handlers/memory.rs` — 11 pairs (design estimated 8-9; higher due to helper sites like `resolve_access` at line 501). Every one of them is a MemoryStore call.
  - `handlers/recall.rs` — 5 pairs (design estimated 3; higher due to `resolve_scope` helper calls). All MemoryStore-related; line 146's `Delete` variant disappears entirely because the SDK request variant is removed.
  - `handlers/search.rs` — 2 pairs. Both MemoryStore.
  - `handlers/scope.rs` — 0 pairs. No change needed here.
  - `handlers/message.rs` — 3 `Handle::current()` references that `block_on` on **MessageRouter**, not MemoryStore. These STAY async (MessageRouter stays async-trait). Do not touch these.
- ✓ `Session::step` (internal) + `Session::step_with_agent_loop` (public) at `crates/pattern_runtime/src/session.rs:577-610`. Both currently `async`. Public caller-visible signature stays unchanged post-Phase-3.
- ✓ 9 `#[async_trait]` attributes in `crates/pattern_core/src/` (MemoryStore + 8 others). Post-Phase-3, 8 remain — the 8 listed above with genuine async needs. `cargo check -p pattern_core` still imports `async_trait` (AC4.8 check).
- ✓ `InMemoryMemoryStore` at `crates/pattern_runtime/src/testing/in_memory_store.rs:67` currently uses `#[async_trait]`; Phase 3 desyncs it in lockstep with the trait.
- ✗ `trybuild` is NOT in `pattern_runtime/Cargo.toml` dev-deps; Phase 3 adds it.
- ✓ `pattern_runtime/CLAUDE.md` lines 33-51 document the multi-thread eval worker design and the `block_in_place` pattern. Phase 3 rewrites that section to describe the plain-OS-thread + sync-mpsc design.
- ✓ Evaluation doc `docs/design-plans/2026-04-18-sqlx-to-rusqlite-evaluation.md` flags that search calls via `handle.block_on()` inside the JIT can cause executor starvation or performance anomalies under certain configurations. The specific symptom isn't spelled out concretely; Phase 3 adds a regression test that exercises the old broken pattern (concurrent search calls from an async context) against the new sync surface to prove the symptom is gone.
- ✓ Tidepool / agent program interaction: agent programs (Haskell) call SDK effects that dispatch through `sdk/handlers/*.rs`. When `RecallReq::Delete` is removed, agent programs that imported `Pattern.Recall.delete` fail at Tidepool compile time with a clear diagnostic. This is intentional — removing the Rust variant + the Haskell symbol is the full removal path.

---

## Dependency changes

`crates/pattern_runtime/Cargo.toml` gets a new dev-dep:

```toml
[dev-dependencies]
# ... existing ...
trybuild = "1"
```

No runtime dep changes. `async_trait` stays in pattern_core's `[dependencies]`.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-4) -->

### Subcomponent A: Desync `MemoryStore` trait + consolidate surface + desync impls

<!-- START_TASK_1 -->
### Task 1: Design consolidated input/output types (`BlockFilter`, `BlockMetadataPatch`, `UndoRedoOp`, `UndoRedoDepth`, `SearchScope`)

**Verifies:** v3-memory-rework.AC4.3, AC4.4, AC4.5, AC4.6 (prerequisite — types need to exist before signatures can use them)

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/core_types.rs` (add the five consolidation types)
- Modify: `crates/pattern_core/src/types/memory_types/search.rs` (add `SearchScope`)
- Modify: `crates/pattern_core/src/types/memory_types/mod.rs` (re-export the new types)

**Implementation:**

1. `BlockFilter` — unifies the three `list_blocks*` variants:

   ```rust
   /// Filter predicate for `MemoryStore::list_blocks`. Replaces the
   /// pre-Phase-3 `list_blocks`, `list_blocks_by_type`, and
   /// `list_all_blocks_by_label_prefix` methods.
   #[derive(Clone, Debug, PartialEq, Eq, Default)]
   #[non_exhaustive]
   pub struct BlockFilter {
       /// If set, only blocks owned by this agent are returned.
       /// If `None`, blocks from every agent are returned (use for
       /// constellation-wide listings).
       pub agent_id: Option<AgentId>,
       /// If set, only blocks with this tier are returned.
       pub block_type: Option<BlockType>,
       /// If set, only blocks whose label starts with this prefix
       /// are returned.
       pub label_prefix: Option<String>,
   }

   impl BlockFilter {
       pub fn by_agent(agent_id: AgentId) -> Self { ... }
       pub fn by_type(agent_id: AgentId, block_type: BlockType) -> Self { ... }
       pub fn by_prefix(prefix: impl Into<String>) -> Self { ... }
       pub fn all() -> Self { Self::default() }
   }
   ```

2. `BlockMetadataPatch` — unifies the four setter methods:

   ```rust
   /// Sparse patch for `MemoryStore::update_block_metadata`. Each
   /// `Some(...)` field is applied; `None` fields leave the stored
   /// value unchanged. Replaces the pre-Phase-3 `set_block_pinned`,
   /// `set_block_type`, `update_block_schema`, `update_block_description`.
   #[derive(Clone, Debug, Default, PartialEq, Eq)]
   #[non_exhaustive]
   pub struct BlockMetadataPatch {
       pub pinned: Option<bool>,
       pub block_type: Option<BlockType>,
       pub schema: Option<BlockSchema>,
       pub description: Option<String>,
   }

   impl BlockMetadataPatch {
       pub fn pinned(mut self, pinned: bool) -> Self { self.pinned = Some(pinned); self }
       pub fn block_type(mut self, bt: BlockType) -> Self { self.block_type = Some(bt); self }
       pub fn schema(mut self, sch: BlockSchema) -> Self { self.schema = Some(sch); self }
       pub fn description(mut self, d: impl Into<String>) -> Self { self.description = Some(d.into()); self }
       pub fn is_empty(&self) -> bool {
           self.pinned.is_none() && self.block_type.is_none()
               && self.schema.is_none() && self.description.is_none()
       }
   }
   ```

3. `UndoRedoOp` and `UndoRedoDepth`:

   ```rust
   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   #[non_exhaustive]
   pub enum UndoRedoOp {
       Undo,
       Redo,
   }

   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub struct UndoRedoDepth {
       pub undo: usize,
       pub redo: usize,
   }
   ```

4. `SearchScope` — unifies `search` (single-agent) and `search_all` (constellation):

   ```rust
   /// Scope for `MemoryStore::search`. Replaces the pre-Phase-3
   /// `search` (agent-scoped) and `search_all` (constellation-scoped)
   /// methods. Phase 8's `MemoryScope` layers additional routing
   /// (persona + project) on top of this.
   #[derive(Clone, Debug, PartialEq, Eq)]
   #[non_exhaustive]
   pub enum SearchScope {
       Agent(AgentId),
       Constellation,
   }
   ```

5. Add rustdoc `///` to every type + field. Cite AC references in the trait doc-comment on the new methods later so reviewers can trace which AC each surface element satisfies.

**Testing:**

Unit tests in each file for builder-pattern correctness and `is_empty` semantics on the patch. No behavioral tests yet — those come when the trait signatures land in Task 2.

**Verification:**

Run: `cargo check -p pattern_core`
Expected: compiles clean; new types exposed.

**Commit:** `[pattern-core] add BlockFilter, BlockMetadataPatch, UndoRedoOp, UndoRedoDepth, SearchScope consolidation types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Desync `MemoryStore` trait; consolidate method surface 28 → 19

**Verifies:** v3-memory-rework.AC4.1, AC4.2, AC4.8

**Files:**
- Modify: `crates/pattern_core/src/traits/memory_store.rs` (lines ~193-411)

**Implementation:**

1. Remove `#[async_trait]` attribute from the trait declaration (the line immediately preceding `pub trait MemoryStore` at line 193).

2. Rewrite every method signature `async fn foo(&self, ...) -> MemoryResult<T>` to `fn foo(&self, ...) -> MemoryResult<T>`. The return type stays identical; only the `async` keyword goes away.

3. Apply the five consolidations:

   ```rust
   // Replaces list_blocks + list_blocks_by_type + list_all_blocks_by_label_prefix.
   fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>>;

   // Replaces set_block_pinned + set_block_type + update_block_schema + update_block_description.
   fn update_block_metadata(
       &self,
       agent_id: &AgentId,
       label: &str,
       patch: BlockMetadataPatch,
   ) -> MemoryResult<()>;

   // Replaces undo_block + redo_block.
   fn undo_redo(&self, agent_id: &AgentId, label: &str, op: UndoRedoOp) -> MemoryResult<bool>;

   // Replaces undo_depth + redo_depth.
   fn history_depth(&self, agent_id: &AgentId, label: &str) -> MemoryResult<UndoRedoDepth>;

   // Replaces search + search_all.
   fn search(
       &self,
       query: &str,
       options: &SearchOptions,
       scope: SearchScope,
   ) -> MemoryResult<Vec<MemorySearchResult>>;
   ```

   All other methods: mechanical `async fn` → `fn` with no other change:
   `create_block`, `get_block`, `get_block_metadata`, `delete_block`, `get_rendered_content`, `persist_block`, `mark_dirty` (already sync), `insert_archival`, `search_archival`, `delete_archival`, `list_shared_blocks`, `get_shared_block`, `has_shared_blocks_with`, `shares_group_with`, `list_constellation_agent_ids`.

4. Final method count: 19. Document the audit in a top-of-trait doc comment listing every consolidation mapping. Explicit example:

   ```rust
   /// # Method surface consolidation (v3-memory-rework Phase 3, 2026-04-XX)
   ///
   /// Reduced from 28 methods to 19 via five consolidations:
   /// - `list_blocks`, `list_blocks_by_type`, `list_all_blocks_by_label_prefix` → `list_blocks(BlockFilter)`
   /// - `set_block_pinned`, `set_block_type`, `update_block_schema`, `update_block_description` → `update_block_metadata(BlockMetadataPatch)`
   /// - `undo_block`, `redo_block` → `undo_redo(UndoRedoOp)`
   /// - `undo_depth`, `redo_depth` → `history_depth`
   /// - `search`, `search_all` → `search(SearchScope)`
   ///
   /// All method signatures are sync (no `#[async_trait]`). The trait
   /// contract is driven by rusqlite under the hood (see pattern_db).
   ///
   /// `delete_archival` is retained as a trait method for human-operator
   /// tooling (CLI curation, TUI); it is NOT reachable via any agent-facing
   /// SDK effect (see v3-memory-rework Phase 3 SDK removal).
   pub trait MemoryStore: Send + Sync + 'static {
       // ...
   }
   ```

   The `Send + Sync + 'static` bound replaces async_trait's invisible `where Self: Send + Sync + 'static` — explicit is clearer.

5. Keep `async_trait` as a dep of pattern_core — the 8 other traits (`ProviderClient`, `EmbeddingProvider`, `DataStream`, `Endpoint`, `EndpointRegistry`, `AgentRuntime`, `Session`, `SourceManager`) still use it. AC4.8 is verified by `grep -rn "async_trait::async_trait\|#\[async_trait\]" crates/pattern_core/src/ | wc -l` returning `8` (down from 9).

**Testing:**

The trait changing compile-breaks every impl — `MemoryCache` in pattern_memory, `InMemoryMemoryStore` in pattern_runtime testing, and any other impl. Those are repaired in Tasks 3 and 4. This task alone will leave the workspace broken; that's expected intermediate state.

**Verification:**

Run: `cargo check -p pattern_core`
Expected: compiles clean (trait has no impls inside pattern_core).

Run: `cargo check -p pattern_memory -p pattern_runtime`
Expected: FAILS — impls don't match the new trait. Task 3 + Task 4 fix.

**Commit:** `[pattern-core] desync MemoryStore trait, consolidate 28 → 19 methods`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Desync `MemoryCache` impl + adapt internal logic to sync DB surface

**Verifies:** v3-memory-rework.AC4.1, AC4.7

**Files:**
- Modify: `crates/pattern_memory/src/cache.rs` (post-Phase-1 location of `MemoryCache`)

**Implementation:**

1. Remove `#[async_trait]` from the `impl MemoryStore for MemoryCache` block.
2. Rewrite every method from `async fn foo(&self, ...) -> MemoryResult<T>` to `fn foo(&self, ...) -> MemoryResult<T>`.
3. Remove `.await` from every internal call that was awaiting the DB layer. With Phase 2's rusqlite port, DB calls are sync `fn`s returning `rusqlite::Result<T>` or pattern_db's domain `Result<T>` — no futures involved.
4. Implement the five consolidated methods. For each, the body dispatches into the pre-consolidation logic:

   ```rust
   fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
       match filter {
           BlockFilter { agent_id: Some(agent), block_type: None, label_prefix: None } => {
               // old list_blocks(&agent) body
           }
           BlockFilter { agent_id: Some(agent), block_type: Some(bt), label_prefix: None } => {
               // old list_blocks_by_type(&agent, bt) body
           }
           BlockFilter { agent_id: None, block_type: None, label_prefix: Some(prefix) } => {
               // old list_all_blocks_by_label_prefix(&prefix) body
           }
           _ => {
               // compose: fetch by agent, then filter in-memory by type + prefix.
               // (combinations the old API didn't directly support but the new one should.)
           }
       }
   }
   ```

5. Mutex/RwLock semantics: previously `async fn` methods that held a lock across an `.await` were correctly using `tokio::sync::Mutex`/`RwLock`. Now that methods are sync, they can use `std::sync::Mutex`/`RwLock` — simpler, no async context needed. Audit each lock usage site and swap. `parking_lot` is acceptable if the project already uses it; otherwise stdlib is fine.

6. `mark_dirty` was already sync — no change.

**Testing:**

Existing MemoryCache unit tests (inline in cache.rs) and integration tests port their assertions verbatim; only test bodies need `.await` removed. The behavioral contract is unchanged.

**Verification:**

Run: `cargo check -p pattern_memory`
Expected: compiles clean.

Run: `cargo nextest run -p pattern_memory`
Expected: all tests pass (AC4.7).

**Commit:** `[pattern-memory] desync MemoryCache; implement consolidated MemoryStore surface`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Desync `InMemoryMemoryStore` test double

**Verifies:** v3-memory-rework.AC4.1, AC4.7

**Files:**
- Modify: `crates/pattern_runtime/src/testing/in_memory_store.rs` (line 67+)

**Implementation:**

Same pattern as Task 3. Remove `#[async_trait]`; swap `async fn` → `fn`; drop `.await` on now-sync paths (for this test double, almost nothing was genuinely async — it was an async wrapper around an in-memory `HashMap`). Implement the 5 consolidated methods.

**Testing:**

Any test using `InMemoryMemoryStore` gets `.await` removed from its fixture setup but assertions unchanged.

**Verification:**

Run: `cargo check -p pattern_runtime --features test-support`
Expected: compiles.

Run: `cargo nextest run -p pattern_runtime --features test-support`
Expected: all tests pass.

**Commit:** `[pattern-runtime] desync InMemoryMemoryStore test double; match new MemoryStore surface`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 5-8) -->

### Subcomponent B: Simplify eval worker + eliminate `block_on` in handlers

<!-- START_TASK_5 -->
### Task 5: Rewrite eval worker with `std::sync::mpsc` + plain OS thread

**Verifies:** v3-memory-rework.AC5.1, AC5.2, AC5.4, AC5.7, AC5.8

**Files:**
- Modify: `crates/pattern_runtime/src/agent_loop/eval_worker.rs` (full rewrite of the runtime + worker setup; keep the eval-business-logic functions in place)
- Modify: `crates/pattern_runtime/src/agent_loop/mod.rs` (if it re-exports changed symbols, update)
- Modify: `crates/pattern_runtime/src/session.rs` (session holds a sync-mpsc sender to the worker; `step` sends request + awaits oneshot reply)
- Modify: `crates/pattern_runtime/CLAUDE.md` (replace the multi-thread runtime doc section with the new sync-thread design)

**Implementation:**

**Library-first rationale (why stdlib, not crossbeam or a supervisor crate):**

- Eval worker has **one** intake channel and **one** reply oneshot per request. No `select!`-style multiplexing on the intake side — plain `recv()` loop suffices.
- No debounce timer, no periodic timer interleaved with intake.
- One worker, not a pool — no need for per-worker identity, worker discovery, or load balancing.
- Lifecycle is tied to `Session::drop`: when the sender is dropped, the channel closes, worker loop exits naturally. No separate cancel token needed.
- Restart-on-panic is explicitly NOT a requirement — AC5.8 says "on panic → session unusable, surface error to user." We want loud failure, not silent restart.

Given all that, `std::sync::mpsc::channel()` + `std::thread::spawn` + `tokio::sync::oneshot` (for the reply side, which genuinely does cross async/sync) is the minimal fit. `crossbeam-channel` would buy us nothing here (no multiplex, no multi-consumer). Task-supervisor crates target tokio tasks, not OS threads, and we don't want restart-on-panic anyway. No existing focused crate wraps "one-shot thread with std::sync::mpsc intake and tokio oneshot reply" in a way that's meaningfully simpler than rolling it directly.

**Implementation**:

1. New `EvalWorker` struct:

   ```rust
   pub struct EvalWorker {
       sender: std::sync::mpsc::Sender<EvalRequest>,
       handle: std::thread::JoinHandle<()>,
   }

   pub struct EvalRequest {
       pub input: TurnInput,
       pub reply: tokio::sync::oneshot::Sender<Result<StepReply, RuntimeError>>,
       // ...any other per-request state the old EvalRequest held
   }

   impl EvalWorker {
       pub fn spawn(deps: EvalWorkerDeps) -> Result<Self, RuntimeError> {
           let (tx, rx) = std::sync::mpsc::channel::<EvalRequest>();
           let handle = std::thread::Builder::new()
               .name("pattern-eval-worker".into())
               .stack_size(256 * 1024 * 1024) // 256 MiB — matches current setting
               .spawn(move || eval_worker_loop(rx, deps))
               .map_err(RuntimeError::SpawnFailed)?;
           Ok(Self { sender: tx, handle })
       }

       /// Submit a request and await the reply via oneshot. Returns `Err` if the
       /// worker thread has panicked (channel closed) or if the reply channel is
       /// dropped (worker crashed mid-request).
       pub async fn submit(&self, input: TurnInput) -> Result<StepReply, RuntimeError> {
           let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
           self.sender
               .send(EvalRequest { input, reply: reply_tx })
               .map_err(|_| RuntimeError::EvalWorkerDead)?;
           reply_rx.await.map_err(|_| RuntimeError::EvalWorkerCrashed)?
       }
   }

   fn eval_worker_loop(
       rx: std::sync::mpsc::Receiver<EvalRequest>,
       deps: EvalWorkerDeps,
   ) {
       for request in rx {
           let result = run_eval(&deps, request.input);
           // oneshot::Sender::send returns Err if the receiver was dropped;
           // caller gave up on waiting. Safe to ignore — just drop the result.
           let _ = request.reply.send(result);
       }
   }
   ```

2. **Remove the per-session multi-thread tokio runtime entirely.** Drop the `tokio::runtime::Builder::new_multi_thread()` call at lines 148-152. Drop the `tokio::task::block_in_place` wrapper at lines 174-176. Drop the `rt.block_on(async { ... })` wrapping of the dispatch loop.

3. **Panic handling** (AC5.8):
   - If `run_eval` panics, the `std::thread::spawn` closure unwinds, the thread terminates, the `std::sync::mpsc::Sender`'s `send` starts returning `Err` (receiver dropped).
   - Callers observe `RuntimeError::EvalWorkerDead` on the next submit, and any in-flight `oneshot_rx.await` wakes with `Err(RecvError)` converted to `RuntimeError::EvalWorkerCrashed`.
   - The session becomes unusable — the sender is permanently broken. This is correct per AC5.8 (no silent deadlock).

4. **Session::step integration.** Previously `session.step` drove the multi-thread runtime internally. New shape:

   ```rust
   // Session owns Arc<EvalWorker>.
   pub struct Session {
       eval_worker: Arc<EvalWorker>,
       // ...
   }

   pub async fn step_with_agent_loop(&self, input: TurnInput) -> Result<StepReply, RuntimeError> {
       self.eval_worker.submit(input).await
   }
   ```

   `step_with_agent_loop` stays `async fn` (AC5.4). Internally it does one sync `send` into the std::sync::mpsc channel + one `await` on the oneshot. No nested runtime.

5. **Update `pattern_runtime/CLAUDE.md` lines 33-51.** Replace the multi-thread+block_in_place rationale with:

   ```markdown
   ## Eval worker (post-v3-memory-rework Phase 3)

   Eval worker is a plain OS thread spawned via `std::thread::spawn`. Intake
   channel is `std::sync::mpsc::Sender<EvalRequest>` owned by `Session`;
   reply channel is `tokio::sync::oneshot::Sender<Result<StepReply, _>>` per
   request. The worker runs Tidepool's Haskell evaluator directly against the
   sync `MemoryStore` surface — no nested tokio runtime, no `block_in_place`,
   no `Handle::current().block_on`.

   Panic handling: worker thread panic terminates the thread; session becomes
   unusable (channel closed); callers observe `RuntimeError::EvalWorkerDead`
   or `::EvalWorkerCrashed`. This is the intended failure mode (fail loud;
   no silent deadlock).

   Freshness date: YYYY-MM-DD (v3-memory-rework Phase 3).
   ```

**Testing:**

- Integration test: `tests/eval_worker_runtime_panic.rs` — spawn an eval worker with a deps fixture that panics on first input; submit a request; observe `RuntimeError::EvalWorkerCrashed` (AC5.8).
- Integration test: `tests/eval_worker_100_requests.rs` — spawn worker, submit 100 inputs concurrently from an async orchestrator (via `futures::future::try_join_all`); assert all complete, none observe "cannot start a runtime from within a runtime" panics (AC5.7).

**Verification:**

Run: `cargo check -p pattern_runtime`
Expected: compiles.

Run: `cargo nextest run -p pattern_runtime --test eval_worker_runtime_panic --test eval_worker_100_requests`
Expected: passes.

Run: `grep -n "runtime::Builder::new_multi_thread\|block_in_place" crates/pattern_runtime/src/agent_loop/eval_worker.rs`
Expected: no matches.

**Commit:** `[pattern-runtime] rewrite eval worker as plain OS thread + std::sync::mpsc + tokio oneshot`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Eliminate `Handle::current().block_on` in memory + recall + search handlers; call sync MemoryStore directly

**Verifies:** v3-memory-rework.AC5.3, AC5.5

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/memory.rs` (remove 11 block_on pairs; call sync methods directly)
- Modify: `crates/pattern_runtime/src/sdk/handlers/recall.rs` (remove 5 block_on pairs; also removes `Delete` variant dispatch — Task 8 handles the variant removal itself, Task 6 removes the block_on)
- Modify: `crates/pattern_runtime/src/sdk/handlers/search.rs` (remove 2 block_on pairs)

**Implementation:**

1. For each site, transform:

   ```rust
   // Before:
   let result = tokio::runtime::Handle::current().block_on(
       store.get_rendered_content(&agent_id, &label)
   )?;

   // After:
   let result = store.get_rendered_content(&agent_id, &label)?;
   ```

2. Handler callsites using the consolidated surface:

   ```rust
   // Before: store.set_block_type(&agent_id, &label, BlockType::Working).block_on()
   // After:
   store.update_block_metadata(
       &agent_id,
       &label,
       BlockMetadataPatch::default().block_type(BlockType::Working),
   )?;
   ```

3. `handlers/message.rs` — the 3 `Handle::current()` references there dispatch to the async `MessageRouter` (network I/O to endpoints). **Do not touch these.** `MessageRouter` stays async-trait. Add a comment immediately above each preserved block_on site: `// MessageRouter stays async — see v3-memory-rework Phase 3 commit message`. (The comment is a fate marker per guidance: shows why it's intentionally not following the same pattern as memory/recall/search.)

4. `handlers/scope.rs` — already has zero block_on sites. No change.

5. For `handlers/recall.rs`'s `Delete` variant (line 146): the entire arm gets removed in Task 8. For Task 6, just rewrite the block_on into a direct call — the arm still exists, just simpler:

   ```rust
   RecallReq::Delete { id } => {
       store.delete_archival(&id)?;
       RecallReply::DeleteOk
   }
   ```

   Task 8 later deletes the `Delete` variant entirely.

**Testing:**

Existing handler unit tests + integration tests continue to pass. Add a specific regression test for AC5.5 — the "pre-existing spawn_blocking-related search bug" — at `tests/search_spawn_blocking_regression.rs`:

- Set up a real `MemoryCache` (post-Phase-3 sync) fixture with a populated block corpus.
- Spawn 10 tokio tasks, each doing 20 `spawn_blocking` wrapped calls into `handler_search(...)`.
- Assert: no "cannot start a runtime from within a runtime" panics; no deadlock within 15s; all searches return results.
- The pre-Phase-3 version would have panicked or deadlocked under this load because of the nested-runtime pattern. The test's passing proves the bug is resolved.

**Verification:**

Run: `grep -n "Handle::current().block_on" crates/pattern_runtime/src/sdk/handlers/{memory,recall,search,scope}.rs`
Expected: no matches in these four files.

Run: `grep -n "Handle::current" crates/pattern_runtime/src/sdk/handlers/message.rs`
Expected: 3 matches (preserved; those are MessageRouter dispatch).

Run: `cargo nextest run -p pattern_runtime --test search_spawn_blocking_regression`
Expected: passes.

**Commit:** `[pattern-runtime] eliminate Handle::current().block_on in memory/recall/search handlers; regression test for spawn_blocking search bug (AC5.5)`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Migrate async callsites to `spawn_blocking` for DB-hitting operations

**Verifies:** v3-memory-rework.AC5.6

**Files:**
- Modify: `crates/pattern_cli/src/commands/agent.rs`, `builder/agent.rs`, `builder/group.rs`, `group.rs`, `data_source_config.rs`, `slash_commands.rs` (6 files identified in Phase 1 investigation)
- Modify: `crates/pattern_runtime/src/memory/adapter.rs` (turn-boundary MemoryStore access)
- Modify: any turn-boundary code in `crates/pattern_runtime/src/session.rs` or `agent_loop/*.rs` that calls MemoryStore from async context

**Implementation:**

1. Audit each async callsite for whether it hits the DB or just reads in-memory cache metadata. Classification rule:
   - **DB-hitting** (requires `spawn_blocking`): anything that could take more than a few microseconds or that might block on pool acquisition. `create_block`, `get_block`, `delete_block`, `insert_archival`, `search_archival`, `search`, `list_blocks` with broad filter, `persist_block`.
   - **Cheap in-memory** (direct call): `mark_dirty`, `get_block_metadata` when the block is already cached (but note: the caller often can't tell if it's cached; prefer `spawn_blocking` when in doubt — the overhead is small).

2. Wrap DB-hitting calls:

   ```rust
   // Before (pattern_cli):
   let blocks = store.list_blocks(BlockFilter::by_agent(agent_id)).await?;
   //                                                            ^^^^^^ but .await is now gone (sync trait)

   // After (pattern_cli — pattern for async callers):
   let store_clone = store.clone();
   let agent_id_clone = agent_id.clone();
   let blocks = tokio::task::spawn_blocking(move || {
       store_clone.list_blocks(BlockFilter::by_agent(agent_id_clone))
   })
   .await
   .map_err(|e| CliError::JoinError(e))??;
   ```

   Wrap `store_clone` is necessary because `MemoryStore: Send + Sync + 'static`; the store is typically `Arc<dyn MemoryStore>` which clones cheaply.

3. Provide a small helper crate-local wrapper if the ergonomic cost in pattern_cli is high:

   ```rust
   // crates/pattern_cli/src/memory_blocking.rs
   use std::future::Future;

   pub async fn blocking<T, F>(f: F) -> Result<T, CliError>
   where
       F: FnOnce() -> Result<T, pattern_core::MemoryError> + Send + 'static,
       T: Send + 'static,
   {
       tokio::task::spawn_blocking(f)
           .await
           .map_err(CliError::JoinError)?
           .map_err(CliError::Memory)
   }
   ```

   Usage:

   ```rust
   let blocks = blocking({
       let store = store.clone();
       let agent_id = agent_id.clone();
       move || store.list_blocks(BlockFilter::by_agent(agent_id))
   }).await?;
   ```

4. **`pattern_runtime/src/memory/adapter.rs` is NOT an async callsite — it lives in sync-land post-Phase-3.** Audit findings (file inspected 2026-04-19, 356 lines):

   - `MemoryStoreAdapter` is a thin passthrough: each `async fn foo(..) { self.inner.foo(..).await }` is a pure delegate with no other logic.
   - It's called from the eval worker thread, which is a **plain OS thread** after Task 5 of this phase (no outer tokio runtime). `spawn_blocking` is not usable there — it requires an outer tokio runtime.
   - `record_write()` and `drain_pending()` are already sync (`Mutex<Vec<_>>`).

   **Phase 3 change to the adapter:** pure desyncification, matching Task 3's MemoryCache changes. Concretely:

   ```rust
   // Before (current):
   #[async_trait]
   impl MemoryStore for MemoryStoreAdapter {
       async fn create_block(&self, agent_id: &str, create: BlockCreate) -> MemoryResult<StructuredDocument> {
           self.inner.create_block(agent_id, create).await
       }
       // ... 27 more async methods, all pure delegates ...
   }

   // After:
   impl MemoryStore for MemoryStoreAdapter {
       fn create_block(&self, agent_id: &str, create: BlockCreate) -> MemoryResult<StructuredDocument> {
           self.inner.create_block(agent_id, create)
       }
       // ... 18 more sync methods, all pure delegates + 5 new consolidated ones ...
   }
   ```

   - Remove `#[async_trait]` attribute.
   - Every `async fn foo(..) { self.inner.foo(..).await }` → `fn foo(..) { self.inner.foo(..) }`.
   - Apply the same consolidation shape as Task 3: 19 methods total, 5 of them are the consolidated ones (`list_blocks(BlockFilter)`, `update_block_metadata`, `undo_redo`, `history_depth`, `search(SearchScope)`). The adapter delegates each consolidated method to `self.inner` unchanged.
   - Test `adapter_delegates_create_block` (current file line 318-330) is currently `#[tokio::test] async fn ... .await`; it becomes `#[test] fn ...` with `.await`s removed.

   `spawn_blocking` appears only at the `pattern_cli` boundary and at any pattern_runtime code paths outside the eval worker that hit MemoryStore from async tokio contexts. The eval worker itself is sync-thread-native; adapter calls from inside the worker are direct function calls, no bridging needed.

**Testing:**

- Unit test per callsite: existing tests that spawn async contexts and invoke these pattern_cli commands should continue to pass without "cannot block on runtime" errors.
- Concurrency test in `crates/pattern_cli/tests/concurrent_memory_ops.rs`: spawn N tokio tasks each invoking a chain of block read/list/search operations; assert all complete.

**Verification:**

Run: `cargo check --workspace`
Expected: clean.

Run: `cargo nextest run --workspace`
Expected: all pass.

Run: `grep -rn "store\.get_block\|store\.list_blocks\|store\.search\|store\.create_block" crates/pattern_cli/src crates/pattern_runtime/src | grep -v "spawn_blocking\|tests/"` (heuristic)
Expected: no bare MemoryStore calls from async contexts outside of `spawn_blocking` wrappers. Any remaining hits are either (a) sync contexts already, or (b) the cheap-path exceptions documented in code comments.

**Commit:** `[pattern-cli] [pattern-runtime] wrap MemoryStore DB calls in spawn_blocking at async callsites`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Remove `RecallReq::Delete` from the agent-facing SDK; remove `Pattern.Recall.delete` from Haskell; add trybuild compile-fail test

**Verifies:** v3-memory-rework.AC4.9

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/requests/recall.rs` (remove `Delete` variant from `RecallReq`)
- Modify: `crates/pattern_runtime/src/sdk/requests/mod.rs` or wherever the GADT bridge lives (remove any mirror of the Delete variant there)
- Modify: `crates/pattern_runtime/src/sdk/handlers/recall.rs` (remove the `Delete` match arm — MemoryStore::delete_archival can no longer be invoked through the SDK)
- Modify: `crates/pattern_runtime/haskell/Pattern/Recall.hs` (remove the `delete :: Member Recall effs => EntryId -> Eff effs ()` declaration; remove its GADT constructor from the Recall effect enum)
- Modify: `crates/pattern_runtime/Cargo.toml` (add `trybuild = "1"` to `[dev-dependencies]`)
- Create: `crates/pattern_runtime/tests/no_archive_delete.rs` (trybuild driver)
- Create: `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs` (the compile-fail input)

**Implementation:**

1. Remove the Rust variant:

   ```rust
   // sdk/requests/recall.rs — BEFORE:
   pub enum RecallReq {
       Insert { ... },
       Search { ... },
       Get { ... },
       Delete { id: String },  // ← remove this variant
   }

   // AFTER:
   pub enum RecallReq {
       Insert { ... },
       Search { ... },
       Get { ... },
   }
   ```

   The match arm in `handlers/recall.rs:146` gets deleted in the same commit; `cargo check -p pattern_runtime` will confirm no stale references.

2. Remove the Haskell symbol:

   ```haskell
   -- haskell/Pattern/Recall.hs — BEFORE:
   delete :: Member Recall effs => EntryId -> Eff effs ()
   delete id = send $ Delete id

   -- Recall effect GADT:
   data Recall m a where
     Insert :: Text -> Map Text Value -> Recall m EntryId
     Search :: Text -> Int -> Recall m [ArchivalEntry]
     Get :: EntryId -> Recall m (Maybe ArchivalEntry)
     Delete :: EntryId -> Recall m ()  -- ← remove this constructor
   ```

   Agent programs that invoke `Pattern.Recall.delete` will fail at Tidepool compile time with the standard `Variable not in scope: delete` diagnostic. No extra error-surfacing work needed — Tidepool's error messages are already clear.

3. Trybuild compile-fail test:

   `crates/pattern_runtime/tests/no_archive_delete.rs`:

   ```rust
   #[test]
   fn archive_delete_no_longer_reachable_via_sdk() {
       let t = trybuild::TestCases::new();
       t.compile_fail("tests/trybuild/no_archive_delete.rs");
   }
   ```

   `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs`:

   ```rust
   use pattern_runtime::sdk::requests::RecallReq;

   fn main() {
       let _ = RecallReq::Delete {
           id: "should-not-compile".into(),
       };
   }
   ```

   This file's compilation fails because `RecallReq::Delete` no longer exists. Trybuild wraps the failure into a passing `#[test]`.

4. **Do NOT remove `MemoryStore::delete_archival` from the trait.** Human-operator tooling (pattern-test-cli curation, the eventual TUI) calls `store.delete_archival(...)` directly. Agents cannot reach it because there is no SDK effect for it.

5. Port-list doc update: add a note to `docs/plans/rewrite-v3-portlist.md`:

   ```markdown
   ### Recall SDK surface shrink (Phase 3 — completed YYYY-MM-DD)

   - Removed `RecallReq::Delete` variant from the agent-facing SDK
     (`pattern_runtime::sdk::requests::recall`).
   - Removed `Pattern.Recall.delete` Haskell symbol and its Recall GADT
     constructor.
   - `MemoryStore::delete_archival` retained in the trait for human-operator
     tooling (CLI / TUI).
   - Agent programs referencing `Pattern.Recall.delete` fail at Tidepool
     compile time with a 'variable not in scope' diagnostic.
   - Trybuild compile-fail test at
     `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs`.
   ```

**Testing:**

- `tests/no_archive_delete.rs` (trybuild compile-fail test).
- Existing Recall handler tests for Insert / Search / Get continue to pass.

**Verification:**

Run: `cargo nextest run -p pattern_runtime --test no_archive_delete`
Expected: passes (the inner compile-fail succeeds as expected).

Run: `grep -n "RecallReq::Delete\|Recall::Delete\|Recall\\.delete" crates/pattern_runtime/src/ crates/pattern_runtime/haskell/`
Expected: zero matches.

Run: `cargo check -p pattern_runtime`
Expected: clean.

**Commit:** `[pattern-runtime] remove RecallReq::Delete + Pattern.Recall.delete; add trybuild compile-fail test (AC4.9)`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 3 Done-when recap

- `cargo check --workspace` clean (AC4.1, AC4.8 — async_trait still present for 8 other traits).
- `MemoryStore` trait has 19 methods; consolidation mapping documented in trait doc (AC4.2).
- `cargo nextest run --workspace` green across all crates — MemoryCache tests pass unchanged in behavior (AC4.7), search-spawn-blocking regression test passes (AC5.5), 100-request eval worker test passes (AC5.7), panic-test passes (AC5.8).
- `cargo test --doc --workspace` green (doctests on new sync trait).
- Zero `Handle::current().block_on` sites in memory/recall/search/scope handlers (AC5.3); message.rs retains its MessageRouter sites with an explanatory fate-marker comment.
- `eval_worker.rs` has no `tokio::runtime::Builder::new_multi_thread` + no `block_in_place` (AC5.1); worker spawned via `std::thread::spawn` + `std::sync::mpsc` (AC5.2).
- Async callsites wrap DB-hitting MemoryStore calls in `tokio::task::spawn_blocking` (AC5.6); cheap metadata operations call directly.
- `Session::step_with_agent_loop` signature still `async fn`; caller contract unchanged (AC5.4).
- `trybuild` dev-dep added; `no_archive_delete.rs` compile-fail test passes (AC4.9).
- `pattern_runtime/CLAUDE.md` freshened.
- Port-list entry recorded.
- Design plan's six "~18" / "18 methods" references updated to 19 as part of Phase 4 Task 8 (the consolidated design-plan update pass — see phase_04.md Task 8 for the exact line list).

## Notes for downstream phases

- **Phase 4** (fs serialization + subscribers): subscribers run as **plain OS threads**, mirroring this phase's eval worker decision. Workload is sync-dominant (rusqlite FTS5 updates, file emission, sha2 hashing); a tokio task wrapping spawn_blocking for every step would be needless overhead. Loro's `subscribe_root` callback is already sync, so intake has no bridge. Library-first picks (confirmed by a sync-thread-pool crate survey — no single crate fits; stdlib + focused libs compose best): **`crossbeam-channel`** for bounded intake + `select!` debounce multiplex (eval worker's stdlib `std::sync::mpsc` suffices here because there's no multiplex requirement, but Phase 4 has one), **`tokio_util::sync::CancellationToken`** for cross-thread cancel (async supervisor ↔ sync worker). The only async-side interaction is pushing re-embed requests to an async queue via `tokio::sync::mpsc::UnboundedSender::send` (sync-callable, no bridge). Supervisor stays async (hand-rolled ~60-line tokio task watching heartbeats; no task-supervisor crate targets OS threads).
- **Phase 5** (jj CLI adapter + quiesce): `quiesce()` signals sync_workers to drain; the drain semantics depend on Phase 4's supervisor. The eval worker's panic-handling pattern (this phase's Task 5) is the reference for quiesce's "worker dead" handling.
- **Phase 8** (MemoryScope): the new `SearchScope` parameter introduced in Task 2 is what `MemoryScope` sits on top of; `MemoryScope::search` translates the persona+project routing policy into a `SearchScope` before delegating to the underlying `MemoryStore`.
- **Plan 2** (`v3-task-skill-blocks`): Task and Skill SDK effects will follow the same desyncification pattern — new sync methods on `MemoryStore`, new consolidated types, new handler code that calls them directly. This phase's patterns are the template.
