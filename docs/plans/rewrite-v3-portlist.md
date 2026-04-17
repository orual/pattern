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

## Pending migrations within pattern_core

- **chrono → jiff**: Files touched for relocation keep chrono; files touched for
  any substantive change port to jiff. Remaining chrono usage in `memory/`,
  `export/`, `config.rs`, `permission.rs`, `error.rs`, `test_helpers.rs` ports
  incrementally as those modules are reworked. Do not do a bulk migration.

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
