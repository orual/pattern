# Pattern v3 Memory Rework Design

## Summary

<!-- TO BE GENERATED after body is written -->

## Definition of Done

Pattern v3 Memory Rework — extracts the memory subsystem from `pattern_core`, migrates its storage backend from `sqlx` to `rusqlite`, sync-ifies the `MemoryStore` trait, and reshapes storage so that markdown files are canonical block content (with loro snapshots as the merge-authoritative CRDT state) while SQLite retains only indexes and archival entries. Version history for memory state is managed via jj. The plan is done when:

### Crate structure

- `pattern_memory` crate extracted from `crates/pattern_core/src/memory/`
- `pattern_core` retains only the `MemoryStore` trait + `Block` / `BlockHandle` / related types (trait-only-core rule satisfied)
- Dependency graph: `pattern_memory → pattern_core + pattern_db`; no reverse deps

### Storage backend (sync, rusqlite)

- `pattern_db` migrated from `sqlx` to `rusqlite` across all ~427 queries
- `MemoryStore` trait sync-ified (28 methods); `async_trait` usage removed from `MemoryStore` specifically (other pattern_core async_trait usage audited and preserved if still warranted)
- FTS5 + `sqlite-vec` revalidated under rusqlite with regression coverage
- Connection pooling via `r2d2-sqlite` (or equivalent) for async callsites; eval worker owns a dedicated Connection for its session lifetime
- WAL journal mode preserved (already enabled)
- Transaction semantics preserved across the port (explicit transactions remain transactional)

### Eval worker simplification

- Per-session multi-thread tokio runtime in `eval_worker.rs` removed; worker becomes a plain OS thread with `std::sync::mpsc`
- All 22 `Handle::current().block_on` sites in memory/recall/search/scope handlers eliminated
- Tech-debt comment in `eval_worker.rs` flagged by the sqlx→rusqlite evaluation doc is resolved (documented fix)
- Reply channel hybrid (sync worker + async dispatcher) works cleanly for both sync and async caller contexts

### Async callsite migration

- All async consumers of `MemoryStore` (`pattern_cli`, turn-boundary code in `pattern_runtime`) updated to `spawn_blocking` wrappers
- No `.await` on memory calls at async callsites except via `spawn_blocking`
- Pre-existing search bug caused by `spawn_blocking` mis-use transitively fixed by the sync surface

### Fs-canonical memory storage

- Block content persisted as markdown files in the persona/project storage root (canonical form)
- Loro snapshots persisted alongside (merge-authoritative CRDT state for concurrent-write resolution)
- SQLite holds: FTS5 indexes, vector embeddings, archival entries, block metadata — **not** block content
- Disk ↔ memory synchronization machinery ported from `rewrite-staging/runtime_subsystems/data_source/file_source.rs` (notify-watcher, conflict detection, bidirectional subscriptions) and adapted for memory-block semantics
- Human edits to markdown files reconciled via loro CRDT merge on read (concurrent-write treatment)
- DB-indexing strategy (e.g., "write to both simultaneously" vs. "loro-primary with db-sync subscriber") decided in brainstorming and documented in the Architecture section

### Version history (jj)

- jj CLI integration via a thin adapter (~15-30 functions) as an internal module of `pattern_memory`
- Adapter covers: workspace add/list/forget/update-stale, commit, log, bookmark set/delete, merge, restore
- Pre-commit quiesce step: flush loro state to disk, `PRAGMA wal_checkpoint(TRUNCATE)`, ensure sqlite file is canonical before jj commits it
- SQLite file itself is version-controlled under pattern-jj alongside markdown and loro snapshots (binary blob; no auto-merge, but the quiesce step makes committed state deterministic)

### Storage modes

