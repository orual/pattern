# Pattern v3 Foundation — Phase 2: pattern_core trait definitions + relocation

**Goal:** Reshape `pattern_core` into a traits + types + errors + preserved-memory-storage crate. Relocate every execution / provider / integration module either to `rewrite-staging/` (future reshape) or to `/dev/null` (retired). Introduce the trait surface every downstream crate depends on.

**Architecture:** Subtractive-minus-storage. Pattern_core retains:
- `memory/` (loro CRDT + sqlite storage; Phase 5 adds composer-side rendering without changing storage)
- `memory_acl/`, `permission/` (core primitives)
- `export/` (no better home; may be reshaped later)
- `config/` (needs future breakup; stays for lack of alternative)
- NEW: `traits/`, `types/`, `error/`, `base_instructions.rs`

Everything else either moves to `rewrite-staging/` (a non-cargo holding pen at the repo root) with fate markers pointing to its Phase-N destination, or is deleted outright. Module layout modernizes to `module.rs` + `module/submodule.rs`.

**Tech Stack:** Rust 2024, `thiserror` + `miette`, `proptest` for id roundtrip tests.

**Scope:** Phase 2 of 6 from the v3 foundation design. Covers all of v3-foundation.AC1.

**Codebase verified:** 2026-04-16

---

## Acceptance Criteria Coverage

This phase implements and tests the following ACs in full:

### v3-foundation.AC1: pattern_core traits are defined, satisfiable, and documented

- **v3-foundation.AC1.1 Success:** `cargo check -p pattern_core` succeeds with zero warnings on the narrowed workspace
- **v3-foundation.AC1.2 Success:** `cargo doc -p pattern_core` produces complete documentation; every public trait and type has rustdoc
- **v3-foundation.AC1.3 Success:** Dummy struct impls of `AgentRuntime`, `Session`, `MemoryStore`, `ProviderClient`, `MessageRouter`, `DataStream`, `SourceManager` all compile, confirming trait shape is satisfiable
- **v3-foundation.AC1.4 Success:** Port-list doc at `docs/plans/rewrite-v3-portlist.md` exists and lists every currently-excluded crate with deferral-plan note
- **v3-foundation.AC1.5 Failure:** Removing a required method from a dummy trait impl causes `cargo check` to fail with a clear "missing implementation" error
- **v3-foundation.AC1.6 Edge:** Referencing a retired crate (e.g., `pattern_auth`) from an active crate's `Cargo.toml` causes explicit workspace error, not silent acceptance
- **v3-foundation.AC1.7 Success:** Every in-flight or pending-move code region has a `// MOVING TO:`, `// REPLACED BY:`, or `// MOVING WITHIN CRATE:` comment identifying its defined fate; port-list doc cross-references these markers
- **v3-foundation.AC1.8 Success:** No surface API contains `unimplemented!()` / `todo!()` without a comment identifying the filling phase and AC
- **v3-foundation.AC1.9 Failure:** A code region pending move that has no fate marker, OR a stubbed API with no phase/AC reference, causes the intermediate-state audit check to fail (grep-based scan during phase verification)
- **v3-foundation.AC1.10 Edge:** Commented-out code blocks in new-or-modified source files fail the audit check (git history is the record, not comments)

---

## Executor Context

**Repo root:** `/home/orual/Projects/PatternProject/pattern`
**Working bookmark:** `rewrite-v3` (created in Phase 1, atop `pre-rewrite-v3`)
**Pre-phase state after Phase 1:** workspace `members = [pattern_core, pattern_runtime, pattern_provider, pattern_db]`; pattern_runtime and pattern_provider are empty skeletons; pattern_db untouched; pattern_core = 142 files, 70K lines, all original modules present.

**Build tools:**
- `cargo check -p pattern_core` — primary verification, must be warning-free by Task 26.
- `cargo doc -p pattern_core --no-deps` — must be warning-free by Task 23.
- `cargo nextest run -p pattern_core --lib` — tests surviving relocation must pass.
- `cargo fmt` — mandatory before every commit.
- `cargo clippy -p pattern_core --all-targets` — zero warnings by phase end.
- `just pre-commit-all` — full hook run before phase-closing commit.

**Commits:** `jj describe -m "[pattern-core] …"` per subcomponent. Explicit retirement commits for deletions use `[meta]`.

**Audit script:** Task 25 creates `scripts/audit-rewrite-state.sh`. Enforces AC1.7–AC1.10. Runs on `pattern_core`, `pattern_runtime`, `pattern_provider`, `pattern_db`, `rewrite-staging`.

**Rust-coding-style reminders (apply throughout):**
- `#[non_exhaustive]` on every public error enum.
- `thiserror::Error` + `miette::Diagnostic` on error types.
- Newtype IDs via `define_id_type!` (or equivalent — see Subcomponent D).
- `module.rs + module/submodule.rs` layout; avoid `mod.rs`.
- Sentence-case rustdoc, period-terminated.
- State machines use enums with associated data, not string tags.

**Design reference:** `/home/orual/Projects/PatternProject/pattern/docs/design-plans/2026-04-16-v3-foundation.md` — Phase 2 between `<!-- START_PHASE_2 -->` and `<!-- END_PHASE_2 -->`. Intermediate-state policy in "Additional Considerations → Intermediate code-state policy".

**Disposition table (reference for Tasks 3–10):**

| pattern_core module | Disposition | Final home | Phase |
|---|---|---|---|
| `memory/` (all storage) | keep | pattern_core | — |
| `memory_acl/` | keep | pattern_core | — |
| `permission/` | keep | pattern_core | — |
| `export/` | keep (fate: may reshape) | pattern_core | — |
| `config/` | keep (fate: breakup needed in future plan) | pattern_core | — |
| `id/` | absorb into new `types/ids.rs` | pattern_core | this phase |
| `messages/` value types | absorb into new `types/message.rs` | pattern_core | this phase |
| `messages/` storage bits | stage | pattern_runtime (or staging until decided) | future |
| `db/` trait shape | extract traits into `traits/`, stage concrete | pattern_core traits | this phase |
| `agent/traits.rs` (Agent trait) | refactor into `traits/agent_runtime.rs` + `traits/session.rs` | pattern_core | this phase |
| `agent/processing/loop_impl.rs` | stage | pattern_runtime | Phase 3 |
| `agent/` remaining | stage | pattern_runtime | Phase 3 |
| `runtime/router.rs` | stage | pattern_runtime | Phase 3 |
| `runtime/` remaining | stage | pattern_runtime | Phase 3 |
| `tool/` | stage | pattern_runtime/sdk | Phase 3 + plugin-system |
| `coordination/` | stage | pattern_runtime | future subagent plan |
| `data_source/` | stage (traits extracted into `traits/`) | pattern_runtime/sources | Phase 3 |
| `context/compression.rs` | stage | pattern_provider/compose | Phase 5 |
| `context/builder.rs` block-render (lines 226–316) | stage | pattern_provider/compose | Phase 5 |
| `context/mod.rs` DEFAULT_BASE_INSTRUCTIONS | extract into `pattern_core/src/base_instructions.rs` | pattern_core | this phase |
| `context/` remaining | stage | pattern_provider/compose | Phase 5 |
| `oauth/` | stage | pattern_provider/auth | Phase 4 |
| `model/` (ModelProvider + request/response) | traits extracted into `traits/provider_client.rs`; impls stage | pattern_provider | Phase 4 |
| `embeddings/` | stage (trait extracted if any) | pattern_provider | future |
| `realtime/` | split: trait/types into `traits/` + `types/`, impls stage | pattern_runtime | future (rework expected) |
| `queue/` | split: trait/types into `traits/` + `types/`, impls stage | pattern_runtime | future (rework expected) |
| `prompt_template/` | **delete** | — | this phase |
| `users/` | **delete** | — | this phase |
| `utils/` | audit: keep reusable helpers, stage the rest | pattern_core | this phase |

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
<!-- START_TASK_1 -->
### Task 1: Create `rewrite-staging/` holding pen

**Verifies:** contributes to AC1.4 and AC1.7 (staging is referenced by port-list doc and is the destination for fate-marked code).

**Files:**
- Create: `/home/orual/Projects/PatternProject/pattern/rewrite-staging/README.md`
- Create: `/home/orual/Projects/PatternProject/pattern/rewrite-staging/.gitkeep` (ensures empty subdirs materialise if needed)

**Step 1: Write README**

```markdown
# rewrite-staging/

Holding pen for pattern_core code in transit during the v3 foundation rewrite.

## Status

Active. Populated in Phase 2, drained by Phases 3–5 and beyond as each
subsystem is reshaped into its final home.

## Policy

- This directory is **not** a cargo member. Cargo never touches anything in here.
- Every file in this tree carries a fate-marker header (first line of every
  source file) naming:
  1. Its original path in pattern_core.
  2. Its destination crate + path.
  3. The phase (and where relevant, the AC) that will consume it.
  4. Any special note about required reshape.
- The port-list doc (`docs/plans/rewrite-v3-portlist.md`) "Staging contents"
  section is the authoritative index. Every file in here must be listed there.
- Files do not leave this directory by being edited in place. They leave by
  being pulled, reshaped, and committed into their final home. When all files
  destined for a crate have been consumed, the staging subdirectory is deleted
  in a dedicated commit.

## Layout

Organised by destination crate, not by origin:

- `agent_runtime/` — destined for pattern_runtime (Phase 3: loop, checkpoint, router)
- `runtime_subsystems/` — destined for pattern_runtime (tool registry, coordination, sources, realtime, queue)
- `context/` — destined for pattern_provider/compose (Phase 5: compression, block rendering)
- `provider/` — destined for pattern_provider (Phase 4: oauth, ModelProvider impls, embeddings)

## Fate-marker header format

Every source file in this tree begins with:

```rust
// MOVING TO: <destination-crate>/<relative-path>
// ORIGIN: <original-pattern_core-path>
// PHASE: <N> (AC: <ac-ids-if-any>)
// RESHAPE: <none | brief-note>
//
// This file is retained verbatim for reference during the v3 foundation
// rewrite. It does not compile in this location; rewrite-staging/ is not a
// cargo workspace member.
```

## When a subdirectory is drained

```bash
# After Phase 3 fully absorbs rewrite-staging/agent_runtime/:
jj new -m "[meta] remove drained staging dir: agent_runtime (absorbed by pattern_runtime)"
rm -r rewrite-staging/agent_runtime
# Update port-list "Staging contents" section accordingly in the same change.
```
```

