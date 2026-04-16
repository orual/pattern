# Pattern v3 Foundation — Phase 1: Branch + scaffold

**Goal:** Establish the `rewrite-v3` branch/bookmark, narrow the workspace `members` list, create empty skeletons for the two new crates (`pattern_runtime`, `pattern_provider`), and publish a port-list document tracking every currently-excluded crate.

**Architecture:** Infrastructure-only phase. No functional code. Sets the project layout that every subsequent phase builds on. Depends on nothing; blocks everything downstream.

**Tech Stack:** Cargo workspace, jj (Jujutsu) with git coexistence, edition 2024.

**Scope:** Phase 1 of 6 from the v3 foundation design.

**Codebase verified:** 2026-04-16

---

## Acceptance Criteria Coverage

**Verifies: None.** Infrastructure phase per design "Done when" — verified operationally (`cargo check` succeeds, port-list doc exists, branch/tag/bookmarks in place). No AC cases implemented in this phase.

Phase 2 onward (traits, runtime, provider, memory, smoke) covers `v3-foundation.AC1` through `v3-foundation.AC9`.

---

## Executor Context

**Repo root:** `/home/orual/Projects/PatternProject/pattern`
**VCS:** jj (Jujutsu) primary, git coexistence. Commits via `jj describe`/`jj commit`. Named referral points use jj bookmarks (jj has no native tag concept; design-plan "tag" means "immovable bookmark" here).
**Edition:** 2024. Workspace version: 0.4.0.
**Commit format:** `[crate-name] brief description` (use `[meta]` for workspace/cross-cutting).
**Pre-commit:** `just pre-commit-all` (mandatory before any commit).
**Format check:** `cargo fmt --check`.
**Lint:** `cargo clippy --all-features --all-targets`.
**Tests (not used this phase):** `cargo nextest run`.

**Existing crate inventory (before this phase):**
- `crates/pattern_api`, `crates/pattern_auth`, `crates/pattern_cli`, `crates/pattern_core`, `crates/pattern_db`, `crates/pattern_discord`, `crates/pattern_macros`, `crates/pattern_mcp`, `crates/pattern_nd`, `crates/pattern_server`, `crates/pattern_surreal_compat`

**Current workspace `members` uses a glob:** `["crates/*"]` — must be converted to an explicit list to narrow.

**Design reference:** `/home/orual/Projects/PatternProject/pattern/docs/design-plans/2026-04-16-v3-foundation.md` — Phase 1 between `<!-- START_PHASE_1 -->` and `<!-- END_PHASE_1 -->`. Port-list doc requirement in "Additional Considerations → Intermediate code-state policy" and fate-marker conventions.

---

<!-- START_TASK_1 -->
### Task 1: Create `pre-rewrite-v3` bookmark at current `main` tip

**Files:** none (VCS operation only).

**Context:** The design plan says "Tag `pre-rewrite-v3`." Since jj has no native tag concept, we use a bookmark as the immutable-by-convention referral point. Do not move this bookmark after creation.

**Step 1: Verify current main tip**
```bash
cd /home/orual/Projects/PatternProject/pattern
jj log -r main --no-graph -T 'commit_id.short() ++ " " ++ description.first_line() ++ "\n"' | head -1
```
Expected: prints the short commit id and first-line description of `main`'s tip. Record the commit id.

**Step 2: Confirm no existing bookmark**
```bash
jj bookmark list | grep -E '^pre-rewrite-v3\b' || echo "ok: bookmark does not exist"
```
Expected: `ok: bookmark does not exist`.

**Step 3: Create bookmark at main's tip**
```bash
jj bookmark create pre-rewrite-v3 -r main
```

**Step 4: Verify**
```bash
jj bookmark list | grep pre-rewrite-v3
jj log -r pre-rewrite-v3 --no-graph -T 'commit_id.short() ++ " " ++ description.first_line() ++ "\n"'
```
Expected: bookmark listed; points at the same commit as `main`.

