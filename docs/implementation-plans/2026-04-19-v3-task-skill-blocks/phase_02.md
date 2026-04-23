# v3-task-skill-blocks Phase 2: Task block index tables + subscriber extension

**Goal:** Stand up the `tasks` + `task_edges` SQLite index tables derived from TaskList loro state, hook the per-doc sync_worker from the sibling memory-rework plan to reconcile those tables on every TaskList block commit, and retire the legacy `coordination_tasks` surface.

**Architecture:** LoroDoc is canonical for task items and their outgoing `blocks` edges. The sync_worker reacts to loro commit events on TaskList blocks by diffing the item set and the per-item `blocks` list against the `tasks` / `task_edges` rows in a single `rusqlite::Transaction`. Reverse direction (who blocks me) is answered by indexed queries on `target_block + target_item`. Scope enforcement piggybacks on whatever mechanism the sibling plan wires for the existing block types.

**Tech Stack:** Rust (pattern_memory, pattern_db), rusqlite 0.39 (post-sibling-migration), SQLite FTS5, `metrics` 0.24 (workspace dep — sibling memory-rework Phase 4 added it crate-level in `pattern_memory`; promoted to workspace on 2026-04-23 pre-flight, see design deviation below), `cargo nextest`.

**Scope:** Phase 2 of 5.

**Codebase verified:** 2026-04-19.

---

## Acceptance Criteria Coverage

### v3-task-skill-blocks.AC2: Task block index tables + migration

- **v3-task-skill-blocks.AC2.1 Success:** Migration `0014_task_block_index.sql` (flat layout) or `memory/XX_task_block_index.sql` (sibling-subtree layout) applies cleanly to a fresh DB — Task 1 picks the exact filename at execution time
- **v3-task-skill-blocks.AC2.2 Success:** Migration round-trip test: fixture DB with pre-migration `tasks` rows migrates; pre-existing columns preserved; new columns added with defaults
- **v3-task-skill-blocks.AC2.3 Success:** `coordination_tasks` table dropped; no remaining references in active code paths
- **v3-task-skill-blocks.AC2.4 Success:** `task_edges` table created with `source_block`, `source_item NOT NULL`, `target_block`, `target_item NULL`; unique expression index over `COALESCE(target_item, '<block>')` serves as the effective primary key
- **v3-task-skill-blocks.AC2.5 Failure:** Attempting to insert a duplicate edge (same source_block + source_item + target_block + target_item) is rejected by the unique constraint
- **v3-task-skill-blocks.AC2.6 Edge:** Column drop of `priority` from `tasks` does not break any remaining queries (verified by `cargo check -p pattern-db`); indexes on `coordination_tasks` are dropped BEFORE the table drop so no DROP INDEX failures occur
- **v3-task-skill-blocks.AC2.7 Edge:** Block-level target reference (target_item NULL) and item-level reference to a hypothetical empty string id cannot collide because `source_item NOT NULL` rejects empty-string ids at the newtype level before insert reaches sqlite

### v3-task-skill-blocks.AC3: Subscriber reconciliation for TaskList blocks

- **v3-task-skill-blocks.AC3.1 Success:** Writing a TaskList block with 5 items + 3 edges triggers subscriber reconciliation; 5 rows in `tasks`, 3 edge rows in `task_edges` (single-source-of-truth model; reverse direction is queried, not stored) match loro state
- **v3-task-skill-blocks.AC3.2 Success:** Deleting a task item removes the corresponding `tasks` row + all edges referencing it from `task_edges`
- **v3-task-skill-blocks.AC3.3 Success:** Modifying a task's `blocks` field (add / remove edge) updates `task_edges` in the same transaction
- **v3-task-skill-blocks.AC3.4 Failure:** Intentionally failing a query mid-subscriber-reconcile (e.g., simulated db lock) rolls back the full transaction; neither `tasks` nor `task_edges` shows half-applied state
- **v3-task-skill-blocks.AC3.5 Failure:** If subscriber panics during reconcile, supervisor restarts worker + `metrics::counter!("memory.sync_worker.restart")` increments
- **v3-task-skill-blocks.AC3.6 Edge:** Subscriber reconcile is idempotent — running it twice with no loro state change produces no rows changed
- **v3-task-skill-blocks.AC3.7 Edge:** Concurrent edits by two agents (one adds edge A→B, other removes edge C→D on a different task) both apply cleanly; loro CRDT merges; subscriber reconciles to final state

---

## Design deviations recorded during planning

