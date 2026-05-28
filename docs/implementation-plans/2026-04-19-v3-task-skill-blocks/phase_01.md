# v3-task-skill-blocks Phase 1: TaskList schema + KDL serialization

**Goal:** Land the `BlockSchema::TaskList` variant and supporting types, and extend the LoroValue↔KdlDocument converter so TaskList blocks round-trip losslessly through canonical `.kdl` files.

**Architecture:** New `TaskList` variant on the existing `BlockSchema` enum holds task-list-level policy (default owner/status, display cap); per-item `TaskItem` records live in a `LoroMovableList` under each TaskList block's LoroDoc and carry status, owner, typed `TaskEdgeRef` edges, metadata, and inline comments. KDL serialization extends the Phase-4-sibling `loro_value_to_kdl` converter with a `task-list` dispatch that emits/parses `item { ... }` children and typed `(block)"..."` entries for `TaskEdgeRef`.

**Tech Stack:** Rust (pattern_core, pattern_memory), `loro` (LoroMovableList + LoroMap), `kdl` v2, `smol_str`, `ferroid` (base32 Mastodon-style Snowflake IDs via workspace `new_snowflake_id()`), `jiff`, `proptest`, `cargo nextest`.

**Scope:** Phase 1 of 5 (v3-task-skill-blocks design plan).

**Codebase verified:** 2026-04-19.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-task-skill-blocks.AC1: TaskList schema + KDL round-trip

- **v3-task-skill-blocks.AC1.1 Success:** `BlockSchema::TaskList { default_owner, default_status, display_limit }` exists and is exported from `pattern_core::types::memory_types`
- **v3-task-skill-blocks.AC1.2 Success:** `TaskItem`, `TaskStatus`, `TaskComment`, `TaskEdgeRef`, `TaskItemId` types exist with documented fields
- **v3-task-skill-blocks.AC1.3 Success:** Property test (proptest) confirms round-trip equivalence: generate arbitrary TaskList with nested items + edges + comments → serialize to KDL → parse back → LoroValue matches original
- **v3-task-skill-blocks.AC1.4 Success:** TaskEdgeRef parses both `(block)"<handle>"` and `(block)"<handle>#<item_id>"` forms
- **v3-task-skill-blocks.AC1.5 Failure:** Malformed TaskEdgeRef annotation (e.g., `(block)""` or missing typed annotation) produces `KdlConversionError` with file:line reference
- **v3-task-skill-blocks.AC1.6 Edge:** Empty TaskList (zero items) round-trips cleanly; self-referential edge (`A.blocks = [A]`) round-trips cleanly
- **v3-task-skill-blocks.AC1.7 Edge:** Item reordering via `LoroMovableList` preserves item ids across round-trip
- **v3-task-skill-blocks.AC1.8 Success:** `new_snowflake_id()` produces a non-empty base32-encoded Mastodon-style Snowflake string usable directly as a `TaskItemId` (which is a `SmolStr` alias per house convention)
- **v3-task-skill-blocks.AC1.9 Success:** 32 concurrent threads calling `new_snowflake_id()` produce 32 distinct ids (ferroid `AtomicSnowflakeGenerator` collision-resistant by construction)

---

## Design deviations recorded during planning

- **Snowflake, not UUID v7:** the design plan text says "UUID v7 as base32 string" but the workspace's time-ordered ID infrastructure is ferroid-backed `SnowflakeMastodonId` (exported as `pattern_core::types::ids::new_snowflake_id() -> SmolStr`, base32-encoded, lexicographically sortable). This is the existing house convention. Call sites mint a `TaskItemId` by calling `new_snowflake_id()` directly. The design's "UUID v7" wording is treated as imprecise — no new UUID generator is introduced. AC1.9 is satisfied by ferroid's `AtomicSnowflakeGenerator<_, MonotonicClock>` (verified in `crates/pattern_core/src/utils.rs`).

- **`TaskItemId` is a `SmolStr` type alias, not a newtype** (locked 2026-04-23 during Task 4 execution). House convention: every ID alias in `pattern_core::types::ids` is a `pub type FooId = SmolStr;` with no validation or newtype ceremony — documented in `ids.rs:3-10` ("Type aliases preserve naming for signature clarity without newtype ceremony; there is no compile-time distinction between kinds. When a distinct type is genuinely useful (rare, e.g. validation-bearing atproto identifiers), wrap explicitly at the site that needs it."). `TaskItemId` has no validation-bearing behaviour that the site-level wire parsers don't already provide. Empty-string rejection happens at `TaskEdgeRef::from_str` (which catches the `"handle#"` and `"#id"` failure paths), not at the id alias itself. Removes ~220 lines of newtype ceremony that the original plan specified (manual Serialize/Deserialize impls, `TaskItemIdError`, `parse` / `new` / `as_str` / `FromStr` / `Display` impls, 13 tests) — all were re-implementing `SmolStr`'s native behaviour.

