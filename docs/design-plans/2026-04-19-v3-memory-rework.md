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

- `pattern_db` migrated from `sqlx` to `rusqlite` across all ~339 queries (~310 compile-time macro-verified + ~29 runtime `query_as`)
- `MemoryStore` trait sync-ified and audited down from 28 methods to ~18 via collapse (`list_blocks` variants merged behind a `BlockFilter` type; `update_block_metadata(id, patch)` replaces four separate setters; `undo_redo(op, label)` and `history_depth` replace four separate methods; `search(scope)` replaces `search` + `search_all`)
- `async_trait` usage removed from `MemoryStore` specifically; pattern_core retains `async_trait` dep for the 8 other traits with genuine async needs (ProviderClient, DataStream, EmbeddingProvider, etc.)
- FTS5 + `sqlite-vec` revalidated under rusqlite with regression coverage (BM25 scoring, `highlight`/`snippet`, hybrid score fusion)
- Connection pooling via `r2d2-sqlite` for async callsites (wrapped in `spawn_blocking` only for DB operations, not for cheap sync calls); eval worker owns a dedicated `Connection` outside the pool for its session lifetime
- WAL journal mode preserved (already enabled); applied uniformly via per-connection `init_connection` hook
- Transaction semantics preserved across the port — all three explicit transaction sites in `queries/memory.rs` (`update_block_config`, `insert_memory_block_update`, `consolidate_checkpoint`) port 1:1 to `rusqlite::Transaction`
- Cargo dep surface: `rusqlite 0.39` with features `bundled-full`, `load_extension`, `jiff`, `serde_json`; `r2d2` + `r2d2-sqlite`; `rusqlite_migration 1.0`; drop direct `libsqlite3-sys` pin (rusqlite's bundled SQLite replaces it)

### Database split

- Single `rusqlite` connection, two SQLite files attached at `init_connection`:
  - `memory.db` — blocks metadata, archival entries, memory FTS/vec indexes; VCS-tracked in pattern-jj or host VCS per mode
  - `messages.db` — message history + batches + FTS/vec indexes; NOT VCS-tracked; lives outside the mount
- Cross-domain queries supported via `ATTACH DATABASE msg` and explicit `msg.<table>` references
- `messages.db` path per mode: `~/.pattern/transient/<project-hash>/messages.db` (Mode A) or `~/.pattern/projects/<id>/messages/messages.db` (Mode B/C)

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

- Block content persisted as files under the mount; per-schema canonical format:
  - Text blocks → `.md`
  - Log blocks → `.jsonl`
  - Map / List / Composite blocks → `.kdl` (via the `kdl` crate; fork-as-insurance if maintainer stance ever disrupts downstream)
  - Skill blocks (Plan 2) → `.md` with YAML frontmatter
- Loro snapshots persisted alongside as the merge-authoritative CRDT state for concurrent-write resolution
- `memory.db` holds: FTS5 indexes, vector embeddings, archival entries, block metadata, loro update log — **not** block content
- Storage topology: **loro-primary with per-doc subscribers**. Writes go to loro; `doc.subscribe_root` callbacks fire post-commit; per-doc `sync_worker` tokio tasks emit the canonical file and update indexes (debounced 50ms). See Architecture for full detail.
- `LoroValue ↔ KdlDocument` conversion is hand-written (no serde); uses the kdl crate's `KdlDocument`/`KdlNode`/`KdlEntry` types; round-trip fidelity per the crate's format-preservation contract
- Disk ↔ memory synchronization adapted from `rewrite-staging/runtime_subsystems/data_source/file_source.rs` (notify-watcher, conflict detection, bidirectional subscriptions)
- Human edits to canonical files reconciled via loro CRDT merge on read (concurrent-write treatment, not overwrite)
- Invalid KDL from human edits is logged + surfaced; never attempted as a loro merge
- Self-emit-echo detection via content hash to prevent write-notify-rewrite loops

### Version history (jj)

- jj CLI integration via a thin adapter (~15-18 functions) as an internal module of `pattern_memory`
- jj-lib explicitly NOT embedded — library API is pre-1.0 and unstable; CLI surface is stable
- Adapter covers: workspace add/list/forget/update-stale, commit, log, bookmark set/delete, merge, restore
- All parseable output consumed via `-T 'json(...)'` template flags; free-form output (commit messages, diffs) parsed directly
- Graceful degradation when `jj` binary missing: Mode A (host VCS only) continues to work; Modes B/C fail loudly at attach time
- Version check at adapter detect-time; minimum jj version documented; version mismatch surfaces with a clear error
- Pre-commit quiesce step drains subscribers + `PRAGMA wal_checkpoint(TRUNCATE)` + fsync emitted files, ensuring pattern-jj commits a canonical + resumable `memory.db` alongside markdown/kdl/jsonl files
- `memory.db` is version-controlled under pattern-jj (Modes B/C) or host VCS (Mode A) — binary blob, no auto-merge, but quiesce makes each commit deterministic
- `messages.db` is NEVER version-controlled — pattern owns its persistence via the backup machinery (below)

### Messages backup/restore

- `messages.db` is persistent long-term state (weeks/months of conversation history), not ephemeral — a load-bearing Pattern differentiator
- SQLite native backup API (via rusqlite's `backup` feature, in `bundled-full`) for atomic snapshots
- Scheduled snapshots to `~/.pattern/backups/<project-id>/messages/<timestamp>.sqlite`
- Rotation policy: keep-N recent + thinning (hourly-for-day, daily-for-month, monthly-forever)
- `pattern backup create` + `pattern backup restore <timestamp>` CLI commands
- Pre-restore safety: auto-snapshot current state before restore as a rollback point
- jj blob-size config in pattern-jj repo init (Modes B/C): `[snapshot] max-new-file-size` set to comfortably hold `memory.db`

### Storage modes

- **Mode A** (in-repo, host-VCS-owned): `<project-repo>/.pattern/shared/` committed by host git/jj; pattern adds no history layer; quiesce runs before host VCS commits
- **Mode B** (separate, pattern-jj-tracked): `~/.pattern/projects/<project-id>/shared/`; pattern-jj owns history; directory optionally symlinked from project for path-resolution convenience
- **Mode C** (sidecar pattern-jj over host-repo working copy): pattern-jj stored at `.pattern/shared/.jj/` (gitignored by host); attempted via a validation spike in Phase 6; if passes explicit pass criteria, implemented with documented fragility caveats; otherwise documented-only with explicit deferral via fate marker
- Per-mount config (`.pattern.kdl`) selects mode and specifies mount-specific settings
- `.pattern.kdl` is a NEW config file in kdl format (existing pattern toml configs untouched in this plan)

### Block model + scopes

- **Two-tier block model** (Core / Working) formalized at the `pattern_memory` crate level; `BlockType::Archival` and `BlockType::Log` variants removed (Log's append-mostly semantics become a `BlockSchema::Log` concern; Archival is not a block tier at all)
- **Archival store** is a separate immutable entry model: `ArchivalEntry` rows in `memory.db`, searchable via FTS5 + vector, retrievable by handle for context insertion
- `MemoryStore::delete_archival` method retained in the trait for human ops (CLI/TUI curation) but removed from the agent-facing SDK effect surface (archival entries are immutable from the agent's perspective)
- `MemoryScope` wrapper type parameterizes MemoryStore access by `(persona_id, project_id, isolate_policy)`; scoped reads/writes route per the policy
- Persona-level memory always separate, always pattern-jj-tracked, always at `~/.pattern/personas/<persona-id>/` (never in a project repo)
- Project-scoped personas (`scope: project`) — persona definitions can live in `<mount>/personas/`, travel with the repo in Mode A or stay private in Mode B
- `isolate_from_persona` flag (`none` / `core-only` / `full`) implemented as a real attachment-time policy with specified read/write routing per tier
- Default write target for scoped access is project scope; persona write-back requires explicit `ctx.memory.write_to_persona` effect (only when policy is `none`)

### Project utilities

- `<mount>/lib/` directory convention: Haskell modules importable by the agent program at session instantiation
- Runtime compile-logic extended to include the project's `lib/` directory in Tidepool's import search path
- Library compilation is **try-with-report, not try-or-fail**: modules that fail to compile are excluded from the import path; main agent program compiles with whatever succeeded; imports of broken modules surface as clear Tidepool 'module not found' errors
- `Pattern.Diagnostics` SDK effect surfaces library compile warnings/errors to the agent without blocking session open (when main program doesn't import broken libs)
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
- Setup hooks (`<mount>/setup/`) — requires lifecycle event system, bound to plugin-system plan
- Subagent fork-as-jj-workspace semantics — Plan 3: `v3-subagents`
- v2 → v3 data migrator — dedicated migrator plan
- Plugin system, MCP, iroh-rpc
- Compaction strategy changes (existing four strategies preserved)
- Session-extension-based time-travel for messages.db (future enhancement beyond snapshot-based backup)
- Session / AgentRuntime trait sync-ification (only `MemoryStore` is desync'd here; the handful of forward-compat-async methods on other traits stay async)

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

## Architecture

Pattern v3 Memory Rework reshapes the memory subsystem across five structural axes that land in coordinated phases: a crate extraction, a storage-backend migration, a trait-surface simplification, a filesystem-canonical storage model, and a version-history integration. Each axis is individually reviewable; together they leave pattern with a cleaner layering, honest synchronous database access, and a storage model that makes the canonical state inspectable and VCS-compatible.

### Crate layering

The memory subsystem splits across three crates:

- **`pattern_core`** (trait-only, shrinks further): `MemoryStore` trait (now sync), trait-signature value types (`BlockType`, `BlockSchema`, `BlockMetadata`, `ArchivalEntry`, `SharedBlockInfo`, `SearchOptions`, `MemorySearchResult`, schema helpers), `MemoryError`, `MemoryResult`. No implementation code.
- **`pattern_memory`** (new): `MemoryCache` impl (the canonical `MemoryStore` implementation), `StructuredDocument` (Loro wrapper), `SharedBlockManager`, schema templates, kdl/jsonl/markdown serialization, loro-native subscriber machinery, jj CLI adapter, storage mode handling, backup/restore. Depends on `pattern_core` + `pattern_db`.
- **`pattern_db`** (rewired): rusqlite-based queries, FTS5 + `sqlite-vec` indexes, connection pool, migrations. Depends on `pattern_core` for trait types.

Dependency graph: `pattern_memory → pattern_core + pattern_db`; `pattern_runtime → pattern_core + pattern_memory`; no cycles.

The type split follows **resolution A**: types that appear in `MemoryStore` trait signatures (data contract types) live in `pattern_core::types::memory_types` alongside the trait. Implementation-only types (`CachedBlock`, `ChangeSource`) move to `pattern_memory`. This keeps the dependency graph clean without forcing contract types into the implementation crate.

### Storage backend: sqlx → rusqlite, sync surface

`pattern_db` migrates from `sqlx` to `rusqlite 0.39` with features `bundled-full`, `load_extension`, `jiff`, and `serde_json`. The direct `libsqlite3-sys` pin is dropped — rusqlite's bundled SQLite (3.51.3) replaces it. `sqlite-vec` continues to load at runtime via `Connection::load_extension` (path shifts from `sqlite3_auto_extension` to per-connection load).

Connection strategy is split by caller pattern:

```
pattern_db::ConstellationDb
  ├── pool: r2d2::Pool<SqliteConnectionManager>    // for async callsites via spawn_blocking
  │     max_size: 10, min_idle: 2, connection_timeout: 30s
  │     each connection passes through init_connection:
  │       - PRAGMA journal_mode=WAL, foreign_keys=ON, busy_timeout=5000
  │       - PRAGMA cache_size=-65536 (64 MiB), mmap_size=268435456 (256 MiB)
  │       - sqlite-vec extension loaded
  │       - messages.db ATTACHed as schema `msg`
  └── dedicated_connection()                       // for eval worker (owns for session lifetime)
        same init_connection hook, NOT pool-managed
```

`MemoryStore` is sync-ified. 28 original methods consolidate to ~18:

- `list_blocks`, `list_blocks_by_type`, `list_all_blocks_by_label_prefix` → one `list_blocks(filter: BlockFilter)`
- `set_block_pinned`, `set_block_type`, `update_block_schema`, `update_block_description` → one `update_block_metadata(id, patch: BlockMetadataPatch)`
- `undo_block`, `redo_block`, `undo_depth`, `redo_depth` → `undo_redo(label, op: UndoRedoOp)` + `history_depth(label) -> UndoRedoDepth`
- `search`, `search_all` → one `search(scope: SearchScope)`

`MemoryStore::delete_archival` stays in the trait for human-operator curation but is removed from the agent-facing SDK effect surface. Agents can `insert_archival` and `search_archival`; archival entries are immutable from the agent's perspective.

Domain scalar types implement rusqlite's `FromSql` / `ToSql` traits (e.g., `BlockType`, `BlockPermission`, JSON-blob columns as `serde_json::Value`). Row-struct deserialization uses inherent `fn from_row(&rusqlite::Row) -> rusqlite::Result<Self>` methods per struct — no `FromRow` helper trait, no derive macro (boilerplate is bounded and explicit is auditable).

### Database split: memory.db + messages.db

Memory and messages live in separate SQLite files, attached through a single rusqlite connection:

```
init_connection:
  open main path                = memory.db (per mode)
  ATTACH path AS msg            = messages.db (pattern-owned, outside VCS)
  apply pragmas to both
  load sqlite-vec extension (applies to attached dbs too)
```

Cross-domain queries reference the attached schema explicitly: `SELECT ... FROM main.memory_blocks b JOIN msg.messages m ON ...`. Attaching at init time keeps the pool simple (one connection = both databases).

Mode-dependent paths:

| File | Mode A | Mode B | Mode C |
|---|---|---|---|
| `memory.db` | in `<mount>/memory.db`, host-VCS-tracked | in `<mount>/memory.db`, pattern-jj-tracked | in `<mount>/memory.db`, pattern-jj-tracked (host VCS gitignores `.jj/`) |
| `messages.db` | `~/.pattern/transient/<project-hash>/messages.db` | `~/.pattern/projects/<id>/messages/messages.db` | `~/.pattern/transient/<project-hash>/messages.db` |

`messages.db` never enters any VCS. Pattern owns its persistence via the backup machinery described below.

### Eval worker simplification

The per-session multi-thread tokio runtime in `crates/pattern_runtime/src/agent_loop/eval_worker.rs` is removed. The worker becomes a plain OS thread fed by `std::sync::mpsc`. All 22 `Handle::current().block_on` call sites in memory / recall / search / scope handlers become direct method calls on the sync `MemoryStore`.

The async orchestrator continues to call `Session::step`; internally, `step` sends the request via `std::sync::mpsc` to the sync eval worker and awaits reply via `tokio::sync::oneshot`. The `Session::step` signature stays async from the caller's perspective; the implementation no longer participates in nested-runtime-from-within-runtime patterns.

Async callsites that touch `MemoryStore` (`pattern_cli` agent/group commands, turn-boundary persist/compaction decisions in `pattern_runtime`) wrap DB operations in `tokio::task::spawn_blocking`. Cheap non-DB sync operations (metadata reads from in-memory caches, handle validation) call directly — `spawn_blocking` is reserved for operations that would starve the async executor.

### Storage topology: loro-primary with per-doc subscribers

Block writes apply to a LoroDoc and commit. The LoroDoc's own subscription machinery is the event source for downstream sync:

```
MemoryStore::put_block(agent_id, label, content)
  1. apply change to LoroDoc (in-memory)
  2. doc.commit()  ─────────────┐  fires doc.subscribe_root callbacks
  3. persist loro delta         │
     to memory.db updates log   │
  4. return to caller           │
                                ▼
                   sync_worker task per loaded doc
                     (tokio task; supervised)
                     ├── debounce 50ms
                     ├── borrow pool connection
                     ├── emit canonical file (md/kdl/jsonl)
                     ├── update FTS5 row for block
                     ├── queue vector re-embed if hash changed
                     ├── release connection
                     └── heartbeat to supervisor
```

Key properties:

- **Per-doc parallelism**: N loaded docs = N sync_worker tasks. No central queue contention.
- **Debounce at the subscriber**: rapid writes (streaming text updates, multiple committed fields) coalesce into a single file emission within 50ms. Loro's commit cadence provides the natural event boundary; the subscriber batches further.
- **Pool-borrow per work unit**: workers don't hold connections while idle between events.
- **Idempotent**: on crash, restart emits current doc state. Loro is the truth; files are derived.
- **Supervisor**: one supervisor per `MemoryCache` instance watches all sync_worker tasks. 30s heartbeat timeout → log ERROR, restart worker, increment `metrics::counter!("memory.sync_worker.restart")`. Bounded channels prevent unbounded growth on backpressure.

### Canonical file serialization

Each block schema maps to a file format chosen for readability and loro round-trip fidelity:

| Schema | Format | Extension | Conversion |
|---|---|---|---|
| Text | Markdown | `.md` | LoroDoc text → raw markdown |
| Log | JSONL | `.jsonl` | one log entry per line |
| Map | KDL | `.kdl` | `LoroValue::Map` → `KdlDocument` nodes |
| List | KDL | `.kdl` | `LoroValue::List` → `KdlDocument` list nodes |
| Composite | KDL | `.kdl` | sections as top-level nodes with children |

KDL was chosen over JSON for Map/List/Composite because it's substantially more human-readable for nested data and the `kdl` crate explicitly guarantees round-trip fidelity ("Documents fully roundtrip"). The conversion between `LoroValue` and `KdlDocument` is hand-written (no serde), using the crate's `KdlDocument` / `KdlNode` / `KdlEntry` types directly. `LoroValue`'s shape (nested Map/List/scalar) maps cleanly to KDL nodes with entries; the converter is bounded in scope (~100-200 lines per format module).

Skill blocks (Plan 2) will use `.md` with YAML frontmatter; the format is locked now so Plan 2 doesn't need to re-decide.

**External edit flow** (human edits a .md/.kdl/.jsonl file):

```
notify watcher (notify 8.2 + notify-debouncer-full, 500ms debounce)
  ↓
hash check: matches our last emission? → self-echo, ignore
  ↓
parse disk via format converter → LoroValue
  ↓
doc.import(as_update) — CRDT merge, not replace
  ↓
doc.commit() → fires our own subscribers → re-emits canonical
  ↓
second notify event matches hash → ignored. stable.
```

Invalid KDL from a human edit is logged + surfaced via `metrics::counter!("memory.kdl.parse_failed")`; the pre-existing loro state continues to win and the next subscriber emission overwrites the broken file with the valid canonical version.

### Version history: jj CLI adapter + pre-commit quiesce

`pattern_memory::jj::adapter` is a thin CLI wrapper (~15-18 functions) that shells out to `jj` and parses `-T 'json(...)'` template output. Coverage:

```rust
JjAdapter::detect(workspace_root) -> Option<Self>

// Workspace ops
workspace_list, workspace_add, workspace_forget, workspace_update_stale

// Commit ops
commit, log, describe

// Bookmark ops
bookmark_set, bookmark_delete, bookmark_list

// Merge + restore
merge, restore_from

// Init (Mode B setup)
init_repo
```

`JjAdapter::detect` probes for the jj binary via `which::which`; returns `None` if missing. The `StorageMode` enum branches on adapter availability: Mode A operates without the adapter (host VCS owns commits); Modes B and C require it at attachment time. Version check at detect-time surfaces `JjError::UnsupportedVersion` with clear remediation text.

**Pre-commit quiesce** (universal across modes):

```rust
fn quiesce(&self) -> Result<()>:
  1. for each sync_worker: signal drain, wait for heartbeat-post-drain
  2. get a connection; PRAGMA wal_checkpoint(TRUNCATE) on memory.db
  3. fsync all emitted files in the mount (best-effort)
  4. return — caller proceeds with VCS commit
```

In Mode A, the caller invokes `quiesce()` before the host VCS commit. In Modes B/C, `quiesce()` runs as part of pattern's own commit flow before `JjAdapter::commit(message)`.

### jj-lib vs CLI

jj-lib is pre-1.0 with an explicitly unstable API. Pattern does **not** embed jj-lib; the CLI surface is stable and sufficient. If jj-lib stabilizes and pattern needs in-process operations, that's a future migration — not this plan.

### Storage modes A / B / C + mount attachment

A mount is a directory containing a Pattern-managed block store. The universal mount layout:

```
<mount>/
  blocks/
    core/              # core-tier blocks (small, always in context)
    working/           # working-tier blocks (on-demand)
  personas/            # project-scoped persona definitions
  lib/                 # project utility Haskell modules
  memory.db            # memory state (VCS-tracked per mode)
  .pattern.kdl         # mount config
```

`.pattern.kdl` is a kdl-format config file specifying mode, persona bindings, isolation policy, and jj options. Example:

```kdl
mount mode="A" memory_db="memory.db"

personas {
    default "@pattern-default"
}

isolate_from_persona policy="none"

jj enabled=true max_new_file_size="100MiB"

project name="pattern-dev" created_at="2026-04-19T12:00:00Z"
```

Per-mode specifics:

- **Mode A** (in-repo, host-VCS-owned): mount at `<project-repo>/.pattern/shared/`. Host VCS (git or jj) commits markdown/kdl/jsonl files + `memory.db`. Pattern never runs `jj` commands in this mode. `messages.db` lives in `~/.pattern/transient/<project-hash>/`.
- **Mode B** (separate, pattern-jj-tracked): mount at `~/.pattern/projects/<project-id>/shared/`. Pattern-jj owns history, colocated with the mount. Optional symlink `<project-repo>/.pattern → ~/.pattern/projects/<project-id>/shared/` for path-resolution convenience.
- **Mode C** (sidecar pattern-jj over host-repo working copy): mount at `<project-repo>/.pattern/shared/` (same as Mode A), with pattern-jj storing metadata at `.pattern/shared/.jj/`. Host git's `.gitignore` excludes `.jj/`. Two VCSes over the same working copy. `jj workspace update-stale` reconciles jj's view after host operations. Requires a validation spike in Phase 6 before committing to ship.

Mount detection at session attach: pattern walks upward from the target directory looking for `.pattern/shared/.pattern.kdl` (Mode A/C) or a pattern-managed symlink. On find: parse config, resolve mount, open memory.db + messages.db, set up subscribers, register with jj if Mode B/C.

### Two-tier block model + scopes

`BlockType` variants collapse to `Core | Working`. The previous `Archival` and `Log` variants were conflations: Archival is a separate data model (immutable entries, not block tiers); Log is a structural schema (`BlockSchema::Log`) orthogonal to tier.

```rust
pub enum BlockType {
    /// Always rendered in segment 3 of the cache layout.
    /// Identity, current-focus content. Bounded by a configurable budget.
    Core,
    /// Referenced by handle. Loaded on demand. Rendered only if attached.
    Working,
}
```

Archival entries live in `memory.db`'s archival table and are accessed via `MemoryStore::insert_archival`, `search_archival`, and `delete_archival` (human-ops only; not exposed as agent effect).

**MemoryScope** is a pure data-transformation wrapper over a `MemoryStore` that routes reads/writes according to an `isolate_from_persona` policy:

```rust
pub struct MemoryScope<S: MemoryStore> {
    inner: S,
    binding: ScopeBinding,
}

pub struct ScopeBinding {
    persona_id: AgentId,
    project_id: Option<ProjectId>,
    isolate_policy: IsolatePolicy,
}

pub enum IsolatePolicy {
    None,        // persona + project merged; bi-directional writes
    CoreOnly,    // persona core read-only from project; project writes stay project-scoped
    Full,        // persona identity only; no persona memory carryover
}
```

Read/write routing per policy:

| Policy | Core blocks | Working blocks | Archival search | Persona identity |
|---|---|---|---|---|
| `None` | persona + project merged, bi-directional writes | persona + project handles both visible | merged search across persona + project archives | full |
| `CoreOnly` | persona core visible as read-only; project core owns writes | project-scope only | project archive only | full |
| `Full` | not visible at all | project-scope only | project archive only | name + instructions only, no memory continuity |

Default write target for scoped access is project scope. Persona write-back requires an explicit `ctx.memory.write_to_persona(...)` effect, which errors unless policy is `None`. This prevents accidental cross-project contamination.

### Project utilities: `<mount>/lib/`

Project-local Haskell modules live at `<mount>/lib/` following Cabal's directory-to-module-name convention:

```
<mount>/lib/
  Project/
    Review.hs        # module Project.Review
    Utils/
      Task.hs        # module Project.Utils.Task
```

At session open, `pattern_runtime::sdk::location::resolve_import_paths` extends Tidepool's import search path to include the mount's `lib/` directory (if present). Agent programs `import Project.Review qualified as Review` and the module resolves through the extended path.

**Compilation is try-with-report, not try-or-fail**:

1. Each `.hs` file in `lib/` is compiled independently at session open
2. Successful modules join the import search path
3. Failed modules are excluded from the path; their diagnostics are captured
4. Main agent program compiles with whatever lib modules succeeded
5. If main program imports a broken module: standard Tidepool 'module not found' with diagnostic reference
6. Agent can query `Pattern.Diagnostics.diagnostics :: Effect [Diagnostic]` to surface library-compile issues programmatically

This keeps session open robust to a single broken helper module while still surfacing problems to the agent so it can respond.

### Messages.db backup/restore

`messages.db` is persistent long-term state — weeks to months of conversation history, searchable over the full range. Neither jj nor git nor LFS is appropriate for this data shape. Pattern owns the backup model directly:

```rust
pub fn create_snapshot(&self) -> Result<SnapshotInfo>:
  1. use rusqlite's native backup API (from `backup` feature in bundled-full)
  2. atomic copy to ~/.pattern/backups/<project-id>/messages/<iso8601>.sqlite
  3. record metadata (timestamp, source_frontier, size, hash)
  4. apply rotation policy (see below)

pub fn restore_snapshot(&self, timestamp: &str) -> Result<()>:
  1. snapshot current messages.db as a rollback safety net
  2. replace messages.db with the selected snapshot
  3. verify the restored database opens + pragmas apply
  4. return — caller re-attaches as needed
```

Rotation policy (configurable via `.pattern.kdl`):

- Keep last N snapshots regardless of age (default N=24)
- Thin older: keep hourly-for-day, daily-for-month, monthly-forever

Snapshot scheduling is initially time-based (configurable interval, default 1 hour during active use). Future enhancements may add cycle-based triggers (on compaction cycle end) or size-based triggers.

CLI surface: `pattern backup create` + `pattern backup restore <timestamp>`. These are minimum-viable commands — polished CLI/TUI experience comes later.

### Cache breakpoint interaction

The foundation plan established the three-segment cache layout (system + instructions / history / current block state). This plan preserves that layout unchanged. Segment 3 (block state) continues to be assembled from the Core + loaded-Working blocks. The storage rework affects WHERE that content comes from (markdown + loro-merged state, not raw loro snapshot from sqlite) but not WHERE it renders in the request.

## Existing Patterns

**Preserved patterns**:

- **loro CRDT for memory blocks**: `StructuredDocument` wraps `LoroDoc` with metadata + accessor tracking. The wrapping layer stays intact; only its storage location (crate) changes.
- **pattern_db FTS5 + `sqlite-vec` hybrid search**: search logic in `crates/pattern_db/src/{fts.rs, vector.rs, search.rs}` ports to rusqlite with identical SQL surface. BM25 scoring (`rank / -10`), `highlight()`, `snippet()`, and hybrid score fusion all preserved verbatim at the SQL level.
- **MessageBatch integrity in compression**: the four compression strategies (Truncate, RecursiveSummarization, ImportanceBased, TimeDecay) continue to operate on complete batches only. Compression code is untouched by this plan.
- **Coordination infrastructure**: supervisor / round-robin / pipeline / voting / sleeptime / dynamic patterns in `pattern_core::coordination` stay intact. Not exercised by this plan's scope but left untouched.
- **Disk ↔ memory sync machinery from `rewrite-staging/runtime_subsystems/data_source/file_source.rs`**: 2039 lines of notify-watcher, conflict detection, and bidirectional subscriptions. Zero sqlx deps (verified). Ported from the staging area into `pattern_memory::fs::*` and adapted for memory-block semantics (file paths derived from block handle + schema instead of arbitrary file-source paths).
- **Schema block types** (Text / Map / List / Log / Composite): structural shapes retained. Log's append-mostly semantics still implemented via `BlockSchema::Log { display_limit, entry_schema }`.
- **Anthropic OAuth credential storage and provider patterns**: unchanged; owned by `pattern_provider` from the foundation plan.

**Divergences from current code**:

- **`pattern_memory` is a new crate**, extracted from `pattern_core::memory::*`. `pattern_core` shrinks to trait-only deepened: the memory trait + data types stay; all implementation code moves.
- **`pattern_db` backend swap**: `sqlx` → `rusqlite`. All ~339 queries rewrite. Pool management shifts from `sqlx::SqlitePool` to `r2d2::Pool<SqliteConnectionManager>`. Migration runner shifts from `sqlx-cli prepare` to `rusqlite_migration`.
- **`MemoryStore` becomes sync**. `async_trait` removed from the trait; 28 async methods become sync; trait surface audited from 28 → ~18 via consolidation.
- **Eval worker loses its tokio runtime**. Multi-thread tokio + block_on bridging replaced by a plain OS thread driven by `std::sync::mpsc`.
- **Block content moves out of `memory.db`**. Storage becomes file-system canonical (md/kdl/jsonl); `memory.db` holds indexes + archival + metadata only.
- **Messages storage splits from memory storage**. New `messages.db` attached via `ATTACH DATABASE`. Backup/restore machinery is new surface.
- **`BlockType` simplified to Core | Working**. Archival and Log variants deleted.
- **New mount model** (Mode A/B/C) with `.pattern.kdl` config. Prior pattern had no formal mount concept.
- **`pattern_macros` crate deleted** pre-phase. No longer in the workspace; derive-macro path consciously declined in favor of explicit from_row impls.

**Patterns not applicable (no existing precedent)**:

- **KDL serialization of LoroValue**: novel. No prior pattern work to reference. Conversion layer is bounded and the `kdl` crate's round-trip guarantee provides a firm contract.
- **Mode C sidecar (pattern-jj over host-VCS working copy)**: novel. No known public reference of jj-in-git-working-copy operating at production-quality. Validated via spike in Phase 6 before committing to ship.
- **`Pattern.Diagnostics` SDK effect**: new SDK surface for surfacing project-lib compile issues to agents.

## Implementation Phases

Nine phases. Sequential dependency chain with Phases 7 and 8 optionally parallelizable.

<!-- START_PHASE_1 -->
### Phase 1: Extract pattern_memory crate (sqlx preserved)

**Goal:** Mechanical structural refactor. `pattern_memory` exists as a crate with the implementation code; `pattern_core` retains only trait + data types. No behavior change.

**Components:**
- `crates/pattern_memory/Cargo.toml` + `src/lib.rs` with module declarations
- Move from `pattern_core/src/memory/`: `cache.rs` → `pattern_memory/src/cache.rs`, `document.rs`, `sharing.rs`, schema templates
- Split `pattern_core/src/memory/types.rs`: trait-signature types (`BlockType`, `BlockSchema`, `BlockMetadata`, `ArchivalEntry`, `SharedBlockInfo`, `SearchOptions`, `MemorySearchResult`, `SearchMode`, `SearchContentType`, `TextViewport`, `CompositeSection`, `FieldDef`, `FieldType`, `LogEntrySchema`) stay in `pattern_core::types::memory_types`; impl-only types (`CachedBlock`, `ChangeSource`) move to `pattern_memory::types_internal`
- `MemoryStore` trait stays in `pattern_core::traits::memory_store` with `#[async_trait]` unchanged (desync happens in Phase 3)
- Update all `pattern_runtime` imports (~17 files per investigation) to reference `pattern_memory::` for implementations and `pattern_core::` for trait + types
- Add `pattern_memory` to workspace `Cargo.toml` `members`
- Update `docs/plans/rewrite-v3-portlist.md` to record the extraction

**Dependencies:** None (first phase)

**Done when:**
- `cargo check --workspace` passes
- `cargo nextest run -p pattern-memory` passes every moved test (memory tests + integration tests)
- `cargo doc -p pattern_memory` produces complete documentation
- API-parity assertion test: `StructuredDocument` and `MemoryCache` expose the same public surface as before the move (smoke test construct + call a handful of methods)
- Covers: `v3-memory-rework.AC1.*`
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Rusqlite migration + DB split + sqlite-vec spike

**Goal:** pattern_db runs on rusqlite with pooled connections. `memory.db` and `messages.db` split via ATTACH. sqlite-vec loads cleanly under rusqlite's bundled SQLite. `BlockType::Archival` and `BlockType::Log` variants removed with call-site audit.

**Prerequisites (blocking):** sqlite-vec compatibility spike (Task 2a below). If spike fails, pause; research fallback (version-pin sqlite-vec, bundle our own, etc.) before proceeding.

**Components:**
- **Task 2a (blocking spike)**: `crates/pattern_db/tests/sqlite_vec_smoke.rs` — open in-memory db, create vec0 virtual table with 384-dim floats, insert 100 vectors, run KNN query, assert ordering. PASS = bundled SQLite 3.51.3 + sqlite-vec 0.1.7-alpha.2 compatible. FAIL = research fallback before further Phase 2 work.
- Cargo dep swap: drop `sqlx`, `libsqlite3-sys` direct pin; add `rusqlite 0.39` with `bundled-full` + `load_extension` + `jiff` + `serde_json` features; add `r2d2` + `r2d2-sqlite`; add `rusqlite_migration 1.0`
- `crates/pattern_db/src/connection.rs`: `ConstellationDb` rewrite with `r2d2::Pool`, `init_connection` hook (WAL, pragmas, sqlite-vec load, messages.db ATTACH), `dedicated_connection()` for eval worker
- Migration runner: use existing `crates/pattern_db/migrations/*.sql` files (rename if `rusqlite_migration` format requires)
- Port ~339 queries across `queries/*.rs` and `fts.rs` / `vector.rs` / `search.rs` — each query moves from `sqlx::query!` / `sqlx::query_as!` to rusqlite statement + `from_row` inherent method on the row struct
- `FromSql` / `ToSql` impls for domain scalar types (`BlockType`, `BlockPermission`, `JsonValue` columns)
- Port 3 explicit transaction sites in `queries/memory.rs`: `update_block_config`, `insert_memory_block_update`, `consolidate_checkpoint`
- `BlockType` enum: remove `Archival` and `Log` variants; migrate all usage sites (schema update in a migration file; re-classify any lingering `BlockType::Log` blocks to `Working` tier with `BlockSchema::Log` schema, and move any `BlockType::Archival` blocks to archival entries)
- Split `messages.db` into its own sqlite file (schema migration: extract messages + message batch tables from current db; data migration deferred to v2→v3 migrator plan — this plan ships the split on new data only)

**Dependencies:** Phase 1 (pattern_memory crate exists)

**Done when:**
- Task 2a spike passes; documented in a note file alongside the design plan
- `cargo check --workspace` passes
- `cargo nextest run -p pattern-db` passes every pattern_db integration test (regression proof of the port)
- FTS5 BM25 snapshot tests (insta) land, capturing scoring output for a representative corpus
- Vector KNN regression test passes against a known similarity structure
- Concurrent pool stress test: 20 concurrent `spawn_blocking` calls make queries without deadlock or contention-related failures
- Transaction atomicity tests: intentionally failing a mid-transaction query leaves the database in the pre-transaction state
- `BlockType` enum has only `Core` + `Working` variants; `cargo check --workspace` confirms no lingering `Archival` or `Log` variant references
- Covers: `v3-memory-rework.AC2.*`, `v3-memory-rework.AC3.*`
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: MemoryStore sync + surface audit + eval worker simplification + async callsite migration

**Goal:** `MemoryStore` is sync, consolidated down to ~18 methods. Eval worker runs on a plain OS thread. Async callsites use `spawn_blocking` only for DB ops. Session::step's internal path no longer uses `block_on`.

**Components:**
- Desync `MemoryStore` trait: remove `#[async_trait]`, change all 28 methods to `fn` returning `MemoryResult<T>` directly
- Surface consolidation:
  - `list_blocks`, `list_blocks_by_type`, `list_all_blocks_by_label_prefix` → `list_blocks(filter: BlockFilter)`
  - `set_block_pinned`, `set_block_type`, `update_block_schema`, `update_block_description` → `update_block_metadata(id, patch: BlockMetadataPatch)`
  - `undo_block`, `redo_block` → `undo_redo(label, op: UndoRedoOp)`
  - `undo_depth`, `redo_depth` → `history_depth(label) -> UndoRedoDepth`
  - `search`, `search_all` → `search(scope: SearchScope)`
- Remove `ctx.memory.archive.delete(...)` from the agent-facing SDK effect surface (`pattern_runtime::sdk::requests::memory`); `MemoryStore::delete_archival` stays in the trait for human ops
- `MemoryCache` impl updated to match new sync signatures; all internal `sqlx::query*` calls shift to rusqlite (inherited from Phase 2)
- `pattern_runtime::agent_loop::eval_worker`: drop per-session tokio runtime; worker runs as `std::thread::spawn` with `std::sync::mpsc::channel` for requests; replies via `tokio::sync::oneshot`
- `pattern_runtime::agent_loop::orchestrate::drive_step` (or `Session::step` impl): send request via sync mpsc, await reply via oneshot. Caller-visible async signature unchanged.
- All 22 `Handle::current().block_on` sites in `handlers/memory.rs`, `handlers/recall.rs`, `handlers/search.rs`, `handlers/scope.rs`: replaced with direct sync calls
- Async callsite updates: `pattern_cli` commands (~40-50 sites) wrap `MemoryStore` calls in `tokio::task::spawn_blocking`. Non-DB sync ops (cache metadata, handle validation) call directly.
- `pattern_runtime` turn-boundary code: similar `spawn_blocking` wrapping

**Dependencies:** Phase 2 (rusqlite in place, MemoryStore behavior preserved)

**Done when:**
- `cargo check --workspace` passes
- `cargo nextest run --workspace` passes (regression proof across the sync surface change)
- `cargo test --doc` passes (doctests on sync trait)
- Eval worker unit test: spawn worker, send 100 eval requests, assert all complete without tokio-runtime-detection panics
- Async interop test in `pattern_cli`: command dispatch + `spawn_blocking` wrapped `MemoryStore` call works correctly
- Search bug regression test: the pre-existing `spawn_blocking`-related search issue flagged in the eval doc is resolved (documented test)
- `delete_archival` effect removed: `cargo check -p pattern_runtime` fails if any agent SDK handler still invokes `ctx.memory.archive.delete`
- Covers: `v3-memory-rework.AC4.*`, `v3-memory-rework.AC5.*`
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Fs serialization + loro-native subscribers + notify watcher

**Goal:** Canonical file emission (md/kdl/jsonl) from LoroDoc commits. External file edits merge via loro CRDT. Subscriber supervisor restarts failed workers.

**Components:**
- `pattern_memory/src/fs/markdown.rs` — Text block ↔ `.md` conversion
- `pattern_memory/src/fs/kdl.rs` — `LoroValue` ↔ `KdlDocument` conversion, hand-written, no serde. Handles Map/List/Composite.
- `pattern_memory/src/fs/jsonl.rs` — Log block ↔ `.jsonl` conversion (one entry per line)
- `pattern_memory/src/subscriber/mod.rs` — per-doc `sync_worker` task spawned when a LoroDoc is loaded into MemoryCache; channel-bounded; 50ms debounce; on each debounce tick: export canonical file, update FTS5 row, queue vector re-embed if hash changed
- `pattern_memory/src/subscriber/supervisor.rs` — per-MemoryCache supervisor; 30s heartbeat watchdog; panic → log ERROR + restart + `metrics::counter!("memory.sync_worker.restart")`
- `pattern_memory/src/fs/watcher.rs` — `notify 8.2` + `notify-debouncer-full` watcher per mount; emits change events into a channel consumed by an ingest task that parses the file and applies as a loro update
- Hash-based self-emit-echo detection: track `last_emitted_hash` per emitted path; watcher events matching the hash are ignored
- Audit/add `metrics` crate as a dep if not already present; wire counters for sync_worker restarts, KDL parse failures, external-edit merges

**Dependencies:** Phase 3 (sync MemoryStore is the subscriber's DB surface)

**Done when:**
- `cargo check --workspace` passes
- Round-trip property tests (proptest) for each format: `md → LoroValue → md`, `kdl → LoroValue → kdl`, `jsonl → LoroValue → jsonl` equivalence
- LoroValue ↔ KdlDocument edge-case tests: nested maps, lists, numeric precision boundaries, special keywords, string escaping
- Subscriber integration test: write block, observe file emitted within 100ms; hash matches expected content
- External-edit test: modify .md file externally, observe loro merge, observe re-emission; initial edit preserved
- Subscriber restart test: panic in worker callback → supervisor detects heartbeat timeout within 30s → restart; metric counter increments
- Self-echo suppression test: write block, observe single emission (not a loop)
- Invalid KDL test: write malformed .kdl externally, observe parse-failed metric increment, observe no loro merge, observe valid content re-emitted
- Covers: `v3-memory-rework.AC6.*`, `v3-memory-rework.AC7.*`
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: jj CLI adapter + pre-commit quiesce

**Goal:** Pattern can run jj workspace/commit/bookmark/merge/restore operations via the CLI adapter. Pre-commit quiesce drains subscribers and checkpoints memory.db.

**Components:**
- `pattern_memory/src/jj/adapter.rs` — `JjAdapter` struct with `detect`, `workspace_*`, `commit`, `log`, `describe`, `bookmark_*`, `merge`, `restore_from`, `init_repo`. All parseable subcommand output parsed from `-T 'json(...)'` templates.
- `pattern_memory/src/jj/error.rs` — `#[non_exhaustive] JjError` covering `BinaryNotFound`, `SubprocessFailed`, `OutputParseFailed`, `UnsupportedVersion`, `WorkspaceNotFound`, `BookmarkNotFound`
- `pattern_memory/src/jj/quiesce.rs` — `quiesce()` function: signal each sync_worker to drain, wait for post-drain heartbeat, `PRAGMA wal_checkpoint(TRUNCATE)` on memory.db, fsync emitted files
- jj version check at `detect()` time; minimum supported version documented in `JjAdapter::MIN_SUPPORTED_VERSION` const
- `StorageMode` enum (in `pattern_memory::modes`) distinguishes whether jj adapter is active per mode

**Dependencies:** Phase 4 (subscriber infrastructure exists to drain)

**Done when:**
- `cargo check --workspace` passes
- JjAdapter integration tests in `pattern_memory/tests/jj_adapter.rs` using temp-dir jj repos (no mocking; real subprocess)
- Template JSON parse snapshot tests (insta) for `workspace list`, `log`, `bookmark list`
- Quiesce integration test: spawn N concurrent writes, call `quiesce()`, assert all sync_worker queues drained + wal truncated + fs state matches loro state
- jj-missing test: `JjAdapter::detect` on a system without jj returns `None`, no panic
- Version-mismatch test: mock a `jj --version` returning too-old output; assert `UnsupportedVersion` error surfaces
- Covers: `v3-memory-rework.AC8.*`
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: Storage modes A + B + Mode C spike + .pattern.kdl + mount attachment

**Goal:** Mounts in Modes A and B work end-to-end. Mode C spike passes or is documented as deferred. `.pattern.kdl` config parses + validates.

**Components:**
- `pattern_memory/src/modes/mod.rs` — `StorageMode` enum variants (A, B, C) with per-mode setup + attach/detach logic
- `pattern_memory/src/config/pattern_kdl.rs` — parse `.pattern.kdl` into a typed `MountConfig` struct; validate mode + persona bindings + jj options
- Mode A path resolution: `<project-repo>/.pattern/shared/`, host VCS detection (look for `.git` or `.jj` at project root), automatic gitignore rules for `.pattern/transient/`
- Mode B path resolution: `~/.pattern/projects/<id>/shared/`; init pattern-jj at mount; optional symlink setup
- Mode C: pattern-jj at `<mount>/.jj/`; host git gitignore for `.jj/`; documented `jj workspace update-stale` reconciliation
- `pattern_memory/src/modes/attach.rs` — `attach(path: &Path) -> Result<MountedStore>`: walk upward for `.pattern/shared/.pattern.kdl`, parse config, open `memory.db` + `messages.db`, spawn subscribers, return mounted store handle
- Minimum CLI entry points (in `pattern_memory/bin/` or extended into `pattern-test-cli`): `pattern mount init <mode>` + `pattern attach <path>` sufficient for manual testing + the smoke-test in Phase 9
- **Task 6a (spike)**: Mode C validation spike — scripted test that initializes host git repo, inits pattern-jj at `.pattern/shared/.jj/`, runs 50 interleaved operations, checks for state divergence. Pass criteria: no user-visible corruption; gitignore + `update-stale` handle typical workflows without manual intervention.

**Dependencies:** Phase 5 (jj adapter exists for Modes B/C)

**Done when:**
- `cargo check --workspace` passes
- Mode A end-to-end test: temp host-git repo + `.pattern/shared/` + block write + host git commit + verify state
- Mode B end-to-end test: pattern-jj temp repo + block write + pattern-jj commit via quiesce → `JjAdapter::commit` + verify state
- Mode C spike executed: documented PASS with evidence (50-op interleaved test log, zero divergence) OR documented FAIL with specific failure modes; fate-marker comment + design-plan update recording the decision
- `.pattern.kdl` parse round-trip tests: sample config files parse to expected `MountConfig` values; parse errors on invalid configs produce clear diagnostics
- Attachment lifecycle test: `attach` + write + read + `detach` + re-`attach` + read yields consistent state
- Covers: `v3-memory-rework.AC9.*`, `v3-memory-rework.AC10.*`
<!-- END_PHASE_6 -->

<!-- START_PHASE_7 -->
### Phase 7: Messages.db backup/restore + rotation

**Goal:** Pattern can snapshot and restore messages.db atomically. Rotation policy keeps recent + thinned history.

**Components:**
- `pattern_memory/src/backup/snapshot.rs` — uses rusqlite's `backup` feature to atomically copy messages.db to `~/.pattern/backups/<project-id>/messages/<timestamp>.sqlite`
- `pattern_memory/src/backup/rotation.rs` — policy engine: keep last N, thin hourly-for-day / daily-for-month / monthly-forever
- `pattern_memory/src/backup/restore.rs` — pre-restore auto-snapshot as rollback safety net; replace messages.db atomically; verify restored db opens cleanly + pragmas apply
- Config integration: `.pattern.kdl` `backup` section for rotation policy + snapshot interval
- CLI surface: `pattern backup create` + `pattern backup restore <timestamp>` + `pattern backup list` (in `pattern-test-cli` or a new minimum-viable binary)

**Dependencies:** Phase 6 (mount paths known; `.pattern.kdl` parser exists)

**Done when:**
- `cargo check --workspace` passes
- Backup-restore round-trip test: write messages, snapshot, corrupt/clear messages.db, restore, verify all messages present + searchable
- Rotation policy unit tests for each retention band (hourly/daily/monthly thinning against synthetic snapshot history)
- Concurrent-with-writes backup test: spawn writes + trigger snapshot, verify snapshot is a valid atomic copy (no mid-write corruption)
- Restore safety test: corrupt current messages.db, call restore, verify auto-snapshot rolled back successfully
- CLI smoke test: `pattern backup create` → `pattern backup list` shows entry → `pattern backup restore <timestamp>` succeeds
- Covers: `v3-memory-rework.AC11.*`
<!-- END_PHASE_7 -->

<!-- START_PHASE_8 -->
### Phase 8: Scopes + project utilities + Pattern.Diagnostics

**Goal:** MemoryScope wrapper routes reads/writes per isolate_from_persona policy. Project-scoped personas load from mount. `<mount>/lib/` Haskell modules importable by agents. Pattern.Diagnostics effect surfaces compile issues.

**Components:**
- `pattern_memory/src/scope.rs` — `MemoryScope<S: MemoryStore>` wrapper + `ScopeBinding` + `IsolatePolicy` enum. Policy enforcement per read/write call.
- `pattern_memory/src/persona.rs` — load + validate persona configs from both `~/.pattern/personas/` (global) and `<mount>/personas/` (project-scoped)
- `pattern_runtime/src/sdk/location.rs` modifications: `resolve_import_paths(sdk_location, project_mount) -> Vec<PathBuf>` extends the Tidepool import search path with `<mount>/lib/` when present
- Lib compile isolation: wrap each `lib/*.hs` module's compile attempt; broken modules excluded from search path; diagnostics captured into session state
- `pattern_runtime/src/sdk/requests/diagnostics.rs` + `pattern_runtime/src/sdk/handlers/diagnostics.rs` — new `Pattern.Diagnostics` effect; `diagnostics :: Effect [Diagnostic]` returns accumulated session diagnostics
- `ctx.memory.write_to_persona` agent SDK effect: explicit persona write-back, errors unless `isolate_policy == IsolatePolicy::None`

**Dependencies:** Phase 6 (mount structure known)

**Done when:**
- `cargo check --workspace` passes
- MemoryScope policy tests: all three `IsolatePolicy` values exercised across read/write scenarios; expected routing + denial behavior verified
- Project-scoped persona load test: place `@reviewer.kdl` in a mount, attach, invoke `@reviewer` → correct persona instantiated
- Global vs project-scoped persona resolution precedence test
- Project-lib compile test: place a working `Project.Foo.hs` in `<mount>/lib/` + a broken `Project.Bar.hs`; verify session opens with Foo importable, Bar excluded, diagnostics effect returns Bar's compile error
- Import-broken-lib-fails test: main program imports `Project.Bar` (broken), session open fails with clear 'module not found / had errors' diagnostic
- `ctx.memory.write_to_persona` authorization test: effect succeeds with `None` policy, errors with `CoreOnly` and `Full`
- Covers: `v3-memory-rework.AC12.*`, `v3-memory-rework.AC13.*`, `v3-memory-rework.AC14.*`
<!-- END_PHASE_8 -->

<!-- START_PHASE_9 -->
### Phase 9: End-to-end smoke test + regression coverage

**Goal:** Single deterministic integration test reproduces the DoD smoke flow. Regression snapshot suite locks in FTS5 + vector + KDL + subscriber behavior.

**Components:**
- `crates/pattern_memory/tests/smoke_e2e.rs` — full flow in a temp-dir fixture:
  1. create persona at `~/.pattern/personas/@test/`
  2. init Mode A project mount in a fresh temp git repo
  3. attach project
  4. write a Core text block + Map block + Log block
  5. verify files emitted (.md / .kdl / .jsonl) matching expected content
  6. verify memory.db indexes updated
  7. external edit to the .md file
  8. wait for notify + merge
  9. verify reconciled content in loro + re-emitted file
  10. call quiesce() + commit via host git
  11. restart (simulate process reload)
  12. re-attach + read blocks → matches committed state
  13. create messages.db backup
  14. write messages, corrupt/clear messages.db, restore from backup → messages present
- FTS5 regression snapshot suite (insta): representative corpus + BM25 scoring + snippet/highlight + hybrid fusion
- Vector KNN regression suite: canonical similarity structure + expected nearest-neighbor ordering
- Multi-agent concurrent stress test: N concurrent MemoryCache instances doing writes against shared memory.db; verify no deadlock, no data loss
- Mock `ProviderClient` (scripted responses) for any model-dependent paths to keep CI deterministic

**Dependencies:** Phases 1-8

**Done when:**
- `cargo nextest run -p pattern-memory --test smoke_e2e` passes deterministically in CI
- `cargo nextest run --workspace` passes across all crates
- FTS5 + vector regression snapshots committed and stable
- Covers: `v3-memory-rework.AC15.*`
<!-- END_PHASE_9 -->

## Execution Mode Recommendation

**Recommendation: Collaborative.**

Reasoning:

- **Novel integrations with real uncertainty**: the KDL ↔ LoroValue converter, the sqlite-vec-with-bundled-rusqlite spike, the Mode C sidecar spike, the subscriber supervisor's liveness semantics, and the messages.db backup machinery all have enough novelty that mechanical execution would miss edge cases. Each benefits from a human checkpoint.
- **Rusqlite migration regression risk**: ~339 queries rewriting without compile-time verification is substantial scope where silent drift is possible. Integration tests catch a lot but not everything; a human review of each phase's changes catches the rest.
- **Storage mode interactions**: attach/detach lifecycles crossed with three modes, an isolation policy with three variants, and project-scoped-vs-global personas produce a combinatorial space that's easy to get wrong in isolation. Incremental review per phase is worth the overhead.
- **9 phases at substantial scope**: too large for Light (1-3 phases); not mechanical enough for Autonomous given the novelty and regression risks.

User can override to Autonomous if comfortable with the risk profile and willing to intervene on spike failures + novel conversions. Light is not appropriate for this scope.

## Additional Considerations

**Blocking research before certain phases:**

- **Phase 2 sqlite-vec spike**: validate rusqlite-bundled SQLite + sqlite-vec compatibility empirically. If the spike fails (extension load errors, vec0 virtual table doesn't work, KNN queries produce wrong results), pause and research: (a) pin sqlite-vec to a version compatible with rusqlite's bundled SQLite, (b) downgrade rusqlite to a version whose bundled SQLite matches the current sqlite-vec build, or (c) build sqlite-vec against rusqlite's SQLite ourselves. Document the decision in a note file alongside this plan.
- **Phase 6 Mode C spike**: pass/fail criteria documented in phase deliverables; if FAIL, Mode C ships as documented-only with a fate-marker comment in `pattern_memory/src/modes/`. No shame in documenting-and-deferring; Mode C is an advanced pattern.

**Regression coverage strategy:**

FTS5 scoring is sensitive to schema changes, query parameter changes, and SQLite version differences. Phase 2's snapshot regression suite captures the current behavior before the migration + after the migration; divergence blocks the phase. Similarly for `sqlite-vec` KNN ordering. These snapshots are permanent fixtures; updates require intentional approval.

**Error handling across layers:**

- Subscriber panic → supervisor logs ERROR + restarts + metric counter. Writes queued during downtime are safe because they're already durable in the loro update log; supervisor recovery re-emits from current doc state.
- Pool exhaustion → explicit `rusqlite::Error::PoolTimeout` surfaces to caller (not silent hang); 30s timeout default.
- Invalid .kdl from human edit → parse error logged + metric incremented; pre-existing loro state retained; next emission overwrites the broken file.
- jj subprocess failure → typed `JjError` with stderr captured; graceful degradation for Mode A (no jj needed anyway); loud failure for Modes B/C at attach time.
- sqlite-vec extension load failure at connection init → explicit `ConnectionInitError::ExtensionLoadFailed`; pool refuses to hand out the connection; diagnostic surfaces to caller.

**jj version compatibility:**

Minimum supported jj version documented in `JjAdapter::MIN_SUPPORTED_VERSION`. Version-mismatch detection at `detect()` time produces a clear error that points users to upgrade. Pattern does not attempt to work around old jj behavior; the CLI moves fast enough that supporting old versions is not worth the maintenance overhead.

**kdl crate maintainer context:**

The `kdl` crate's maintainer (zkat) has been publicly hostile to AI-assisted development. She has not (as of this plan's writing) taken hostile actions against downstream users, but the risk is non-zero. Pattern's mitigation: maintain a fork path. If the maintainer ever introduces hostile licensing changes or active deprecation, pattern forks the crate at the last safe version and continues — following the same pattern established with the local `miette` fork. This is a contingency, not a plan; the current crate is used directly.

**pattern_macros deletion:**

`pattern_macros` was retired pre-phase. No longer in the workspace. The from_row pattern for rusqlite row structs is intentionally hand-written (explicit, auditable, no proc-macro overhead). If the hand-written boilerplate ever becomes painful at scale, a derive can be added in a future focused pass — at which point the shape will be informed by real usage data.

**Intermediate code-state policy (carryover from foundation plan):**

Fate markers (`// MOVING TO:`, `// REPLACED BY:`, `// MOVING WITHIN CRATE:`) apply to any transitional code during this plan. Cruft (undefined fate, commented-out code, orphaned `unimplemented!()`) fails the intermediate-state audit run at each phase boundary. The port-list doc tracks any temporary exclusions.

**Cross-phase dependency audit:**

Phase 1's extraction surfaces all `BlockType::Archival` and `BlockType::Log` usage sites; Phase 2 resolves them via schema migration + variant removal. Phases 4, 5, 6 build on each other's outputs in a clear chain. Phases 7 and 8 are nominally parallelizable but sequenced serially for review simplicity.