- **Migration numbering:** the design references `migrations/memory/0012_task_block_index.sql` assuming the sibling memory-rework plan splits migrations into subtrees. The current repo (pre-sibling-landing) is FLAT (`crates/pattern_db/migrations/` with 0001–0013 taken; 0012 is already used by `queued_message_full_content.sql`). The correct number at execution time is **whatever the next free slot in the sibling plan's final migration layout is**. Task 1 below re-confirms the layout at execution time and picks the right filename; the plan uses `0014_task_block_index.sql` as the fallback for a flat layout, or `memory/0013_task_block_index.sql` if the sibling lands a `memory/` subtree.
- **rusqlite vs sqlx:** the sibling memory-rework plan migrates pattern_db from sqlx 0.8 to rusqlite 0.39. Phase 2 depends on that migration having landed. Task 1 re-verifies. If not landed, Phase 2 STOPS.
- **Subscriber module path:** sibling plan does not yet publish the exact file path for the per-doc sync_worker. Task 1 re-verifies at execution time via re-reading the latest sibling implementation plan files. Fallback assumption if still unclear: `crates/pattern_memory/src/subscriber/mod.rs` with per-schema dispatch functions in `subscriber/task.rs`, `subscriber/skill.rs` etc.
- **`metrics` crate:** pre-flight audit (2026-04-23) found sibling memory-rework Phase 4 landed `metrics = "0.24"` crate-level in `pattern_memory/Cargo.toml` only (not workspace as originally planned). Pre-flight fix promoted it to `[workspace.dependencies]` in root `Cargo.toml` at version `0.24` so downstream crates (this phase's `pattern_db` work + future `pattern_server` observability) can take it via `{ workspace = true }`. Task 1 verification below reflects the post-promotion state. No version-pin drift expected — 0.24 is a minor bump from the originally-speced 0.23 and API is compatible.
- **FTS5 `tasks_fts` virtual table:** there is currently no FTS5 table for tasks. Phase 2 creates one in the same migration so AC5.3's keyword filter in Phase 3 has an index to hit.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-2, plus shared-types task 1b) -->
### Subcomponent A: Prerequisite verification + shared query types

Prerequisite + type-landing tasks. **Verifies: None** (setup) except where query types are exercised by later phases.

<!-- START_TASK_1 -->
### Task 1: Verify sibling-plan prerequisites

**Files:** none written.

**Step 1: Confirm rusqlite migration landed**

Run:
```
rg -n 'rusqlite' crates/pattern_db/Cargo.toml
rg -n 'sqlx' crates/pattern_db/Cargo.toml
```

Expected: rusqlite present; sqlx gone (or scoped to legacy modules only). If sqlx is still the primary driver, STOP — sibling memory-rework Phase 2 hasn't landed yet.

**Step 2: Confirm subscriber module layout**

Run: `fd -e rs subscriber crates/pattern_memory/src`
Expected: a subscriber submodule tree exists (likely `src/subscriber/mod.rs` + per-schema files). Record the exact paths into the scratch file `target/plan-phase2-subscriber-paths.txt`.

If missing: STOP — sibling memory-rework Phase 4 has not landed yet.

**Step 3: Confirm metrics crate available**

Run: `rg '^metrics' Cargo.toml crates/pattern_memory/Cargo.toml`
Expected:
- Root `Cargo.toml`: `metrics = "0.24"` under `[workspace.dependencies]` (promoted 2026-04-23 pre-flight).
- `crates/pattern_memory/Cargo.toml`: `metrics = { workspace = true }`.

If missing from workspace: STOP — the pre-flight promotion step in the v3-task-skill-blocks patch didn't land. Re-run the promotion before proceeding.

For this phase, use `metrics = { workspace = true }` in any crate Cargo.toml that needs to emit counters/gauges (currently only `pattern_memory`; this phase does NOT add metrics to `pattern_db`).

**Step 4: Confirm migration layout**

Run: `ls crates/pattern_db/migrations/`
Record the highest-numbered existing migration file and whether there is a `memory/` subtree. Choose the new filename accordingly:
- Flat + highest is 0013 → `0014_task_block_index.sql`.
- Sibling split into `memory/` subtree → use the sibling's next free `memory/XX_task_block_index.sql`.

Record the chosen path in `target/plan-phase2-migration-path.txt` — used by Task 3.

**Step 5: No commit.**
<!-- END_TASK_1 -->

<!-- START_TASK_1b -->
### Task 1b: Land shared query types in `pattern_core`

**Verifies:** None (these types support AC2.*, AC3.*, AC4.*, AC5.* down the line — individual tests cover them in later tasks).

**Rationale:** Phase 2's query layer (`list_tasks_filtered`, `query_task_graph_bfs` in Task 6/7) needs `TaskFilter`, `GraphQuery`, `GraphSlice`, `Direction`, and friends. Phase 3's SDK handlers also reference the same types. Landing them in Phase 2 keeps the query layer self-contained (no forward references to Phase 3). Phase 3 Task 2 will only add small handler-local types + helper methods on top. Additionally this task lands `BlockSchemaKind` + `SearchScope::Schema(BlockSchemaKind)` so Phase 5's skill-scoped search can filter cleanly instead of post-filtering.

**Files:**
- Create: `crates/pattern_core/src/types/memory_types/task_query.rs`
- Create: `crates/pattern_core/src/types/memory_types/block_schema_kind.rs`
- Modify: `crates/pattern_core/src/types/memory_types/mod.rs` (add `pub mod task_query; pub mod block_schema_kind; pub use ...`).
- Modify: the sibling-plan-introduced `SearchScope` enum location (discovered via Task 1: likely `pattern_core::types::memory_types::search` — grep `pub enum SearchScope` to confirm). Add `#[non_exhaustive]` if missing, plus a new `Schema(BlockSchemaKind)` variant.

**Implementation:**

In `block_schema_kind.rs`:
```rust
/// Discriminator variant of [`BlockSchema`], used for filtering without
/// carrying the variant's associated payload (e.g., `default_owner`,
/// `default_status`, `expected_keys`). Callers build filters against kind,
/// not the full schema value.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockSchemaKind {
    Text,
    Map,
    List,
    Log,
    Composite,
    TaskList,
    Skill,
}

impl From<&BlockSchema> for BlockSchemaKind {
    fn from(schema: &BlockSchema) -> Self {
        match schema {
            BlockSchema::Text => Self::Text,
            BlockSchema::Map { .. } => Self::Map,
            BlockSchema::List { .. } => Self::List,
            BlockSchema::Log { .. } => Self::Log,
            BlockSchema::Composite { .. } => Self::Composite,
            BlockSchema::TaskList { .. } => Self::TaskList,
            BlockSchema::Skill { .. } => Self::Skill,
        }
    }
}
```

In `task_query.rs`, define (all `#[derive(Clone, Debug, Serialize, Deserialize)]` unless stated):

- `TaskSpec { subject: String, description: String, active_form: Option<String>, status: Option<TaskStatus>, owner: Option<AgentId>, metadata: serde_json::Value }` — edges NOT set on creation.
- `TaskPatch { subject: Option<String>, description: Option<String>, active_form: Option<Option<String>>, status: Option<TaskStatus>, owner: Option<Option<AgentId>>, metadata: Option<serde_json::Value> }` — `Option<Option<T>>` pattern allows explicitly clearing a field. (See Phase 3 design-deviations for rationale.)
- `TaskFilter { status: Option<Vec<TaskStatus>>, owner: Option<AgentId>, has_blockers: Option<bool>, keyword: Option<String> }` with `#[derive(Default)]`.
- `TaskView { block_ref: BlockRef, subject: String, status: TaskStatus, owner: Option<AgentId>, blocker_count: usize, blocks_count: usize }` — projection for UI/agent consumption.
- `GraphQuery { direction: Direction, depth: Option<u32>, max_nodes: Option<u32> }` with sensible defaults (depth=16, max_nodes=1000 resolved at query time).
- `Direction { Forward, Reverse, Both }` — `#[non_exhaustive]`; serde kebab-case.
- `GraphSlice { nodes: Vec<BlockRef>, edges: Vec<(BlockRef, BlockRef)>, truncated: bool }`.

**`SearchScope` extension:**

After locating the enum (sibling-plan-introduced; Task 1 found its path), add:
```rust
#[non_exhaustive]
pub enum SearchScope {
    Agent(AgentId),       // existing
    Constellation,        // existing
    Schema(BlockSchemaKind),  // NEW — this plan
    // possible other existing variants
}
```

If the sibling enum is not `#[non_exhaustive]` yet, add that attribute in the same commit — consistent with the Phase 1 treatment of `BlockSchema`.

**Testing:**

Unit tests in each new module:
- Serde round-trip for each type.
- `TaskFilter::default()` returns all-None.
- `GraphQuery::default()` returns `{ direction: Forward, depth: None, max_nodes: None }`.
- `BlockSchemaKind::from(&BlockSchema::TaskList { .. })` returns `TaskList`.
- `SearchScope::Schema(BlockSchemaKind::Skill)` serializes and round-trips.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::task_query`
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::block_schema_kind`

**Commit:**
```
jj commit -m "[pattern-core] shared query types for TaskList + BlockSchemaKind + SearchScope::Schema"
```
<!-- END_TASK_1b -->

<!-- START_TASK_2 -->
### Task 2: Audit and remove `coordination_tasks` callers

**Verifies:** v3-task-skill-blocks.AC2.3.

**Files:**
- Potentially modify: `crates/pattern_db/src/queries/coordination.rs` (removal target), plus any callers.

**Step 1: Locate callers**

Run:
```
rg -n 'coordination_tasks|coordination::' crates/ --type rust
rg -n 'use pattern_db::queries::coordination' crates/ --type rust
```

**Step 2: Categorize each hit**

For each hit, record: file:line + whether the call is (a) in active code paths, (b) in disabled/legacy modules (e.g., `rewrite-staging/`), or (c) in tests.

**Step 3: Delete `queries/coordination.rs` and every active caller**

- Delete the file.
- For each active caller, either remove the call entirely (if the caller was delegation-specific work now handled via task blocks + `link`) OR replace the call with a `// REPLACED BY: pattern_db::queries::task` comment plus a TODO-level callsite rework. **No TODO comments may remain when the phase concludes** — if a caller can't be cleanly rewritten, block on human input before proceeding.

**Step 4: Verify compile**

Run: `cargo check --workspace`
Expected: no errors. If `rewrite-staging/` still imports coordination, that's acceptable (it's legacy-isolated per repo convention).