- **`TaskEdgeRef` rename** (locked 2026-04-23 during Task 5 execution). The original plan named the task-graph edge type `BlockRef`, but `pattern_core::types::block_ref::BlockRef` already exists for a different purpose (context-loading references: `{ label, block_id, agent_id }`). That type is load-bearing across ~6 consumer files in `pattern_core` and `pattern_runtime`. Renaming the new task-graph type avoids the collision. `TaskEdgeRef` is explicit about its role (task dependency graph) and the KDL wire format (`(block)"handle"` typed annotation) is unchanged — only the Rust type name differs.
- **Sibling-plan prerequisites (must land before Phase 1 executes):**
  - `v3-memory-rework` Phase 1 relocates `BlockSchema` to `pattern_core::types::memory_types::schema` and applies `#[non_exhaustive]`.
  - `v3-memory-rework` Phase 4 creates `crates/pattern_memory/src/fs/kdl.rs` exposing `loro_value_to_kdl(&LoroValue) -> Result<KdlDocument, KdlConversionError>` and its inverse, plus the `KdlConversionError` error type. This plan extends that module; it does NOT create it.

---

## Implementation phases

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
### Subcomponent A: Prerequisites & workspace wiring

Infrastructure tasks. **Verifies: None** — these are setup steps.

<!-- START_TASK_1 -->
### Task 1: Verify sibling prerequisites

**Files:**
- Read: `crates/pattern_core/src/types/memory_types/schema.rs` (or whichever path the sibling Phase 1 landed `BlockSchema` at)
- Read: `crates/pattern_memory/src/fs/kdl.rs`

**Step 1: Confirm `BlockSchema` location and attributes**

Run:
```
rg -n '^pub enum BlockSchema' crates/pattern_core/src
rg -n '#\[non_exhaustive\]' crates/pattern_core/src/types/memory_types
```

Expected: `BlockSchema` enum is at `pattern_core::types::memory_types::schema` (re-exported from `pattern_core::types::memory_types`). The `#[non_exhaustive]` attribute is NOT expected yet — sibling Phase 1 is a pure attribute-preserving move and will not add it. Task 7 below adds `#[non_exhaustive]` together with the `TaskList` variant.

If the relocation has not landed: STOP. The sibling `v3-memory-rework` Phase 1 has not landed or diverged from its plan. Report the discrepancy to the human and block on it.

**Step 2: Confirm KDL converter exists**

Run:
```
rg -n 'pub fn loro_value_to_kdl' crates/pattern_memory/src/fs/kdl.rs
rg -n 'KdlConversionError' crates/pattern_memory/src
```

Expected: both `loro_value_to_kdl` and an inverse (`kdl_to_loro_value` or equivalent) exist in `crates/pattern_memory/src/fs/kdl.rs`, plus a `KdlConversionError` error type with variants for span-bearing errors.

If missing: STOP. Sibling Phase 4 has not landed. Block.

**Step 3: Confirm `BlockHandle` construction API**

Run:
```
rg -n 'impl FromStr for BlockHandle|impl From<.*> for BlockHandle|pub fn new.*BlockHandle' crates/pattern_core/src
```

Expected: either a `FromStr` / `From<&str>` impl OR a `BlockHandle::new(s: impl Into<SmolStr>)` constructor exists. Record the signature — Task 5's `TaskEdgeRef::from_str` uses it.

If neither form is available, the implementor must add `impl From<&str> for BlockHandle` in `crates/pattern_core/src/types/block.rs` as a sub-step before Task 5. Stays in scope for Phase 1.

**Step 4: Record observed entry-point signatures in a scratch note**

`target/plan-phase1-prereqs.txt` captures: `BlockSchema` module path, KDL converter function names, `BlockHandle` constructor shape. Used by Tasks 5, 7, 9. This task produces no source changes; it gates the rest of Phase 1. Do not commit.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Confirm `ferroid` + `new_snowflake_id` are exportable to `pattern_memory`

**Files:** none written; this task gates TaskItemId's dependency choice.

**Step 1: Verify the generator is in scope from pattern_memory**

Run:
```
rg -n 'pub use .*new_snowflake_id|pub fn new_snowflake_id' crates/pattern_core/src
rg -n 'ferroid' crates/pattern_memory/Cargo.toml 2>/dev/null
```

Expected: `new_snowflake_id` is exported from `pattern_core::types::ids` (re-exported at `pattern_core::new_snowflake_id` per existing `lib.rs`). `pattern_memory` may or may not need to depend on it directly — `TaskItemId` lives in `pattern_core`, so only `pattern_core` needs the generator.

**Step 2: Record** — no change, no commit. If export is missing, add it (e.g., ensure `pub use types::ids::new_snowflake_id;` in `pattern_core/src/lib.rs`). In the current repo state it's already exported.
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Re-enumerate all `BlockSchema` match sites

**Files:** none written; this task produces a checklist used by Task 8.

**Step 1: Grep for match sites**