- **Mode A** (in-repo, host-VCS-owned): `<project-repo>/.pattern/shared/` committed by host git/jj; pattern adds no history layer
- **Mode B** (separate, pattern-jj-tracked): `~/.pattern/projects/<project-id>/`; pattern-jj owns history; directory optionally symlinked from project
- **Mode C** (sidecar pattern-jj over host-repo working copy): attempted; if straightforward, implemented with documented fragility caveats; otherwise documented-only with explicit deferral
- Per-project config selects mode

### Context model + scopes

- Three-tier context model (Core / Working / Archival) formalized at the `pattern_memory` crate level (not just in architecture docs)
- Persona-level memory always separate and always pattern-jj-tracked (never in a project repo)
- Project-scoped personas (`scope: project:<id>`) — persona definitions can live in a project's `.pattern/shared/personas/`
- `isolate_from_persona` flag (`none` / `core-only` / `full`) implemented as a real attachment-time policy

### Project utilities

- `.pattern/shared/lib/` directory convention: Haskell modules importable by the agent program at session instantiation
- Runtime compile-logic extended to include the project's `lib/` directory in Tidepool's import search path
- No lifecycle hooks yet — setup hooks deferred to the plugin-system plan

### Testing (per-phase, deterministic-preferred)

- Each phase ships its own unit, property, snapshot, and integration tests
- Existing `MemoryStore` integration tests must pass both before and after the sqlx→rusqlite port (regression proof of the migration)
- Deterministic tests for: fs↔memory sync (with temp-dir fixtures), jj adapter (isolated jj repos in temp dirs), mode A/B/C setup/attach/detach, isolate_from_persona variants, three-tier routing
- No live-model dependency in CI paths
- FTS5 + sqlite-vec regression coverage against representative queries (e.g., BM25 scoring, hybrid score fusion, vector similarity)

### Smoke demonstration

- End-to-end flow: create persona → attach Mode A project → write core block → block persists as markdown + loro snapshot + sqlite index → edit .md file externally → next read reconciles via loro merge → commit state via pattern-jj → restart → state resumes from committed checkpoint
- CI-runnable with deterministic fixtures (no live model)

### Explicitly OUT OF SCOPE (deferred to future plans)

- **Task** block subtype (lifecycle, graph dependencies, `ctx.tasks.*` SDK surface) — Plan 2: `v3-task-skill-blocks`
- **Skill** block subtype (trust tagging, on-demand load, `ctx.skills.*` SDK surface) — Plan 2
- Setup hooks (`.pattern/shared/setup/`) — requires lifecycle event system, bound to plugin-system plan
- Subagent fork-as-jj-workspace semantics — Plan 3: `v3-subagents`
- v2 → v3 data migrator — dedicated migrator plan
- Plugin system, MCP, iroh-rpc
- Compaction strategy changes (existing four strategies preserved)
- Message log storage reorganization (messages stay in sqlite, untouched)

### Context

This is the second design plan in the Pattern v3 rewrite sequence. Builds on `docs/design-plans/2026-04-16-v3-foundation.md` (foundation). Informed by:

- `docs/plans/2026-04-16-rewrite-v3-design-draft.md` §3 (memory system)
- `docs/design-plans/2026-04-18-sqlx-to-rusqlite-evaluation.md` (rusqlite migration tradeoffs)
- `docs/notes/2026-04-17-pattern-runtime-modularity-eval.md` (cosa-prep refactors; opportunistic folding if touched here)

Future v3 plans follow this one:

- Plan 2: `v3-task-skill-blocks` — Task + Skill block subtypes with graph dependencies and trust tagging
- Plan 3: `v3-subagents` — ephemeral/fork/sibling primitives, fork-as-jj-workspace, coordination patterns rewired
- Plan 4 (if scope permits): `v3-plugins-mcp-iroh` — CC-compatible plugin system, MCP inverted surface, iroh-rpc transport

## Acceptance Criteria

<!-- TO BE GENERATED and validated before glossary -->

## Glossary

<!-- TO BE GENERATED after body is written -->
