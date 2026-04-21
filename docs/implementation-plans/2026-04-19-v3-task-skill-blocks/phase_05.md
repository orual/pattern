# v3-task-skill-blocks Phase 5: `ctx.skills.*` SDK surface + end-to-end smoke

**Goal:** Expose the agent-facing `Pattern.Skills` effect algebra (`list`, `get_metadata`, `load`, `search`) and prove the full Plan 2 surface with a deterministic end-to-end integration smoke test exercising both `ctx.tasks.*` and `ctx.skills.*` against a scripted mock provider.

**Architecture:** `list` and `search` reuse the existing `MemoryStore::search` + `list_blocks(BlockFilter)` surface filtered to `BlockSchema::Skill`. `get_metadata` fetches the block and projects its LoroDoc into `SkillMetadata`. `load` fetches the body, injects a `[skill:loaded] … [skill:loaded:end]` pseudo-message into segment 2 of the current turn via the existing `render_change_event`-style mechanism from sibling Phase 4, and records a sqlite row via `pattern_db::queries::skill_usage::record_usage`. Usage stats sit in the sqlite table introduced in Phase 4 Task 9 — never in the LoroDoc. The smoke test lives at `crates/pattern_memory/tests/task_skill_smoke.rs` and uses `MockProviderClient` + an in-memory sqlite + `tempfile::TempDir` mount to assert the composed request contains the expected skills/tasks state deterministically.

**Tech Stack:** Rust (pattern_runtime, pattern_memory, pattern_db, pattern_provider), Haskell SDK, `insta` snapshots for composed-request assertions, `cargo nextest`.

**Scope:** Phase 5 of 5.

**Codebase verified:** 2026-04-19.

---

## Acceptance Criteria Coverage

### v3-task-skill-blocks.AC8: `ctx.skills.*` SDK surface methods

- **v3-task-skill-blocks.AC8.1 Success:** `list()` enumerates all Skill-schema blocks visible in current scope; returns `SkillInfo` with correct handles + metadata summary
- **v3-task-skill-blocks.AC8.2 Success:** `get_metadata(handle)` on a Skill block returns typed `SkillMetadata` with hook fields preserved as `serde_json::Value`
- **v3-task-skill-blocks.AC8.3 Success:** `get_metadata(handle)` on a non-Skill block returns `None`
- **v3-task-skill-blocks.AC8.4 Success:** `search(query)` returns matching `SkillInfo` via FTS5 over skill name + description + keywords + body; relevance-ranked
- **v3-task-skill-blocks.AC8.5 Failure:** `load(handle)` on a non-existent block returns `MemoryError::BlockNotFound`
- **v3-task-skill-blocks.AC8.6 Failure:** `load(handle)` on a non-Skill block returns `SkillError::NotASkill(handle)`

### v3-task-skill-blocks.AC9: Skill `load` behavior + metadata updates

- **v3-task-skill-blocks.AC9.1 Success:** `load(handle)` injects a `[skill:loaded]` marker + skill body + `[skill:loaded:end]` marker as a pseudo-message in segment 2 of the current turn's composed model request (snapshot-tested)
- **v3-task-skill-blocks.AC9.2 Success:** Loaded skill persists in segment 2 for subsequent turns; segment 1 cache is not invalidated by load
- **v3-task-skill-blocks.AC9.3 Success:** `load` updates the Skill block's usage stats (last_used, last_used_by, use_count++) in the `skill_usage_stats` sqlite table ONLY; the canonical `.md` file is NOT re-emitted and stays content-hash-stable across loads. Verify by computing file hash before and after load calls — hash unchanged.
- **v3-task-skill-blocks.AC9.3b Success:** Post-load, `get_metadata` returns typed `SkillMetadata` without any usage-stat fields; `get_usage_stats(handle)` returns fresh `SkillUsageStats` from the sqlite table
- **v3-task-skill-blocks.AC9.4 Success:** Multiple loads of different skills in the same turn produce multiple [skill:loaded] markers; order preserved
- **v3-task-skill-blocks.AC9.5 Edge:** Loading the same skill twice in the same turn produces two [skill:loaded] markers (no dedup in v1, by design)
- **v3-task-skill-blocks.AC9.6 Edge:** Skill usage stats update does NOT cause VCS dirtiness — `git status` (or `jj status`) in a Mode A mount shows no pending changes after 100 skill loads