**Commit:** No commit — bookmark creation is the deliverable.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Create `rewrite-v3` bookmark and fresh working-copy change

**Files:** none (VCS operation only).

**Step 1: Confirm no existing bookmark**
```bash
jj bookmark list | grep -E '^rewrite-v3\b' || echo "ok: bookmark does not exist"
```
Expected: `ok: bookmark does not exist`.

**Step 2: Create bookmark at the `pre-rewrite-v3` commit**
```bash
jj bookmark create rewrite-v3 -r pre-rewrite-v3
```

**Step 3: Start a fresh empty change atop the bookmark**
```bash
jj new rewrite-v3 -m "[meta] v3 foundation phase 1: begin scaffold"
```

**Step 4: Verify**
```bash
jj bookmark list | grep rewrite-v3
jj log -r @ --no-graph -T 'description ++ "\n"'
```
Expected: bookmark present at the same commit as `pre-rewrite-v3`; working-copy description matches the message above.

**Commit:** No commit — the `jj new` call created the empty change. Subsequent tasks populate it.
<!-- END_TASK_2 -->

<!-- START_SUBCOMPONENT_A (tasks 3-5) -->
<!-- START_TASK_3 -->
### Task 3: Narrow workspace `members` to the four active crates

**Files:**
- Modify: `/home/orual/Projects/PatternProject/pattern/Cargo.toml` — replace `members = ["crates/*"]` with an explicit list.

**Step 1: Read current root Cargo.toml**
```bash
cat /home/orual/Projects/PatternProject/pattern/Cargo.toml | head -30
```
Note the current `[workspace]` block (resolver + members glob).

**Step 2: Replace `members`**

Change the `[workspace]` block so `members` reads:
```toml
[workspace]
resolver = "3"
members = [
    "crates/pattern_core",
    "crates/pattern_runtime",
    "crates/pattern_provider",
    "crates/pattern_db",
]
```

Leave all other keys (`[workspace.package]`, `[workspace.dependencies]`, etc.) untouched.

**Step 3: Verify (will fail — expected)**
```bash
cargo check --workspace 2>&1 | head -20
```
Expected: error because `crates/pattern_runtime` and `crates/pattern_provider` do not exist yet. This is fine; Tasks 4 and 5 create them. **Do not commit between Tasks 3 and 5 — the workspace is in a broken state.**
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Scaffold `pattern_runtime` crate

**Files:**
- Create: `/home/orual/Projects/PatternProject/pattern/crates/pattern_runtime/Cargo.toml`
- Create: `/home/orual/Projects/PatternProject/pattern/crates/pattern_runtime/src/lib.rs`
- Create: `/home/orual/Projects/PatternProject/pattern/crates/pattern_runtime/CLAUDE.md`

**Step 1: Cargo.toml**
```toml
[package]
name = "pattern_runtime"
version.workspace = true
edition.workspace = true

[lints]
workspace = true

[dependencies]
pattern_core = { path = "../pattern_core" }
```

**Step 2: src/lib.rs**
```rust
//! Pattern runtime: Tidepool embedding, agent execution loop, SDK effect handlers.
//!
//! This crate owns the execution machinery that was previously embedded in
//! `pattern_core`. It depends on `pattern_core` only for trait definitions and
//! shared types; it does not re-expose `pattern_core` internals.
//!
//! Populated incrementally across v3 foundation phases 3–5:
//! - Phase 3: Tidepool FFI, timeout harness, SDK effect algebra, agent loop, checkpoint, `time`/`log` handlers.
//! - Phase 5: Memory adapter (wraps preserved storage), pseudo-message emission, pre-turn `current_state` pseudo-turn.
```

