# Pattern v3 Memory Rework — Phase 1 Implementation Plan

**Goal:** Extract `pattern_memory` as a new workspace crate from `pattern_core::memory::*`; `pattern_core` keeps only the `MemoryStore` trait and trait-signature data types. No behavior change (sqlx preserved, trait still async, no method audit yet).

**Architecture:** Pure structural refactor. Trait-signature types (those that appear in `MemoryStore` signatures) **move** from `pattern_core/src/memory/{types,schema,store}.rs` into a new, cleanly-organized `pattern_core::types::memory_types` module. Impl-only types (`CachedBlock`, `ChangeSource`) **move** to `pattern_memory::types_internal`. `MemoryCache`, `StructuredDocument`, `SharedBlockManager`, and schema templates **move** to `pattern_memory`. The `pattern_core/src/memory/` directory is emptied and deleted — nothing stays behind, no re-export stubs, no orphaned sub-modules. All consumers (pattern_runtime, pattern_cli, pattern_provider) get their imports rewired to the new paths.

**Tech Stack:** Rust 2021 edition, workspace inheritance for deps, `async_trait` (stays on trait this phase), `loro`, existing test infrastructure (`cargo nextest`).

**Scope:** Phase 1 of 8 from the design plan (full scope 8 phases after folding original Phase 9 into Phase 8).

**Codebase verified:** 2026-04-19 (codebase-investigator agent afa60f74bc8165450).

**Execution posture:** Autonomous subagent delegation is appropriate for this phase — mechanical refactor, no gates requiring human sign-off. Implementor should commit incrementally (one commit per subcomponent) to keep the history bisect-able.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC1: pattern_memory crate extraction is clean and reversible

- **v3-memory-rework.AC1.1 Success:** `cargo check --workspace` passes after extraction
- **v3-memory-rework.AC1.2 Success:** `cargo nextest run -p pattern-memory` passes every moved test (all memory-domain tests from pattern_core are runnable in pattern_memory)
- **v3-memory-rework.AC1.3 Success:** `cargo doc -p pattern_memory` produces complete rustdoc for every public item
- **v3-memory-rework.AC1.4 Success:** Every `pattern_runtime` file importing memory types imports trait types from `pattern_core` and impl types from `pattern_memory`; no `pattern_runtime` file depends on `pattern_memory` private internals
- **v3-memory-rework.AC1.5 Failure:** A file in pattern_core attempting to import from `pattern_memory` (reverse dependency) fails to compile
- **v3-memory-rework.AC1.6 Edge:** Workspace `members` list is updated; port-list doc records the extraction with a "completed" note

---

## Codebase verification findings

Key realities found during investigation (from design-plan assumptions that needed correcting):