### v3-task-skill-blocks.AC10: End-to-end smoke + scope enforcement

- **v3-task-skill-blocks.AC10.1 Success:** Smoke test at `crates/pattern_memory/tests/task_skill_smoke.rs` passes deterministically in CI
- **v3-task-skill-blocks.AC10.2 Success:** Mock ProviderClient in the smoke test produces deterministic output
- **v3-task-skill-blocks.AC10.3 Success:** Scope enforcement smoke — TaskList + Skill blocks in project scope with `Full` isolation are invisible to persona-default sessions
- **v3-task-skill-blocks.AC10.4 Failure:** Any step in the smoke flow failing produces a clear error identifying which step and which assertion
- **v3-task-skill-blocks.AC10.5 Edge:** Smoke test runs concurrently with other `pattern-memory` integration tests without shared-state interference
- **v3-task-skill-blocks.AC10.6 Success (Plan 1 interop):** External `.kdl` edit reconciliation — test externally edits a TaskList `.kdl` file, notify-watcher fires, loro CRDT merge imports the change, subscriber emits the canonical file again, `tasks` + `task_edges` index rows reflect the added item
- **v3-task-skill-blocks.AC10.7 Success (Plan 1 interop):** Quiesce + commit cycle — call `quiesce()` on the mount, `memory.db` reaches canonical state (WAL truncated), host VCS commit produces a clean commit; task index state preserved across restart-from-checkpoint
- **v3-task-skill-blocks.AC10.8 Success (cross-block-type search):** FTS5 search spanning Text, TaskList, and Skill blocks returns results from all three with correct BM25 scoring

---

## Design deviations recorded during planning

- **Pseudo-message injection:** uses the existing `render_change_event`-style mechanism at `crates/pattern_provider/src/compose/pseudo_messages.rs` (verified by Phase 5 investigation). Add a parallel `render_skill_loaded_event(name, trust_tier, body) -> PseudoMessage` function that produces the canonical `[skill:loaded] name="…" trust_tier="…"\n\n<body>\n\n[skill:loaded:end]` shape. Segment 2 composition (`crates/pattern_provider/src/compose/passes/segment_2.rs`) picks up the pseudo-message like any other.
- **`load` mechanism, concretely:** the handler emits the pseudo-message via the existing segment-2 append path and writes the sqlite row. No LoroDoc mutation. No canonical-file write. The skill's `.md` hash is content-stable across any number of load calls.
- **`get_usage_stats` as a new SDK method:** the design's AC9.3b requires `get_usage_stats(handle)`. This plan adds it as a fifth method in `Pattern.Skills` (not four as the design's summary implied) — distinct from `get_metadata` because `SkillMetadata` and `SkillUsageStats` are separate types. Total `Pattern.Skills` method count: five.
- **`SkillError` error enum:** introduce `SkillError::NotASkill(BlockHandle)`. Wrap MemoryError where appropriate so `load` can return either a `BlockNotFound` or a `NotASkill` with a clear variant for the Haskell side.
- **Quiesce dependency:** sibling memory-rework Phase 5 ships `quiesce()`. Task 1 re-verifies it's available before the AC10.7 integration test lands.
- **Concurrent tests:** every smoke test uses its own `tempfile::TempDir` mount + fresh in-memory sqlite — no shared fixture files, no `test_db()` singleton.
- **`SearchScope::Schema(BlockSchemaKind)`** landed in Phase 2 Task 1b of this plan (not a sibling contribution). Phase 5 uses it directly; no coordination ask on sibling.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
### Subcomponent A: Prerequisites + shared types

<!-- START_TASK_1 -->
### Task 1: Verify Phase 4 + sibling prerequisites

