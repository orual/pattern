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

### pattern_auth (retired Phase 4)
- **Fate:** **retired** — directory deleted.
- **Absorbed into:** `pattern_provider::creds_store` (Anthropic OAuth
  keychain + JSON fallback storage); `ProviderCredential` now lives at
  `pattern_core::types::provider::ProviderCredential` with `SecretString`
  wrappers for tokens.
- **Deferred to:** plugin-migration plan (ATProto + Discord bits, staged
  to `rewrite-staging/provider/` during Phase 2).
- **Retirement actions (all landed together in the Task 7 commit):**
  (a) removed `pattern-auth` path dep from `pattern_core/Cargo.toml`,
  (b) dropped `CoreError::AuthError(#[from] pattern_auth::AuthError)`
  variant — auth errors belong in `pattern_provider::ProviderError` now,
  (c) verified no downstream `CoreError::AuthError` matches existed in
  active crates, (d) deleted `crates/pattern_auth/`.
- **AC1.6 verified:** adding `pattern-auth = { path = "../pattern_auth" }`
  to an active crate's Cargo.toml produces an explicit
  `failed to read Cargo.toml: No such file or directory` workspace
  error, confirming the retirement is loud-failing as intended.

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

## v3-memory-rework additions

### pattern_memory (Phase 1 — completed 2026-04-19)
- Extracted from `pattern_core::memory::*` during the v3-memory-rework plan, Phase 1.
- Hosts `MemoryCache`, `SharedBlockManager`, schema templates.
- `StructuredDocument` remains in `pattern_core::memory` (trait-signature type).
- `pattern_core` retains the `MemoryStore` trait + trait-signature data types
  under `pattern_core::types::memory_types::*`.
- Dependency graph: `pattern_memory -> pattern_core + pattern_db`; reverse-dep
  guard is `crates/pattern_core/tests/no_pattern_memory_dep.rs`.

### jj CLI adapter + quiesce + StorageMode (Phase 5 — completed 2026-04-20)

- `pattern_memory::modes::StorageMode` — enum skeleton (A/B/C). Phase 6 adds
  per-mode path resolution + `.pattern.kdl` config parsing + attach/detach logic.
- `pattern_memory::quiesce::quiesce()` — universal pre-commit step (all modes):
  drain subscribers, WAL checkpoint `memory.db`, fsync emitted canonical files.
  Callers: Mode A host VCS integrations; Modes B/C via `JjAdapter::commit` (Phase 6).
- `MemoryCache::wal_checkpoint()` — delegates to `ConstellationDb::checkpoint()`
  which runs `PRAGMA wal_checkpoint(TRUNCATE)`.
- CI canary added to `.github/workflows/ci.yml`: installs jj 0.40.0 and runs
  `cargo nextest run -p pattern-memory --test 'jj_adapter_*'` on every CI run.
- Decision: CLI over jj-lib for on-disk format ownership (Mode A+C format-drift
  safety). See `docs/implementation-plans/2026-04-19-v3-memory-rework/phase_05.md`.
- Packaging: non-NixOS distribution bundles must ship the `jj` binary alongside
  `tidepool-extract`. Tracked as a follow-up in the packaging workstream.

### Scopes + project personas + lib modules + Pattern.Diagnostics (Phase 8 — completed 2026-04-20)

- `pattern_memory::scope::MemoryScope<S>` wraps any `MemoryStore` with
  IsolatePolicy routing (None / CoreOnly / Full).
- Persona discovery across global (`~/.pattern/personas/`) + project
  (`<mount>/personas/`) scopes; project-scoped takes precedence on collision.
- `<mount>/lib/*.hs` include-path extension (Approach A: per-module probe-compile validation via `tidepool_runtime::compile_haskell`;
  broken modules surface as `DiagnosticEvent` entries via `Pattern.Diagnostics`).
- `Pattern.Diagnostics.diagnostics` SDK effect returns a JSON-encoded list
  of session diagnostic events (lib-compile failures + handler errors).
- `ctx.memory.writeToPersona` effect allows explicit persona-scope write
  when policy is None; rejects under CoreOnly or Full with IsolationDenied.