**Step 5: Commit**

```
jj commit -m "[pattern-db] remove coordination_tasks query surface (REPLACED BY: queries::task)"
```
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->
### Subcomponent B: Migration + schema

<!-- START_TASK_3 -->
### Task 3: Write migration `0014_task_block_index.sql` (or sibling subtree equivalent)

**Verifies:** v3-task-skill-blocks.AC2.1, AC2.3, AC2.4, AC2.6.

**Files:**
- Create: `crates/pattern_db/migrations/0014_task_block_index.sql` (or the subtree path recorded in Task 1).

**Implementation:**

Write the migration in this exact ordering (ordering matters for AC2.6):

```sql
-- Drop coordination_tasks indexes BEFORE the table they reference.
DROP INDEX IF EXISTS idx_tasks_status;
DROP INDEX IF EXISTS idx_tasks_assigned;

-- coordination_tasks: strict subset of the new tasks-as-index schema.
-- "Coordination" framing will be rebuilt on task blocks in Plan 3 (v3-subagents).
DROP TABLE IF EXISTS coordination_tasks;

-- Extend the existing tasks table with block-provenance + comments columns,
-- and align nomenclature with the Rust TaskItem.subject field.
ALTER TABLE tasks RENAME COLUMN title TO subject;
ALTER TABLE tasks ADD COLUMN block_handle TEXT;
ALTER TABLE tasks ADD COLUMN task_item_id TEXT;
ALTER TABLE tasks ADD COLUMN owner_agent_id TEXT;
ALTER TABLE tasks ADD COLUMN comments_json TEXT NOT NULL DEFAULT '[]';
CREATE INDEX idx_tasks_block ON tasks(block_handle, task_item_id);
CREATE INDEX idx_tasks_owner ON tasks(owner_agent_id, status);

-- Drop unused legacy column. SQLite 3.35+ supports DROP COLUMN.
-- `priority` has no indexes, foreign keys, or triggers per pre-migration audit.
ALTER TABLE tasks DROP COLUMN priority;

-- Single-direction edges table (derived from loro task `blocks` fields).
-- NOTE: we deliberately DO NOT use WITHOUT ROWID — SQLite requires an explicit
-- PRIMARY KEY on WITHOUT ROWID tables, and the natural key here (source_block +
-- source_item + target_block + target_item-with-NULL-collapse) can't be a
-- straight PRIMARY KEY because NULL is not equal to NULL under PK constraints.
-- The unique expression index `idx_task_edges_pk` below provides the dedup
-- guarantee. WITHOUT ROWID would give marginal storage savings not worth the
-- constraint-ergonomics cost.
CREATE TABLE task_edges (
    source_block TEXT NOT NULL,
    source_item  TEXT NOT NULL,
    target_block TEXT NOT NULL,
    target_item  TEXT
);
-- Unique expression index serves as the effective primary key, distinguishing
-- block-level targets (NULL -> '<block>' sentinel) from item-level targets.
-- '<block>' is not a valid snowflake/base32 id, so collision is impossible.
CREATE UNIQUE INDEX idx_task_edges_pk ON task_edges(
    source_block, source_item, target_block, COALESCE(target_item, '<block>')
);
CREATE INDEX idx_task_edges_source ON task_edges(source_block, source_item);
CREATE INDEX idx_task_edges_target ON task_edges(target_block, target_item);

-- FTS5 virtual table for keyword filtering (AC5.3 in Phase 3).
CREATE VIRTUAL TABLE tasks_fts USING fts5(
    subject,
    description,
    comments_json,
    content='tasks',
    content_rowid='rowid'
);
-- Triggers to keep tasks_fts in sync with tasks.
CREATE TRIGGER tasks_fts_insert AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts(rowid, subject, description, comments_json)
    VALUES (new.rowid, new.subject, new.description, new.comments_json);
END;
CREATE TRIGGER tasks_fts_delete AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, subject, description, comments_json)
    VALUES ('delete', old.rowid, old.subject, old.description, old.comments_json);
END;
CREATE TRIGGER tasks_fts_update AFTER UPDATE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, subject, description, comments_json)
    VALUES ('delete', old.rowid, old.subject, old.description, old.comments_json);
    INSERT INTO tasks_fts(rowid, subject, description, comments_json)
    VALUES (new.rowid, new.subject, new.description, new.comments_json);
END;
```