**Files:** none written.

**Step 1:** Run:
```
rg -n 'render_change_event|pseudo_messages' crates/pattern_provider/src/compose
rg -n 'pub fn quiesce|pub async fn quiesce' crates/pattern_memory/src
rg -n 'skill_usage_stats' crates/pattern_db/migrations
rg -n 'BlockSchema::Skill' crates/pattern_core/src crates/pattern_memory/src
```
Expected: all four match. The `render_change_event` / `pseudo_messages.rs` module exists (sibling Phase 4). `quiesce` is exposed (sibling Phase 5). Phase 4 Task 9's migration landed. `BlockSchema::Skill` exists per Phase 4 Task 4.

If any check fails: STOP and report the gap.

**Step 2:** Read `crates/pattern_runtime/src/testing.rs` for `MockProviderClient::with_turns` shape, and record the seed pattern for integration tests.

**Step 3:** No commit.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `SkillInfo` + `SkillError` shared types

**Verifies:** AC8.1/AC8.2/AC8.3/AC8.5/AC8.6 support.

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/skill.rs` (add `SkillInfo`).
- Modify: the MemoryError (or nearby) to add `SkillError` alongside.

**Implementation:**

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillInfo {
    pub handle: BlockHandle,
    pub name: String,
    pub description: Option<String>,
    pub trust_tier: SkillTrustTier,
    pub keywords: Vec<String>,
    pub last_used: Option<Timestamp>,
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("block `{0}` is not a Skill block")]
    NotASkill(BlockHandle),

    #[error("skill metadata for `{0}` could not be read from LoroDoc")]
    MalformedMetadata(BlockHandle),
}
```

`SkillInfo.last_used` is populated via sqlite join at list/search time (Task 4 / 6). Not derived from metadata.

**Testing:**

- Serde round-trip for `SkillInfo`.
- `SkillError::NotASkill(...)` Display includes the handle.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::skill`

**Commit:**
```
jj commit -m "[pattern-core] SkillInfo + SkillError types"
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Skeleton of Rust request + handler modules

**Files:**
- Create: `crates/pattern_runtime/src/sdk/requests/skills.rs`.
- Create: `crates/pattern_runtime/src/sdk/handlers/skills.rs`.
- Modify: `crates/pattern_runtime/src/sdk/requests.rs` (add `pub mod skills;`).
- Modify: `crates/pattern_runtime/src/sdk/handlers.rs` (add `pub mod skills;`).

**Implementation:**

```rust
// requests/skills.rs
use pattern_macros::FromCore;

#[derive(Debug, FromCore)]
pub enum SkillsReq {
    // variants added per-method in later tasks
}
```
```rust
// handlers/skills.rs
pub struct SkillsHandler;
// Handler impl filled in by Tasks 4-7.
```

**Verification:**
- Run: `cargo check --workspace`.

**Commit:**
```
jj commit -m "[pattern-runtime] scaffolding for ctx.skills SDK surface"
```
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-7) -->
### Subcomponent B: `Pattern.Skills` implementation

<!-- START_TASK_4 -->
### Task 4: Haskell `Pattern.Skills` + Rust `SkillsReq` variants

**Verifies:** AC8.* entry points.

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Skills.hs`.
- Modify: cabal/package file to register the module.
- Modify: `crates/pattern_runtime/src/sdk/requests/skills.rs` — fill all five variants.

**Implementation:**

Haskell GADT (mirroring `Pattern.Memory` / `Pattern.Tasks`):
```haskell
module Pattern.Skills where
-- imports ...

data Skills a where
  List            :: Skills Text                 -- returns [SkillInfo]-as-json
  GetMetadata     :: BlockHandle -> Skills Text  -- returns Maybe SkillMetadata-as-json
  Load            :: BlockHandle -> Skills ()
  Search          :: Text        -> Skills Text  -- query string -> [SkillInfo]-as-json
  GetUsageStats   :: BlockHandle -> Skills Text  -- returns SkillUsageStats-as-json