Run:
```
rg -n 'match .*BlockSchema|match schema' crates/ --type rust
rg -n 'BlockSchema::' crates/ --type rust | rg -v 'BlockSchema::Text|BlockSchema::Map|BlockSchema::List|BlockSchema::Log|BlockSchema::Composite' | rg 'match|=>'
```

**Step 2: Record sites**

Record each site as: `crate/path:line function_name — what the arm returns`. Minimum expected sites (per Phase 1B investigation, 2026-04-19, current repo state prior to sibling Phase 1 relocation):
- `crates/pattern_core/src/memory/schema.rs` — 4 helper methods (`is_field_read_only`, `read_only_fields`, `is_section_read_only`, `get_section_schema`). **Post-sibling-relocation these move to `pattern_core/src/types/memory_types/schema.rs` — use the current location.**
- `crates/pattern_cli/src/commands/debug.rs` — 1 debug formatter.

Save the list to a scratch file: `target/plan-phase1-blockschema-sites.txt`. This is a local working note, not committed.

**Step 3: No commit** (no source changes).
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->
### Subcomponent B: Core task types

Functionality tasks. Introduces `TaskItemId`, `TaskStatus`, `TaskComment`, `TaskEdgeRef`, `TaskItem`.

<!-- START_TASK_4 -->
### Task 4: `TaskItemId` type alias + Snowflake generator tests

**Verifies:** v3-task-skill-blocks.AC1.8, v3-task-skill-blocks.AC1.9.

**Scope-correction note (2026-04-23):** The original plan specified `TaskItemId` as a newtype wrapping `SmolStr` with a dedicated module, error enum, `new` / `parse` / `as_str` / `FromStr` / `Display` impls, manual serde, and 13 tests. That violated the house convention documented in `crates/pattern_core/src/types/ids.rs:3-10`: every identifier is a `pub type FooId = SmolStr;` alias unless validation is genuinely needed, and none of the newtype ceremony was load-bearing. Empty-string rejection at the id level is redundant — it's already enforced at the wire boundary that sees external data (`TaskEdgeRef::from_str` in Task 5). Task re-scoped to a one-line alias addition + two tests on `new_snowflake_id()`.

**Files:**
- Modify: `crates/pattern_core/src/types/ids.rs` — add `pub type TaskItemId = SmolStr;` next to the other id aliases, with a brief doc comment noting it's minted via `new_snowflake_id()`.
- Modify: `crates/pattern_core/src/types/memory_types.rs` — re-export `pub use crate::types::ids::TaskItemId;` so downstream `use pattern_core::types::memory_types::TaskItemId;` continues to resolve.

**Implementation:**

```rust
// In crates/pattern_core/src/types/ids.rs, alongside the other aliases:

/// A task item identifier — unique within its parent TaskList block.
///
/// Minted via [`new_snowflake_id`] for lexicographic time-ordering;
/// any non-empty string is also acceptable (used in test fixtures and
/// in agent-supplied references via wire formats like `TaskEdgeRef`).
/// Empty-string validation lives at the wire boundaries that see
/// external data (see `TaskEdgeRef::from_str`), not on this alias.
pub type TaskItemId = SmolStr;
```

No `new()`, no `parse()`, no error enum, no manual serde. Callers mint ids by calling `new_snowflake_id()` directly.

**Testing:**

Add to the existing `#[cfg(test)] mod tests` block in `ids.rs`:
- `new_snowflake_id_is_non_empty`: `assert!(!new_snowflake_id().is_empty())`. Verifies AC1.8.
- `new_snowflake_id_is_collision_resistant_concurrently`: spawn 32 threads that each call `new_snowflake_id()`, collect results into `HashSet<SmolStr>`, assert length is 32. Verifies AC1.9.

Downstream integration coverage: Task 5's `TaskEdgeRef::from_str` tests exercise empty-handle and empty-item-id rejection, covering the wire-level validation AC1.5 references.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::ids`
- Expected: 2 new tests pass alongside the existing `new_id_*` tests.

**Commit:**
```
jj commit -m "[pattern-core] add TaskItemId alias + snowflake generator tests"
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `TaskStatus`, `TaskComment`, `TaskEdgeRef`, `TaskItem` types

**Verifies:** v3-task-skill-blocks.AC1.2.

**Files:**
- Create: `crates/pattern_core/src/types/memory_types/task.rs`
- Modify: `crates/pattern_core/src/types/memory_types/mod.rs` to add `mod task;` and `pub use task::*;`.

**Implementation:**

1. `TaskStatus` enum — `#[non_exhaustive]`, `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]`, serialized as kebab-case strings (`"pending"`, `"in-progress"`, `"blocked"`, `"completed"`, `"cancelled"`). Use `serde(rename_all = "kebab-case")`.

2. `TaskComment { author: AgentId, timestamp: Timestamp, text: String }` — standard derives plus `Serialize`/`Deserialize`. `Timestamp` is `jiff::Timestamp`.

