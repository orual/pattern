# v3-task-skill-blocks Phase 1: TaskList schema + KDL serialization

**Goal:** Land the `BlockSchema::TaskList` variant and supporting types, and extend the LoroValue↔KdlDocument converter so TaskList blocks round-trip losslessly through canonical `.kdl` files.

**Architecture:** New `TaskList` variant on the existing `BlockSchema` enum holds task-list-level policy (default owner/status, display cap); per-item `TaskItem` records live in a `LoroMovableList` under each TaskList block's LoroDoc and carry status, owner, typed `BlockRef` edges, metadata, and inline comments. KDL serialization extends the Phase-4-sibling `loro_value_to_kdl` converter with a `task-list` dispatch that emits/parses `item { ... }` children and typed `(block)"..."` entries for `BlockRef`.

**Tech Stack:** Rust (pattern_core, pattern_memory), `loro` (LoroMovableList + LoroMap), `kdl` v2, `smol_str`, `ferroid` (base32 Mastodon-style Snowflake IDs via workspace `new_snowflake_id()`), `jiff`, `proptest`, `cargo nextest`.

**Scope:** Phase 1 of 5 (v3-task-skill-blocks design plan).

**Codebase verified:** 2026-04-19.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-task-skill-blocks.AC1: TaskList schema + KDL round-trip

- **v3-task-skill-blocks.AC1.1 Success:** `BlockSchema::TaskList { default_owner, default_status, display_limit }` exists and is exported from `pattern_core::types::memory_types`
- **v3-task-skill-blocks.AC1.2 Success:** `TaskItem`, `TaskStatus`, `TaskComment`, `BlockRef`, `TaskItemId` types exist with documented fields
- **v3-task-skill-blocks.AC1.3 Success:** Property test (proptest) confirms round-trip equivalence: generate arbitrary TaskList with nested items + edges + comments → serialize to KDL → parse back → LoroValue matches original
- **v3-task-skill-blocks.AC1.4 Success:** BlockRef parses both `(block)"<handle>"` and `(block)"<handle>#<item_id>"` forms
- **v3-task-skill-blocks.AC1.5 Failure:** Malformed BlockRef annotation (e.g., `(block)""` or missing typed annotation) produces `KdlConversionError` with file:line reference
- **v3-task-skill-blocks.AC1.6 Edge:** Empty TaskList (zero items) round-trips cleanly; self-referential edge (`A.blocks = [A]`) round-trips cleanly
- **v3-task-skill-blocks.AC1.7 Edge:** Item reordering via `LoroMovableList` preserves item ids across round-trip
- **v3-task-skill-blocks.AC1.8 Success:** `TaskItemId::parse("")` returns `TaskItemIdError::Empty`; `TaskItemId::new()` produces a valid Snowflake string (base32-encoded Mastodon-style via `new_snowflake_id`)
- **v3-task-skill-blocks.AC1.9 Success:** Two concurrent agents calling `TaskItemId::new()` produce distinct ids (Snowflake collision-resistant by construction)

---

## Design deviations recorded during planning

- **Snowflake, not UUID v7:** the design plan text says "UUID v7 as base32 string" but the workspace's time-ordered ID infrastructure is ferroid-backed `SnowflakeMastodonId` (exported as `pattern_core::types::ids::new_snowflake_id() -> SmolStr`, base32-encoded, lexicographically sortable). This is the existing house convention for any ID that must order turns/batches/messages. `TaskItemId::new()` delegates to `new_snowflake_id()`. The design's "UUID v7" wording is treated as imprecise — no new UUID generator is introduced. AC1.9 (collision resistance across concurrent multi-agent creates) is satisfied by ferroid's `AtomicSnowflakeGenerator<_, MonotonicClock>` under the existing global `MESSAGE_POSITION_GENERATOR` (verified in `crates/pattern_core/src/utils.rs`).
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

Expected: either a `FromStr` / `From<&str>` impl OR a `BlockHandle::new(s: impl Into<SmolStr>)` constructor exists. Record the signature — Task 5's `BlockRef::from_str` uses it.

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

Functionality tasks. Introduces `TaskItemId`, `TaskStatus`, `TaskComment`, `BlockRef`, `TaskItem`.

<!-- START_TASK_4 -->
### Task 4: `TaskItemId` newtype with Snowflake generation

**Verifies:** v3-task-skill-blocks.AC1.8, v3-task-skill-blocks.AC1.9.