**Step 3: CLAUDE.md**
```markdown
# pattern_runtime

Agent runtime for Pattern v3. Houses Tidepool (Haskell-in-Rust) embedding, the
agent turn loop, `freer-simple` effect handlers, and turn-level checkpoint
machinery. Depends only on `pattern_core` trait definitions.

See the v3 foundation design at
`docs/design-plans/2026-04-16-v3-foundation.md` for the substrate choice,
SDK hierarchy, and phase ordering.
```

**Step 4: Check workspace config accepts the crate**
```bash
cargo check -p pattern_runtime 2>&1 | tail -10
```
Expected: `Finished ...` — empty crate compiles cleanly.
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Scaffold `pattern_provider` crate

**Files:**
- Create: `/home/orual/Projects/PatternProject/pattern/crates/pattern_provider/Cargo.toml`
- Create: `/home/orual/Projects/PatternProject/pattern/crates/pattern_provider/src/lib.rs`
- Create: `/home/orual/Projects/PatternProject/pattern/crates/pattern_provider/CLAUDE.md`

**Step 1: Cargo.toml**
```toml
[package]
name = "pattern_provider"
version.workspace = true
edition.workspace = true

[lints]
workspace = true

[dependencies]
pattern_core = { path = "../pattern_core" }
```

No `genai`, `keyring`, `reqwest`, etc. yet — Phase 4 wires those in when they are needed. This phase is skeleton-only.

**Step 2: src/lib.rs**
```rust
//! Pattern provider: LLM authentication, request shaping, rate limiting, token counting.
//!
//! Owns the three-tier auth resolver (session-pickup → PKCE → API key), the
//! rebased `rust-genai` fork, and the request composer that emits the
//! three-segment cache layout defined in the v3 foundation design.
//!
//! Populated incrementally across v3 foundation phase 4 (auth/shaping/rate
//! limiting/token counting) and phase 5 (request composer with segmented
//! `cache_control` markers).
```

**Step 3: CLAUDE.md**
```markdown
# pattern_provider

LLM provider integration for Pattern v3. Owns Anthropic authentication
(three-tier: session-pickup, PKCE, API key), request shaping (honest pattern
identification), per-provider rate limiting, provider-reported token counting,
and the request composer that emits the three-segment cache layout.

Absorbs the Anthropic-facing bits of the retired `pattern_auth` crate. Depends
on `pattern_core` for trait definitions; carries its own rebased fork of
`rust-genai` (auth-only patches on current upstream, plus any Opus-4.7
migration patches not yet in upstream).

See `docs/design-plans/2026-04-16-v3-foundation.md` §Provider and §Architecture
for the auth flow diagram and shaping contract.
```

**Step 4: Verify workspace**
```bash
cargo check --workspace 2>&1 | tail -20
```
Expected: all four crates compile. Zero warnings on the new crates. Pre-existing `pattern_core` warnings (if any) should be noted in the commit message but not fixed here — they go on the Phase 2 work list.
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_TASK_6 -->
### Task 6: Write port-list tracking document

**Files:**
- Create: `/home/orual/Projects/PatternProject/pattern/docs/plans/rewrite-v3-portlist.md`

**Step 1: Write the doc**

```markdown
# v3 Rewrite Port List

**Status:** active; updated as the rewrite progresses.
**Tracks:** every crate currently excluded from the workspace `members` list.
**Related:** `docs/design-plans/2026-04-16-v3-foundation.md`, future v3 design plans (memory-fs, subagents, plugin-system, plugin-migration, v2→v3 migrator).

## Fate taxonomy

Each excluded crate has one of these fates:

- **port** — code will return to the workspace under a future plan, possibly reshaped.
- **absorb** — responsibilities fold into a different crate; origin crate retires.
- **retire** — directory will be deleted once its responsibilities have migrated or its value has elapsed.

## Fate markers in source

Code in transition carries one of these comments, at module or item level:

- `// MOVING TO: crates/<target>` — code staying put temporarily; move by the phase that introduces the target.
- `// REPLACED BY: <new-path> — delete after phase N lands` — old code awaiting replacement landing.
- `// MOVING WITHIN CRATE: <new-path>` — relocation inside the same crate.