**Step 2: Create directory structure**

```bash
cd /home/orual/Projects/PatternProject/pattern
mkdir -p rewrite-staging/{agent_runtime,runtime_subsystems,context,provider}
touch rewrite-staging/.gitkeep
```

**Step 3: Verify**

```bash
test -d rewrite-staging && test -f rewrite-staging/README.md && echo ok
cargo metadata --no-deps --format-version 1 | jq -r '.workspace_members[]' | grep staging && echo "ERROR: staging should not be a workspace member" || echo "ok: staging not a cargo member"
```

**Commit:**

```bash
jj describe -m "[meta] create rewrite-staging holding pen for v3 phase 2 relocations"
jj new
```
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Write migration manifest

**Verifies:** contributes to AC1.4, AC1.7 (authoritative map of what's going where).

**Files:**
- Create: `/home/orual/Projects/PatternProject/pattern/rewrite-staging/migration-manifest.md`

**Step 1: Walk pattern_core source tree**

```bash
cd /home/orual/Projects/PatternProject/pattern
find crates/pattern_core/src -type f -name '*.rs' | sort > /tmp/pattern-core-files.txt
wc -l /tmp/pattern-core-files.txt
```

**Step 2: Write manifest**

Produce `rewrite-staging/migration-manifest.md` with one row per source file, in a table:

```markdown
# Phase 2 Migration Manifest

Generated 2026-04-16. Authoritative until this phase completes; archived after.

## Columns

- **Origin path** — path inside `crates/pattern_core/src/` as of Phase 1 end.
- **Disposition** — `keep` / `stage` / `split` / `delete` / `extract-then-stage`.
- **Destination** — final home (crate + path), `/dev/null` for deletes, `rewrite-staging/<subdir>` for stages.
- **Phase** — consuming phase.
- **Notes** — any reshape guidance.

## Manifest

| Origin path | Disposition | Destination | Phase | Notes |
|---|---|---|---|---|
| `agent/mod.rs` | stage | `rewrite-staging/agent_runtime/agent_mod.rs` | 3 | Kept verbatim; reshape during runtime integration |
| `agent/traits.rs` | extract-then-stage | `pattern_core/src/traits/agent_runtime.rs` + `traits/session.rs`; legacy shape → `rewrite-staging/agent_runtime/legacy_agent_traits.rs` | 2 (this phase) + 3 | Agent trait body decomposes into AgentRuntime + Session |
| `agent/processing/loop_impl.rs` | stage | `rewrite-staging/agent_runtime/loop_impl.rs` | 3 | Gutting required: Tidepool substrate replaces direct async loop |
| `agent/processing/` remainder | stage | `rewrite-staging/agent_runtime/processing/` | 3 | See Phase 3 for decomposition |
| `coordination/mod.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/mod.rs` | future-subagent | "Left intact" per design; reshape during subagent plan |
| `coordination/**` | stage | `rewrite-staging/runtime_subsystems/coordination/**` | future-subagent | — |
| `context/mod.rs` | split | `DEFAULT_BASE_INSTRUCTIONS` → `pattern_core/src/base_instructions.rs`; rest → `rewrite-staging/context/mod.rs` | 2 + 5 | Constant extracted; builder logic stages |
| `context/builder.rs` | stage | `rewrite-staging/context/builder.rs` | 5 | Lines 226–316 (block render) become composer input |
| `context/compression.rs` | stage | `rewrite-staging/context/compression.rs` | 5 | Four strategies retained; callsites swap to provider-reported token counts |
| `context/**` remainder | stage | `rewrite-staging/context/**` | 5 | — |
| `data_source/mod.rs` | split | traits → `pattern_core/src/traits/{data_stream,source_manager}.rs`; impls → `rewrite-staging/runtime_subsystems/data_source/` | 2 + 3 | Trait shapes refined; concrete sources stage |
| `data_source/**` | stage | `rewrite-staging/runtime_subsystems/data_source/**` | 3 | — |
| `db/` | extract-then-stage | trait shapes (if any) → `pattern_core/src/traits/`; rest → `rewrite-staging/` | 2 | Audit contents in Task 9 |
| `embeddings/mod.rs` | split | trait → `pattern_core/src/traits/embedder.rs` (if it has one); impls → `rewrite-staging/provider/embeddings/` | 2 + future | — |
| `embeddings/**` | stage | `rewrite-staging/provider/embeddings/**` | future | — |
| `error.rs` | rewrite-in-place | `pattern_core/src/error/` (split into CoreError/RuntimeError/ProviderError/MemoryError) | 2 | See Task 13 |
| `export/` | keep | pattern_core | — | Fate comment: may reshape if file-format plan lands |
| `config/` | keep | pattern_core | — | Fate comment: breakup needed in future config-cleanup plan |
| `id.rs` | absorb | `pattern_core/src/types/ids.rs` | 2 | Merge existing define_id_type! newtypes with new WorkspaceId/ProjectId |
| `lib.rs` | rewrite | `pattern_core/src/lib.rs` (replace exports with traits/types/error/memory surface) | 2 | — |
| `memory/cache.rs` | keep | pattern_core | — | Preserved verbatim per design |
| `memory/document.rs` | keep | pattern_core | — | Preserved |
| `memory/schema.rs` | keep | pattern_core | — | Preserved |
| `memory/store.rs` | keep (refined) | pattern_core/src/traits/memory_store.rs (trait) + memory/store.rs (impl) | 2 | Trait split from impl per Task 16 |
| `memory_acl/**` | keep | pattern_core | — | — |
| `messages/mod.rs` | split | value types → `pattern_core/src/types/message.rs`; storage helpers → `rewrite-staging/runtime_subsystems/messages/` | 2 | Audit per Task 9 |
| `model/mod.rs` | split | trait shape → `pattern_core/src/traits/provider_client.rs`; impls → `rewrite-staging/provider/model/` | 2 + 4 | See Task 17 for ProviderClient design |
| `model/**` | stage | `rewrite-staging/provider/model/**` | 4 | — |
| `oauth/**` | stage | `rewrite-staging/provider/oauth/**` | 4 | Absorbs into pattern_provider/auth |
| `permission/**` | keep | pattern_core | — | — |
| `prompt_template/**` | delete | — | 2 | Unused per user directive. git history preserves the pre-v3 implementation for reference when a future adaptive-base-prompt-templating plan revisits the concept; see Phase 4's "What this phase deliberately does NOT do" note. |
| `queue/**` | split | trait/types → `pattern_core/src/{traits,types}/`; impls → `rewrite-staging/runtime_subsystems/queue/` | 2 + future | Rework expected |
| `realtime/**` | split | trait/types → `pattern_core/src/{traits,types}/`; impls → `rewrite-staging/runtime_subsystems/realtime/` | 2 + future | Rework expected |
| `runtime/router.rs` | stage | `rewrite-staging/agent_runtime/router.rs` | 3 | MessageRouter trait extracted in Task 19 |
| `runtime/**` | stage | `rewrite-staging/agent_runtime/runtime/**` | 3 | — |
| `tool/**` | stage | `rewrite-staging/runtime_subsystems/tool/**` | 3 + plugin-system | Registry + DynamicTool reshape in plugin plan |
| `users/**` | delete | — | 2 | Vestigial per user directive |
| `utils/**` | audit | `pattern_core/src/utils/` for reusable helpers; rest → `rewrite-staging/runtime_subsystems/utils/` | 2 | Task 9 audits contents |
```

**Step 2a:** If the actual file tree differs from the rows above (e.g., module exists but isn't listed), add rows. The manifest must cover every `.rs` file in `crates/pattern_core/src/` as of Phase 1 end. Use `diff /tmp/pattern-core-files.txt <(awk -F'|' '/^\| `/{print $2}' rewrite-staging/migration-manifest.md | tr -d '` ')` to verify no file is missing.

**Step 3: Verify**

```bash
# Every .rs file in pattern_core must appear in the manifest.
comm -23 <(find crates/pattern_core/src -type f -name '*.rs' -printf '%P\n' | sort) \
         <(awk -F'|' '/^\| `/{gsub(/[` ]/,"",$2); print $2}' rewrite-staging/migration-manifest.md | sort)
```
Expected: empty output (every file accounted for).

**Commit:**

```bash
jj describe -m "[meta] phase 2 migration manifest: enumerate every pattern_core file with disposition"
jj new
```
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-10) -->
<!-- START_TASK_3 -->
### Task 3: Delete retired modules

**Verifies:** AC1.6 edge (retired crate/module references fail loudly downstream).

**Files:**
- Delete: `crates/pattern_core/src/prompt_template/` (entire subtree)
- Delete: `crates/pattern_core/src/users/` (entire subtree)
- Modify: `crates/pattern_core/src/lib.rs` (remove `pub mod prompt_template;` and `pub mod users;`)

**Step 1:** Confirm no internal references survive.

```bash
cd /home/orual/Projects/PatternProject/pattern
rg 'prompt_template|crate::users|use.*users::' crates/pattern_core/src/ --files-with-matches
```
If the grep finds anything, list the hits and fix them (delete imports, inline constants, etc.) before the `rm`. If a surviving `permission/` or other kept module references these, that's a flag — either the reference is dead (delete) or the dependency is real (surface to user before proceeding).

**Step 2:** Remove.

```bash
rm -r crates/pattern_core/src/prompt_template
rm -r crates/pattern_core/src/users
```

**Step 3:** Edit `crates/pattern_core/src/lib.rs` to drop the two `pub mod` lines.

**Step 4:** Verify.

```bash
cargo check -p pattern_core 2>&1 | tail -40
```
Expect errors — lots of downstream code still references removed modules. That's fine for this task; fixes accumulate through Subcomponent B.

**Commits:** Two dedicated deletion commits, per the port-list retired-directory policy:

```bash
jj describe -m "[pattern-core] remove retired module: prompt_template (unused)"
jj new
# … after users/ delete:
jj describe -m "[pattern-core] remove retired module: users (vestigial)"
jj new
```

Do each delete in its own change so bisection can point at either.
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Stage agent_runtime subsystems

**Verifies:** AC1.7 (fate markers), AC1.9 (no unmarked in-flight code), contributes to AC1.1 (post-relocation compile).

**Relocations (physical `git mv` / `jj`-aware mv where possible):**
- `crates/pattern_core/src/agent/` → `rewrite-staging/agent_runtime/agent/`
- `crates/pattern_core/src/runtime/` → `rewrite-staging/agent_runtime/runtime/`

**Step 1:** Preserve `agent/traits.rs` temporarily. Its legacy shape is the input for Task 18's new `AgentRuntime` + `Session` trait split. Copy it out first:

```bash
cp crates/pattern_core/src/agent/traits.rs /tmp/legacy_agent_traits.rs
```

**Step 2:** Move the directories.

```bash
mv crates/pattern_core/src/agent rewrite-staging/agent_runtime/agent
mv crates/pattern_core/src/runtime rewrite-staging/agent_runtime/runtime
```

**Step 3:** Place the preserved legacy traits file in staging for reference.

```bash
mv /tmp/legacy_agent_traits.rs rewrite-staging/agent_runtime/legacy_agent_traits.rs
```

**Step 4:** Fate-marker headers. Every `.rs` file in `rewrite-staging/agent_runtime/**` gets a header prepended per the README format. Commit a helper script at `scripts/stage-header.sh` (first-used here, reused across Tasks 4–9):

```bash
# scripts/stage-header.sh — prepend fate-marker headers to staged files.
#
# REMOVE-WHEN: rewrite-staging/ is fully drained and deleted (end of whichever
# phase absorbs the last staged module). Delete in the same commit as the final
# staging teardown.
#
# Usage: stage-header.sh <dest-crate-path> <origin-path> <phase-id> <reshape-note> <file>

cat >scripts/stage-header.sh <<'SH'
#!/usr/bin/env bash
set -euo pipefail
dest_crate="$1"; origin_path="$2"; phase="$3"; reshape="$4"; file="$5"
header="// MOVING TO: ${dest_crate}\n// ORIGIN: ${origin_path}\n// PHASE: ${phase}\n// RESHAPE: ${reshape}\n//\n// This file is retained verbatim for reference during the v3 foundation rewrite.\n// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.\n\n"
tmp=$(mktemp)
printf '%b' "$header" > "$tmp"
cat "$file" >> "$tmp"
mv "$tmp" "$file"
SH
chmod +x scripts/stage-header.sh
```

Apply per destination (example for loop_impl.rs):

```bash
scripts/stage-header.sh \
  "pattern_runtime/src/loop_impl.rs" \
  "crates/pattern_core/src/agent/processing/loop_impl.rs" \
  "3" \
  "Replace async loop body with Tidepool instantiate/step/yield cycle per Phase 3 design" \
  rewrite-staging/agent_runtime/agent/processing/loop_impl.rs
```

Do this for every `.rs` under `rewrite-staging/agent_runtime/**`. Use the manifest's Destination column as the `dest_crate` arg. The script commits as part of this task's change — do not leave it as a `/tmp` artifact.

**Step 5:** Remove `pub mod agent;` and `pub mod runtime;` from `crates/pattern_core/src/lib.rs`.

**Step 6:** Verify fate markers.

```bash
# Every .rs in agent_runtime/ must have the MOVING TO header.
find rewrite-staging/agent_runtime -type f -name '*.rs' | while read f; do
  head -1 "$f" | grep -q '^// MOVING TO:' || echo "MISSING header: $f"
done
```
Expected: no output.

**Commit:**

```bash
jj describe -m "[pattern-core] stage agent/ and runtime/ to rewrite-staging/agent_runtime/ (destined for pattern_runtime in phase 3)"
jj new
```

Expect `cargo check -p pattern_core` to still be broken after this task. Fine.
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Stage coordination, tool, data_source subsystems

**Verifies:** AC1.7, AC1.9.

**Relocations:**
- `crates/pattern_core/src/coordination/` → `rewrite-staging/runtime_subsystems/coordination/`
- `crates/pattern_core/src/tool/` → `rewrite-staging/runtime_subsystems/tool/`
- `crates/pattern_core/src/data_source/` → `rewrite-staging/runtime_subsystems/data_source/` (after extracting trait shapes — see Task 19; for this task, move wholesale and Task 19 copies the trait definitions back out)

**Step 1:** Move.

```bash
mv crates/pattern_core/src/coordination rewrite-staging/runtime_subsystems/coordination
mv crates/pattern_core/src/tool         rewrite-staging/runtime_subsystems/tool
mv crates/pattern_core/src/data_source  rewrite-staging/runtime_subsystems/data_source
```

**Step 2:** Fate-marker headers on every `.rs` file. Destinations per the manifest:
- coordination → `pattern_runtime/src/coordination/<path>` (phase: future-subagent; reshape: "Full reshape pending subagent-primitives plan")
- tool → `pattern_runtime/src/sdk/tool_bridge.rs` for registry bits; concrete tool impls → plugin-system plan (phase: 3 + plugin-system)
- data_source → `pattern_runtime/src/sources/<path>` (phase: 3; reshape: "Trait surface extracted in phase 2; concrete backends reshape here")

**Step 3:** Remove the three `pub mod` lines from lib.rs.

**Step 4:** Verify header coverage (same grep as Task 4).

**Commit:**

```bash
jj describe -m "[pattern-core] stage coordination/, tool/, data_source/ to rewrite-staging/runtime_subsystems/"
jj new
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Stage context/, extracting DEFAULT_BASE_INSTRUCTIONS

**Verifies:** AC1.7, AC1.9, contributes to Phase 5 (preserves composer input).

**Files:**
- Extract: `DEFAULT_BASE_INSTRUCTIONS` constant from `crates/pattern_core/src/context/mod.rs:27-78` → `crates/pattern_core/src/base_instructions.rs`
- Relocate: `crates/pattern_core/src/context/` → `rewrite-staging/context/`
- Modify: `crates/pattern_core/src/lib.rs` (remove `pub mod context;`, add `pub mod base_instructions;`, add `pub use base_instructions::DEFAULT_BASE_INSTRUCTIONS;`)

**Step 1:** Read the current constant.

```bash
sed -n '27,78p' crates/pattern_core/src/context/mod.rs > /tmp/base_instructions.txt
cat /tmp/base_instructions.txt | head -5
```

Confirm the file actually contains the constant at those lines. If line numbers drift post-Phase-1, find it by `rg 'DEFAULT_BASE_INSTRUCTIONS'`.

**Step 2:** Write `crates/pattern_core/src/base_instructions.rs`.

```rust
//! Pattern's default base instructions, preserved verbatim across the v3 rewrite.
//!
//! This constant is positioned byte-for-byte in segment 1 of the three-segment
//! cache layout (see Phase 5). Changing it invalidates every cached segment-1
//! prefix across every persona, so it is stabilised here as a first-class
//! module rather than living inside the composer.

/// Pattern's default instructions about burst consciousness, memory-as-continuity,
/// and authenticity. Copied verbatim from pre-rewrite `context/mod.rs`; Phase 5's
/// composer emits it as part of the system-prompt prefix.
pub const DEFAULT_BASE_INSTRUCTIONS: &str = r#"
<<< PASTE THE CONTENTS OF /tmp/base_instructions.txt HERE, BYTE-FOR-BYTE.
    Do not paraphrase, reformat, re-indent, or alter whitespace. The file
    was captured in Step 1 of this task via `sed -n '27,78p'`. If the
    captured text contains `"#`, extend the raw-string delimiter with
    more `#` characters (e.g. `r##"..."##`) until the delimiter is
    unique within the content. Byte-for-byte match is part of AC7.4. >>>
"#;
```

The placeholder block above is a directive to the executor, not literal content — do not land the `<<< ... >>>` text in source. Open `/tmp/base_instructions.txt` from Step 1 and paste its full contents where the placeholder is.

**Step 3:** Relocate the context module.

```bash
mv crates/pattern_core/src/context rewrite-staging/context
```

**Step 4:** Fate markers:
- `compression.rs` → `pattern_provider/src/compose/compression.rs`; phase 5; reshape: "Token-count call sites become async, consuming ProviderClient::count_tokens"
- `builder.rs` → `pattern_provider/src/compose/memory_render.rs` (specifically lines 226–316 — block rendering logic); phase 5; reshape: "Output rendered as [memory:current_state] pseudo-turn, not inline in system prompt"
- `builder.rs` remainder → possibly split; file header notes "lines 226–316 are the block-render excerpt; remainder reshapes as provider composer glue"
- Other files → fate marker with phase 5 and reshape note per their purpose.

**Step 5:** Update lib.rs.

```rust
// Remove: pub mod context;
pub mod base_instructions;
pub use base_instructions::DEFAULT_BASE_INSTRUCTIONS;
```

**Step 6:** Verify.

```bash
cargo check -p pattern_core 2>&1 | grep -E 'DEFAULT_BASE_INSTRUCTIONS|context' | head -20
```
Expected: no errors referencing `DEFAULT_BASE_INSTRUCTIONS`. Errors about `context::*` re-exports are expected; those get cleaned up in Task 21's lib.rs rewrite.

**Commit:**

```bash
jj describe -m "[pattern-core] extract DEFAULT_BASE_INSTRUCTIONS; stage context/ to rewrite-staging/ (destined for pattern_provider/compose in phase 5)"
jj new
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Stage oauth, model, embeddings (provider-destined)

**Verifies:** AC1.7, AC1.9.

**Relocations:**
- `crates/pattern_core/src/oauth/` → `rewrite-staging/provider/oauth/`
- `crates/pattern_core/src/model/` → `rewrite-staging/provider/model/` (after extracting trait shape — see Task 17; move wholesale for this task, Task 17 copies back)
- `crates/pattern_core/src/embeddings/` → `rewrite-staging/provider/embeddings/` (same — Task 19 extracts trait if any)

**Step 1:** Move.

```bash
mv crates/pattern_core/src/oauth      rewrite-staging/provider/oauth
mv crates/pattern_core/src/model      rewrite-staging/provider/model
mv crates/pattern_core/src/embeddings rewrite-staging/provider/embeddings
```

**Step 2:** Fate markers:
- oauth → `pattern_provider/src/auth/<path>`; phase 4; reshape: "Absorbs into three-tier resolver; pattern_auth crate retires in same phase"
- model → `pattern_provider/src/<path>`; phase 4; reshape: "ProviderClient trait lands in pattern_core in this phase; impls reshape in phase 4 atop rebased rust-genai"
- embeddings → `pattern_provider/src/embeddings/<path>`; phase: future; reshape: "Deferred until provider-side embedding use case confirmed"

**Step 3:** Drop `pub mod oauth;`, `pub mod model;`, `pub mod embeddings;` from lib.rs.

**Step 4:** Verify headers.

**Commit:**

```bash
jj describe -m "[pattern-core] stage oauth/, model/, embeddings/ to rewrite-staging/provider/"
jj new
```
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Split realtime and queue (trait/types in core, impls staged)

**Verifies:** AC1.3 (trait shapes satisfiable), AC1.7.

**Files:**
- Audit: `crates/pattern_core/src/realtime/` and `crates/pattern_core/src/queue/` — identify the trait shapes and value types worth preserving vs the concrete impls.
- Create: `crates/pattern_core/src/traits/realtime.rs` (trait definitions extracted, whatever they are — likely something like `RealtimeBus` / subscriber / event publisher).
- Create: `crates/pattern_core/src/traits/queue.rs` (trait definitions for whatever queue abstraction exists).
- Create: `crates/pattern_core/src/types/realtime.rs` (value types — event payloads, subscription descriptors).
- Create: `crates/pattern_core/src/types/queue.rs` (task descriptors, etc.).
- Relocate: remaining impls → `rewrite-staging/runtime_subsystems/realtime/` and `rewrite-staging/runtime_subsystems/queue/`.

**Step 1:** Audit.

```bash
rg 'pub trait ' crates/pattern_core/src/realtime/ crates/pattern_core/src/queue/
rg 'pub (struct|enum) ' crates/pattern_core/src/realtime/ crates/pattern_core/src/queue/
```

List every trait and every public value type. Decide per item whether it's trait-surface, value-type, or impl. If the module is mostly impl (concrete bus implementation, runtime integration, etc.) and the trait surface is minimal, extract just the trait and value types to the new core locations, stage the rest.

**Step 2:** Extract.

Write `pattern_core/src/traits/realtime.rs` containing only the trait definitions (with rustdoc). Same for `queue.rs`. Value types go to `types/realtime.rs` and `types/queue.rs`.

If the existing code's trait shape is bad legacy, rewrite the trait shape — this is part of the clean-rewrite mandate. Don't copy bad shapes verbatim. Surface the decision in the commit message.

**Step 3:** Relocate impls.

```bash
mv crates/pattern_core/src/realtime rewrite-staging/runtime_subsystems/realtime
mv crates/pattern_core/src/queue    rewrite-staging/runtime_subsystems/queue
```

**Step 4:** Fate markers on staged impls — phase: future; reshape: "Full rework expected; trait shape in pattern_core may be refined when rework happens".

**Step 5:** Update lib.rs: remove `pub mod realtime;` and `pub mod queue;`.

**Commit:**

```bash
jj describe -m "[pattern-core] split realtime/ and queue/: trait shapes and value types extracted to traits/types/; impls staged for future rework"
jj new
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Audit and split messages/, db/, utils/

**Verifies:** AC1.7, contributes to AC1.1.

**Files:**
- Audit: `crates/pattern_core/src/messages/`, `crates/pattern_core/src/db/`, `crates/pattern_core/src/utils/`.
- Create: `crates/pattern_core/src/types/message.rs` (value types extracted from `messages/`).
- Create: `crates/pattern_core/src/traits/db.rs` if `db/` exposes a trait abstraction (e.g., `DbStore`, `Connection`).
- Possibly create: `crates/pattern_core/src/utils.rs` + `utils/<submodule>.rs` for helpers worth keeping.
- Relocate: impl bits → `rewrite-staging/runtime_subsystems/`.

**Step 1:** Audit `messages/`.

```bash
ls crates/pattern_core/src/messages/
rg 'pub (struct|enum|trait|fn) ' crates/pattern_core/src/messages/
```

Current shape per investigator: `Message` type with `id`, `role`, `owner_id`, `content`, `metadata`, `options`, `has_tool_calls`, `word_count`, `created_at`, `position`, `batch`, `sequence_num`, `batch_type`. This is largely a value type but may carry storage/serialization helpers that drag in concrete DB deps. Keep the value shape; stage anything pulling in surreal/sqlx/etc. from pattern_db directly.

Destination for kept bits: `pattern_core/src/types/message.rs`. Refine the shape if it has bad-legacy fields (e.g., snowflake position may belong elsewhere; surface to user only if the refactor would be non-trivial).

**Step 2:** Audit `db/`.

```bash
ls crates/pattern_core/src/db/
rg 'pub trait ' crates/pattern_core/src/db/
```

If `db/` exposes trait abstractions, extract to `traits/`. Concrete adapters → staging. If it's purely concrete code, stage wholesale.

**Step 3:** Audit `utils/`.

```bash
ls crates/pattern_core/src/utils/
```

Utils subfolders are commonly mixed. Keep pure helpers (formatting, time helpers, pure-function stuff); stage anything entangled with runtime concerns. If `utils/` is a single file, consider whether inlining into dependents is better than keeping a named module.

**Step 4:** Execute the splits per audit.

- Messages value types → `pattern_core/src/types/message.rs`.
- Messages storage/runtime helpers → `rewrite-staging/runtime_subsystems/messages/`.
- DB traits → `pattern_core/src/traits/db.rs` (if any).
- DB impls → `rewrite-staging/runtime_subsystems/db/`.
- Utils kept pieces → `pattern_core/src/utils/` (with `module.rs` + `module/submodule.rs` layout per Task 11).
- Utils staged pieces → `rewrite-staging/runtime_subsystems/utils/`.

**Step 5:** Fate markers on staged files.

**Step 6:** Update lib.rs: drop old `pub mod messages; pub mod db; pub mod utils;` and wire new module paths (exact lib.rs rewrite in Task 21).

**Commit:**

```bash
jj describe -m "[pattern-core] split messages/, db/, utils/: value types and traits kept in core; impls staged"
jj new
```
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: Intermediate checkpoint — let it be broken

**Verifies:** nothing directly; sanity checkpoint before Subcomponent C.

**Step 1:** Confirm what's left.

```bash
ls crates/pattern_core/src/
```

Expected survivors: `memory/`, `memory_acl/`, `permission/`, `export/`, `config/`, `base_instructions.rs`, `lib.rs`, `error.rs`, `id.rs`, and any splits from Tasks 8–9 (`types/` dir may already exist from Task 8; same for `traits/`). NO `agent/`, `runtime/`, `coordination/`, `tool/`, `data_source/`, `context/`, `oauth/`, `model/`, `embeddings/`, `realtime/`, `queue/`, `prompt_template/`, `users/`, `messages/` (moved/split), `db/` (moved/split), `utils/` (possibly split).

**Step 2:** Confirm compile state is broken.

```bash
cargo check -p pattern_core 2>&1 | tail -5
```

Expected: errors. The `lib.rs` still references removed paths, and `memory/`, `error.rs`, `export/`, `config/` likely pull in types that moved. This is fine — Tasks 11–22 repair it.

**No commit.** This is an observation task.
<!-- END_TASK_10 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (task 11) -->
<!-- START_TASK_11 -->
### Task 11: Modernize module layout

**Verifies:** contributes to AC1.1, AC1.2.

For every retained pattern_core module that currently uses `foo/mod.rs`, convert to `foo.rs` + `foo/submodule.rs` per rust-coding-style.

**Modules to convert (audit after Tasks 3–10; this list reflects expected survivors):**
- `memory/mod.rs` → `memory.rs` + `memory/{cache,document,schema,store}.rs`
- `memory_acl/mod.rs` → `memory_acl.rs` + submodules
- `permission/mod.rs` → `permission.rs` + submodules
- `export/mod.rs` → `export.rs` + submodules
- `config/mod.rs` → `config.rs` + submodules
- `traits/mod.rs` → `traits.rs` + `traits/<name>.rs` (created fresh in Task 16–19)
- `types/mod.rs` → `types.rs` + `types/<name>.rs` (created fresh in Task 12)
- `error/mod.rs` → `error.rs` + `error/<name>.rs` (if we split error into submodules; see Task 13)
- `utils/mod.rs` (if surviving) → `utils.rs` + submodules

**Step 1:** For each convertee, do:

```bash
# Example for memory/:
git mv crates/pattern_core/src/memory/mod.rs crates/pattern_core/src/memory.rs
# If memory.rs would conflict with an existing sibling file, rename conflict resolution first.
```

**Step 2:** Verify no broken `mod.rs` references remain.

```bash
find crates/pattern_core/src -name mod.rs
```
Expected: empty output.

**Step 3:** Compile check.

```bash
cargo check -p pattern_core 2>&1 | tail -20
```
Still broken (lib.rs and internal imports need updating). Continue.

**Commit:**

```bash
jj describe -m "[pattern-core] modernize module layout: foo/mod.rs -> foo.rs + foo/submodule.rs"
jj new
```
<!-- END_TASK_11 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 12-15) -->
<!-- START_TASK_12 -->
### Task 12: Types surface (IDs, Block, Message, Caller, Turn I/O, Snapshots)

**Verifies:** AC1.2 (rustdoc), AC1.3 (satisfiability via doctests in Task 20).

**Files:**
- Create: `crates/pattern_core/src/types.rs` (module root with re-exports)
- Create: `crates/pattern_core/src/types/ids.rs` (absorbs existing `id.rs`; adds `WorkspaceId`, `ProjectId`)
- Create: `crates/pattern_core/src/types/block.rs` (`Block`, `BlockHandle`)
- Create: `crates/pattern_core/src/types/message.rs` (refined `Message` from Task 9)
- Create: `crates/pattern_core/src/types/caller.rs` (`Caller` enum: `Agent(AgentId)` / `Human(UserId)`)
- Create: `crates/pattern_core/src/types/turn.rs` (`TurnInput`, `TurnOutput`, `TurnId`)
- Create: `crates/pattern_core/src/types/snapshot.rs` (`PersonaSnapshot`, `SessionSnapshot`)
- Possibly create: `crates/pattern_core/src/types/realtime.rs`, `types/queue.rs` (from Task 8)
- Delete: `crates/pattern_core/src/id.rs` (after absorbing into `types/ids.rs`)

**Implementation:**

Each type follows rust-coding-style: `#[non_exhaustive]` on enums, newtype wrappers with validation, thorough rustdoc including at least one code example.

**IDs — reuse the existing `define_id_type!` macro from the old `id.rs`.** That macro handles UUID-based newtype + display/from_str/serde. Add `WorkspaceId` and `ProjectId` as new macro invocations alongside the existing ones. Preserve `AgentId`'s custom non-UUID shape if it exists (investigator noted it accepts arbitrary strings).

**Caller** — enum with two mandatory variants:

```rust
/// Who is initiating a turn or writing to memory.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Caller {
    /// An agent acting on its own, typically mid-loop or via scheduled wake.
    Agent(AgentId),
    /// A human interacting via some transport (CLI, Discord, etc.).
    /// Transport-specific identity lives on the accompanying Message/TurnInput.
    Human(UserId),
}
```

Non-exhaustive because future subagent/plugin sources may add variants (e.g., `Plugin(PluginId)`, `Scheduler`).

**TurnInput / TurnOutput** — shape should make a single agent turn explicit and checkpointable. At minimum: `TurnInput { caller: Caller, messages: Vec<Message>, turn_id: TurnId }` and `TurnOutput { messages: Vec<Message>, block_writes: Vec<BlockWrite>, usage: Option<Usage>, completed_at: Timestamp }`. Exact shape: task-implementor generates fresh, matching what the Phase 3 agent loop actually produces. Include rustdoc explaining the turn boundary contract.

**Block / BlockHandle** — `Block` is the value type read out of storage (what the composer renders). `BlockHandle` is the identifier by which agents reference it (label or id). If the existing `memory/store.rs` already has `BlockMetadata` or similar, harmonize — don't duplicate.

**PersonaSnapshot / SessionSnapshot** — the checkpoint types used by Phase 3's `Session::checkpoint()`/`restore()`. `SessionSnapshot` captures the Tidepool `EnvSnapshot` plus any persona-scoped state needed to deterministically restart a turn. Implementation detail deferred to Phase 3; Phase 2 lands the type shape.

**Step 1:** Write each file. Use doctests showing construction for every public type (AC1.2 requires doctests on public surface).

**Step 2:** Update `types.rs` (module root) to re-export each submodule's public items.

**Step 3:** Add `pub mod types;` and `pub use types::*;` (or explicit re-exports) to `lib.rs`.

**Step 4:** Delete the old `id.rs`.

**Step 5:** Verify.

```bash
cargo check -p pattern_core 2>&1 | grep -E 'types|ids|Caller|Turn|Snapshot|Block' | head -20
```
Should narrow the error set; types compile even if trait surface isn't wired yet.

**Commit:**

```bash
jj describe -m "[pattern-core] types surface: IDs (absorb id.rs, add Workspace+Project), Block, Message, Caller, TurnInput/Output, PersonaSnapshot/SessionSnapshot"
jj new
```
<!-- END_TASK_12 -->

<!-- START_TASK_13 -->
### Task 13: Error hierarchy split

**Verifies:** AC1.1 (warnings), AC1.2 (docs), AC1.5 (trait satisfaction indirectly — error variants referenced by traits).

**Files:**
- Delete: `crates/pattern_core/src/error.rs` (old monolithic 220-line CoreError)
- Create: `crates/pattern_core/src/error.rs` (module root re-exporting submodule items) or `crates/pattern_core/src/error/` with submodules
- Create: `crates/pattern_core/src/error/core.rs` (top-level `CoreError`)
- Create: `crates/pattern_core/src/error/runtime.rs` (`RuntimeError`)
- Create: `crates/pattern_core/src/error/provider.rs` (`ProviderError`)
- Create: `crates/pattern_core/src/error/memory.rs` (`MemoryError`)
- Modify: `crates/pattern_core/src/lib.rs` (update `pub use error::*` re-exports)

**Implementation:**

Per rust-coding-style:
- Every error enum is `#[non_exhaustive]`.
- `#[derive(Debug, Error, Diagnostic)]` using `thiserror` + `miette`.
- Top-level `CoreError` wraps sub-errors transparently via `#[from]` variants; preserves `#[diagnostic(transparent)]` for sub-errors that already have diagnostic info.
- Required variants per design (not exhaustive of what's needed — extend as the implementer writes call sites):

`RuntimeError` needs: `Timeout { wall_ms: u64, cpu_ms: u64 }`, `EffectOverflow`, `GhcPanic { reason: String }`, `RuntimeCrashed`, `CheckpointFailed { reason: String }`.

`ProviderError` needs: `AuthFlowTimeout`, `RefreshFailed { source: ... }`, `CredentialStoreUnavailable`, `TokenCountFailed { source: ... }`, `RateLimited { retry_after: Duration }`, `RequestFailed { status: u16, body: Option<String> }`.

`MemoryError` needs: `BlockNotFound { handle: BlockHandle, available: Vec<BlockHandle> }`, `StoreCorrupted { detail: String }`, `ConcurrentWriteConflict { handle: BlockHandle }`.

`CoreError` wraps: `Runtime(#[from] RuntimeError)`, `Provider(#[from] ProviderError)`, `Memory(#[from] MemoryError)`, plus any variants needed for the kept non-subsystem-specific concerns (config errors, export errors if export stays trait-free).

**Design notes:**
- Errors carry structured context (fields), not stringly-typed wrappers.
- `#[label(...)]` for source-span labels only where a source span makes sense (not every variant).
- Every variant gets at least one line of rustdoc.

**Step 1:** Survey the old `error.rs` variants and categorise each into Core/Runtime/Provider/Memory. Capture in a comment at the top of each new file: "This file replaces the following variants from pre-v3 CoreError: [...]".

**Step 2:** Write each new file. Use doctests showing construction + `.to_string()` output for every variant.

**Step 3:** Update lib.rs to re-export `CoreError`, `RuntimeError`, `ProviderError`, `MemoryError`, `Result` typedef (`pub type Result<T> = std::result::Result<T, CoreError>;`).

**Step 4:** Verify.

```bash
cargo check -p pattern_core 2>&1 | tail -20
```

Errors in `memory/`, `export/`, `config/` that reference old variant names now need updating — fix them to use the new split hierarchy. Keep the fixes minimal (wire to the right variant; don't refactor their callsites further).

**Commit:**

```bash
jj describe -m "[pattern-core] split CoreError into CoreError/RuntimeError/ProviderError/MemoryError hierarchy"
jj new
```
<!-- END_TASK_13 -->

<!-- START_TASK_14 -->
### Task 14: Error variant doctests

**Verifies:** AC1.2 (doc completeness).

**Files:**
- Modify: each new `error/<name>.rs` file — add runnable doctests for each variant.

**Implementation pattern:**

```rust
/// Error indicating the Tidepool agent program exceeded its wall-clock or CPU budget.
///
/// # Example
///
/// ```
/// use pattern_core::error::RuntimeError;
///
/// let err = RuntimeError::Timeout { wall_ms: 30_000, cpu_ms: 10_000 };
/// assert!(err.to_string().contains("wall"));
/// ```
Timeout { wall_ms: u64, cpu_ms: u64 },
```

Every variant gets one doctest showing construction + display format assertion.

**Step 1:** Add doctests per the pattern.

**Step 2:** Run.

```bash
cargo test --doc -p pattern_core 2>&1 | tail -10
```

All doctests must pass.

**Commit:**

```bash
jj describe -m "[pattern-core] error doctests: one per variant, verifying construction and display"
jj new
```
<!-- END_TASK_14 -->

<!-- START_TASK_15 -->
### Task 15: Property tests for id roundtrips

**Verifies:** AC1.2 (documented behaviour exercised).

**Files:**
- Create: `crates/pattern_core/src/types/ids/tests.rs` or `tests/id_roundtrip.rs` (pick per convention used in `types/ids.rs` itself).

**Implementation:**

For each `*Id` newtype, a `proptest!` test asserting:
- `id.to_string().parse::<Id>() == Ok(id)` (roundtrip)
- Display format matches the documented shape (UUID, prefix, etc.).

**Step 1:** Add `proptest` as a dev-dependency to `crates/pattern_core/Cargo.toml`:

```toml
[dev-dependencies]
proptest = "1"
```

(Workspace-pin if proptest isn't already in workspace deps.)

**Step 2:** Write tests.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn agent_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = AgentId::from_uuid(uuid::Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: AgentId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }
    }
    // … one per ID type …
}
```

**Step 3:** Run.

```bash
cargo nextest run -p pattern_core id_roundtrip 2>&1 | tail -5
```

**Commit:**

```bash
jj describe -m "[pattern-core] proptest id roundtrips for all *Id newtypes"
jj new
```
<!-- END_TASK_15 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_SUBCOMPONENT_E (tasks 16-22) -->
<!-- START_TASK_16 -->
### Task 16: `MemoryStore` trait (refined)

**Verifies:** AC1.3 (dummy impl satisfies trait).

**Files:**
- Create: `crates/pattern_core/src/traits/memory_store.rs`
- Modify: `crates/pattern_core/src/memory/store.rs` — if the existing `MemoryStore` trait lives here, move the trait definition to `traits/memory_store.rs` and have `memory/store.rs` re-export + implement it.

**Implementation:**

Refine the existing trait. Preserve working async shape. Add methods needed for Phase 5's pseudo-message emission and pre-turn rendering:

- `create_block(&self, block: NewBlock) -> Result<BlockHandle, MemoryError>`
- `get_block(&self, handle: &BlockHandle) -> Result<Option<Block>, MemoryError>`
- `update_block(&self, handle: &BlockHandle, content: BlockContent, author: Caller) -> Result<(), MemoryError>`
- `list_blocks(&self, filter: BlockFilter) -> Result<Vec<Block>, MemoryError>` (supersedes `list_blocks_by_type`)
- `search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, MemoryError>` (hybrid FTS + vector, consumed from pattern_db)
- `block_changes_since(&self, turn: TurnId) -> Result<Vec<BlockChange>, MemoryError>` — Phase 5 pseudo-message emission consumes this
- `subscribe_writes(&self) -> BlockWriteStream` — optional, for realtime pseudo-message surfacing; may be stubbed Phase 2

**Associated types / helper types** live in `types/block.rs` (e.g., `NewBlock`, `BlockContent`, `BlockFilter`, `SearchQuery`, `SearchHit`, `BlockChange`, `BlockWriteStream`). Add any missing ones to Task 12's list.

If refining the existing trait would break many internal sites unnecessarily, flag to the user before changing method signatures. Additions are safe; renames and removes need discussion.

**Rustdoc:** full docs including an example dummy impl in the trait-level comment (AC1.3).

**Step 1:** Write the trait.

**Step 2:** Refactor the existing `memory/store.rs` to implement the trait at its new home. Update internal call sites in `memory/cache.rs`, `memory/document.rs` to use the refined method names where they changed.

**Step 3:** Verify.

```bash
cargo check -p pattern_core 2>&1 | grep -E 'MemoryStore|memory::store' | head -10
```

**Commit:**

```bash
jj describe -m "[pattern-core] MemoryStore trait: refine existing trait with phase-5 methods (block_changes_since, subscribe_writes); preserve storage impl"
jj new
```
<!-- END_TASK_16 -->

<!-- START_TASK_17 -->
### Task 17: `ProviderClient` trait

**Verifies:** AC1.3.

**Files:**
- Create: `crates/pattern_core/src/traits/provider_client.rs`

**Implementation:**

```rust
//! Provider-client trait: async LLM completion, token counting, usage capture.
//!
//! Implemented by `pattern_provider::AnthropicClient`. The trait is intentionally
//! minimal — rate limiting, retries, and session-UUID management are internal
//! concerns of the concrete impl, not surfaced here.

#[async_trait::async_trait]
pub trait ProviderClient: Send + Sync {
    /// Stream completion chunks for a composed request.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, ProviderError>>, ProviderError>;

    /// Return Anthropic-reported token count for a composed request.
    ///
    /// Used pre-request by compaction and context-length decisions; replaces the
    /// pre-v3 heuristic token approximation. See v3-foundation.AC5b.
    async fn count_tokens(
        &self,
        request: &CompletionRequest,
    ) -> Result<TokenCount, ProviderError>;

    /// Extract the provider-reported usage from a completed response.
    ///
    /// Called post-response to feed accurate counts into the subsequent
    /// estimation cache.
    fn usage(&self, response: &CompletionResponse) -> Usage;
}
```

Supporting types (`CompletionRequest`, `CompletionChunk`, `CompletionResponse`, `TokenCount`, `Usage`) live in `types/provider.rs` — create that submodule as part of this task.

Keep the trait minimal; extension points (cache TTL selection, shaping) belong inside `CompletionRequest` as fields or nested options, not as trait methods.

**Step 1:** Write the trait + request/response types.

**Step 2:** Update `types.rs` re-exports.

**Step 3:** Verify.

**Commit:**

```bash
jj describe -m "[pattern-core] ProviderClient trait: async complete + count_tokens + usage; supporting request/response types"
jj new
```
<!-- END_TASK_17 -->

<!-- START_TASK_18 -->
### Task 18: `AgentRuntime` + `Session` traits

**Verifies:** AC1.3.

**Files:**
- Create: `crates/pattern_core/src/traits/agent_runtime.rs`
- Create: `crates/pattern_core/src/traits/session.rs`

**Implementation:**

Decompose the pre-v3 `Agent` + concrete `AgentRuntime` into two trait layers:

- `AgentRuntime` — factory / supervisor. Owns the dependencies (memory store, provider client, message router). Spawns sessions.
- `Session` — per-turn execution. Holds the Haskell interpreter state, dispatches effects, captures checkpoints.

Per design §Forward-compatibility: "AgentRuntime trait designed around cosa-like semantics (per-statement observability, cheap fork, reifiable env) so a future cosa-native runtime plan can slot in without changing the trait".

Sketch (task-implementor fills in async signatures matching Phase 3's actual Tidepool bridge):

```rust
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync {
    type Session: Session;

    async fn open_session(
        &self,
        persona: PersonaSnapshot,
    ) -> Result<Self::Session, RuntimeError>;

    async fn shutdown(&self) -> Result<(), RuntimeError>;
}

#[async_trait::async_trait]
pub trait Session: Send {
    /// Execute one agent turn against the given input.
    async fn step(&mut self, input: TurnInput) -> Result<TurnOutput, RuntimeError>;

    /// Capture the session's environment for later restore.
    fn checkpoint(&self) -> Result<SessionSnapshot, RuntimeError>;

    /// Restore a captured environment.
    fn restore(&mut self, snapshot: SessionSnapshot) -> Result<(), RuntimeError>;
}
```

Associated-type `Session` vs trait object: prefer associated type for zero-cost dispatch; if Phase 3 needs heterogeneous sessions behind a trait object, the impl can expose an erased wrapper.

**Step 1:** Write both files with full rustdoc + doctest dummy impls demonstrating the associated-type dance.

**Step 2:** Verify.

**Commit:**

```bash
jj describe -m "[pattern-core] AgentRuntime + Session traits: cosa-compatible factory/session split, checkpoint surface"
jj new
```
<!-- END_TASK_18 -->

<!-- START_TASK_19 -->
### Task 19: `MessageRouter`, `DataStream`, `SourceManager` traits

**Verifies:** AC1.3.

**Files:**
- Create: `crates/pattern_core/src/traits/message_router.rs`
- Create: `crates/pattern_core/src/traits/data_stream.rs`
- Create: `crates/pattern_core/src/traits/source_manager.rs`

**Implementation:**

Extract / refine:

- `MessageRouter` — what pre-v3 `AgentMessageRouter` did but abstracted. Input: a `Message` + source descriptor. Output: dispatch decisions (which endpoint, which agent). Methods at least: `register_endpoint(&self, endpoint: Arc<dyn MessageEndpoint>)`, `route(&self, message: Message, origin: MessageOrigin) -> Result<RouteDecision, CoreError>`. Supporting `MessageEndpoint`, `MessageOrigin`, `RouteDecision` types in `types/routing.rs`.

- `DataStream` — async subscription to a data source. Method: `subscribe(&self) -> impl Stream<Item = StreamEvent>`. Concrete sources (ATProto, Discord, RSS, etc.) live in staging for this phase; trait stays here.

- `SourceManager` — owner of registered streams. Methods: `register(&mut self, name: SourceName, stream: Arc<dyn DataStream>)`, `list(&self) -> Vec<SourceName>`, `stream(&self, name: &SourceName) -> Option<Arc<dyn DataStream>>`.

Same pattern as prior tasks: rustdoc, doctest dummy impls.

**Step 1:** Write all three files.

**Step 2:** Verify.

**Commit:**

```bash
jj describe -m "[pattern-core] MessageRouter, DataStream, SourceManager traits: extracted from pre-v3 concrete types"
jj new
```
<!-- END_TASK_19 -->

<!-- START_TASK_20 -->
### Task 20: Dummy-impl doctests for every trait (AC1.3 + AC1.5)

**Verifies:** AC1.3 (satisfiable), AC1.5 (removing a method breaks compile).

**Files:**
- Modify: each `traits/<name>.rs` file — add or strengthen the dummy-impl doctest that lives in the trait-level doc comment.

**Pattern:**

```rust
//! ```
//! use pattern_core::traits::MemoryStore;
//! use pattern_core::types::{Block, BlockHandle, ...};
//! use pattern_core::error::MemoryError;
//! # use async_trait::async_trait;
//!
//! struct Dummy;
//!
//! #[async_trait]
//! impl MemoryStore for Dummy {
//!     async fn create_block(&self, _: NewBlock) -> Result<BlockHandle, MemoryError> {
//!         unimplemented!("dummy: satisfaction-only example; AC1.3")
//!     }
//!     // … every method, each returning unimplemented!("dummy: ..., AC1.3") …
//! }
//! ```
```

Per AC1.8, every `unimplemented!()` carries a comment naming the phase and AC — the doctest examples qualify by saying "AC1.3".

**Step 1:** Add a doctest per trait.

**Step 2:** Verify.

```bash
cargo test --doc -p pattern_core 2>&1 | tail -20
```

All trait-level doctests pass.

**Step 3:** Manually verify AC1.5 by removing one method from one dummy impl and running `cargo check -p pattern_core`. Expected: compile error naming the missing method. Document the verification in the commit message; then restore the method.

**Commit:**

```bash
jj describe -m "[pattern-core] dummy-impl doctests for every trait (AC1.3 satisfiability; AC1.5 verified manually)"
jj new
```
<!-- END_TASK_20 -->

<!-- START_TASK_21 -->
### Task 21: lib.rs rewrite + module wiring

**Verifies:** contributes to AC1.1, AC1.2.

**Files:**
- Rewrite: `crates/pattern_core/src/lib.rs`.

**Implementation:**

Replace the pre-v3 lib.rs wholesale. New shape:

```rust
//! # pattern_core
//!
//! Traits and types that every Pattern v3 component implements or consumes.
//! Contains no execution machinery — the runtime lives in `pattern_runtime`,
//! LLM integration in `pattern_provider`. Memory storage (loro CRDT + sqlite)
//! is preserved in this crate because it has no alternative home and its
//! storage semantics are stable across the rewrite.
//!
//! See `docs/design-plans/2026-04-16-v3-foundation.md` for the layering
//! rationale.

pub mod base_instructions;
pub mod config;
pub mod error;
pub mod export;
pub mod memory;
pub mod memory_acl;
pub mod permission;
pub mod traits;
pub mod types;
pub mod utils; // if kept by Task 9

// Common re-exports.
pub use base_instructions::DEFAULT_BASE_INSTRUCTIONS;
pub use error::{CoreError, MemoryError, ProviderError, Result, RuntimeError};
pub use traits::{
    AgentRuntime, DataStream, MemoryStore, MessageRouter, ProviderClient,
    Session, SourceManager,
    // plus any trait re-exports from realtime/queue splits
};
pub use types::{
    AgentId, Block, BlockHandle, Caller, Message, PersonaSnapshot, ProjectId,
    SessionSnapshot, TurnInput, TurnOutput, UserId, WorkspaceId,
    // plus other IDs from ids.rs
};
```

No `pub use` wildcards. Explicit is better than `*` glob re-exports for rustdoc clarity.

**Step 1:** Rewrite.

**Step 2:** Verify.

```bash
cargo check -p pattern_core 2>&1 | tail -20
```

Most errors should be gone. Remaining errors are import paths inside `memory/`, `export/`, `config/`, `memory_acl/`, `permission/` referring to moved types. Fix each by switching to the new `crate::types::...` / `crate::error::...` paths.

**Step 3:** `cargo check -p pattern_core` must pass.

**Commit:**

```bash
jj describe -m "[pattern-core] lib.rs rewrite: trait-only surface with explicit re-exports; internal imports updated"
jj new
```
<!-- END_TASK_21 -->

<!-- START_TASK_22 -->
### Task 22: Zero-warning compile check

**Verifies:** AC1.1.

**Step 1:** Run clippy with deny-warnings — this is the single warning-freeness gate.

```bash
cargo clippy -p pattern_core --all-features --all-targets -- -D warnings 2>&1 | tee /tmp/phase2-clippy.log
```

Clippy with `-D warnings` catches both rustc warnings and clippy lints; a separate `cargo check` warning grep is redundant. Exit code is non-zero if any warning fires.

**Step 2:** Fix every warning. Do NOT `#[allow(...)]` unless there's a real reason (document it in a comment). Common post-relocation warnings:
- Unused imports (delete).
- Dead code after trait extraction (delete, or mark with fate comment if it's actually staging material that slipped).
- Missing docs (add; AC1.2 enforces this).

**Step 3:** Re-run until the command succeeds (exit 0) with no warning lines in the log.

**Step 4:** `cargo nextest run -p pattern_core --lib` passes (surviving tests, post-relocation).

**Commit:**

```bash
jj describe -m "[pattern-core] phase 2 close: zero warnings on cargo check and clippy (AC1.1)"
jj new
```
<!-- END_TASK_22 -->
<!-- END_SUBCOMPONENT_E -->

<!-- START_SUBCOMPONENT_F (tasks 23-26) -->
<!-- START_TASK_23 -->
### Task 23: Zero-warning rustdoc

**Verifies:** AC1.2.

**Step 1:** Run.

```bash
cargo doc -p pattern_core --no-deps 2>&1 | tee /tmp/phase2-doc.log
grep -c warning /tmp/phase2-doc.log
```

Baseline before Phase 2 was 14 warnings, mostly in `config/`. After relocations, the count may be lower (if the warning sources staged out). Whatever remains must go to zero.

**Step 2:** Fix each warning.
- Broken intra-doc links: fix the path or use full path.
- Unclosed HTML tags (often in config.rs): fix or escape.
- Missing docs on public items: add rustdoc with at least one sentence.

**Step 3:** Re-run until zero.

**Step 4:** Open the docs locally (optional visual check):

```bash
cargo doc -p pattern_core --no-deps --open
```

Verify every trait, type, and error has a meaningful description — not just autogenerated placeholders.

**Commit:**

```bash
jj describe -m "[pattern-core] phase 2 close: cargo doc warning-free (AC1.2)"
jj new
```
<!-- END_TASK_23 -->

<!-- START_TASK_24 -->
### Task 24: Port-list "Staging contents" section

**Verifies:** AC1.4, AC1.7.

**Files:**
- Modify: `/home/orual/Projects/PatternProject/pattern/docs/plans/rewrite-v3-portlist.md` — append a "Staging contents" section.

**Implementation:**

Section structure:

```markdown
## Staging contents (`rewrite-staging/`)

Generated at end of Phase 2. Reflects every file in `rewrite-staging/` with
destination + phase. Drained by subsequent phases; this section shrinks as
files are absorbed. See `rewrite-staging/migration-manifest.md` for the full
per-file provenance.

### Destined for pattern_runtime (Phase 3 + future subagent plan)

- `rewrite-staging/agent_runtime/agent/…` — agent state + processing loop
- `rewrite-staging/agent_runtime/runtime/…` — router, orchestration
- `rewrite-staging/runtime_subsystems/tool/…` — tool registry (also plugin-system plan)
- `rewrite-staging/runtime_subsystems/coordination/…` — supervisor, round-robin, etc. (subagent plan)
- `rewrite-staging/runtime_subsystems/data_source/…` — concrete source backends (Phase 3 core; plugin-migration for ATProto/Discord)
- `rewrite-staging/runtime_subsystems/realtime/…` — impls only; traits live in core (rework expected)
- `rewrite-staging/runtime_subsystems/queue/…` — impls only; traits live in core (rework expected)
- `rewrite-staging/runtime_subsystems/messages/…` — storage/runtime helpers from pre-v3 messages module
- `rewrite-staging/runtime_subsystems/db/…` — concrete DB adapters (trait shape kept in core, if applicable)
- `rewrite-staging/runtime_subsystems/utils/…` — staged utils

### Destined for pattern_provider (Phase 4)

- `rewrite-staging/provider/oauth/…` — pre-v3 oauth module; absorbs into auth/
- `rewrite-staging/provider/model/…` — pre-v3 ModelProvider impls (trait shape kept in core)
- `rewrite-staging/provider/embeddings/…` — embedding backends (future)

### Destined for pattern_provider/compose (Phase 5)

- `rewrite-staging/context/compression.rs` — four compaction strategies
- `rewrite-staging/context/builder.rs` — contains block-render excerpt at lines 226–316
- `rewrite-staging/context/…` — remaining system-prompt composer glue

### Draining protocol

When a phase fully consumes a staging subdirectory, a dedicated commit removes
the subdirectory and updates this section:

```
[meta] remove drained staging dir: <subdir> (absorbed by <target>)
```

Commit body lists what moved to where. Section above is deleted in the same
change.
```

**Step 1:** Write the section.

**Step 2:** Cross-check: every file in `rewrite-staging/**` should appear under some destination heading.

```bash
find rewrite-staging -type f -name '*.rs' | wc -l
# Cross-check against the section's coverage manually or with a grep against the manifest.
```

**Commit:**

```bash
jj describe -m "[meta] port-list: staging contents section (AC1.4, AC1.7)"
jj new
```
<!-- END_TASK_24 -->

<!-- START_TASK_25 -->
### Task 25: Intermediate-state audit script

**Verifies:** AC1.7, AC1.8, AC1.9, AC1.10.

**Files:**
- Create: `/home/orual/Projects/PatternProject/pattern/scripts/audit-rewrite-state.sh`

**Implementation:**

```bash
#!/usr/bin/env bash
set -euo pipefail

# audit-rewrite-state.sh: enforces v3-foundation.AC1.7–AC1.10 across the
# active workspace crates. Exit non-zero on any violation.

workspace_dirs=(crates/pattern_core crates/pattern_runtime crates/pattern_provider crates/pattern_db)
staging_dir="rewrite-staging"

fail=0

# AC1.7: staging files must carry MOVING TO fate markers.
while IFS= read -r file; do
    if ! head -1 "$file" | grep -q '^// MOVING TO:'; then
        echo "AC1.7 violation: staged file missing MOVING TO header: $file"
        fail=1
    fi
done < <(find "$staging_dir" -type f -name '*.rs')

# AC1.8: unimplemented!()/todo!() in workspace crates must have a phase/AC reference nearby.
while IFS= read -r hit; do
    file="${hit%%:*}"
    line="${hit#*:}"; line="${line%%:*}"
    # Look at the line itself plus the preceding 3 lines for "AC" or "phase"
    context=$(sed -n "$((line-3)),${line}p" "$file")
    if ! echo "$context" | grep -qiE 'phase|AC[0-9]|AC1\.'; then
        echo "AC1.8 violation: unimplemented/todo without phase/AC marker at $file:$line"
        fail=1
    fi
done < <(grep -rnE 'unimplemented!\(|todo!\(' "${workspace_dirs[@]}" || true)

# AC1.9: code regions with fate markers must be syntactically coherent (no dangling markers on nothing).
# Simpler proxy: every MOVING TO / REPLACED BY / MOVING WITHIN CRATE marker inside workspace crates
# must be inside a comment block (not random text).
while IFS= read -r hit; do
    file="${hit%%:*}"
    line="${hit#*:}"; line="${line%%:*}"
    # Confirm the line starts with // (comment).
    content=$(sed -n "${line}p" "$file")
    if ! echo "$content" | grep -qE '^\s*//'; then
        echo "AC1.9 violation: fate marker not inside a comment at $file:$line"
        fail=1
    fi
done < <(grep -rnE '// (MOVING TO|REPLACED BY|MOVING WITHIN CRATE):' "${workspace_dirs[@]}" || true)

# AC1.10: commented-out code blocks in workspace crates fail the audit.
# Heuristic: `//` followed by obvious Rust syntax (pub fn, fn, struct, enum, impl, use crate::, let mut).
# Rustdoc lines (///, //!) are excluded: those are doc-comments, not commented-out
# code. Also excluded: fate markers, explicit Example/doc mentions, and
# "SAFETY:" / "TODO:" / "NOTE:" style annotation prefixes common in well-
# commented Rust code.
while IFS= read -r hit; do
    file="${hit%%:*}"
    line="${hit#*:}"; line="${line%%:*}"
    # Allow fate markers and doc markers; flag everything else.
    content=$(sed -n "${line}p" "$file")
    # Skip rustdoc (/// or //!) and module-level doc-comments entirely.
    if echo "$content" | grep -qE '^\s*(///|//!)'; then
        continue
    fi
    if echo "$content" | grep -qE '^\s*//\s*(pub )?(fn|struct|enum|impl|use crate::|let mut) '; then
        if ! echo "$content" | grep -qE 'MOVING TO|REPLACED BY|MOVING WITHIN CRATE|Example|doc|SAFETY|TODO|NOTE'; then
            echo "AC1.10 violation: commented-out code at $file:$line"
            echo "    > $content"
            fail=1
        fi
    fi
done < <(grep -rnE '^\s*//\s*(pub )?(fn|struct|enum|impl|use crate::|let mut) ' "${workspace_dirs[@]}" || true)

if [ "$fail" -eq 0 ]; then
    echo "audit: clean (AC1.7–AC1.10)"
fi
exit "$fail"
```

**Step 1:** Write the script. `chmod +x` it.

**Step 2:** Run it.

```bash
bash scripts/audit-rewrite-state.sh
```

Expected: exit 0 and "audit: clean (AC1.7–AC1.10)".

If it fails, fix the reported violations — add fate markers, remove commented-out code, add phase/AC references. Re-run until clean.

**Step 3:** Wire into pre-commit (optional — user's call).

If adding to `just pre-commit-all`: add a line that runs `bash scripts/audit-rewrite-state.sh` and fails the target on non-zero exit. This enforces the rule on every commit. Surface the opt-in as a commit-message choice: "recommend wiring the audit into pre-commit; disabled by default so as not to block phase 3+ work while staging is still populated". Leave it commented-out-if-intended-to-be-disabled is NOT allowed (AC1.10) — either wire it in or don't.

**Commit:**

```bash
jj describe -m "[meta] audit-rewrite-state.sh: enforce AC1.7-AC1.10 (fate markers, unimplemented phase refs, no commented code)"
jj new
```
<!-- END_TASK_25 -->

<!-- START_TASK_26 -->
### Task 26: Final Phase 2 verification

**Verifies:** AC1.1, AC1.2, AC1.3 (via doctests), AC1.4, AC1.7, AC1.8, AC1.9, AC1.10.

**Step 1:** Full check.

```bash
cd /home/orual/Projects/PatternProject/pattern
cargo check -p pattern_core 2>&1 | tee /tmp/final-check.log
! grep -q 'warning:' /tmp/final-check.log && echo "AC1.1 pass" || { echo "AC1.1 FAIL"; exit 1; }

cargo doc -p pattern_core --no-deps 2>&1 | tee /tmp/final-doc.log
! grep -q 'warning:' /tmp/final-doc.log && echo "AC1.2 pass" || { echo "AC1.2 FAIL"; exit 1; }

cargo test --doc -p pattern_core 2>&1 | tail -5
cargo nextest run -p pattern_core --lib 2>&1 | tail -5

bash scripts/audit-rewrite-state.sh
```

**Step 2:** AC1.5 manual spot-check.

Pick one trait (e.g., `MemoryStore`), open the doctest dummy impl, comment out one method, run `cargo test --doc -p pattern_core`. Expected: compile failure naming the missing method. Restore the method. Record the verification in the phase-close commit message.

**Step 3:** AC1.6 manual spot-check.

Add `pattern_auth = { path = "../pattern_auth" }` to `crates/pattern_core/Cargo.toml`. Run `cargo check -p pattern_core`. Expected: workspace error ("path dependency `pattern_auth` points to directory not in workspace members" or similar). Remove the line. Record verification.

**Step 4:** AC1.4 spot-check.

```bash
test -f docs/plans/rewrite-v3-portlist.md
grep -c '^### ' docs/plans/rewrite-v3-portlist.md   # should be > 9 (every excluded crate + staging sections)
```

**Step 5:** `just pre-commit-all` passes.

**Commit:**

```bash
jj describe -m "[meta] phase 2 complete: pattern_core traits-only, staging populated, AC1.1-1.10 verified

AC1.1 cargo check zero warnings: PASS (see /tmp/final-check.log)
AC1.2 cargo doc zero warnings: PASS (see /tmp/final-doc.log)
AC1.3 dummy trait impls compile: PASS (doctests run clean)
AC1.4 port-list doc complete: PASS (grep verified)
AC1.5 missing method fails compile: PASS (manually verified MemoryStore)
AC1.6 retired-crate ref fails workspace: PASS (manually verified pattern_auth)
AC1.7 fate markers on every staged file: PASS (audit script)
AC1.8 no unimplemented without phase/AC ref: PASS (audit script)
AC1.9 no unmarked in-flight code: PASS (audit script)
AC1.10 no commented-out code blocks: PASS (audit script)"
jj new
```
<!-- END_TASK_26 -->
<!-- END_SUBCOMPONENT_F -->

---

## Phase 2 "Done when" checklist

- [ ] `rewrite-staging/` exists with README + migration manifest + populated subdirectories
- [ ] `crates/pattern_core/src/` contains only: `lib.rs`, `base_instructions.rs`, `traits/`, `types/`, `error.rs` (or `error/`), `memory/`, `memory_acl/`, `permission/`, `export/`, `config/`, `utils/` (if kept)
- [ ] `prompt_template/` and `users/` deleted (jj log shows dedicated retirement commits)
- [ ] All seven new traits (`AgentRuntime`, `Session`, `MemoryStore`, `ProviderClient`, `MessageRouter`, `DataStream`, `SourceManager`) defined with rustdoc + dummy-impl doctests
- [ ] Error hierarchy split: `CoreError`, `RuntimeError`, `ProviderError`, `MemoryError`, all `#[non_exhaustive]` with thiserror + miette
- [ ] `cargo check -p pattern_core` — zero warnings (AC1.1)
- [ ] `cargo doc -p pattern_core` — zero warnings (AC1.2)
- [ ] `cargo test --doc -p pattern_core` — all doctests pass (AC1.3)
- [ ] `cargo nextest run -p pattern_core --lib` — surviving tests pass
- [ ] `cargo clippy -p pattern_core --all-features --all-targets -- -D warnings` — clean
- [ ] `docs/plans/rewrite-v3-portlist.md` updated with Staging contents section (AC1.4)
- [ ] `scripts/audit-rewrite-state.sh` passes clean (AC1.7–AC1.10)
- [ ] AC1.5, AC1.6 manually verified and recorded in phase-close commit message
- [ ] `just pre-commit-all` passes

## What this phase deliberately does NOT do

- Does not add any runtime functionality to `pattern_runtime` (still an empty skeleton; Phase 3 fills it).
- Does not add any provider functionality to `pattern_provider` (still an empty skeleton; Phase 4 fills it).
- Does not delete `pattern_auth`, `pattern_macros`, or `pattern_surreal_compat` directories — they stay per the port-list retire policy. Their deletions are dedicated commits in later phases once responsibilities have migrated.
- Does not reshape any staged code. Reshape happens at the consuming phase (3, 4, 5, or later).
- Does not touch `config/` beyond fixing import paths. Config breakup is out of scope; fate marker in `config.rs` notes this.
- Does not add compaction / rendering logic changes — both stage and are reshaped in Phase 5.
- Does not attempt to keep pre-v3 downstream crates (pattern_cli, pattern_server, etc.) compiling. They're excluded from `members` and will be re-integrated in later plans.
