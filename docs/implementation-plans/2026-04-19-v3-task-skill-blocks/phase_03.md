# v3-task-skill-blocks Phase 3: `ctx.tasks.*` SDK surface

**Goal:** Expose an agent-facing effect algebra for task operations: eight methods (`create_task`, `update_task`, `transition_status`, `link`, `unlink`, `list_tasks`, `query_graph`, `add_comment`) wired through the existing pattern_runtime SDK bridge pattern.

**Architecture:** Mirror the existing `Pattern.Memory` GADT + Rust request/handler pattern. A Haskell module `Pattern.Tasks` declares the algebra; `pattern_runtime/src/sdk/requests/tasks.rs` carries the Rust-side `TasksReq` enum (with `#[core(module, name)]` attributes) derived from the GADT; `pattern_runtime/src/sdk/handlers/tasks.rs` implements a sync handler that calls the sync `MemoryStore` trait directly (post sibling Phase 2 sync refactor) plus the sync `pattern_db::queries::task` surface from Phase 2.

**Tech Stack:** Rust (pattern_runtime, pattern_core, pattern_db, pattern_memory), Haskell SDK (ghc/cabal per existing Pattern SDK), sync `MemoryStore` + sync `pattern_db` (rusqlite), `cargo nextest`.

**Scope:** Phase 3 of 5.

**Codebase verified:** 2026-04-19.

---

## Acceptance Criteria Coverage

### v3-task-skill-blocks.AC4: `ctx.tasks.*` SDK surface methods

- **v3-task-skill-blocks.AC4.1 Success:** `create_task` writes a new task item to the target block; returned `TaskItemId` matches the item's id in loro; subsequent `list_tasks` includes it
- **v3-task-skill-blocks.AC4.2 Success:** `update_task` patches specified fields; unspecified fields unchanged; `updated_at` refreshed
- **v3-task-skill-blocks.AC4.3 Success:** `transition_status` changes status field; `updated_at` refreshed; if new status is `Completed`, optional `completed_at` set (if block schema tracks it)
- **v3-task-skill-blocks.AC4.4 Success:** `link(A, B)` adds one edge in A's loro doc (A.blocks += B) as a single atomic commit; `task_edges` has exactly one row (source=A, target=B) after subscriber reconciles; the reverse direction (tasks that block B) is queryable via `task_edges WHERE target_block+target_item = B` — no separate reverse row needed
- **v3-task-skill-blocks.AC4.5 Success:** `unlink(A, B)` removes the entry from A.blocks in a single loro commit; `task_edges` row deleted by subscriber on next reconcile
- **v3-task-skill-blocks.AC4.5b Edge:** `link(A, B)` where A and B are in different TaskList blocks is atomic — only A's block is modified, so there's no cross-document commit coordination required
- **v3-task-skill-blocks.AC4.6 Success:** `add_comment(task, text)` appends `TaskComment { author: current_agent, timestamp: now, text }` to the task's comments list
- **v3-task-skill-blocks.AC4.7 Failure:** `update_task` on a nonexistent `TaskEdgeRef` returns `MemoryError::TaskNotFound` with the offending ref in the error message
- **v3-task-skill-blocks.AC4.8 Edge:** `link(A, A)` (self-edge) is allowed — graph topology is unconstrained in v1

### v3-task-skill-blocks.AC5: `list_tasks` + `query_graph`

- **v3-task-skill-blocks.AC5.1 Success:** `list_tasks(block=Some(h), TaskFilter::default())` returns all tasks in block h as TaskViews
- **v3-task-skill-blocks.AC5.2 Success:** `list_tasks(block=None, ...)` returns tasks from all scope-visible TaskList blocks
- **v3-task-skill-blocks.AC5.3 Success:** `list_tasks` with `status: Some([InProgress, Blocked])` filters to those statuses; `owner: Some(agent)` filters to owner; `has_blockers: Some(true)` filters to tasks with at least one blocked_by edge; `keyword` filters via FTS5
- **v3-task-skill-blocks.AC5.4 Success:** `query_graph(root, GraphQuery { direction, depth, max_nodes })` returns BFS slice respecting all three params; `Forward` follows outgoing edges, `Reverse` follows incoming (queried via target_block lookup), `Both` combines; nodes + edges consistent
- **v3-task-skill-blocks.AC5.5 Success:** Scope enforcement — agent with `CoreOnly` isolation sees only project-scope tasks via `list_tasks`; persona-scope TaskList blocks are invisible
- **v3-task-skill-blocks.AC5.6 Failure:** `query_graph` with `depth: Some(0)` returns only the root node, no edges
- **v3-task-skill-blocks.AC5.6b Success:** `query_graph` respects `max_nodes` cap (default 1000 if None); when cap hit, `truncated: true` in result and BFS frontier dropped
- **v3-task-skill-blocks.AC5.7 Edge:** Graph traversal on a cyclic subgraph terminates at the depth or max_nodes limit (whichever hit first)
- **v3-task-skill-blocks.AC5.8 Edge:** Runaway graph test: 10,000 tasks, `query_graph` returns ≤1000 nodes with `truncated: true`; traversal completes in bounded time