Cruft (code with no fate marker, `unimplemented!()`/`todo!()` without phase/AC reference, commented-out code, empty modules, dangling `use` statements) fails the intermediate-state audit in every phase's "done when" check.

## Excluded crates

### pattern_api
- **Fate:** port.
- **Location:** `crates/pattern_api/`.
- **Deferred to:** plugin-migration plan.
- **Notes:** Shared API types and contracts. Revisit alongside `pattern_server` when the plugin surface is re-established.

### pattern_auth
- **Fate:** absorb + retire.
- **Location:** `crates/pattern_auth/`.
- **Absorbs into:** `pattern_provider` (Anthropic OAuth keychain storage).
- **Deferred to:** plugin-migration plan (ATProto + Discord bits).
- **Notes:** Directory deleted in a dedicated commit after Phase 4 lands. ATProto and Discord auth bits move to their respective plugin crates in a later plan.

### pattern_cli
- **Fate:** port.
- **Location:** `crates/pattern_cli/`.
- **Deferred to:** CLI/TUI polish plan (post-foundation).
- **Notes:** Phase 6 adds a minimal driver CLI in `pattern_runtime/bin/` for the smoke test; the polished CLI returns under a dedicated plan.

### pattern_discord
- **Fate:** port.
- **Location:** `crates/pattern_discord/`.
- **Deferred to:** plugin-migration plan (social integrations).

### pattern_nd
- **Fate:** port.
- **Location:** `crates/pattern_nd/`.
- **Deferred to:** plugin-migration plan (ADHD-specific tools and personalities).

### pattern_macros
- **Fate:** retire.
- **Location:** `crates/pattern_macros/`.
- **Notes:** Legacy derive macros. Not depended on by `pattern_core` or `pattern_db` in the narrowed workspace. Directory deleted in a dedicated commit alongside `pattern_surreal_compat` once neither is needed.

### pattern_mcp
- **Fate:** port.
- **Location:** `crates/pattern_mcp/`.
- **Deferred to:** plugin-system plan (MCP client + server are part of plugin scope).

### pattern_server
- **Fate:** port.
- **Location:** `crates/pattern_server/`.
- **Deferred to:** plugin-migration plan.

### pattern_surreal_compat
- **Fate:** retire.
- **Location:** `crates/pattern_surreal_compat/`.
- **Notes:** v2 SurrealDB compatibility shim. Not needed in v3. Directory deleted in a dedicated commit alongside `pattern_macros` once the v2→v3 data migrator plan has either landed or concluded it doesn't need this shim.

## Retired-directory deletion policy

Crates marked `retire` keep their source on disk (excluded from `members`) until their responsibilities have fully migrated. Deletion happens in a dedicated commit with subject:

`[meta] remove retired crate <name> (responsibilities migrated, see <reference>)`

Not deleted in the same commit as the migration work — makes bisection easier.

## Audit checklist (run at every phase boundary)

```bash
# Fate markers on transitional code
rg '// (MOVING TO|REPLACED BY|MOVING WITHIN CRATE):' crates/pattern_core crates/pattern_runtime crates/pattern_provider crates/pattern_db

# unimplemented!/todo! without phase/AC reference
rg 'unimplemented!\(|todo!\(' crates/pattern_core crates/pattern_runtime crates/pattern_provider crates/pattern_db

# Commented-out code blocks in new-or-modified files (manual review; false positives acceptable)
rg '^\s*// (pub )?(fn|struct|enum|impl|use) ' crates/pattern_core crates/pattern_runtime crates/pattern_provider crates/pattern_db
```