### Recall SDK surface shrink (Phase 3 — completed 2026-04-19)

- Removed `RecallReq::Delete` variant from the agent-facing SDK
  (`pattern_runtime::sdk::requests::recall`).
- Removed `Pattern.Recall.delete` Haskell symbol and its Recall GADT
  constructor (`RecallDelete`).
- `MemoryStore::delete_archival` retained in the trait for human-operator
  tooling (CLI / TUI).
- Agent programs referencing `Pattern.Recall.delete` fail at Tidepool
  compile time with a 'variable not in scope' diagnostic.
- Trybuild compile-fail test at
  `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs`.

## Retired-directory deletion policy

Crates marked `retire` keep their source on disk (excluded from `members`) until their responsibilities have fully migrated. Deletion happens in a dedicated commit with subject:

`[meta] remove retired crate <name> (responsibilities migrated, see <reference>)`

Not deleted in the same commit as the migration work — makes bisection easier.

## Pending migrations within pattern_core

- **chrono → jiff**: Files touched for relocation keep chrono; files touched for
  any substantive change port to jiff. Remaining chrono usage in `memory/`,
  `export/`, `config.rs`, `permission.rs`, `error.rs`, `test_helpers.rs` ports
  incrementally as those modules are reworked. Do not do a bulk migration.

- **AC1.6 verification deferred to Phase 4 retirement**: Phase 2's AC1.6
  (referencing a retired crate fails loudly) cannot be satisfied by cargo at
  Phase 2 time — `path = "../retired"` deps to non-member crates compile
  silently. The real failure mode fires when `pattern_auth` is deleted in
  Phase 4. The Phase 4 retirement commit must simultaneously (a) remove the
  `pattern-auth` path dep from `crates/pattern_core/Cargo.toml`, (b) drop or
  restructure `CoreError::AuthError` (auth errors belong in
  `pattern_provider::ProviderError`), and (c) update any downstream
  `CoreError::AuthError` pattern matches. See the `pattern_auth` entry above
  for details.

- **Workspace path-dep audit (performed 2026-04-16)**: `grep 'path = "' crates/*/Cargo.toml`
  shows a single path dep to a non-workspace crate: `pattern-auth` from
  `pattern_core`. All other path deps (`pattern-db`, `pattern-core`) point at
  active workspace members. No new leaks accumulated during Phase 2.

- **`BlockHandle` now a `SmolStr` alias** (was `pub struct BlockHandle(pub String)`):
  matches the ID-alias policy in `crates/pattern_core/CLAUDE.md` and drops
  newtype ceremony that carried no invariant. `BlockRef.block_id` /
  `BlockRef.agent_id` still use `String`; those port opportunistically when
  the composer (Phase 5) touches them.

- **Opaque `serde_json::Value` payloads on `PersonaConfig` / `PersonaSnapshot` /
  `SessionSnapshot`**: Phase 2 lands the shape with opaque JSON payloads per
  the original plan. Phase 3 populates the concrete runtime-state shape when
  the Tidepool session lifecycle lands; consider wrapping in a
  `#[non_exhaustive] OpaquePayload(serde_json::Value)` newtype then so callers
  don't pattern-match on the raw JSON.

- **`pattern-db` / `pattern-auth` clippy-fix scope leak**: during the Phase 2
  close commit, `cargo clippy --fix` was run against pattern-db + pattern-auth
  (to satisfy `-D warnings` across transitive deps). Includes a mechanical
  rename of `ContentType::from_str` → `parse_from_str` at 4 call sites in
  pattern-db. Acknowledged deviation; noted for traceability, no rework
  planned.

- **Crate-level `#![allow(...)]` entries in pattern_core for pre-existing
  style lints**: present in `export/`, `error/core.rs`, `memory/document.rs`
  with rationale comments (feature-gated code, `#[derive]` `#[non_exhaustive]`
  interaction). Revisit in Phase 3 or 4 when those modules see substantive
  touch.

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
- `rewrite-staging/runtime_subsystems/config.rs` — pre-v3 Pattern config system (Phase 3 reassembly — depends on staged data_source/runtime/context/agent types)

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