```

Rust request:
```rust
#[derive(Debug, FromCore)]
pub enum SkillsReq {
    #[core(module = "Pattern.Skills", name = "List")]
    List,
    #[core(module = "Pattern.Skills", name = "GetMetadata")]
    GetMetadata(String),
    #[core(module = "Pattern.Skills", name = "Load")]
    Load(String),
    #[core(module = "Pattern.Skills", name = "Search")]
    Search(String),
    #[core(module = "Pattern.Skills", name = "GetUsageStats")]
    GetUsageStats(String),
}
```

Convenience wrappers on the Haskell side (`listSkills :: Eff r [SkillInfo]`, etc.) for agent ergonomics.

**Verification:**
- Build Haskell side clean.
- Run: `cargo check --workspace`.

**Commit:**
```
jj commit -m "[pattern-runtime] Pattern.Skills GADT + SkillsReq variants"
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `list` + `get_metadata` + `get_usage_stats` handlers

**Verifies:** AC8.1, AC8.2, AC8.3, AC9.3b.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/skills.rs`.

**Implementation:**

- `handle_list(cx)`:
  - Resolve scope via `scope::resolve_scope` (sync).
  - `store.list_blocks(BlockFilter { schema: Some(BlockSchemaKind::Skill), scope_agents: Some(resolved), .. })`.
  - For each returned block metadata, fetch its LoroDoc via `store.get_block(agent, label)`, project into `SkillMetadata`, pluck `name`, `description`, `trust_tier`, `keywords`.
  - Batch-fetch usage stats via `pattern_db::queries::skill_usage::get_usage_stats_batch(&conn, &handles)`. Merge `last_used` into each SkillInfo.
  - Return `Vec<SkillInfo>` as JSON.

- `handle_get_metadata(cx, handle)`:
  - Fetch block; if schema != Skill, return `None` as JSON (per AC8.3 — `Option<SkillMetadata>`, not an error).
  - Project LoroDoc → `SkillMetadata`. Return as JSON.

- `handle_get_usage_stats(cx, handle)`:
  - Resolve scope; scope-check the handle belongs to a visible agent.
  - `pattern_db::queries::skill_usage::get_usage_stats(&conn, &handle)`. Returns `SkillUsageStats::default()` when no row exists (first-time query).

**Testing:**

- `list_enumerates_skill_blocks`: seed 3 skills + 2 text blocks; list returns 3 `SkillInfo`.
- `list_populates_last_used_from_sqlite`: seed 2 skills, call `record_usage` on one, list returns one with `Some(timestamp)` and one with `None`.
- `get_metadata_returns_typed_frontmatter`: seed a skill with nested `hooks`; `get_metadata` returns `SkillMetadata` where `hooks` is the same JSON value.
- `get_metadata_on_text_block_returns_none`: fetch a Text block, assert `None`.
- `get_usage_stats_default_for_new_skill`: no loads yet, returns `SkillUsageStats::default()`.
- `get_usage_stats_after_three_loads`: `record_usage` 3 times, handler returns `use_count == 3`.

**Verification:**
- Run: `cargo nextest run -p pattern-runtime --lib handlers::skills`

**Commit:**
```
jj commit -m "[pattern-runtime] skills handlers: list, get_metadata, get_usage_stats"
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: `search` handler + `render_skill_loaded_event`

**Verifies:** AC8.4.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/skills.rs`.
- Modify: `crates/pattern_provider/src/compose/pseudo_messages.rs` — add `render_skill_loaded_event`.

**Implementation:**

- `render_skill_loaded_event(name: &str, trust_tier: SkillTrustTier, body: &str) -> PseudoMessage`:
  produces a `PseudoMessage` wrapping:
  ```
  [skill:loaded] name="<name>" trust_tier="<kebab>"

  <body>

  [skill:loaded:end]
  ```
  Mirror the exact type shape used by `render_change_event` (same `PseudoMessage` return type, same origin-tagging convention — `MessageOrigin::SkillLoaded { handle }`). If no such origin variant exists yet, extend `MessageOrigin` with a new `#[non_exhaustive]`-friendly variant. Task 1's verification should have surfaced the current origin enum.