---

## Design deviations recorded during planning

- **Haskell SDK module convention:** design says "Haskell SDK module `Pattern.Tasks` in `pattern_runtime`'s SDK resource directory" — the actual path is `crates/pattern_runtime/haskell/Pattern/`. Uses cabal, qualified-import convention for modules whose symbols would collide with Prelude (Tasks is `Tasks.create`, `Tasks.list`, etc.).
- **GADT bridge attribute:** design doesn't spell out the `#[core(module = "Pattern.Tasks", name = "…")]` attribute usage — it's a `FromCore` derive convention from the existing codebase. Tasks request enum must mirror the Haskell GADT constructor names exactly.
- **MemoryStore is sync post-refactor:** the sibling memory-rework plan sync-ifies `MemoryStore` (Phase 2 of sibling plan). Phase 3 of THIS plan runs after the refactor has landed, so handlers call `store.get_block(...)` etc. directly as sync methods. No `handle.block_on(store.method())`, no wrapping MemoryStore calls in `spawn_blocking`. Task 1 re-verifies the sync signature at execution time.
- **Task-index queries go through `pattern_db` directly, not MemoryStore:** MemoryStore doesn't expose `list_tasks_filtered` / `query_task_graph_bfs`. Pre-flight audit (2026-04-23) corrected an earlier misconception in this plan that claimed "handlers/search.rs uses an existing adapter surface for FTS5 queries and handlers reuse that path" — that is false. `handlers/search.rs` routes through `self.store.search(...)` (a MemoryStore trait method); it does NOT touch `pattern_db` directly, and there is no existing handler-level connection-acquisition pattern. **The actual path:** `SessionContext::db()` at `crates/pattern_runtime/src/session.rs:311` returns `&Arc<pattern_db::ConstellationDb>`. Handlers acquire a pooled `rusqlite::Connection` via `cx.user().db().get()?` inside `EffectHandler::handle`. `ConstellationDb::get()` lives at `crates/pattern_db/src/connection.rs:146` and returns `DbResult<r2d2::PooledConnection<SqliteConnectionManager>>` — `Deref<Target = rusqlite::Connection>`, usable with the Phase 2 `pattern_db::queries::task` sync surface directly.
- **`TaskNotFound` error variant:** MemoryError doesn't currently have a TaskNotFound variant. Task 2 adds one.
- **Scope resolution:** handlers call `resolve_scope(&scope, caller, &store)` from `handlers/scope.rs` before any cross-agent/cross-block query, then intersect the result with the queried block's scope. For `list_tasks(block=None)`, enumerate all TaskList blocks across the resolved agent set.
- **`TaskPatch.active_form` uses `Option<Option<String>>`** to allow explicit clearing — the design plan says `Option<String>` which can only set-or-leave-untouched. The double-option matches the treatment of `owner` in the same struct; the design's single-option for `active_form` was likely an oversight. Documented here; type itself lives in Phase 2 Task 1b.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
### Subcomponent A: Prerequisites + shared types

<!-- START_TASK_1 -->
### Task 1: Verify Phase 2 surface + locate DB connection acquisition

**Files:** none written.

**Step 1:** Run:
```
rg -n 'pub fn list_tasks_filtered|pub fn query_task_graph_bfs' crates/pattern_db/src/queries
```
Expected: both present (Phase 2 Tasks 6 + 7 landed). If missing, STOP.

**Step 2:** Read the following files in order and save a condensed reference to `target/plan-phase3-context-surface.txt`:

1. **`crates/pattern_runtime/src/session.rs`** (lines ~41-100 for the struct, line 311 for `fn db()`, line ~165-180 for the relevant constructor):
   - Confirm `SessionContext::db()` returns `&Arc<pattern_db::ConstellationDb>`.
   - Confirm `SessionContext::memory_store()` / `SessionContext::adapter()` surface for MemoryStore access.
   - Note the `cancel_state()` accessor (see cancellation pattern in `handlers/search.rs:77`).
2. **`crates/pattern_db/src/connection.rs`** (line 146):
   - Confirm `ConstellationDb::get(&self) -> DbResult<PooledConnection<SqliteConnectionManager>>`.
   - The returned `PooledConnection` derefs to `rusqlite::Connection` and can be passed to `pattern_db::queries::task::*` functions directly.
3. **`crates/pattern_runtime/src/sdk/describe.rs`** (line 65 for `trait DescribeEffect`, lines 12-62 for `EffectDecl` shape):
   - Record the `EffectDecl { type_name, description, constructors, type_defs, helpers }` field set — TasksHandler mirrors this.
4. **`crates/pattern_runtime/src/sdk/handlers/search.rs`** end-to-end (~200 lines):
   - Record the handler struct (holds `Arc<dyn MemoryStore>`), `DescribeEffect` impl at `:45`, `EffectHandler<SessionContext>` impl with `type Request = SearchReq; fn handle(...)`.
   - Record the cancellation-check pattern: `let state = cx.user().cancel_state(); if state.cancellation.load(Ordering::SeqCst) { return Err(EffectError::Handler(...)); }`.
   - Record the `HandlerGuard` / `CANCELLED_SENTINEL` usage around long-running work.
5. **`crates/pattern_runtime/src/sdk/handlers/memory.rs`** end-to-end:
   - Record the `record_exchange` / post-mutation hook pattern — TasksHandler uses the same for mutation methods (`create_task`, `update_task`, `transition_status`, `link`, `unlink`, `add_comment`).
6. **`crates/pattern_runtime/src/sdk/requests/memory.rs`** (lines 1-40):
   - Record the `#[derive(Debug, FromCore)] enum MemoryReq` shape and `#[core(module = "Pattern.Memory", name = "…")]` attribute usage. TasksReq uses `module = "Pattern.Tasks"`.
7. **`crates/pattern_runtime/src/sdk/bundle.rs`**:
   - Find `SdkBundle` (HList via `frunk::HCons`). Record the current tag ordering — TasksHandler needs a new tag (likely after the last existing handler; Task 10 extends this).

**VERIFY:** Run `rg -n 'async fn' crates/pattern_core/src/traits/memory_store.rs` — expect zero matches (confirms sync refactor landed). If async methods remain, STOP.

**VERIFY:** Run `rg -n 'pub fn db\b' crates/pattern_runtime/src/session.rs` — expect a match at or near line 311. If missing, STOP — handler db-access pattern has been refactored away and this task needs a fresh audit.

**Step 3:** Read `crates/pattern_runtime/haskell/Pattern/Memory.hs` end-to-end. Record the GADT declaration style and the qualified-import convention.

**Step 4:** Read `crates/pattern_runtime/src/sdk/bundle.rs` to find the `SdkBundle` HList — Task 10 extends it.

**Step 5:** No commit.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: SDK-local error variants + filter helpers

**Verifies:** AC4.7 (`TaskNotFound`), supporting AC4.*.

**Scope note:** The shared query types (`TaskSpec`, `TaskPatch`, `TaskFilter`, `TaskView`, `GraphQuery`, `Direction`, `GraphSlice`) already landed in **Phase 2 Task 1b**. This task only adds error variants + handler-facing helper methods on `TaskFilter`.

**Files:**
- Modify: existing `MemoryError` enum in `pattern_core` (find via `rg -n 'enum MemoryError' crates/pattern_core/src`).
- Modify: `crates/pattern_core/src/types/memory_types/task_query.rs` — add helper methods on `TaskFilter`.

**Implementation:**

In `MemoryError` (or whichever error enum the memory surface uses — could also be a new `TasksError`), add:
- `TaskNotFound { block: BlockHandle, item: TaskItemId }` — implement `Display` so the rendered message includes both parts.
- `NotATaskList { block: BlockHandle }` — raised when a handler operates on a non-TaskList block.