Any hit requires either a fate-marker justification or a cleanup commit before the phase closes.
```

**Step 2: Verify**
```bash
test -f /home/orual/Projects/PatternProject/pattern/docs/plans/rewrite-v3-portlist.md && wc -l /home/orual/Projects/PatternProject/pattern/docs/plans/rewrite-v3-portlist.md
```
Expected: file exists, non-trivial line count.
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Operational verification

**Step 1: Workspace check**
```bash
cargo check --workspace 2>&1 | tail -20
```
Expected: all four crates compile. Pre-existing warnings in `pattern_core` are acceptable for this phase (cleaned up incrementally through Phase 2); new crates must be warning-clean.

**Step 2: Format check**
```bash
cargo fmt --check
```
Expected: clean.

**Step 3: Lint on new crates**
```bash
cargo clippy -p pattern_runtime -p pattern_provider --all-targets
```
Expected: zero warnings on the new skeletons.

**Step 4: Pre-commit pipeline**
```bash
just pre-commit-all
```
Expected: passes. If it fails on code excluded from `members`, skip or scope the pre-commit hooks to the narrowed set (document any workaround in the commit message).

**Step 5: Branch hygiene**
```bash
jj log -r "pre-rewrite-v3..@" --no-graph -T 'change_id ++ " " ++ description.first_line() ++ "\n"'
```
Expected: exactly one pending change (the scaffold change from Task 2) plus whatever commits you've made in Tasks 3–6.
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Commit

**Step 1: Stage review**
```bash
jj diff --stat
```
Expected: the root `Cargo.toml`, the two new crate skeletons, and the port-list doc.

**Step 2: Describe and finalize**
```bash
jj describe -m "[meta] v3 foundation phase 1: narrow workspace, scaffold runtime+provider crates, port-list doc

Narrowed workspace members from glob ['crates/*'] to explicit list:
[pattern_core, pattern_runtime, pattern_provider, pattern_db].

Created empty skeletons for pattern_runtime and pattern_provider. Both
depend only on pattern_core and will be populated in phases 3 and 4
respectively.

Published docs/plans/rewrite-v3-portlist.md tracking every excluded crate
with fate (port/absorb/retire) and deferred-plan references.

Bookmark pre-rewrite-v3 created at main tip as an immutable-by-convention
referral point (jj has no native tag concept). Bookmark rewrite-v3 cut
from the same commit for active rewrite work."
jj new
```

**Step 3: Final verification**
```bash
cargo check --workspace
jj log -r "pre-rewrite-v3..@-" --no-graph
```
Expected: clean compile; exactly the phase-1 scaffold commit visible between the tag and the empty working copy.
<!-- END_TASK_8 -->

---

## Phase 1 "Done when" checklist

- [ ] `jj bookmark list | grep pre-rewrite-v3` prints the bookmark at main's tip.
- [ ] `jj bookmark list | grep rewrite-v3` prints the active bookmark at the same commit.
- [ ] Root `Cargo.toml` `members` is the explicit 4-entry list.
- [ ] `crates/pattern_runtime/` exists with Cargo.toml, src/lib.rs, CLAUDE.md.
- [ ] `crates/pattern_provider/` exists with Cargo.toml, src/lib.rs, CLAUDE.md.
- [ ] `docs/plans/rewrite-v3-portlist.md` exists and lists all 9 excluded crates with fate + deferral notes.
- [ ] `cargo check --workspace` succeeds on the narrowed workspace.
- [ ] Commit landed on `rewrite-v3` bookmark.
- [ ] `rg 'unimplemented!\(|todo!\(' crates/pattern_runtime crates/pattern_provider` returns no hits (skeletons should be genuinely empty).

## What this phase deliberately does NOT do

- Does not touch any code inside `pattern_core` (trait extraction happens in Phase 2).
- Does not delete `crates/pattern_macros/` or `crates/pattern_surreal_compat/` (deletion commits come later per port-list policy).
- Does not add dependencies to the new crate skeletons beyond `pattern_core` (Phase 3/4 add real deps as they need them).
- Does not modify `pattern_db` (preserved as-is; it's in `members` because memory storage depends on it downstream).