- `handle_search(cx, query)`:
  - Scope-resolve.
  - `store.search(query, SearchOptions::default(), SearchScope::Schema(BlockSchemaKind::Skill))` — both `BlockSchemaKind` and the `Schema` variant on `SearchScope` landed in **Phase 2 Task 1b**. No post-filter pass needed.
  - Project results to `SkillInfo` with batch usage-stat join (same path as `list`).
  - Return JSON.

**Testing:**

- `search_matches_skill_name`: seed 3 skills with different names; query matches only one.
- `search_matches_skill_description`: query hits description text.
- `search_matches_skill_body`: query hits body markdown.
- `search_relevance_ranked`: two skills match; BM25 score orders them correctly; snapshot via insta.
- `render_skill_loaded_event_snapshot`: known skill → known marker text (insta).

**Verification:**
- Run: `cargo nextest run -p pattern-runtime --lib handlers::skills search`
- Run: `cargo nextest run -p pattern-provider --lib compose::pseudo_messages::render_skill_loaded_event`

**Commit:**
```
jj commit -m "[pattern-runtime] [pattern-provider] skills.search + render_skill_loaded_event"
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `load` handler — segment 2 injection + sqlite stat write

**Verifies:** AC8.5, AC8.6, AC9.1, AC9.2, AC9.3, AC9.4, AC9.5, AC9.6.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/skills.rs`.

**Implementation:**

`handle_load(cx, handle)`:
1. Fetch block via `store.get_block`. If not found, return `MemoryError::BlockNotFound` (AC8.5).
2. Inspect schema. If not Skill, return `SkillError::NotASkill(handle)` (AC8.6).
3. Project the LoroDoc into `SkillMetadata` + body string.
4. Build pseudo-message via `render_skill_loaded_event(metadata.name, metadata.trust_tier, &body)`.
5. Append to the current turn's segment-2 composition via the existing helper used by sibling Phase 4 (investigate via Task 1; likely `cx.session().push_pseudo_message(msg)` or equivalent).
6. Write the sqlite stat row:
   ```rust
   let now = jiff::Timestamp::now();
   let agent = cx.user().agent_id().to_owned();
   pattern_db::with_transaction(&conn, |tx| {
       pattern_db::queries::skill_usage::record_usage(tx, &handle, &agent, now)
   })?;
   ```
7. Return Unit. Do NOT touch the LoroDoc. Do NOT write the canonical `.md`.

**Testing:**

- `load_missing_block_returns_block_not_found`: handle with no block → `BlockNotFound` (AC8.5).
- `load_text_block_returns_not_a_skill`: handle on Text block → `SkillError::NotASkill` (AC8.6).
- `load_injects_pseudo_message_segment_2`: load a seeded skill, snapshot the composed model request's segment-2 messages via insta, confirm `[skill:loaded]` + body + `[skill:loaded:end]` appear in order (AC9.1).
- `load_persists_in_history_across_turns`: multiple turn advances, segment-2 still shows the marker (AC9.2).
- `load_updates_use_count`: load 5 times, query `get_usage_stats`, assert `use_count == 5` (AC9.3).
- `load_does_not_modify_canonical_file`: capture the `.md` file's blake3 hash, load 100 times, assert hash unchanged (AC9.3).
- `load_two_skills_preserves_order`: load A then B, segment-2 pseudo-messages appear in A-then-B order (AC9.4).
- `load_same_skill_twice_emits_two_markers`: load A twice, two markers present (AC9.5 — documented non-dedup).
- `load_does_not_dirty_mount` (Mode-A integration test): in a mode-A jj-tracked mount, load skill 100 times, `jj status` shows no modifications (AC9.6).

**Verification:**
- Run: `cargo nextest run -p pattern-runtime --lib handlers::skills load`
- Run: `cargo nextest run -p pattern-memory --test skills_load_mode_a`