3. `TaskEdgeRef { block: BlockHandle, task_item: Option<TaskItemId> }` with:
   - `Display` impl that emits `"<handle>"` when `task_item` is `None`, `"<handle>#<item_id>"` otherwise.
   - `FromStr` impl that parses both forms; empty handle or empty item-id chunk returns `TaskEdgeRefParseError`. Use `#[non_exhaustive]` on the error enum; variants at minimum `EmptyHandle`, `EmptyItemId`.
   - Serde: derive struct-form serde so JSON representation is `{"block": "...", "task_item": null or "..."}`. KDL encoding is handled separately in Task 9 — serde and KDL are distinct surfaces.

4. `TaskItem` struct — fields exactly as the design specifies:
   - `pub id: TaskItemId`
   - `pub subject: String` (imperative form)
   - `pub description: String` (markdown body)
   - `pub active_form: Option<String>`
   - `pub status: TaskStatus`
   - `pub owner: Option<AgentId>`
   - `pub blocks: Vec<TaskEdgeRef>` — outgoing edges only (see design's "Single-source-of-truth edge model"); there is no `blocked_by` field.
   - `pub metadata: serde_json::Value` (freeform JSON).
   - `pub comments: Vec<TaskComment>` (append-mostly; no dedup).
   - `pub created_at: Timestamp`
   - `pub updated_at: Timestamp`
   Document in the rustdoc that `blocks` is the *only* edge storage and that reverse lookups happen via the `task_edges` index (Phase 2).

**Testing:**

- Unit tests in `task.rs` confirm kebab-case status serialization round-trip for every variant.
- `TaskEdgeRef::from_str("handle")` yields `TaskEdgeRef { block: ..., task_item: None }`.
- `TaskEdgeRef::from_str("handle#id")` yields the item form.
- `TaskEdgeRef::from_str("")`, `"#id"`, `"handle#"` each return `Err`.
- `TaskEdgeRef::to_string().parse()` round-trips for both forms.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::task`
- Expected: all task-type tests pass.

**Commit:**
```
jj commit -m "[pattern-core] add TaskItem, TaskStatus, TaskComment, TaskEdgeRef types"
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Extended serde + parse tests for task types

**Verifies:** v3-task-skill-blocks.AC1.2 (round-trip coverage), v3-task-skill-blocks.AC1.8 (combined with Task 4 empty-id path).

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/task.rs` (add tests module) or create `tests/task_types.rs` if the inline test module is already heavy — implementor's call.

**Implementation:**

Add tests that cover the cross-type contract:
- `TaskItem` JSON round-trip via `serde_json` — construct an instance with all fields populated including a TaskEdgeRef vector containing both block-level and item-level refs, encode, decode, assert equality.
- `TaskItem` with empty `blocks` and empty `comments` vectors round-trips cleanly.
- `TaskItem` with a self-edge (an item whose `blocks` contains a `TaskEdgeRef` pointing at its own `TaskItemId` inside its own block) round-trips cleanly. This anchors AC1.6.
- `TaskComment` with multiline text and UTF-8 (emoji, combining marks) round-trips.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::task`
- Expected: all round-trip tests pass.

**Commit:**
```
jj commit -m "[pattern-core] cover TaskItem serde round-trip incl. self-edge"
```
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 7-8) -->
### Subcomponent C: `BlockSchema::TaskList` variant

<!-- START_TASK_7 -->
### Task 7: Add `BlockSchema::TaskList` variant

**Verifies:** v3-task-skill-blocks.AC1.1.

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/schema.rs` (post-sibling-Phase-1 location; fall back to `crates/pattern_core/src/memory/schema.rs` only if Task 1 confirms sibling relocation has not landed — in which case STOP per Task 1's block rule).

**Implementation:**

Append to the `BlockSchema` enum:
```rust
TaskList {
    default_owner: Option<AgentId>,
    default_status: Option<TaskStatus>,
    display_limit: Option<usize>,
},
```

Use the same serde pattern as the existing variants (internally-tagged or adjacent — mirror what Map/List/Composite do). Import `TaskStatus` and `AgentId` appropriately.

**Add `#[non_exhaustive]` to the enum as part of this commit.** The sibling Phase 1 pure-move preserved attributes as-is (i.e., without the attribute), and no sibling phase adds it. This plan is the appropriate place: we're already extending the enum, and non-exhaustive future-proofs every downstream match site for the `Skill` variant in Phase 4 plus any further additions. Add `#[non_exhaustive]` immediately above `pub enum BlockSchema { ... }`. This will compile-break any external match site missing a `_ =>` catch-all — Task 8 then fixes each site.

**Testing:**