Notes:
- `tasks.title` is renamed to `tasks.subject` so the whole stack (Rust `TaskItem.subject`, SQL column, FTS5 column) agrees. The `tasks` table is currently unused in active code paths (Phase 2 Task 2 audit confirms this), so the rename is zero-cost in terms of callers; it's a deliberate compat break and any residual consumer migrates via the CAR-file agent-state export path.
- If rusqlite's SQLite bundle is < 3.35, `ALTER TABLE ... RENAME COLUMN` and `DROP COLUMN` both fail. Task 1's migration-layout check validated the bundled version is ≥ 3.35.

**Testing:**

Covered by Task 4's migration round-trip test. This task produces the SQL file only.

**Commit:**

```
jj commit -m "[pattern-db] add migration 0014 task_block_index (tasks + task_edges + tasks_fts)"
```
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Migration round-trip + rollback-safety tests

**Verifies:** v3-task-skill-blocks.AC2.1, AC2.2, AC2.5, AC2.6.

**Files:**
- Create: `crates/pattern_db/tests/migration_task_block_index.rs`

**Implementation:**

Test setup helpers:
- `fresh_db()` — opens an in-memory rusqlite connection and runs ALL migrations through 0014 (or equivalent).
- `pre_migration_db()` — opens a connection and runs migrations only through the previous number (0013 in a flat layout).