**Files:**
- Create: `crates/pattern_core/src/types/memory_types/task_item_id.rs`
- Modify: `crates/pattern_core/src/types/memory_types/mod.rs` (add `mod task_item_id;` + `pub use task_item_id::{TaskItemId, TaskItemIdError};`)
- Test: same file (unit tests at bottom — follow `pattern_core` convention for inline `#[cfg(test)] mod tests`).

**Implementation:**

- `TaskItemId(SmolStr)` newtype, `#[derive(Clone, Debug, PartialEq, Eq, Hash)]`. Implement `Display` (prints the wrapped string) and `FromStr` (delegates to `parse`).
- `pub fn new() -> Self { Self(crate::types::ids::new_snowflake_id()) }` — delegates to the workspace's existing ferroid-backed generator. This yields a base32-encoded Mastodon-style Snowflake; lexicographically sortable; collision-resistant across concurrent multi-agent creates via `AtomicSnowflakeGenerator<_, MonotonicClock>`.
- `pub fn parse(s: &str) -> Result<Self, TaskItemIdError>` rejects empty strings with `TaskItemIdError::Empty`; otherwise wraps. Do NOT validate Snowflake shape at parse — tolerates externally supplied ids (including short synthetic ids used in fixtures) as long as they're non-empty.
- `pub fn as_str(&self) -> &str` returns the inner SmolStr's `&str`.
- Serde: derive `Serialize` / `Deserialize` as transparent string (use `serde(transparent)` on the struct or a manual impl that reuses `parse`). Deserialize must reject empty strings via `TaskItemIdError::Empty` surfaced as `serde::de::Error`.
- Error type: `#[non_exhaustive] pub enum TaskItemIdError` with at minimum an `Empty` variant. Use `thiserror::Error`.

**Testing:**

Tests in the same file verify:
- `v3-task-skill-blocks.AC1.8`: `TaskItemId::parse("")` returns `Err(TaskItemIdError::Empty)`; `TaskItemId::new()` produces a non-empty string that round-trips through `parse`.
- `v3-task-skill-blocks.AC1.9`: spawning 32 threads that each call `TaskItemId::new()` once and collect into a `HashSet` yields 32 distinct values. (Thread spawn + join is sufficient — the underlying ferroid `AtomicSnowflakeGenerator` already handles concurrent-access collision resistance via atomic counter bumps within the same millisecond.)
- Serde transparent behaviour: `serde_json::to_string(&id)` produces a quoted string; deserialization rejects `""`.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib task_item_id`
- Expected: all task_item_id tests pass.

**Commit:**
```
jj commit -m "[pattern-core] add TaskItemId newtype using snowflake generator"
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `TaskStatus`, `TaskComment`, `BlockRef`, `TaskItem` types

**Verifies:** v3-task-skill-blocks.AC1.2.

**Files:**
- Create: `crates/pattern_core/src/types/memory_types/task.rs`
- Modify: `crates/pattern_core/src/types/memory_types/mod.rs` to add `mod task;` and `pub use task::*;`.

**Implementation:**

1. `TaskStatus` enum — `#[non_exhaustive]`, `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]`, serialized as kebab-case strings (`"pending"`, `"in-progress"`, `"blocked"`, `"completed"`, `"cancelled"`). Use `serde(rename_all = "kebab-case")`.

2. `TaskComment { author: AgentId, timestamp: Timestamp, text: String }` — standard derives plus `Serialize`/`Deserialize`. `Timestamp` is `jiff::Timestamp`.

3. `BlockRef { block: BlockHandle, task_item: Option<TaskItemId> }` with:
   - `Display` impl that emits `"<handle>"` when `task_item` is `None`, `"<handle>#<item_id>"` otherwise.
   - `FromStr` impl that parses both forms; empty handle or empty item-id chunk returns `BlockRefParseError`. Use `#[non_exhaustive]` on the error enum; variants at minimum `EmptyHandle`, `EmptyItemId`.
   - Serde: derive struct-form serde so JSON representation is `{"block": "...", "task_item": null or "..."}`. KDL encoding is handled separately in Task 9 — serde and KDL are distinct surfaces.