Minimal unit test: construct `BlockSchema::TaskList { default_owner: None, default_status: Some(TaskStatus::Pending), display_limit: Some(20) }`, round-trip through `serde_json`, assert equality.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib schema`
- Expected: schema test passes.

**Commit:**
```
jj commit -m "[pattern-core] add BlockSchema::TaskList variant + #[non_exhaustive]"
```
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Wire `BlockSchema::TaskList` into `pattern_core::memory::document` dispatch sites

**Verifies:** v3-task-skill-blocks.AC1.1 (indirectly — the new variant is wired into all pattern_core dispatch sites).

**Scope-correction note (2026-04-23):** The original plan targeted schema helper methods and `pattern_cli/src/commands/debug.rs`. Task 3 at execution found: (a) the schema helper methods all use `_ =>` catch-alls and require no TaskList arm under `#[non_exhaustive]`; (b) `pattern_cli/src/commands/debug.rs` does not exist — no pattern_cli source file references `BlockSchema`. The *real* set of 6 exhaustive match sites is recorded in `target/plan-phase1-blockschema-sites.txt`. This task now covers the 4 `pattern_core/src/memory/document.rs` sites. The remaining 2 sites (`pattern_memory/src/subscriber/worker.rs` and `pattern_memory/src/cache.rs`) depend on `TopShape::TaskList` and are handled as part of Task 9 in the same implementor dispatch (see Task 9 Files list).

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs` — 4 exhaustive match sites.

**Implementation:**

All four arms target `BlockSchema::TaskList { default_owner, default_status, display_limit }` (using `{ .. }` where fields aren't read).

1. **`export_for_editing` inner schema-name match (~line 759):** trivial string label.
```rust
BlockSchema::TaskList { .. } => "TaskList",
```

2. **`import_from_json` (~line 791):** TaskList expects `{"items": [...]}` JSON shape. Each array element is a TaskItem JSON object matching the serde shape from Task 5 (`id`, `subject`, `description`, `active_form`, `status`, `owner`, `blocks`, `metadata`, `comments`, `created_at`, `updated_at`). Implementation mirrors the existing `BlockSchema::List { .. }` arm, with `LoroMovableList` instead of `LoroList` and TaskItem shape validation per element.

Sketch (implementor may adapt to match the conventions of the surrounding code):
```rust
BlockSchema::TaskList { .. } => {
    let items = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
        items.clone()
    } else {
        return Err(DocumentError::Other(
            "TaskList schema expects array or object with 'items' field".to_string(),
        ));
    };
    // Clear the existing movable list, then re-insert each item as a LoroMap.
    // Mirror the existing List arm's pattern but target the "items" movable
    // list (not "items" list). Look for the equivalent helper/constructor the
    // existing arms use to convert a serde_json::Value into LoroValue. If no
    // public helper exists in pattern_core, add one — but NOT `json_to_loro`
    // from pattern_memory (that crosses crate boundaries the wrong way).
    // ...
}
```

**Scope guardrail:** if this arm needs a generic `json_to_loro` helper that doesn't already exist in `pattern_core`, either (a) extract one from pattern_memory and re-home it in pattern_core, or (b) do the JSON→LoroValue walk inline within this match arm using serde_json match patterns. Do NOT stub. Do NOT add a TaskList `import_from_json` that just returns `Err(...)` — the external-edit path routes KDL→JSON→`import_from_json` for all schemas, so TaskList needs real import.

3. **`subscribe_content` (~line 943):** subscribe to the movable list named "items".
```rust
BlockSchema::TaskList { .. } => self.doc.get_movable_list("items").id(),
```

4. **`render_schema` (~line 1007):** rich per-item rendering for LLM context. Respect the TaskList's `display_limit` field — slice the items list to at most `display_limit` elements before rendering, and emit a truncation indicator when sliced. For each item, include: `id`, `subject`, `status`, `owner` (if present), `active_form` (if present), `blocks` (if non-empty, as `(block)"handle"` or `(block)"handle#item_id"` strings), and a brief `description` excerpt (first line or first ~80 chars).

Sketch:
```rust
BlockSchema::TaskList { display_limit, default_status, default_owner } => {
    let items_list = self.doc.get_movable_list("items");
    let total = items_list.len();
    let shown = display_limit.map(|lim| lim.min(total)).unwrap_or(total);
    let mut out = String::new();
    out.push_str(&format!("TaskList ({total} items"));
    if shown < total { out.push_str(&format!(", showing {shown}")); }
    if let Some(s) = default_status { out.push_str(&format!("; default_status={s:?}")); }
    if let Some(o) = default_owner { out.push_str(&format!("; default_owner=@{o}")); }
    out.push_str(")\n");
    // Iterate the first `shown` items, extract fields from each LoroMap,
    // and format one line per item (multi-line if description has content).
    // Example per-item format:
    //   - id=01H2Z7 subject="write spec" status=in-progress owner=@r active_form="writing spec"
    //     blocks: (block)"alpha#01H2Z5", (block)"beta"
    //     description: Draft the initial architecture...
    // ...
    if shown < total {
        out.push_str(&format!("\n... {} more items not shown (display_limit={})\n",
            total - shown, display_limit.unwrap()));
    }
    out
}
```

Implementor's call on exact line format; match the tone/density of other `render_schema` arms. **Rich enough to be useful, bounded enough to not eat context.**

**Testing:**

Inline unit tests in `document.rs` (or in the test module that exercises the other `render_schema` arms):
- `export_for_editing` with a TaskList schema yields the "TaskList" schema name header.
- `import_from_json` accepts `{"items": [TaskItem, ...]}` and populates the movable list.
- `import_from_json` rejects malformed JSON (non-array `items`, wrong schema shape) with `DocumentError`.
- `subscribe_content` returns a container id whose type-tag is MovableList (not List, not Map).
- `render_schema` respects `display_limit` and emits the truncation indicator when items exceed it.
- `render_schema` for an empty TaskList emits `"TaskList (0 items)"` with no item lines.

**Verification:**
- Run: `cargo check --workspace` — compiles without warnings.
- Run: `cargo nextest run -p pattern-core --lib memory::document`
- Expected: all new TaskList-dispatch tests pass; existing tests unchanged.

**Commit:**
```
jj commit -m "[pattern-core] wire BlockSchema::TaskList into document.rs dispatch"
```
<!-- END_TASK_8 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 9-11) -->
### Subcomponent D: KDL converter extension for TaskList

<!-- START_TASK_9 -->
### Task 9: Extend KDL converter with `task-list` dispatch + wire `pattern_memory` consumers

**Verifies:** v3-task-skill-blocks.AC1.4 (TaskEdgeRef parse, both forms), v3-task-skill-blocks.AC1.6 (empty TaskList + self-edge canonical form). Also completes the AC1.1 wiring started in Task 8 (the 2 `pattern_memory` exhaustive match sites).

**Scope note (2026-04-23):** Extends the original plan to include the 2 `pattern_memory` exhaustive match sites (`worker.rs:45` and `cache.rs:791` inner closure plus `cache.rs:1280` catch-all turned into explicit arm) that depend on the new `TopShape::TaskList` variant. Landing the variant and its consumers in one atomic commit avoids the stub pattern (the guidance explicitly forbids stubs — any intermediate "TaskList handled with a todo!()" commit would violate it).

**Files:**
- Modify: `crates/pattern_memory/src/fs/kdl.rs` — extend `TopShape` enum with a `TaskList` variant; extend `loro_value_to_kdl` + `kdl_to_loro_value` match arms to delegate to the new module; extend `KdlConversionError` enum with `TaskEdgeRef { span, source }` and `MissingBlockAnnotation { span }` variants.
- Create: `crates/pattern_memory/src/fs/kdl_task_list.rs` — new sibling module exposing `pub(super) fn task_list_to_kdl(value: &LoroValue) -> Result<KdlDocument, KdlConversionError>` and `pub(super) fn kdl_to_task_list(doc: &KdlDocument) -> Result<LoroValue, KdlConversionError>`. Task-list-specific encoding/decoding lives here (item nodes, typed `(block)` annotations, metadata/comments).
- Modify: `crates/pattern_memory/src/fs/mod.rs` — add `mod kdl_task_list;` (or wherever `mod kdl;` is declared).
- Modify: `crates/pattern_memory/src/subscriber/worker.rs` (~line 45) — add `BlockSchema::TaskList { .. } => { ... }` arm in `render_canonical_from_disk_doc`. The body extracts the `items` movable list from `disk_doc.get_deep_value()`, wraps it in a `LoroValue::Map` with `{"schema": "task-list", "items": List(...), ...}` discriminator shape, calls `loro_value_to_kdl(&value, TopShape::TaskList)`, and returns `("kdl", bytes)`.
- Modify: `crates/pattern_memory/src/cache.rs`:
  - (~line 791) Inner `apply_external_edit` closure — add `BlockSchema::TaskList { .. } => { ... }` arm that decodes UTF-8, calls `parse_kdl`, then `kdl_to_loro_value(&doc, TopShape::TaskList)`, converts to JSON via `loro_value_to_json`, and applies via `apply_json_to_loro_doc(&disk_doc, &json, &schema)`.
  - (~line 1280) `apply_json_to_loro_doc` currently catch-alls to `_ => Err(...)`. Add an explicit `BlockSchema::TaskList { .. } => { ... }` arm that reads the JSON `items` array and populates `disk_doc.get_movable_list("items")` — mirrors Task 8's `import_from_json` TaskList arm, targeting the disk_doc directly instead of the memory_doc. Factor out a shared helper if the duplication becomes significant (inside `pattern_memory` since cache.rs lives there; pattern_core's `import_from_json` can stay self-contained).

**Architecture decision (locked 2026-04-23):** The task-list dispatch becomes a first-class `TopShape::TaskList` variant on the schema-directed hint enum, consistent with how `Map`/`List` work today. The body delegates to `kdl_task_list.rs` to keep `kdl.rs` focused on generic Map/List/Composite handling — the task-list body is too large to inline cleanly. Future KDL-shaped schemas follow this same pattern: new `TopShape::X` variant + new `kdl_x.rs` module. This preserves the "caller consults BlockSchema, tells us the shape" convention (see `kdl.rs:11-13` module docs).

**Implementation:**

Forward (`LoroValue → KdlDocument`):
- The dispatch is by reading the root `LoroValue::Map`'s `schema` entry. If `schema == "task-list"`, emit a single top-level KDL node named `task-list` with these entries:
  - Named properties: `default_status="..."` (from map, when present), `display_limit=N` (from map, when present).
  - (No positional args; properties only.)
- For each entry in the `items` LoroMovableList (LoroValue::List of `LoroValue::Map`), emit a child `item` node:
  - `id="<snowflake>"` named property (required; must be non-empty — serialized Snowflake string per `TaskItemId`).
  - `status="<kebab>"` named property.
  - `owner="@agent"` named property (when present).
  - Children:
    - `subject` node with a single positional string arg (the subject text).
    - `description` node with a single positional string arg.
    - `active_form` node (when present).
    - `metadata { ... }` child node — recurse into the generic loro-to-kdl converter for the nested map (reuse the existing Map handling).
    - `blocks` node whose entries are typed annotations: `(block)"<handle>"` or `(block)"<handle>#<item_id>"`. Emit using `kdl::KdlEntry` with the `(block)` type annotation and a string value.
    - `comments { entry author="..." timestamp="..." { text "..." } ... }` child node per comment. Timestamps use ISO-8601 jiff string form.

Reverse (`KdlDocument → LoroValue`):
- Caller passes `TopShape::TaskList`; `kdl_to_loro_value` dispatches to `kdl_task_list::kdl_to_task_list(doc)`.
- Read entries and produce the `schema: "task-list"` discriminator map with `items` list populated from child `item` nodes.
- For typed `(block)"..."` entries inside `blocks` nodes: call `TaskEdgeRef::from_str` on the string value. On error, propagate as `KdlConversionError::TaskEdgeRef { span, source }`. `KdlConversionError` is already `#[non_exhaustive]`; add the new variants carrying the kdl `miette::SourceSpan` and the underlying `TaskEdgeRefParseError`. Preserve the KDL span so error messages include file:line.
- On a non-typed entry inside `blocks` (plain string without the `(block)` annotation), return `KdlConversionError::MissingBlockAnnotation { span }`.