Tests:
- `migration_applies_to_empty_db`: call `fresh_db()`, assert `SELECT name FROM sqlite_master WHERE name IN ('tasks','task_edges','tasks_fts')` returns three rows.
- `migration_preserves_pre_existing_task_rows`: open `pre_migration_db()`, insert a row into `tasks` with the pre-migration shape (id, agent_id, title, description, status, priority=5, …), apply migration 0014, assert the row is still there with `priority` column gone, new columns `block_handle=NULL`, `task_item_id=NULL`, `owner_agent_id=NULL`, `comments_json='[]'`.
- `migration_drops_coordination_tasks`: insert a row into `coordination_tasks` at the pre-migration stage; apply 0014; assert `coordination_tasks` table no longer exists via `PRAGMA table_list`.
- `task_edges_unique_constraint_rejects_duplicates`: insert an edge row twice with identical `(source_block, source_item, target_block, target_item=NULL)` → second insert errors with a UNIQUE constraint violation. Repeat with `target_item="id-xyz"`.
- `task_edges_block_vs_item_distinct`: insert one edge with `target_item=NULL` and one with `target_item="anything-not-<block>"` to the same source — both succeed (AC2.7).
- `priority_drop_does_not_break_existing_queries`: after migration, `cargo check -p pattern-db` verifies this compile-time. Add a smoke test that runs every query function exported by `pattern_db::queries::task` (once Task 5 lands) against the freshly migrated schema.

**Verification:**

- Run: `cargo nextest run -p pattern-db --test migration_task_block_index`
- Expected: all tests pass.

**Commit:**

```
jj commit -m "[pattern-db] migration 0014 round-trip and constraint tests"
```
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-7) -->
### Subcomponent C: Query rewrite + types

<!-- START_TASK_5 -->
### Task 5: `FromSql`/`ToSql` for `TaskStatus` + `from_row` for `TaskRow`, `TaskEdgeRow`

**Verifies:** v3-task-skill-blocks.AC2.4 (row shape correctness — exercised indirectly by Task 6 + 7 tests).

**Files:**
- Create: `crates/pattern_db/src/queries/task_row.rs`
- Modify: `crates/pattern_db/src/queries/mod.rs` to add `pub mod task_row;` and re-export `TaskRow`, `TaskEdgeRow`.

**Implementation:**

- `struct TaskRow` mirroring the post-migration `tasks` table: `rowid`, `id`, `agent_id`, `subject`, `description`, `status: TaskStatus`, `due_at`, `scheduled_at`, `completed_at`, `parent_task_id`, `block_handle: Option<BlockHandle>`, `task_item_id: Option<TaskItemId>`, `owner_agent_id: Option<AgentId>`, `comments_json: String`, `created_at`, `updated_at`. (Field name `subject` matches the renamed sqlite column from Task 3 migration.)
- `struct TaskEdgeRow { source_block: BlockHandle, source_item: TaskItemId, target_block: BlockHandle, target_item: Option<TaskItemId> }`.
- `impl rusqlite::types::FromSql for TaskStatus` — parses kebab-case strings; returns `Err(FromSqlError::Other)` on unknown variants.
- `impl rusqlite::types::ToSql for TaskStatus` — emits kebab-case strings matching the serde representation from Phase 1.
- `impl TaskRow { pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> { … } }` — extracts every column via indexed `row.get`. Document the column order so the SELECT statements in Task 6 match.
- Same for `TaskEdgeRow`.

**Testing:**

Unit tests:
- `TaskStatus::Pending.to_sql()` produces `"pending"`; round-trip through `FromSql` returns the same variant.
- `TaskStatus` from `"unknown"` returns an error.

**Verification:**

- Run: `cargo nextest run -p pattern-db --lib queries::task_row`

**Commit:**