4. `TaskItem` struct — fields exactly as the design specifies:
   - `pub id: TaskItemId`
   - `pub subject: String` (imperative form)
   - `pub description: String` (markdown body)
   - `pub active_form: Option<String>`
   - `pub status: TaskStatus`
   - `pub owner: Option<AgentId>`
   - `pub blocks: Vec<BlockRef>` — outgoing edges only (see design's "Single-source-of-truth edge model"); there is no `blocked_by` field.
   - `pub metadata: serde_json::Value` (freeform JSON).
   - `pub comments: Vec<TaskComment>` (append-mostly; no dedup).
   - `pub created_at: Timestamp`
   - `pub updated_at: Timestamp`
   Document in the rustdoc that `blocks` is the *only* edge storage and that reverse lookups happen via the `task_edges` index (Phase 2).

**Testing:**

- Unit tests in `task.rs` confirm kebab-case status serialization round-trip for every variant.
- `BlockRef::from_str("handle")` yields `BlockRef { block: ..., task_item: None }`.
- `BlockRef::from_str("handle#id")` yields the item form.
- `BlockRef::from_str("")`, `"#id"`, `"handle#"` each return `Err`.
- `BlockRef::to_string().parse()` round-trips for both forms.

**Verification:**
- Run: `cargo nextest run -p pattern-core --lib types::memory_types::task`
- Expected: all task-type tests pass.

**Commit:**
```
jj commit -m "[pattern-core] add TaskItem, TaskStatus, TaskComment, BlockRef types"
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Extended serde + parse tests for task types

**Verifies:** v3-task-skill-blocks.AC1.2 (round-trip coverage), v3-task-skill-blocks.AC1.8 (combined with Task 4 empty-id path).

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/task.rs` (add tests module) or create `tests/task_types.rs` if the inline test module is already heavy — implementor's call.

**Implementation:**

Add tests that cover the cross-type contract:
- `TaskItem` JSON round-trip via `serde_json` — construct an instance with all fields populated including a BlockRef vector containing both block-level and item-level refs, encode, decode, assert equality.
- `TaskItem` with empty `blocks` and empty `comments` vectors round-trips cleanly.
- `TaskItem` with a self-edge (an item whose `blocks` contains a `BlockRef` pointing at its own `TaskItemId` inside its own block) round-trips cleanly. This anchors AC1.6.
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
### Task 8: Update every `BlockSchema` match site for `TaskList`

**Verifies:** v3-task-skill-blocks.AC1.1 (indirectly — the new variant is wired into all schema-dispatching call sites).

**Files:**
- Modify: every site recorded in Task 3's scratch file (`target/plan-phase1-blockschema-sites.txt`).
- Expected concrete sites (re-verify at execution; sibling-plan relocation may shift paths):
  - `crates/pattern_core/src/types/memory_types/schema.rs` helper methods `is_field_read_only`, `read_only_fields`, `is_section_read_only`, `get_section_schema`.
  - `crates/pattern_cli/src/commands/debug.rs` debug printer.

**Implementation:**

For each helper method on `BlockSchema`:
- `is_field_read_only(&self, _field: &str) -> bool`: `TaskList { .. } => false` — all fields are agent-editable; the schema doesn't pre-lock any.
- `read_only_fields(&self) -> &'static [&'static str]`: `TaskList { .. } => &[]`.
- `is_section_read_only(&self, _section: &str) -> bool`: `TaskList { .. } => false`.
- `get_section_schema(&self, _section: &str) -> Option<BlockSchema>`: `TaskList { .. } => None` — sections don't nest inside TaskList; use `items` indexing at the loro/KDL layer instead.

For the pattern_cli debug printer: add a match arm that prints `"TaskList(default_status={...}, display_limit={...})"` (mirror the formatting style of the neighbouring arms).

If `#[non_exhaustive]` is present on `BlockSchema`, `match` sites outside the defining crate MUST have a `_ =>` catch-all. Leave those catch-alls in place — don't add a specific `TaskList` arm unless the call site needs per-variant behaviour. In this phase, only the sites listed above need explicit handling.

**Testing:**

No new tests; `cargo check --workspace` proves the variant is handled. Add a compile-fail insta test only if one already exists for BlockSchema in the workspace (Task 3 verifies).

**Verification:**
- Run: `cargo check --workspace`
- Expected: compiles without warnings.
- Run: `cargo nextest run -p pattern-core -p pattern-cli --lib`
- Expected: all existing tests still pass.

**Commit:**
```
jj commit -m "[pattern-core] [pattern-cli] handle BlockSchema::TaskList at match sites"
```
<!-- END_TASK_8 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 9-11) -->
### Subcomponent D: KDL converter extension for TaskList

<!-- START_TASK_9 -->
### Task 9: Extend `loro_value_to_kdl` / reverse with `task-list` dispatch