**Testing:**

Unit tests in `crates/pattern_memory/src/fs/kdl_task_list.rs` (inline `#[cfg(test)] mod tests`):
- Empty TaskList (`schema: "task-list"` with empty `items`) round-trips.
- Single-item TaskList with `blocks=[self]` (self-referential edge) round-trips; the canonical KDL includes a `blocks (block)"<self_handle>#<own_id>"` entry.
- TaskList with five items where two have outgoing edges to a third round-trips.
- Item with `metadata { priority "high"; estimated_hours=2.5 }` round-trips (reuses the existing Map converter — exercises the nested recursion).
- Item with `comments { entry author="@r" timestamp="..." { text "..." } }` round-trips.

Integration-style tests in `crates/pattern_memory/src/subscriber/worker.rs` + `cache.rs` tests modules:
- `worker.rs`: `render_canonical_from_disk_doc` with a TaskList schema emits KDL bytes that parse back via `kdl_to_loro_value(.., TopShape::TaskList)` into the original disk_doc state.
- `cache.rs`: `apply_external_edit` with a KDL blob representing an edited TaskList applies to disk_doc and memory_doc correctly; CRDT merge works; no panics; no spurious emits.
- `cache.rs`: `apply_json_to_loro_doc` with a JSON `{"items": [...]}` blob populates the movable list; empty items array produces an empty movable list.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --lib fs::kdl_task_list`
- Expected: converter tests pass.
- Run: `cargo nextest run -p pattern-memory --lib subscriber::worker` and `--lib cache`
- Expected: new TaskList-dispatch tests pass; existing tests unchanged.
- Run: `cargo check --workspace`
- Expected: all crates compile with `#[non_exhaustive]` on `BlockSchema` fully wired.