**Commit:**
```
jj commit -m "[pattern-runtime] skills.load: segment-2 injection + sqlite stat write"
```
<!-- END_TASK_7 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 8-9) -->
### Subcomponent C: Bundle integration + end-to-end smoke

<!-- START_TASK_8 -->
### Task 8: Register `SkillsHandler` in `SdkBundle`

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` — insert `SkillsHandler` in the HList.
- Modify: `crates/pattern_runtime/src/sdk/describe.rs` — register describe decls.

**Implementation:**

Follow the same pattern used by Phase 3 Task 10 for `TasksHandler`. Target ordering in the HList: `Memory, Search, Recall, Tasks, Skills, Message, ...` (Skills adjacent to Tasks per investigator's Phase 3 recommendation).

**Testing:**

- Describe-effects snapshot shows `Pattern.Skills` with all five methods.

**Verification:**
- Run: `cargo nextest run -p pattern-runtime`

**Commit:**
```
jj commit -m "[pattern-runtime] register Pattern.Skills in SdkBundle"
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: End-to-end smoke test (`task_skill_smoke.rs`)

**Verifies:** AC10.1, AC10.2, AC10.3, AC10.4, AC10.5, AC10.8.

**Files:**
- Create: `crates/pattern_memory/tests/task_skill_smoke.rs`.

**Test layout:**

Single file, four `#[test]` functions — each with its own `TempDir` (isolation) and scripted mock provider turn sequence. Splitting reduces debugging friction when a subsystem regresses; AC10.1 "smoke test passes deterministically in CI" remains file-level.

Shared fixture helper (private to the test file):
```rust
fn new_mount_with_seeds(dir: &TempDir) -> (Mount, BlockHandle /*task-list*/, BlockHandle /*skill*/, BlockHandle /*text*/) { ... }
```

Seeds:
- TaskList block with 5 tasks + 3 cross-block edges.
- Skill block with structured frontmatter including nested `hooks`.
- Text block with some content (cross-block-type FTS coverage).

Tests:

1. **`smoke_tasks_surface`**: script `ctx.tasks.create_task` → `update_task` → `transition_status` → `link` → `list_tasks` → `query_graph`. Assert each result (new id present in list, link reflected in graph, status transition visible, graph returns expected nodes + edges).

2. **`smoke_skills_surface`**: script `ctx.skills.list` → `get_metadata` → `search` → `load`. Assert list includes seeded skill with correct trust_tier, metadata round-trips through the SDK boundary, search returns the skill, `load` injects the expected pseudo-message into segment 2 (insta snapshot on the composed request). Canonical `.md` hash unchanged before/after load.

3. **`smoke_cross_schema_fts`**: seed all three block types, run a single search query that matches at least one of each type, assert all three appear in results, insta snapshot the BM25 ordering.

4. **`smoke_scope_enforcement`**: mount with `MemoryScope::Full` isolation on a project-scoped TaskList + Skill; persona-default session tries `list_tasks(None)` and `ctx.skills.list()` — both return empty for the persona session, populated for the project session (AC10.3).

Each test uses human-readable assertion messages identifying which step failed (AC10.4).

**Concurrency isolation (AC10.5):**