```
jj commit -m "[pattern-db] TaskRow/TaskEdgeRow row types with rusqlite conversions"
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Rewrite `queries/task.rs` for the index shape

**Verifies:** v3-task-skill-blocks.AC2.4 (shape), AC3.1 (`upsert_task_row`), AC3.2 (`delete_task_row`), AC3.3 (edge upsert).

**Files:**
- Modify: `crates/pattern_db/src/queries/task.rs` (replace contents).

**Implementation:**

Expose these sync functions (rusqlite is sync; callers wrap in `spawn_blocking`):

- `pub fn upsert_task_row(tx: &Transaction, row: &TaskRow) -> rusqlite::Result<()>` — uses `INSERT OR REPLACE` (or `ON CONFLICT` if the unique key is `(block_handle, task_item_id)`; see Step 2).
- `pub fn delete_task_row(tx: &Transaction, block: &BlockHandle, item: &TaskItemId) -> rusqlite::Result<usize>` — returns rows affected.
- `pub fn upsert_task_edges(tx: &Transaction, source_block: &BlockHandle, source_item: &TaskItemId, edges: &[BlockRef]) -> rusqlite::Result<()>` — idempotent: DELETE existing edges for `(source_block, source_item)` then INSERT the new set. Phase 2's reconcile prefers this wholesale replacement to a diff-based approach because it's simpler and the source-side edge count is bounded.
- `pub fn delete_task_edges_for_item(tx: &Transaction, block: &BlockHandle, item: &TaskItemId) -> rusqlite::Result<usize>` — deletes all rows where source matches.
- `pub fn delete_task_edges_targeting(tx: &Transaction, target_block: &BlockHandle, target_item: Option<&TaskItemId>) -> rusqlite::Result<usize>` — for cleanup when a target goes away.
- `pub fn list_tasks_filtered(conn: &Connection, filter: &TaskFilter) -> rusqlite::Result<Vec<TaskRow>>` — translates `TaskFilter` (defined in Phase 2 Task 1b) into a parameterized SELECT with optional FTS5 join when `keyword` is set.
- `pub fn query_task_graph_bfs(conn: &Connection, root: &BlockRef, direction: Direction, depth: u32, max_nodes: u32) -> rusqlite::Result<GraphSlice>` — BFS walker with visited-set. `Direction` + `GraphSlice` defined in Phase 2 Task 1b.

**Step 2: Unique key on tasks**

Post-migration, the natural index key is `(block_handle, task_item_id)`. The migration in Task 3 created `idx_tasks_block` over this pair but didn't declare it UNIQUE (preserving the table's existing primary key shape). Verify by running a test: two INSERTs with the same `(block_handle, task_item_id)` should *currently* both succeed, and `upsert_task_row` uses `ON CONFLICT (block_handle, task_item_id)` only if the index is unique. If not, the implementation uses an explicit "delete-then-insert" inside the transaction (acceptable because reconcile always runs inside a tx). Choose based on actual index state; document the choice in the code.

**Testing:**

Tests in `crates/pattern_db/tests/queries_task.rs`:
- Upsert one row, list all, assert count is 1.
- Upsert twice with same `(block, item)` — result is one row (either via UNIQUE constraint or delete-then-insert path).
- Delete row, list is empty.
- Insert 3 edges for the same source, delete one, assert 2 remain.
- `delete_task_edges_for_item` wipes all matching edges.
- `list_tasks_filtered` with status filter, owner filter, keyword filter (FTS5) each produce expected subsets over a 10-row fixture.
- **insta snapshot test for FTS5 relevance ordering:** fixture of 5 diverse task subjects + descriptions (e.g., "fix login timeout", "update auth docs", "review migration safety", "refactor token rotation", "audit password hashing"); run keyword queries like `"auth"`, `"review"`, `"timeout"`; snapshot the returned `(task_item_id, score)` pairs in relevance order. Guarantees stable BM25 output across runs, satisfying the phase's done-when.

**Verification:**

- Run: `cargo nextest run -p pattern-db --test queries_task`
- Accept new snapshots the first time: `INSTA_UPDATE=auto cargo nextest run -p pattern-db --test queries_task`

**Commit:**

```
jj commit -m "[pattern-db] rewrite queries/task for TaskList block index + FTS5 snapshot"
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `query_task_graph_bfs` implementation

**Verifies:** Phase 3's AC5.4/AC5.6b/AC5.7/AC5.8 rely on this function. Tests land here, but the AC list is anchored to Phase 3's SDK coverage. Phase 2 verifies the primitive is correct in isolation.

**Files:**
- Modify: `crates/pattern_db/src/queries/task.rs` (add function).
- Create: `crates/pattern_db/tests/queries_task_graph.rs`

**Implementation:**