**Commit (two atomic commits recommended, in order):**
```
jj commit -m "[pattern-memory] add KDL task-list converter (TopShape::TaskList + kdl_task_list module)"
jj commit -m "[pattern-memory] wire BlockSchema::TaskList into subscriber worker + cache dispatch"
```

**Implementor dispatch note (2026-04-23):** Tasks 8 and 9 land as a single unit — dispatch both to the same implementor so the two commits can be authored together and the full non_exhaustive match coverage lands without stub intermediates. Two atomic commits are preferred over one combined commit for bisect-ability.
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: proptest round-trip strategy for TaskList

**Verifies:** v3-task-skill-blocks.AC1.3, v3-task-skill-blocks.AC1.6, v3-task-skill-blocks.AC1.7.

**Files:**
- Create: `crates/pattern_memory/tests/task_list_kdl_roundtrip.rs` (integration test file).
- Modify: `crates/pattern_memory/Cargo.toml` — confirm `proptest` is a `[dev-dependencies]` entry (sibling Phase 4 likely already added it for the Map/List round-trip). Add if missing.

**Implementation:**

Define a bounded `Strategy` for `TaskItem`:
- `subject`: any non-empty printable UTF-8 string, bounded to 120 chars.
- `description`: any printable UTF-8 string, bounded to 500 chars (including newlines).
- `active_form`: optional version of `subject`.
- `status`: `prop_oneof!` over the five `TaskStatus` variants.
- `owner`: optional `AgentId` (use the workspace's existing `AgentId` strategy if defined; otherwise a simple "@[a-z]{3,12}" regex strategy).
- `metadata`: bounded `serde_json::Value` strategy — use `prop_recursive` with depth ≤ 2, branch factor ≤ 4, leaf = number/string/bool/null.
- `comments`: `Vec<TaskComment>`, length 0..=3.
- `blocks`: `Vec<TaskEdgeRef>`, length 0..=5, elements drawn from a small pool of synthetic `BlockHandle`s + optional item ids. **Allow self-referential edges** (do not forbid an item from referring to itself in its blocks list) — AC1.6 requires this.
- `id`: minted by `new_snowflake_id()` at the strategy level (not generated from a shrinkable space — proptest shrinking on random time-ordered snowflakes is unhelpful, and we want the id comparison in round-trip to be stable).
- `created_at`, `updated_at`: fixed reference timestamps (skipping time-shrink complexity; the KDL converter treats them as opaque strings).

Define a bounded `Strategy` for `TaskList`-shaped LoroValue:
- `default_owner`, `default_status`, `display_limit`: optional from simple strategies.
- `items`: `Vec<TaskItem>`, length 0..=8 (covers empty TaskList for AC1.6).

Properties:
- `round_trip_preserves_content`: `loro → kdl → loro` preserves the full LoroValue (including `schema: "task-list"` discriminator, items order, every field). Compare by serializing both sides to a canonical JSON form and asserting equality (avoids accidental LoroValue-container-id noise; real content equality is what matters).
- `reorder_preserves_item_ids`: starting from an items list, apply a deterministic permutation inside a LoroMovableList (`mov(from, to)` operations), commit, export, round-trip through KDL. Every original `TaskItemId` still appears in the output exactly once. This anchors AC1.7. Use at least 64 proptest cases with permutations sampled by index-pair generators.

**Testing:**

- Run: `cargo nextest run -p pattern-memory --test task_list_kdl_roundtrip`
- Expected: proptest runs complete; no shrunken counterexamples.

**Commit:**
```
jj commit -m "[pattern-memory] proptest TaskList ↔ KDL round-trip + reorder preservation"
```
<!-- END_TASK_10 -->

<!-- START_TASK_11 -->
### Task 11: TaskEdgeRef error-path tests in KDL parsing

**Verifies:** v3-task-skill-blocks.AC1.5.

**Files:**
- Modify: `crates/pattern_memory/src/fs/kdl_task_list.rs` tests module (the Task 9 module houses these — keeps task-list error-path tests colocated with the dispatch).

**Implementation:**

Add unit tests (not proptest — these are deterministic failure assertions):
- Input KDL with `blocks (block)""` — parser returns `Err(KdlConversionError::TaskEdgeRef { span, .. })` and the `span` points at the offending entry. Assert the error's `Display` includes a file-like marker (line/column) using `miette::SourceSpan` → `miette::Report` formatting.
- Input KDL with `blocks "handle-without-annotation"` (plain string, missing the `(block)` typed annotation) — parser returns `Err(KdlConversionError::MissingBlockAnnotation { span })`.
- Input KDL with `blocks (block)"#no-handle-before-hash"` — returns `Err(KdlConversionError::TaskEdgeRef { source: TaskEdgeRefParseError::EmptyHandle, .. })`.
- Input KDL with `blocks (block)"handle#"` — returns `Err(... TaskEdgeRefParseError::EmptyItemId ...)`.

**Testing:**

- Run: `cargo nextest run -p pattern-memory --lib fs::kdl_task_list`
- Expected: all four error-path tests pass.

**Commit:**
```
jj commit -m "[pattern-memory] test TaskEdgeRef KDL error paths (empty, missing annotation)"
```
<!-- END_TASK_11 -->
<!-- END_SUBCOMPONENT_D -->

---

## Phase 1 Done when

- Task 1 prerequisite check passes (sibling Phase 1 + Phase 4 landed).
- `cargo check --workspace` passes on the branch with all Phase 1 commits applied.
- `cargo nextest run -p pattern-core -p pattern-memory --lib` passes (no new test regressions).
- `cargo nextest run -p pattern-memory --test task_list_kdl_roundtrip` passes (proptest round-trip clean).
- All acceptance criteria listed in the coverage section verify via the tests referenced in each task.
- No `TODO`, `unimplemented!()`, or commented-out code introduced; all match sites have explicit `TaskList` arms or documented catch-all fallbacks.
- No changes to `.kdl` canonical files for other block schemas; existing Map/List/Composite round-trip tests still pass.