Each test function creates its own `TempDir`. No shared static state. Use `#[cfg(test)]` fixture helpers that take a `TempDir` reference.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --test task_skill_smoke`
- Run (concurrency check): `cargo nextest run -p pattern-memory --test-threads=8` — asserts no shared-state flakiness.

**Commit:**
```
jj commit -m "[pattern-memory] end-to-end smoke: tasks + skills full surface"
```
<!-- END_TASK_9 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 10-12) -->
### Subcomponent D: Plan 1 interop tests

<!-- START_TASK_10 -->
### Task 10: External `.kdl` edit reconciliation (AC10.6)

**Verifies:** AC10.6.

**Files:**
- Create: `crates/pattern_memory/tests/external_kdl_edit_reconcile.rs`.

**Test scenario:**

1. Initialize a mount with a seeded TaskList block.
2. Call `quiesce()` to flush state to canonical file.
3. Externally edit `<mount>/blocks/.../task-list.kdl` — add a new `item` node via text-level append.
4. Wait for the notify-watcher debounce (500ms per sibling plan) to fire.
5. Wait for subscriber reconcile (50ms debounce per sibling plan).
6. Query `tasks` + `task_edges` — assert the added item appears with its new id indexed.
7. Read the `.kdl` file — confirm it was NOT re-emitted with a different shape (echo suppression verified: file content matches what the test wrote externally).

**Verification:**
- Run: `cargo nextest run -p pattern-memory --test external_kdl_edit_reconcile`

**Commit:**
```
jj commit -m "[pattern-memory] test external .kdl edit reconciliation via notify watcher"
```
<!-- END_TASK_10 -->

<!-- START_TASK_11 -->
### Task 11: Quiesce + commit cycle (AC10.7)

**Verifies:** AC10.7.

**Files:**
- Create: `crates/pattern_memory/tests/quiesce_commit_cycle.rs`.

**Test scenario:**

1. Initialize a mount (with jj adapter) in Mode A.
2. Seed TaskList + Skill + Text blocks; load the skill once to populate `skill_usage_stats`.
3. Call `quiesce()` on the mount.
4. Assert:
   - `memory.db`'s WAL is truncated (check the `-wal` file size is 0 or absent — rusqlite `wal_checkpoint(TRUNCATE)` sibling contract).
   - All canonical files written and fsynced (file hashes match the LoroDoc projections).
5. Perform a `jj commit` against the mount. Assert the commit contains `.kdl`, `.md`, and `memory.db` but NOT any `-wal`/`-shm` files.
6. Drop the mount. Re-open from the same path. Assert the `tasks` + `task_edges` indexes still report the same state (index preserved across restart — AC10.7's "preserved across restart-from-checkpoint").

**Verification:**
- Run: `cargo nextest run -p pattern-memory --test quiesce_commit_cycle`

**Commit:**
```
jj commit -m "[pattern-memory] test quiesce + commit cycle preserves task index"
```
<!-- END_TASK_11 -->

<!-- START_TASK_12 -->
### Task 12: Cross-block-type FTS coverage (AC10.8)

**Verifies:** AC10.8.

**Files:**
- Create: `crates/pattern_memory/tests/cross_schema_fts.rs` (or add to existing smoke test if the file is already packed).

**Test scenario:**

1. Seed one Text block mentioning "hydration", one TaskList with a task subject about "hydration", one Skill with keyword "hydration" in frontmatter.
2. Call `MemoryStore::search("hydration", SearchOptions::default(), SearchScope::Default)`.
3. Assert all three block types appear in the result set.
4. Assert BM25 relevance scoring is stable (insta snapshot).
5. Confirm no block schema is silently excluded by filtering logic.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --test cross_schema_fts`

**Commit:**
```
jj commit -m "[pattern-memory] test FTS5 spans text, task-list, and skill blocks"
```
<!-- END_TASK_12 -->
<!-- END_SUBCOMPONENT_D -->

---

## Phase 5 Done when

- Task 1 prerequisites pass (pseudo_messages module, quiesce, skill_usage_stats migration, BlockSchema::Skill).
- `cargo check --workspace` passes.
- `cargo nextest run -p pattern-runtime --lib --tests` passes (all handler + smoke tests).
- `cargo nextest run -p pattern-memory --tests` passes (all integration tests including smoke, external-edit reconcile, quiesce-commit cycle, cross-schema FTS).
- Haskell `Pattern.Skills` module compiles.
- Describe-effects snapshot enumerates all five Skills methods alongside the eight Tasks methods.
- insta snapshot for `render_skill_loaded_event` is committed.
- No `TODO`, `unimplemented!()`, or commented-out code introduced.
- Final `ctx.tasks.*` + `ctx.skills.*` surfaces exercised end-to-end through a mock provider run that asserts the composed request state.