**Verifies:** v3-task-skill-blocks.AC1.4 (BlockRef parse, both forms), v3-task-skill-blocks.AC1.6 (empty TaskList + self-edge canonical form).

**Files:**
- Modify: `crates/pattern_memory/src/fs/kdl.rs` — extend `TopShape` enum with a `TaskList` variant; extend `loro_value_to_kdl` + `kdl_to_loro_value` match arms to delegate to the new module; extend `KdlConversionError` enum with `BlockRef { span, source }` and `MissingBlockAnnotation { span }` variants.
- Create: `crates/pattern_memory/src/fs/kdl_task_list.rs` — new sibling module exposing `pub(super) fn task_list_to_kdl(value: &LoroValue) -> Result<KdlDocument, KdlConversionError>` and `pub(super) fn kdl_to_task_list(doc: &KdlDocument) -> Result<LoroValue, KdlConversionError>`. Task-list-specific encoding/decoding lives here (item nodes, typed `(block)` annotations, metadata/comments).
- Modify: `crates/pattern_memory/src/fs/mod.rs` — add `mod kdl_task_list;` (or wherever `mod kdl;` is declared).

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
- For typed `(block)"..."` entries inside `blocks` nodes: call `BlockRef::from_str` on the string value. On error, propagate as `KdlConversionError::BlockRef { span, source }`. `KdlConversionError` is already `#[non_exhaustive]`; add the new variants carrying the kdl `miette::SourceSpan` and the underlying `BlockRefParseError`. Preserve the KDL span so error messages include file:line.
- On a non-typed entry inside `blocks` (plain string without the `(block)` annotation), return `KdlConversionError::MissingBlockAnnotation { span }`.

**Testing:**

Unit tests in `crates/pattern_memory/src/fs/kdl_task_list.rs` (inline `#[cfg(test)] mod tests`):
- Empty TaskList (`schema: "task-list"` with empty `items`) round-trips.
- Single-item TaskList with `blocks=[self]` (self-referential edge) round-trips; the canonical KDL includes a `blocks (block)"<self_handle>#<own_id>"` entry.
- TaskList with five items where two have outgoing edges to a third round-trips.
- Item with `metadata { priority "high"; estimated_hours=2.5 }` round-trips (reuses the existing Map converter — exercises the nested recursion).
- Item with `comments { entry author="@r" timestamp="..." { text "..." } }` round-trips.

**Verification:**
- Run: `cargo nextest run -p pattern-memory --lib fs::kdl_task_list`
- Expected: converter tests pass.

**Commit:**
```
jj commit -m "[pattern-memory] extend KDL converter for TaskList blocks"
```
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
- `blocks`: `Vec<BlockRef>`, length 0..=5, elements drawn from a small pool of synthetic `BlockHandle`s + optional item ids. **Allow self-referential edges** (do not forbid an item from referring to itself in its blocks list) — AC1.6 requires this.
- `id`: from `TaskItemId::new()` at the strategy level (not generated from a shrinkable space — proptest shrinking on random time-ordered snowflakes is unhelpful, and we want the id comparison in round-trip to be stable).
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
### Task 11: BlockRef error-path tests in KDL parsing

**Verifies:** v3-task-skill-blocks.AC1.5.

**Files:**
- Modify: `crates/pattern_memory/src/fs/kdl_task_list.rs` tests module (the Task 9 module houses these — keeps task-list error-path tests colocated with the dispatch).

**Implementation:**

Add unit tests (not proptest — these are deterministic failure assertions):
- Input KDL with `blocks (block)""` — parser returns `Err(KdlConversionError::BlockRef { span, .. })` and the `span` points at the offending entry. Assert the error's `Display` includes a file-like marker (line/column) using `miette::SourceSpan` → `miette::Report` formatting.
- Input KDL with `blocks "handle-without-annotation"` (plain string, missing the `(block)` typed annotation) — parser returns `Err(KdlConversionError::MissingBlockAnnotation { span })`.
- Input KDL with `blocks (block)"#no-handle-before-hash"` — returns `Err(KdlConversionError::BlockRef { source: BlockRefParseError::EmptyHandle, .. })`.
- Input KDL with `blocks (block)"handle#"` — returns `Err(... BlockRefParseError::EmptyItemId ...)`.

**Testing:**

- Run: `cargo nextest run -p pattern-memory --lib fs::kdl_task_list`
- Expected: all four error-path tests pass.

**Commit:**
```
jj commit -m "[pattern-memory] test BlockRef KDL error paths (empty, missing annotation)"
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