Walker outline:
1. Initialize `visited: HashSet<BlockRef>` with `root`, `frontier: VecDeque<(BlockRef, u32 /*depth*/)>` with `(root, 0)`, `nodes: Vec<BlockRef>` with `root`, `edges: Vec<(BlockRef, BlockRef)>`, `truncated = false`.
2. Pop `(current, d)` from frontier. If `d >= max_depth`, skip neighbours. Otherwise, SELECT neighbours:
   - `Direction::Forward` — `SELECT target_block, target_item FROM task_edges WHERE source_block = ? AND source_item = ?` (only item-level sources have outgoing edges).
   - `Direction::Reverse` — `SELECT source_block, source_item FROM task_edges WHERE target_block = ? AND (target_item IS ? OR (target_item IS NULL AND ? IS NULL))` (parameter twice because NULL in SQLite doesn't equate).
   - `Direction::Both` — union of the two.
3. For each neighbour, if not in `visited`, add edge to `edges`, push neighbour to `nodes`. If `nodes.len() >= max_nodes`, set `truncated = true` and stop enqueueing further.
4. Return once the frontier is empty or `truncated` is set.

Use indexed lookups — the migration already created `idx_task_edges_source` and `idx_task_edges_target`.

**Testing:**

Fixture construction helper: builds a configurable N-node graph with given edges.
- `depth=0` returns root only, zero edges.
- 5-node chain `A→B→C→D→E`, `Direction::Forward`, `depth=Unlimited` returns 5 nodes + 4 edges.
- Same chain, `depth=2`, returns 3 nodes + 2 edges.
- Cycle `A→B→C→A`, `Direction::Forward`, `depth=10` terminates and returns 3 nodes + 3 edges (visited-set prevents infinite walk). Covers AC5.7.
- 10k-node graph with long chain + branching: `max_nodes=1000` truncates; `truncated == true`; walker completes in under one second measured via `Instant::now()`. Covers AC5.8.
- `Direction::Reverse` on a block-level target (`target_item = NULL`) returns sources correctly.

**Verification:**

- Run: `cargo nextest run -p pattern-db --test queries_task_graph`

**Commit:**

```
jj commit -m "[pattern-db] query_task_graph_bfs with depth + max_nodes caps"
```
<!-- END_TASK_7 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 8-10) -->
### Subcomponent D: Subscriber reconciliation

<!-- START_TASK_8 -->
### Task 8: Extend subscriber dispatch to reconcile TaskList blocks

**Verifies:** v3-task-skill-blocks.AC3.1, AC3.2, AC3.3, AC3.6.

**Files:**
- Create (or modify, per Task 1 findings): `crates/pattern_memory/src/subscriber/task.rs` — new per-schema handler module.
- Modify: `crates/pattern_memory/src/subscriber/mod.rs` — register the new dispatch arm matching `BlockSchema::TaskList { .. }`.

**Implementation:**

`reconcile_task_list(tx: &Transaction, block_handle: &BlockHandle, doc: &LoroDoc)`:
1. Read the root LoroMap's `items` LoroMovableList. Collect each item's `(id, fields…)` into a `Vec<TaskItem>`.
2. Fetch existing rows: `SELECT task_item_id FROM tasks WHERE block_handle = ?`. Build a `HashSet<TaskItemId>` of existing ids.
3. Diff:
   - For each item in loro state, call `upsert_task_row(tx, &TaskRow::from_loro(item, block_handle))`.
   - For each existing id NOT in loro state, call `delete_task_row(tx, block_handle, &id)` and `delete_task_edges_for_item(tx, block_handle, &id)`.
4. For each item in loro state, call `upsert_task_edges(tx, block_handle, &item.id, &item.blocks)` (wholesale replace — already idempotent).

Idempotency (AC3.6): running the function twice on unchanged loro state performs the same INSERT OR REPLACE / DELETE-INSERT operations, which produce the same final rows with zero net change. Verify by running the reconcile twice and using `SELECT changes()` (rusqlite `conn.changes()`) — second call reports 0 changes *in terms of row count deltas*, though individual INSERT-or-REPLACE statements do re-write the same data. If stricter "zero writes" idempotency is desired, gate upsert/delete-edges by content comparison — record this as a follow-up optimization; AC3.6 cares about logical idempotency, not zero-rewrites.

Cross-dispatch: the subscriber `mod.rs` matches on block schema:
```rust
match block.schema {
    BlockSchema::TaskList { .. } => task::reconcile_task_list(tx, handle, doc)?,
    // existing arms for Text/Map/List/Composite/Log remain.
    BlockSchema::Skill { .. } => {} // placeholder; Phase 4 Task 10 replaces this with the skill reconcile path (FTS5 indexing).
    _ => {} // non-exhaustive catch-all.
}
```

**Testing:**

Integration tests in `crates/pattern_memory/tests/subscriber_task_list.rs`:
- Write a TaskList with 5 items + 3 edges → 5 `tasks` rows + 3 `task_edges` rows (AC3.1 — source-only model; each edge stored once on the source task, reverse direction queried not stored).
- Delete one item from loro; re-run subscriber → row + edges gone (AC3.2).
- Add edge to one item's `blocks` list; re-run subscriber → new `task_edges` row (AC3.3).
- Remove edge; re-run → row gone.
- Run subscriber twice with no loro changes → final row set identical (AC3.6).

**Verification:**