- ✓ `pattern_core/src/memory/` has `cache.rs` (2225 lines), `document.rs` (1991), `sharing.rs` (383), `schema.rs` (608), `types.rs` (274), plus `store.rs` (83) that the design did NOT enumerate.
- ✗ Design said trait-signature types `BlockMetadata`, `ArchivalEntry`, `SharedBlockInfo` live in `types.rs`. Reality: they live in `pattern_core/src/memory/store.rs`. The plan below accommodates this.
- ✗ Design said schema types `TextViewport`, `CompositeSection`, `FieldDef`, `FieldType`, `LogEntrySchema` live in `types.rs`. Reality: they live in `schema.rs`. The plan below accommodates this.
- ✓ `MemoryStore` trait is at `pattern_core/src/traits/memory_store.rs:193`; 27 async + 1 sync methods = 28 total (matches design claim).
- ✗ Design estimated ~17 pattern_runtime files importing memory. Reality: 9 files. (Lower volume, same rewiring pattern.)
- + Also need rewiring: 5 pattern_provider files (pattern_provider's compose pipeline imports types but not impls — stays on pattern_core after extraction). The original Phase 1 investigation (2026-04-19, pre-strip) also flagged 6 pattern_cli files, but those were v2-era files that have since been moved to `rewrite-staging/` as part of the orual-coordinated `pattern_cli` pre-strip (ratatui scaffolding now in place). **pattern_cli files to rewire depend on post-strip state; the implementor verifies at execution time by re-running the investigation's grep against the current branch.**
- + `pattern_core/src/memory/store.rs` also re-exports `MemoryStore` from `crate::traits::memory_store` for backward-compat. That re-export must be removed in this phase; callers import the trait from `pattern_core::traits::MemoryStore` directly.
- ✓ `pattern_db` has zero `pattern_core::memory` imports — confirms the dep graph `pattern_memory → pattern_core + pattern_db` is clean.
- ✓ Workspace `members` list currently has 4 crates (`pattern_core`, `pattern_runtime`, `pattern_provider`, `pattern_db`). Insert `pattern_memory` between `pattern_core` and `pattern_runtime` (alphabetical + dependency order).
- ✓ `LoroValue::Binary` audit: single match-skip site in `document.rs:1185` (`LoroValue::Binary(_) => return None, // Skip binary data`). Zero construction sites. Note this for Phase 4 — the skip becomes an explicit rejection (`KdlConversionError::UnsupportedBinary`) in the KDL converter.
- ✓ Port-list doc exists at `docs/plans/rewrite-v3-portlist.md`. No `pattern_memory` entry yet — needs a new section.
- ✓ Rustdoc coverage pre-move is sparse (~30% per investigator ballpark). AC1.3 asks for complete rustdoc on every public item in `pattern_memory`. Implementors are expected to write docs for items that lacked them, not just preserve whatever was there. Same applies to moved items that land in `pattern_core::types::memory_types` — write the missing docs as part of the move. This is additive work; keep the doc-writing commits separate from the mechanical move commits so the diffs are reviewable.

**Type relocation plan (Resolution A from the design plan):**

Trait-signature types stay in `pattern_core` but **move** out of the scattered `memory/{types,schema,store}.rs` layout into a clean new `pattern_core::types::memory_types` module. No types remain in `pattern_core/src/memory/` after Phase 1 — the directory is deleted.

Relocations:

- `pattern_core/src/memory/types.rs` → `pattern_core::types::memory_types::core_types`: `BlockType`, `SearchOptions`, `MemorySearchResult`, `SearchMode`, `SearchContentType`, `MemoryError`, `MemoryResult<T>`
- `pattern_core/src/memory/schema.rs` → `pattern_core::types::memory_types::schema`: `BlockSchema`, `TextViewport`, `CompositeSection`, `FieldDef`, `FieldType`, `LogEntrySchema`
- `pattern_core/src/memory/store.rs` → `pattern_core::types::memory_types::metadata`: `BlockMetadata`, `ArchivalEntry`, `SharedBlockInfo`

Impl-only types relocate out of pattern_core entirely, into `pattern_memory::types_internal`:

- `pattern_core/src/memory/types.rs` → `pattern_memory::types_internal`: `CachedBlock`, `ChangeSource`

Schema template helpers (functions/constants, not types) move to `pattern_memory::schema_templates`.

After all Phase 1 moves, `pattern_core/src/memory/` is deleted wholesale. `pattern_core/src/lib.rs` drops its `pub mod memory;` line. No compatibility re-exports are left behind.

---

## Implementation tasks

Tasks grouped into subcomponents. Each subcomponent ends with a gating compile/test step and a commit.

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

### Subcomponent A: Create `pattern_memory` crate scaffold and `pattern_core::types::memory_types` module

<!-- START_TASK_1 -->
### Task 1: Create `pattern_core::types::memory_types` module and migrate trait-signature types

**Verifies:** v3-memory-rework.AC1.1 (partial — structural prerequisite for downstream checks)

**Files:**
- Create: `crates/pattern_core/src/types/memory_types/mod.rs`
- Create: `crates/pattern_core/src/types/memory_types/core_types.rs` (for BlockType, MemoryError, MemoryResult)
- Create: `crates/pattern_core/src/types/memory_types/search.rs` (for SearchOptions, SearchMode, SearchContentType, MemorySearchResult)
- Create: `crates/pattern_core/src/types/memory_types/schema.rs` (for BlockSchema, TextViewport, CompositeSection, FieldDef, FieldType, LogEntrySchema)
- Create: `crates/pattern_core/src/types/memory_types/metadata.rs` (for BlockMetadata, ArchivalEntry, SharedBlockInfo)
- Modify: `crates/pattern_core/src/types/mod.rs` (add `pub mod memory_types;`)
- Modify: `crates/pattern_core/src/memory/types.rs` (remove migrated types; the residual file continues to hold `CachedBlock`, `ChangeSource` temporarily until Task 3)
- Modify: `crates/pattern_core/src/memory/schema.rs` (remove migrated types; keep schema-template helper functions for now until Task 4)
- Modify: `crates/pattern_core/src/memory/store.rs` (remove `BlockMetadata`, `ArchivalEntry`, `SharedBlockInfo` definitions; remove trait re-export)
- Modify: `crates/pattern_core/src/traits/memory_store.rs` (update imports to pull signature types from `crate::types::memory_types::*`)

**Implementation:**

1. Create the `pattern_core::types::memory_types` module tree. The sub-file split (`core_types.rs` / `search.rs` / `schema.rs` / `metadata.rs`) mirrors the logical grouping and keeps file size manageable (largest: `schema.rs` with ~6 types). `mod.rs` re-exports everything at the module root so consumers write `use pattern_core::types::memory_types::BlockType;` regardless of which sub-file owns the type.
2. Move each type (impl blocks, trait impls, derive macros, serde attributes, rustdoc) verbatim from its current location into the new location. **Do not reformat, do not reword rustdoc, do not change derives** — preserve every attribute exactly. This keeps the refactor a pure move.
3. Update `crates/pattern_core/src/memory/types.rs` to retain only `CachedBlock`, `ChangeSource`, and any `use` statements they still need. If the file becomes empty of public items, leave it in place for Task 3's cross-crate move.
4. Update `crates/pattern_core/src/memory/schema.rs` to retain only the template-helper functions (schema constructor helpers). Move all schema type definitions into `types/memory_types/schema.rs`.
5. Update `crates/pattern_core/src/memory/store.rs`: remove the three type definitions AND remove the `pub use crate::traits::memory_store::MemoryStore;` re-export line. Any callers using `pattern_core::memory::MemoryStore` switch to `pattern_core::traits::MemoryStore` in Task 5.
6. Update `crates/pattern_core/src/traits/memory_store.rs`:
   - Change imports at the top of the file from `use crate::memory::{...}` or `use super::memory::{...}` patterns to `use crate::types::memory_types::{...}`.
   - The trait definition itself and its `#[async_trait]` attribute remain unchanged.
7. If any of the moved types re-exported to `pattern_core::memory::*` at the crate root, add temporary re-exports at `crates/pattern_core/src/memory/mod.rs` (`pub use crate::types::memory_types::*;`) so inter-crate callers keep compiling during this task — these re-exports get removed in Task 5 once consumers are updated.

**Testing:**

This task is a pure move. No new unit tests. Verification is operational:

- `cargo check -p pattern-core` passes (all internal imports resolve).
- `cargo check --workspace` passes (consumer crates still compile via the temporary re-exports at `pattern_core::memory::*`).
- `cargo nextest run -p pattern-core` passes — every existing test that touched moved types still works (they now resolve through the new paths or the compat re-exports).
- `cargo doc -p pattern-core` produces output without broken intra-doc links.

**Verification:**

Run: `cargo check --workspace`
Expected: clean build, no warnings referencing missing types.

Run: `cargo nextest run -p pattern-core`
Expected: all tests pass, same count as pre-move baseline.

Run: `cargo doc -p pattern-core --no-deps 2>&1 | grep -i "warning\|error"`
Expected: no unresolved-link warnings on the moved items.

**Commit:** `[pattern-core] extract trait-signature memory types into types::memory_types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Create `pattern_memory` crate scaffold and register in workspace

**Verifies:** v3-memory-rework.AC1.6 (workspace members updated)

**Files:**
- Create: `crates/pattern_memory/Cargo.toml`
- Create: `crates/pattern_memory/src/lib.rs`
- Create: `crates/pattern_memory/CLAUDE.md`
- Modify: `Cargo.toml` (workspace root — insert `"crates/pattern_memory"` between `pattern_core` and `pattern_runtime` in `members`)

**Implementation:**

1. `crates/pattern_memory/Cargo.toml`:

   ```toml
   [package]
   name = "pattern_memory"
   version = { workspace = true }
   edition = { workspace = true }
   license = { workspace = true }
   authors = { workspace = true }
   repository = { workspace = true }

   [dependencies]
   pattern_core = { path = "../pattern_core" }
   pattern_db = { path = "../pattern_db" }

   # Runtime
   tokio = { workspace = true }
   async-trait = { workspace = true }

   # Data
   loro = { workspace = true }
   serde = { workspace = true }
   serde_json = { workspace = true }

   # Errors + logging
   thiserror = { workspace = true }
   miette = { workspace = true }
   tracing = { workspace = true }

   # Utilities inherited from the original pattern_core::memory surface
   dashmap = { workspace = true }
   chrono = { workspace = true }

   [dev-dependencies]
   tokio = { workspace = true, features = ["test-util", "macros", "rt-multi-thread"] }
   # Additional dev deps added in later phases (proptest, insta, etc.)
   ```

   Verify each workspace-inherited dep is actually declared at the workspace root (`Cargo.toml` `[workspace.dependencies]`). Any that aren't inherited must be pinned explicitly with the same version the consumers use.

2. `crates/pattern_memory/src/lib.rs`:

   ```rust
   //! # pattern_memory
   //!
   //! Implementation crate for Pattern's memory subsystem. Hosts the `MemoryCache`
   //! canonical `MemoryStore` implementation, `StructuredDocument` (Loro wrapper),
   //! `SharedBlockManager`, schema templates, and (in later phases) filesystem
   //! serialization, the loro-native subscriber machinery, the jj CLI adapter,
   //! storage-mode handling, and backup/restore.
   //!
   //! The [`MemoryStore`](pattern_core::traits::MemoryStore) trait lives in
   //! `pattern_core`; all data-contract types live in
   //! [`pattern_core::types::memory_types`]. Nothing in `pattern_core` depends on
   //! this crate.
   ```

   No module declarations yet; Task 3 wires them in.

3. `crates/pattern_memory/CLAUDE.md` — short stub establishing the crate's charter. Follow the template used by `crates/pattern_core/CLAUDE.md` and `crates/pattern_db/CLAUDE.md`. Keep it under 40 lines:

   - One-paragraph overview (what this crate owns).
   - Dependency rule: `pattern_memory → pattern_core + pattern_db`; nothing flows back.
   - Testing posture: unit tests in-file, integration tests in `tests/` dir, `cargo nextest run -p pattern-memory`.
   - Freshness-dated note: "Created 2026-04-19 during v3-memory-rework Phase 1; populated incrementally in Phases 1–8."

4. Workspace `Cargo.toml` `members` array: insert `"crates/pattern_memory"` after `"crates/pattern_core"`. Keep trailing comma consistent with surrounding style.

**Testing:**

Operational verification only — the crate has no code yet beyond the lib.rs module comment.

**Verification:**

Run: `cargo check --workspace`
Expected: `pattern_memory` compiles as an empty library; other crates unaffected.

Run: `cargo metadata --format-version=1 | grep pattern_memory`
Expected: crate appears in the workspace member list.

**Commit:** `[pattern-memory] scaffold new crate with workspace registration`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->

### Subcomponent B: Move implementation modules into `pattern_memory`

<!-- START_TASK_3 -->
### Task 3: Move `MemoryCache`, `StructuredDocument`, `SharedBlockManager` into `pattern_memory`

**Verifies:** v3-memory-rework.AC1.1, v3-memory-rework.AC1.2 (partial — tests that travel with the impls now run in pattern_memory)

**Files:**
- Create: `crates/pattern_memory/src/cache.rs` (receives content of `pattern_core/src/memory/cache.rs`)
- Create: `crates/pattern_memory/src/document.rs` (receives content of `pattern_core/src/memory/document.rs`)
- Create: `crates/pattern_memory/src/sharing.rs` (receives content of `pattern_core/src/memory/sharing.rs`)
- Create: `crates/pattern_memory/src/types_internal.rs` (receives `CachedBlock`, `ChangeSource` from `pattern_core/src/memory/types.rs`)
- Modify: `crates/pattern_memory/src/lib.rs` (add `pub mod cache; pub mod document; pub mod sharing; mod types_internal;` and public re-exports per `pattern_core::memory::mod.rs`'s previous surface)
- Delete: `crates/pattern_core/src/memory/cache.rs`
- Delete: `crates/pattern_core/src/memory/document.rs`
- Delete: `crates/pattern_core/src/memory/sharing.rs`
- Modify: `crates/pattern_core/src/memory/mod.rs` (remove `pub mod cache; pub mod document; pub mod sharing;`)
- Modify: `crates/pattern_core/src/memory/types.rs` (delete `CachedBlock`, `ChangeSource` — they are now in `pattern_memory::types_internal`)

**Implementation:**

1. **Move files, don't reformat.** Each file's contents transplant byte-for-byte. Only the `use` statements at the top of each file change — rewrite imports from `use crate::memory::{...}` or `use crate::types::{...}` to the new paths:
   - Trait-signature types: `use pattern_core::types::memory_types::{BlockType, BlockSchema, BlockMetadata, ...};`
   - `MemoryStore` trait: `use pattern_core::traits::MemoryStore;`
   - Error types: `use pattern_core::types::memory_types::{MemoryError, MemoryResult};`
   - Impl-only types: `use crate::types_internal::{CachedBlock, ChangeSource};` (once they land).
   - Loro, serde, tokio, etc. stay identical.
2. Identify and move the `CachedBlock` + `ChangeSource` definitions from `pattern_core/src/memory/types.rs` into `crates/pattern_memory/src/types_internal.rs`. Update `types.rs` to remove them. If `types.rs` becomes empty (no residual public items), delete it and remove its `mod types;` declaration from `memory/mod.rs`.
3. Update `crates/pattern_memory/src/lib.rs` to declare the new modules and re-export the public surface to match the **pre-move** `pattern_core::memory::*` surface. Consumers currently write `pattern_core::memory::MemoryCache`, etc. — those paths stop working after Task 5 updates the consumers, but during Task 3 we want `pattern_memory::MemoryCache` to be callable by consumers once they're rewired in Task 5. Example:

   ```rust
   //! (module-level doc from Task 2)

   pub mod cache;
   pub mod document;
   pub mod sharing;
   mod types_internal;

   pub use cache::MemoryCache;
   pub use document::StructuredDocument;
   pub use sharing::SharedBlockManager;
   // Internal types intentionally NOT re-exported.
   ```

4. Update `crates/pattern_core/src/memory/mod.rs` to delete `pub mod cache;` / `pub mod document;` / `pub mod sharing;` declarations. Keep any temporary re-exports set up in Task 1 for trait-signature types so consumers still compile against `pattern_core::memory::BlockType` etc. — those re-exports are removed in Task 5.
5. **Unit tests travel with the code.** If `cache.rs` contains `#[cfg(test)] mod tests { ... }` blocks, those blocks travel along. Any test that constructs fixtures will need its imports fixed up the same way.
6. **Integration tests.** Look in `crates/pattern_core/tests/` for files that exercise `MemoryCache` / `StructuredDocument` / `SharedBlockManager`. Move those files to `crates/pattern_memory/tests/` verbatim (same import-rewiring rule). Any test-support utility modules referenced by both moved and non-moved integration tests either (a) get duplicated to `pattern_memory/tests/common/` if small, or (b) extracted to a dev-dep helper crate in a follow-up task. Flag any case-(b) situation to the user; don't extract a new crate inside Phase 1 without approval.

**Testing:**

Verification is operational plus existing-test regression:

- `cargo check --workspace` compiles after the move.
- `cargo nextest run -p pattern-memory` runs every moved test; counts match the pre-move baseline (record it by running `cargo nextest run -p pattern-core --list | wc -l` before Task 3, subtracting moved-test count afterwards).
- `cargo nextest run -p pattern-core` still passes (the non-memory tests that stayed behind).

**Verification:**

Run: `cargo check --workspace`
Expected: clean build.

Run: `cargo nextest run -p pattern-memory`
Expected: every moved test passes.

Run: `cargo nextest run -p pattern-core`
Expected: remaining pattern_core tests pass (no memory-domain tests left behind).

**Commit:** `[pattern-memory] move MemoryCache, StructuredDocument, SharedBlockManager from pattern_core`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Move schema template helpers into `pattern_memory`

**Verifies:** v3-memory-rework.AC1.1

**Files:**
- Create: `crates/pattern_memory/src/schema_templates.rs` (receives the template-helper functions from `pattern_core/src/memory/schema.rs`)
- Modify: `crates/pattern_memory/src/lib.rs` (add `pub mod schema_templates;` and any re-exports for callers that used `pattern_core::memory::schema::*` helpers)
- Delete: `crates/pattern_core/src/memory/schema.rs` (schema types migrated in Task 1; template helpers migrated now; file becomes empty and is removed)
- Modify: `crates/pattern_core/src/memory/mod.rs` (remove `pub mod schema;` declaration)

**Implementation:**

1. Open `crates/pattern_core/src/memory/schema.rs` (post-Task 1 state — types already removed). What remains is constructor/template helper functions (e.g., functions returning canned `BlockSchema` values for common block shapes). Verify via `grep -n "^pub fn\|^pub(crate) fn" crates/pattern_core/src/memory/schema.rs`.
2. Move those functions verbatim into `crates/pattern_memory/src/schema_templates.rs`. Fix imports (types live at `pattern_core::types::memory_types::*` now).
3. If any helper was marked `pub(crate)` in pattern_core but is genuinely needed by callers outside pattern_memory, flag it to the user before converting to `pub` — visibility changes are a design decision, not a mechanical one.
4. Delete `crates/pattern_core/src/memory/schema.rs`.
5. Update `crates/pattern_core/src/memory/mod.rs` to drop the module declaration.

**Testing:**

Operational + regression:

- `cargo check --workspace` passes.
- `cargo nextest run -p pattern-memory` — any template-helper tests that travel with the code still pass.
- Any pattern_core test that used a schema template helper now imports from `pattern_memory::schema_templates` — those are dev-only imports (tests don't need `pattern_core` to depend on `pattern_memory`; they use `pattern_memory` as a dev-dependency if necessary, but the expectation is that schema-template users are already in pattern_memory or pattern_runtime).

**Verification:**

Run: `cargo check --workspace`
Expected: clean build.

Run: `cargo nextest run -p pattern-memory`
Expected: all tests pass.

**Commit:** `[pattern-memory] move schema template helpers from pattern_core`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-6) -->

### Subcomponent C: Rewire consumers and remove compatibility re-exports

<!-- START_TASK_5 -->
### Task 5: Rewire consumers (`pattern_runtime`, `pattern_cli`, `pattern_provider`) and remove compatibility re-exports

**Verifies:** v3-memory-rework.AC1.1, v3-memory-rework.AC1.2, v3-memory-rework.AC1.4 (runtime imports split correctly)

**Files:**

Modify, with exact import rewrites per file:

**pattern_runtime (9 files):**
- `crates/pattern_runtime/Cargo.toml` — add `pattern_memory = { path = "../pattern_memory" }` to `[dependencies]`.
- `crates/pattern_runtime/src/session.rs` — split imports: trait + signature types from `pattern_core`; if the file uses `MemoryCache` directly, import from `pattern_memory`.
- `crates/pattern_runtime/src/bin/pattern-test-cli.rs` — same split.
- `crates/pattern_runtime/src/memory/adapter.rs` — uses impl types (`MemoryCache`) and signature types (`ArchivalEntry`, `BlockMetadata`, `BlockSchema`); split accordingly.
- `crates/pattern_runtime/src/memory/turn_history.rs` — currently imports `pattern_core::types::block::BlockWrite` which is orthogonal; verify no change needed, confirm in the commit diff.
- `crates/pattern_runtime/src/sdk/handlers/memory.rs` — trait from `pattern_core::traits`; signature types from `pattern_core::types::memory_types`; impl types (if any) from `pattern_memory`.
- `crates/pattern_runtime/src/sdk/handlers/search.rs` — same pattern.
- `crates/pattern_runtime/src/sdk/handlers/recall.rs` — same pattern.
- `crates/pattern_runtime/src/sdk/handlers/scope.rs` — same pattern.
- `crates/pattern_runtime/src/testing/in_memory_store.rs` — this is a MemoryStore impl for tests; trait from pattern_core; anything else needed goes to pattern_memory. If the in-memory store IS essentially a simpler test double, keep it in pattern_runtime/testing; it doesn't need to move.

**pattern_cli (6 files):**
- `crates/pattern_cli/Cargo.toml` — add `pattern_memory = { path = "../pattern_memory" }` if it ends up needing impl types; usually CLI only needs trait + signature types and goes through the runtime adapter, so verify first. If not needed, do not add the dep.
- `crates/pattern_cli/src/commands/agent.rs` and the five others flagged by the investigator — update imports to pattern_core for types, pattern_memory only where the file constructs impls directly.

**pattern_provider (5 files — compose pipeline):**
- `crates/pattern_provider/Cargo.toml` — no change expected (compose uses types, not impls).
- `crates/pattern_provider/src/compose/current_state.rs` / `pseudo_messages.rs` / `passes.rs` / `passes/segment_2.rs` / `passes/segment_3.rs` — update imports to `pattern_core::types::memory_types::*`. **Do not add a `pattern_memory` dep** here — if any file apparently needs an impl type, that's a red flag that memory impls have leaked into the provider; flag to the user before adding the dep.

After all consumer files are rewired:

- Modify: `crates/pattern_core/src/memory/mod.rs` — remove the temporary re-exports set up in Task 1 (the `pub use crate::types::memory_types::*;`). Directory is now essentially empty; either delete `crates/pattern_core/src/memory/` entirely OR keep it as an empty module with a doc comment explaining it's been evacuated. Prefer deletion; it's cleaner. If deleted, also remove `pub mod memory;` from `crates/pattern_core/src/lib.rs`.
- Modify: `crates/pattern_core/src/memory/store.rs` — if any content remains (there shouldn't, after Tasks 1 and 3), remove. Remove `pub mod store;` from `memory/mod.rs`. If `memory/mod.rs` itself is now empty, delete it.
- Modify: `crates/pattern_core/src/memory/types.rs` — delete (content already migrated in Tasks 1 and 3).

**Implementation:**

1. Rewire one consumer crate at a time. Build + test between crates to localize breakage.
2. For each file, read the current imports, categorize each imported name:
   - Is it the trait? → `pattern_core::traits::MemoryStore`
   - Is it a signature type (see list in "Codebase verification findings" above)? → `pattern_core::types::memory_types::<Name>`
   - Is it an impl type (`MemoryCache`, `StructuredDocument`, `SharedBlockManager`)? → `pattern_memory::<Name>`
3. After rewiring a crate, run `cargo check -p <crate>` and fix any remaining imports.
4. Once all consumers compile, remove the temporary re-exports in `pattern_core::memory::*` and run `cargo check --workspace`. Any remaining compile errors mean a consumer still expects the old path — find and fix.
5. Remove the `memory/` directory from `pattern_core/src/` entirely.

**Testing:**

- `cargo check --workspace` passes.
- `cargo nextest run --workspace` passes (every test in every crate, including the ones moved in Task 3).
- `cargo test --doc --workspace` passes (doctests that reference memory types must resolve).

**Verification:**

Run: `cargo check --workspace`
Expected: clean build, no warnings about unresolved imports or dead re-exports.

Run: `cargo nextest run --workspace`
Expected: all tests pass.

Run: `cargo test --doc --workspace`
Expected: all doctests pass.

Run: `grep -rn "pattern_core::memory" crates/ --include="*.rs"`
Expected: zero matches (compatibility paths fully removed).

**Commit:** `[pattern-runtime] [pattern-cli] [pattern-provider] rewire memory imports to pattern_memory + pattern_core split`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Enforce reverse-dependency guard + API-parity smoke test

**Verifies:** v3-memory-rework.AC1.5 (reverse dep fails to compile), v3-memory-rework.AC1.2 (API parity)

**Files:**
- Create: `crates/pattern_core/tests/no_pattern_memory_dep.rs` (compile-fail guard via `trybuild`) — adds `trybuild` as a dev-dep to pattern_core if not already present
- Create: `crates/pattern_core/tests/trybuild/no_pattern_memory_dep.rs` (the input file that must fail)
- Create: `crates/pattern_memory/tests/api_parity.rs` (smoke test constructing `MemoryCache` + `StructuredDocument` + a `SharedBlockManager` and calling representative methods — confirms public surface is intact)

**Implementation:**

1. **Reverse-dep guard.** The cleanest way is a `trybuild` compile-fail test:

   `crates/pattern_core/tests/no_pattern_memory_dep.rs`:
   ```rust
   #[test]
   fn pattern_core_cannot_import_pattern_memory() {
       let t = trybuild::TestCases::new();
       t.compile_fail("tests/trybuild/no_pattern_memory_dep.rs");
   }
   ```

   `crates/pattern_core/tests/trybuild/no_pattern_memory_dep.rs`:
   ```rust
   use pattern_memory::MemoryCache;

   fn main() {}
   ```

   This test only passes if the import fails to resolve — i.e., pattern_core has no dependency on pattern_memory (which is enforced by `Cargo.toml` — pattern_core's `[dependencies]` does not list pattern_memory).

   If `trybuild` is not already a dev-dep of pattern_core, add it:
   ```toml
   [dev-dependencies]
   trybuild = "1.0"
   ```

2. **API-parity smoke test.** A single `#[tokio::test]` that exercises the moved public surface:

   `crates/pattern_memory/tests/api_parity.rs`:
   ```rust
   //! API parity smoke test — confirms the extraction didn't silently drop
   //! public items. Covers v3-memory-rework.AC1.2.

   use pattern_memory::{MemoryCache, StructuredDocument, SharedBlockManager};
   // Include representative calls on each impl type. Exact calls depend on
   // the current public surface — mirror what pattern_core::memory tests
   // did pre-extraction.

   #[tokio::test]
   async fn memory_cache_constructs_and_exposes_public_surface() {
       // Exact instantiation matches pre-extraction test patterns; the goal
       // is not new coverage, just proof that the move preserved the surface.
       // Implementor: lift this test body from the pre-extraction fixture
       // discovered during investigation.
       todo!("lift fixture from pre-extraction pattern_core::memory tests");
   }
   ```

   **Important:** the `todo!()` placeholder is an instruction to the implementor — before the task is committed, the `todo!` MUST be replaced with the actual pre-extraction fixture code. Do not commit with `todo!()` left in (that would violate the guidance file's "shim/stub pollution" rule).

   If no pre-extraction fixture exists that naturally fits a smoke check, the implementor constructs one that:
   - Instantiates `MemoryCache::new(...)` with a minimal fixture.
   - Calls 2-3 representative methods (e.g., `create_block`, `get_block`, `list_blocks`) and asserts non-panicking return.
   - Instantiates `StructuredDocument` with a small text doc, calls one method.
   - Instantiates `SharedBlockManager`, calls one method.

3. **Port-list doc update.** Modify `docs/plans/rewrite-v3-portlist.md`: add a new section header `## v3-memory-rework additions` (or under an existing "Staged additions" header if present) with the entry:

   ```markdown
   ### pattern_memory (Phase 1 — completed YYYY-MM-DD)
   - Extracted from `pattern_core::memory::*` during the v3-memory-rework plan, Phase 1.
   - Hosts `MemoryCache`, `StructuredDocument`, `SharedBlockManager`, schema templates.
   - `pattern_core` retains the `MemoryStore` trait + trait-signature data types
     under `pattern_core::types::memory_types::*`.
   - Dependency graph: `pattern_memory → pattern_core + pattern_db`; reverse-dep
     guard is `crates/pattern_core/tests/no_pattern_memory_dep.rs`.
   ```

   Replace `YYYY-MM-DD` with the actual commit date (`jj log -r @ -T 'committer.timestamp()' --no-graph | head -c 10` or equivalent git command).

**Testing:**

This task's deliverables ARE tests. Running them is verification.

**Verification:**

Run: `cargo nextest run -p pattern-core --test no_pattern_memory_dep`
Expected: test passes (the inner compile-fail test confirms pattern_memory is unreachable from pattern_core).

Run: `cargo nextest run -p pattern-memory --test api_parity`
Expected: smoke test passes; asserts succeed, no panics.

Run: `cargo doc -p pattern_memory --no-deps 2>&1 | grep -i "warning\|error" | head`
Expected: no unresolved-link warnings.

**Commit:** `[pattern-memory] add reverse-dep guard and API-parity smoke test; record port-list entry`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase 1 Done-when recap

- `cargo check --workspace` clean (AC1.1).
- `cargo nextest run -p pattern-memory` passes every moved test plus the API-parity smoke (AC1.2).
- `cargo nextest run --workspace` passes across the board (no collateral regressions).
- `cargo doc -p pattern_memory` produces complete rustdoc output without broken intra-doc links on the items that carried documentation pre-move (AC1.3). (No attempt to increase coverage; that's a separate effort.)
- `grep -rn "pattern_core::memory" crates/ --include="*.rs"` returns zero matches — imports are split cleanly (AC1.4).
- Reverse-dep guard at `crates/pattern_core/tests/no_pattern_memory_dep.rs` passes (AC1.5).
- Workspace `members` updated; port-list doc has a completed entry (AC1.6).

## Notes for downstream phases

- Phase 2 (rusqlite migration) needs to know that `MemoryCache` lives in `pattern_memory::cache` and inherits whatever sqlx dep is currently in pattern_core. When pattern_db swaps to rusqlite, pattern_memory's dep surface changes accordingly — plan accordingly.
- Phase 3 (MemoryStore sync-ification) desyncs the trait in `pattern_core::traits::memory_store`. It does NOT move the trait; it only removes the `#[async_trait]` attribute and changes method signatures.
- Phase 4 (fs serialization + KDL converter) implements the rejection of `LoroValue::Binary` per policy. Phase 1's audit confirmed no construction sites — the one `Binary(_) => return None` match site in `document.rs:1185` becomes an explicit rejection with `KdlConversionError::UnsupportedBinary` at the converter boundary (not in this phase).
- Phase 8 capstone (absorbed Phase 9) runs `cargo nextest run --workspace` as its final gate; Phase 1's passing workspace build is the foundation all subsequent phases build on.