Apply `#[non_exhaustive]` on the error enum (if not already set).

On `TaskFilter`, add helper methods used by Phase 3 Task 9's `handle_list_tasks`:
```rust
impl TaskFilter {
    pub fn scoped_to_block(self, block: BlockHandle) -> ScopedTaskFilter { ... }
    pub fn scoped_to_agents(self, agents: Vec<AgentId>) -> ScopedTaskFilter { ... }
}

pub struct ScopedTaskFilter {
    pub inner: TaskFilter,
    pub scope: TaskQueryScope,
}

pub enum TaskQueryScope {
    SingleBlock(BlockHandle),
    Agents(Vec<AgentId>),
}
```

`pattern_db::queries::task::list_tasks_filtered` is updated in a follow-up Phase 2 refactor (Task 6 of this phase's sibling plan work) OR stays as-is consuming `&TaskFilter` with scope applied separately — implementor's call based on what Phase 2 actually landed.

**Testing:**

Unit tests:
- `TaskFilter::default().scoped_to_block(h)` produces a ScopedTaskFilter with inner defaults.
- Error `Display` for `TaskNotFound` includes handle + item id.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::task_query`
- Run: `cargo nextest run -p pattern-core --lib errors`

**Commit:**
```
jj commit -m "[pattern-core] task SDK error variants + TaskFilter scope helpers"
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Skeleton of Rust request + handler modules

**Files:**
- Create: `crates/pattern_runtime/src/sdk/requests/tasks.rs` — empty enum + FromCore derive skeleton.
- Create: `crates/pattern_runtime/src/sdk/handlers/tasks.rs` — empty Handler impl skeleton.
- Modify: `crates/pattern_runtime/src/sdk/requests.rs` (add `pub mod tasks;`).
- Modify: `crates/pattern_runtime/src/sdk/handlers.rs` (add `pub mod tasks;`).

**Implementation:**

Minimal compiling skeleton:
```rust
// requests/tasks.rs
use pattern_macros::FromCore;

#[derive(Debug, FromCore)]
pub enum TasksReq {
    // variants added per-method in later tasks
}
```
```rust
// handlers/tasks.rs
use super::super::requests::tasks::TasksReq;

pub struct TasksHandler;

// Handler impl is filled in by Task 4 onward.
```

Commit this skeleton so subsequent tasks show focused diffs.

**Verification:**
- Run: `cargo check --workspace` — passes.

**Commit:**
```
jj commit -m "[pattern-runtime] scaffolding for ctx.tasks SDK surface"
```
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->
### Subcomponent B: Haskell + Rust bridge for eight methods

<!-- START_TASK_4 -->
### Task 4: `Pattern.Tasks` Haskell GADT + module

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Tasks.hs`.
- Modify: the project's cabal/package file to register the module (path discovered in Task 1).

**Implementation:**

Declare the GADT with eight constructors and matching data types. Mirror the shape found in `Pattern.Memory` and `Pattern.Search`. Because the Rust side serializes method arguments as JSON strings, each GADT constructor takes either primitive args (String, Int, Bool) or a JSON-encoded payload string.

GADT sketch (exact syntax to match existing module style):
```haskell
module Pattern.Tasks where
-- imports …

data Tasks a where
  Create       :: BlockHandle -> Text -> Tasks TaskItemId      -- block, TaskSpec-as-json
  Update       :: TaskEdgeRef    -> Text -> Tasks ()              -- ref, TaskPatch-as-json
  Transition   :: TaskEdgeRef    -> Text -> Tasks ()              -- ref, TaskStatus-as-json
  Link         :: TaskEdgeRef    -> TaskEdgeRef -> Tasks ()
  Unlink       :: TaskEdgeRef    -> TaskEdgeRef -> Tasks ()
  List         :: Maybe BlockHandle -> Text -> Tasks Text       -- block, TaskFilter-as-json, returns [TaskView]-as-json
  QueryGraph   :: TaskEdgeRef    -> Text -> Tasks Text             -- root, GraphQuery-as-json, returns GraphSlice-as-json
  AddComment   :: TaskEdgeRef    -> Text -> Tasks ()
```

Provide convenience wrappers mirroring `Pattern.Memory`'s style (e.g., `createTask :: BlockHandle -> TaskSpec -> Eff r TaskItemId` that JSON-encodes on the Haskell side).

**Verification:**
- Build the Haskell SDK via its existing cabal/stack entrypoint (command documented in `crates/pattern_runtime/haskell/README.md`, or wherever the project records it). Expected: compiles clean.

**Commit:**
```
jj commit -m "[pattern-runtime] Pattern.Tasks Haskell GADT"
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Fill `TasksReq` enum with all eight variants + FromCore attrs

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/requests/tasks.rs`.

**Implementation:**

Each variant matches a Haskell GADT constructor exactly:
```rust
#[derive(Debug, FromCore)]
pub enum TasksReq {
    #[core(module = "Pattern.Tasks", name = "Create")]
    Create(String /* BlockHandle */, String /* TaskSpec JSON */),

    #[core(module = "Pattern.Tasks", name = "Update")]
    Update(String /* TaskEdgeRef */, String /* TaskPatch JSON */),

    #[core(module = "Pattern.Tasks", name = "Transition")]
    Transition(String /* TaskEdgeRef */, String /* TaskStatus JSON */),

    #[core(module = "Pattern.Tasks", name = "Link")]
    Link(String, String),

    #[core(module = "Pattern.Tasks", name = "Unlink")]
    Unlink(String, String),

    #[core(module = "Pattern.Tasks", name = "List")]
    List(Option<String>, String /* TaskFilter JSON */),

    #[core(module = "Pattern.Tasks", name = "QueryGraph")]
    QueryGraph(String /* root TaskEdgeRef */, String /* GraphQuery JSON */),

    #[core(module = "Pattern.Tasks", name = "AddComment")]
    AddComment(String, String),
}
```

**Verification:**
- `cargo check --workspace` — passes.

**Commit:**
```
jj commit -m "[pattern-runtime] TasksReq enum + FromCore wiring"
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: `TasksHandler` Handler impl, skeleton for all eight variants

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/tasks.rs`.

**Implementation:**

Mirror the post-sync `handlers/memory.rs`:
- Declare `TasksHandler` with `DescribeEffect` impl registering `Pattern.Tasks` preamble text.
- Implement the sync handler entry point that receives `TasksReq` and a `SessionContext`:
  ```rust
  pub fn handle(cx: &SessionContext, req: TasksReq) -> Result<Value, EffectError> {
      let store = cx.user().memory_store();
      let agent_id = cx.user().agent_id().to_owned();

      match req {
          TasksReq::Create(block, spec_json) => handle_create(&store, &agent_id, block, spec_json),
          TasksReq::Update(reff, patch_json) => handle_update(&store, reff, patch_json),
          // ...
      }
  }
  ```
  If the SDK bridge needs thread isolation at the boundary (e.g., the outer dispatch spawns a blocking thread before calling `handle`), that's the adapter's concern — handler bodies stay sync.
- Each `handle_*` function is a stub returning `unimplemented!("Tasks::X")` for now. Subsequent tasks replace each stub. This lets Task 10 wire bundle/describe without waiting for every method body. The phase-level "no `unimplemented!()`" done-when applies at **phase completion** (after Task 10), not after each intermediate task — this is expected transient state.

**Verification:**
- `cargo check --workspace` — passes.

**Commit:**
```
jj commit -m "[pattern-runtime] TasksHandler dispatch skeleton"
```
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 7-9) -->
### Subcomponent C: Handler bodies

<!-- START_TASK_7 -->
### Task 7: `create_task`, `update_task`, `transition_status`, `add_comment`

**Verifies:** v3-task-skill-blocks.AC4.1, AC4.2, AC4.3, AC4.6, AC4.7.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/tasks.rs` — fill `handle_create`, `handle_update`, `handle_transition`, `handle_add_comment`.

**Implementation sketch:**

- `handle_create`: fetch the TaskList LoroDoc via `store.get_block(agent_id, block)`; assert schema is TaskList (else error with `NotATaskList`); mint a new `TaskItemId` via `new_snowflake_id()`; insert a new LoroMap under the `items` LoroMovableList with all TaskSpec fields + derived `created_at` + `updated_at` timestamps (jiff `now()`); commit the LoroDoc; return the new id. Subscriber reconciles the `tasks` row on its own schedule.
- `handle_update`: locate the item by id; apply each `Some(field)` from the patch; set `updated_at`; commit. Return `TaskNotFound` if the item doesn't exist.
- `handle_transition`: special case of update that only modifies `status`; if new status is `Completed`, also set `completed_at` in metadata (per AC4.3's "if block schema tracks it" — the design leaves this as schema-optional; use a `completed_at` key inside the item's loro map, not a separate DB column).
- `handle_add_comment`: locate item; append `TaskComment { author: cx.user().agent_id(), timestamp: jiff::Timestamp::now(), text }` to the item's `comments` loro list; commit.

**Testing:**

Tests in `crates/pattern_runtime/src/sdk/handlers/tasks.rs` (or a sibling `tests/tasks_handler.rs`) using `TestMemoryStore` from `pattern_runtime::testing::in_memory_store`:
- `create_then_list_returns_it`: create a task, immediately list, assert the new id is present.
- `update_patches_specified_fields_only`: seed a task, patch `subject`, assert description unchanged, `updated_at` refreshed.
- `transition_to_completed_sets_completed_at`: transition, then inspect block, assert metadata has `completed_at`.
- `add_comment_appends`: add three comments, list returns them in order, each with the current agent id.
- `update_on_missing_ref_returns_task_not_found`: call update with a bogus TaskEdgeRef, assert `MemoryError::TaskNotFound`.

**Verification:**
- Run: `cargo nextest run -p pattern-runtime --lib handlers::tasks`

**Commit:**
```
jj commit -m "[pattern-runtime] implement create/update/transition/add_comment handlers"
```
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: `link` + `unlink` handlers

**Verifies:** v3-task-skill-blocks.AC4.4, AC4.5, AC4.5b, AC4.8.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/tasks.rs`.

**Implementation:**

- `handle_link(source: TaskEdgeRef, target: TaskEdgeRef)`:
  - Fetch source block's LoroDoc; locate item by `source.task_item` (error if source is block-level — edges originate from items only).
  - Append `target` to the item's `blocks` list if not already present (dedup here is optional — Phase 2's upsert is wholesale replacement so duplicates in loro would still collapse in sqlite, but dedup in loro keeps the canonical `.kdl` file tidy).
  - Commit the source's LoroDoc only. Do NOT touch target's doc — this is the single-source-of-truth edge model.
- `handle_unlink(source, target)`: fetch source; remove target from the item's `blocks` list; commit. No-op if the edge doesn't exist.

**Testing:**

- `link_adds_single_edge`: call link(A, B); wait for subscriber (use the existing test helpers — likely `subscriber.flush().await`); assert exactly one row in `task_edges` where source=A and target=B.
- `link_cross_block_is_atomic`: A in TaskList block L1, B in TaskList block L2. Call link(A, B). Assert only L1's LoroDoc received a commit (L2's doc version counter unchanged). Assert the edge row exists.
- `unlink_removes_edge`: after `link`, call `unlink`, assert the row is gone after reconcile.
- `self_edge_allowed`: call link(A, A). Succeeds. One edge row with source == target.
- `double_link_is_idempotent_after_dedup`: call link(A, B) twice. Canonical `.kdl` file shows exactly one edge entry.

**Verification:**
- Run: `cargo nextest run -p pattern-runtime --lib handlers::tasks::link_tests`

**Commit:**
```
jj commit -m "[pattern-runtime] implement link/unlink handlers (source-only edges)"
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: `list_tasks` + `query_graph` handlers

**Verifies:** v3-task-skill-blocks.AC5.*.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/tasks.rs`.

**Implementation:**

- `handle_list_tasks(block: Option<BlockHandle>, filter: TaskFilter)`:
  - Resolve scope: call `handlers::scope::resolve_scope(&scope, &agent_id, &store)` (sync post-refactor). Record the list of resolved agent ids.
  - If `block == Some(h)`, scope-check that the block belongs to one of the resolved agents. If not, return `EffectError::PermissionDenied` (existing variant). Then `pattern_db::queries::task::list_tasks_filtered(&conn, &filter.scoped_to_block(h))`.
  - If `block == None`, enumerate via `pattern_db::queries::task::list_tasks_filtered(&conn, &filter.scoped_to_agents(resolved_agents))`. `TaskFilter` gets helper methods `scoped_to_block` / `scoped_to_agents` that embed the scope constraint into the SQL WHERE clause.
  - Project `TaskRow → TaskView` (derive `blocker_count` from `task_edges WHERE target_block+target_item = row.block+row.item`, `blocks_count` from `WHERE source_block+source_item = row.block+row.item`). Batch these counts via two aggregate queries rather than N+1.
- `handle_query_graph(root: TaskEdgeRef, query: GraphQuery)`:
  - Scope-check root via resolve_scope as above.
  - Call `pattern_db::queries::task::query_task_graph_bfs(&conn, &root, query.direction, query.depth.unwrap_or(16), query.max_nodes.unwrap_or(1000))`. Returns `GraphSlice`.

**Testing:**

- `list_tasks_block_scope`: seed two blocks, list block=block_1, assert only block_1's tasks returned (AC5.1).
- `list_tasks_no_block_scope_visible`: seed blocks in two different agents' scopes, call `list(None)` as agent A — see only A's tasks (AC5.2).
- `list_tasks_status_filter`: seed 5 tasks with mixed statuses; filter `status=Some(vec![InProgress, Blocked])` returns exactly the matching subset (AC5.3).
- `list_tasks_keyword_filter`: seed tasks with diverse subjects; keyword matches via FTS5 (AC5.3).
- `list_tasks_has_blockers`: seed tasks with some blocked edges; filter `has_blockers=Some(true)` returns the blocked subset.
- `query_graph_forward_chain_5`: chain A→B→C→D→E, unlimited depth, returns 5 nodes + 4 edges (AC5.4).
- `query_graph_depth_zero`: returns root only, no edges (AC5.6).
- `query_graph_max_nodes_truncates`: 10k fixture, returns ≤1000 nodes, `truncated == true` (AC5.6b, AC5.8).
- `query_graph_cycle_terminates`: cycle fixture, returns bounded result (AC5.7).
- `query_graph_reverse_direction`: A→B, query B with Reverse, returns [B, A] + edge (AC5.4 Reverse).
- `list_tasks_persona_scope_hidden`: agent with `CoreOnly` isolation can't see persona-scope TaskList blocks (AC5.5).

**Verification:**
- Run: `cargo nextest run -p pattern-runtime --lib handlers::tasks list_tasks`
- Run: `cargo nextest run -p pattern-runtime --lib handlers::tasks query_graph`

**Commit:**
```
jj commit -m "[pattern-runtime] implement list_tasks + query_graph handlers"
```
<!-- END_TASK_9 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (task 10) -->
### Subcomponent D: SDK bundle integration

<!-- START_TASK_10 -->
### Task 10: Register `TasksHandler` in `SdkBundle` + `DescribeEffect`

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` — insert `TasksHandler` into the HList in a stable position (adjacent to Memory/Search). Match the existing type-list ordering convention.
- Modify: `crates/pattern_runtime/src/sdk/describe.rs` — register effect decls for Pattern.Tasks.

**Implementation:**

Follow the existing pattern for Memory: add the handler to the HList, thread it through any places that iterate over handlers at construction time, register its describe block.

**Testing:**

- Smoke test: `cargo test -p pattern-runtime --test describe_effects` (or whatever test validates the describe output) — assert that `Pattern.Tasks` now appears with all eight method names.
- Integration test: run a minimal session that dispatches a `TasksReq::Create` end-to-end through the bundle and expects a valid `TaskItemId` back.

**Verification:**
- Run: `cargo nextest run -p pattern-runtime`
- Expected: all existing tests plus new Tasks tests pass.

**Commit:**
```
jj commit -m "[pattern-runtime] register Pattern.Tasks in SdkBundle"
```
<!-- END_TASK_10 -->
<!-- END_SUBCOMPONENT_D -->

---

## Phase 3 Done when

- Task 1 prerequisite check passes (Phase 2 landed, connection acquisition patterns recorded).
- `cargo check --workspace` passes.
- `cargo nextest run -p pattern-runtime --lib --tests` passes including all handler tests.
- Haskell SDK module `Pattern.Tasks` compiles cleanly.
- Effect-describe output enumerates all eight `Pattern.Tasks` methods.
- All eight handlers implemented — no `unimplemented!()` remaining in `handlers/tasks.rs`.
- Scope enforcement verified end-to-end via the `persona_scope_hidden` and `cross_agent` tests.
- No `TODO` comments introduced.