- Run: `cargo nextest run -p pattern-memory --test subscriber_task_list`

**Commit:**

```
jj commit -m "[pattern-memory] subscriber: reconcile tasks + task_edges for TaskList blocks"
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Transaction atomicity + supervisor restart + metrics

**Verifies:** v3-task-skill-blocks.AC3.4, AC3.5.

**Files:**
- Modify: `crates/pattern_memory/src/subscriber/task.rs` — wrap reconcile in the existing worker's `rusqlite::Transaction` scope (the sibling plan's subscriber loop already opens a tx per commit event; this task confirms our code uses it correctly).
- Modify: `crates/pattern_memory/src/subscriber/mod.rs` — confirm panic→restart path exists from sibling Phase 4. If it exists, this task only adds a metrics counter increment. If it doesn't, coordinate with sibling plan before proceeding.

**Implementation:**

- On Ok: tx commits; no metrics change.
- On Err(e) mid-reconcile: propagate via `?`; sibling's worker logic rolls back the tx and logs. Add one line at the error-handling site:
  ```rust
  metrics::counter!("memory.sync_worker.reconcile_error", "schema" => "task-list").increment(1);
  ```
- On panic: sibling's supervisor restarts the worker. Add:
  ```rust
  metrics::counter!("memory.sync_worker.restart").increment(1);
  ```
  inside the supervisor's restart branch if not already present.

**Testing:**

- `atomicity_rolls_back_partial_reconcile`: seed the test db's `task_edges` table with a temporary CHECK constraint that fails on a specific sentinel `source_item` value (e.g., `CHECK (source_item != '__panic_sentinel__')`). Construct a TaskList commit where the first task's `blocks` references that sentinel. Run reconcile: the `upsert_task_row` for the innocent first task succeeds in-transaction, then `upsert_task_edges` hits the CHECK violation and errors. Assert both `tasks` and `task_edges` tables show the PREVIOUS state — the innocent row insertion was rolled back with the failing edge insertion. Drop the CHECK constraint after the assertion. Deterministic; no lock/timing fragility.
- `subscriber_panic_restarts_worker`: inject a panic into the reconcile path (e.g., via a schema with a deliberately-malformed LoroMap key that the mapper panics on). Assert the supervisor restarts and the metric counter `memory.sync_worker.restart` increments. This test uses the `metrics-util::debugging` recorder for assertion.

**Verification:**

- Run: `cargo nextest run -p pattern-memory --test subscriber_task_list atomicity`
- Run: `cargo nextest run -p pattern-memory --test subscriber_task_list panic_restart`

**Commit:**

```
jj commit -m "[pattern-memory] atomic reconcile + sync_worker.restart metric"
```
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: Concurrent-edit and scope-enforcement coverage

**Verifies:** v3-task-skill-blocks.AC3.7, plus the Phase 2 piece of scope enforcement (TaskList blocks respect `MemoryScope::isolate_from_persona` like any block).

**Files:**
- Create: `crates/pattern_memory/tests/subscriber_task_list_concurrent.rs`

**Implementation:**

- `concurrent_edits_merge_cleanly`: simulate two agents by opening two LoroDoc instances seeded from the same snapshot. Agent A adds `A.blocks += C` in its doc; Agent B removes an edge from a different task in its doc. Merge both change sets into a third doc. Run the subscriber on the merged doc. Assert the final `task_edges` table reflects both agents' changes.
- `scope_enforcement_project_only`: create a mount, spin up two sessions — one with persona scope, one with `MemoryScope::CoreOnly` / `isolate_from_persona=true`. Write a TaskList block at project scope. Confirm the project-scope session sees the tasks via `list_tasks_filtered`, and the persona session does not. (Scope routing is the sibling plan's concern; this test just exercises it end-to-end for TaskList.)

**Verification:**

- Run: `cargo nextest run -p pattern-memory --test subscriber_task_list_concurrent`

**Commit:**

```
jj commit -m "[pattern-memory] concurrent-merge + scope-enforcement tests for TaskList"
```
<!-- END_TASK_10 -->
<!-- END_SUBCOMPONENT_D -->

---

## Phase 2 Done when

- Task 1 prerequisite check passes (rusqlite in place, subscriber module exists, metrics crate available, migration layout known).
- `cargo check --workspace` passes.
- `cargo nextest run -p pattern-db --lib --tests` passes including new migration + queries tests.
- `cargo nextest run -p pattern-memory --lib --tests` passes including subscriber + concurrent-edit tests.
- `coordination_tasks` table and `queries/coordination.rs` removed; no references remain in active code.
- `metrics::counter!` sites emit `memory.sync_worker.restart` on supervisor restart and `memory.sync_worker.reconcile_error` on reconcile failure.
- FTS5 snapshot test on representative task fixtures (Phase 2 adds at least one insta snapshot test covering 3-5 diverse task subjects/descriptions) produces stable BM25 ordering.
- No `TODO`, `unimplemented!()`, or commented-out code introduced.
