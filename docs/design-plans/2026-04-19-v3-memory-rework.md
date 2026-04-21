# Pattern v3 Memory Rework Design

## Summary

The Pattern v3 Memory Rework redesigns and extracts the memory subsystem of the Pattern multi-agent system. The work has four structural pillars that land together over nine sequential phases.

First, the memory implementation is extracted from `pattern_core` into a new `pattern_memory` crate, while `pattern_core` retains only the `MemoryStore` trait and shared data types — enforcing a clean dependency graph where nothing flows backward. Second, the database layer migrates from `sqlx` (async, compile-time-verified SQL) to `rusqlite` (synchronous, bundled SQLite), and the `MemoryStore` trait is correspondingly made synchronous. This eliminates an architectural awkwardness where the eval worker had to spin up a nested async runtime inside an already-async context. Connection pooling for async callers is handled via `r2d2-sqlite` and `tokio::task::spawn_blocking`.

Third, block content moves out of the database entirely. Rather than storing block data as blobs in SQLite, each block is now persisted as a human-readable canonical file on disk — Markdown for text, KDL for structured data, JSONL for logs — with a Loro CRDT document as the authoritative merge state. SQLite retains only indexes, metadata, and archival entries. A per-block subscriber task, driven by Loro's own commit callbacks, keeps the file and the search indexes in sync. Human edits to files on disk are reconciled back into Loro as CRDT merges rather than overwrites.

Fourth, version history for the memory state is managed through the `jj` version control system via a thin CLI adapter, with a "quiesce" step that drains in-flight writes and checkpoints the database before any commit. Three storage modes let projects choose whether the host VCS or a Pattern-managed jj repository owns the history.

## Definition of Done

Pattern v3 Memory Rework — extracts the memory subsystem from `pattern_core`, migrates its storage backend from `sqlx` to `rusqlite`, sync-ifies the `MemoryStore` trait, and reshapes storage so that markdown files are canonical block content (with loro snapshots as the merge-authoritative CRDT state) while SQLite retains only indexes and archival entries. Version history for memory state is managed via jj. The plan is done when:

### Crate structure

- `pattern_memory` crate extracted from `crates/pattern_core/src/memory/`
- `pattern_core` retains only the `MemoryStore` trait + `Block` / `BlockHandle` / related types (trait-only-core rule satisfied)
- Dependency graph: `pattern_memory → pattern_core + pattern_db`; no reverse deps

### Storage backend (sync, rusqlite)

- `pattern_db` migrated from `sqlx` to `rusqlite` across all ~236 queries across `pattern_db/src/` (verified via grep on 2026-04-19)
- `MemoryStore` trait sync-ified and audited down from 28 methods to 19 via collapse (`list_blocks` variants merged behind a `BlockFilter` type; `update_block_metadata(id, patch)` replaces four separate setters; `undo_redo(op, label)` and `history_depth` replace four separate methods; `search(scope)` replaces `search` + `search_all`)
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
- All `Handle::current().block_on` sites in `handlers/memory.rs`, `handlers/recall.rs`, and `handlers/search.rs` that exist solely to bridge async `MemoryStore` calls are eliminated (~15-16 call pairs). `handlers/message.rs` contains a small number of `block_on` calls that dispatch to the async MessageRouter — these are NOT MemoryStore-related and remain async (router stays async-trait)
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
- Storage topology: **loro-primary with per-doc subscribers**. Writes go to loro; `doc.subscribe_local_update` callbacks fire post-commit; per-doc `sync_worker` OS threads emit the canonical file and update indexes (debounced 50ms). See Architecture for full detail. (Note: design originally referenced `subscribe_root`; the implementation uses `subscribe_local_update` which carries the actual update bytes needed by the worker channel, avoiding a separate export step.)
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

### v3-memory-rework.AC1: pattern_memory crate extraction is clean and reversible

- **v3-memory-rework.AC1.1 Success:** `cargo check --workspace` passes after extraction
- **v3-memory-rework.AC1.2 Success:** `cargo nextest run -p pattern-memory` passes every moved test (all memory-domain tests from pattern_core are runnable in pattern_memory)
- **v3-memory-rework.AC1.3 Success:** `cargo doc -p pattern_memory` produces complete rustdoc for every public item
- **v3-memory-rework.AC1.4 Success:** Every `pattern_runtime` file importing memory types imports trait types from `pattern_core` and impl types from `pattern_memory`; no `pattern_runtime` file depends on `pattern_memory` private internals
- **v3-memory-rework.AC1.5 Failure:** A file in pattern_core attempting to import from `pattern_memory` (reverse dependency) fails to compile
- **v3-memory-rework.AC1.6 Edge:** Workspace `members` list is updated; port-list doc records the extraction with a "completed" note

### v3-memory-rework.AC2: Rusqlite migration preserves query semantics end-to-end

- **v3-memory-rework.AC2.1 Success:** `cargo check --workspace` passes after sqlx → rusqlite swap
- **v3-memory-rework.AC2.2 Success:** Every pre-existing `pattern_db` integration test passes post-migration without modification to assertions
- **v3-memory-rework.AC2.3 Success:** FTS5 BM25 snapshot tests (insta) produce identical scoring output on a representative corpus
- **v3-memory-rework.AC2.4 Success:** Vector KNN regression test returns identical nearest-neighbor ordering on a canonical similarity structure
- **v3-memory-rework.AC2.5 Success:** All three explicit transaction sites in `queries/memory.rs` port to `rusqlite::Transaction` preserving atomicity
- **v3-memory-rework.AC2.6 Failure:** A query that previously committed atomically as a transaction, if mid-transaction forced to fail, leaves the database in pre-transaction state (no partial commit)
- **v3-memory-rework.AC2.7 Failure:** Concurrent pool stress test: 20 concurrent `spawn_blocking` callers making queries complete without deadlock or pool exhaustion
- **v3-memory-rework.AC2.8 Edge:** Direct `libsqlite3-sys` dep is absent from `pattern_db/Cargo.toml`; rusqlite's bundled SQLite is the sole source
- **v3-memory-rework.AC2.9 Edge:** sqlite-vec compatibility spike test at `crates/pattern_db/tests/sqlite_vec_smoke.rs` passes: 100 test vectors inserted into a vec0 virtual table return correct KNN ordering
- **v3-memory-rework.AC2.10 Edge:** `messages.db` splits into its own file; ATTACH statement in `init_connection` succeeds; cross-db queries `SELECT ... FROM main.X JOIN msg.Y` work

### v3-memory-rework.AC3: BlockType simplification is clean across call sites

- **v3-memory-rework.AC3.1 Success:** `BlockType` enum contains only `Core` and `Working` variants after Phase 2
- **v3-memory-rework.AC3.2 Success:** `cargo check --workspace` produces no errors or warnings referencing removed variants
- **v3-memory-rework.AC3.3 Success:** Existing blocks with the old `BlockType::Log` classification are migrated to `Working` tier + `BlockSchema::Log` schema via the phase's schema migration
- **v3-memory-rework.AC3.4 Success:** Existing blocks with the old `BlockType::Archival` classification are converted to archival entries via the phase's schema migration
- **v3-memory-rework.AC3.5 Failure:** Attempting to deserialize an old record with `BlockType::Archival` or `BlockType::Log` on disk produces a clear migration error pointing to the migrator, not a silent decode
- **v3-memory-rework.AC3.6 Edge:** A Log-schema block can be loaded into either Core or Working tier (previously ambiguous due to variant conflation)

### v3-memory-rework.AC4: MemoryStore sync-ification + surface audit

- **v3-memory-rework.AC4.1 Success:** `MemoryStore` trait has no `#[async_trait]` decorator
- **v3-memory-rework.AC4.2 Success:** Trait has 19 methods matching the consolidated surface (audited down from 28); consolidation detail captured in trait-method doc comments
- **v3-memory-rework.AC4.3 Success:** `list_blocks(BlockFilter)` replaces the three previous variants; every filter combination works
- **v3-memory-rework.AC4.4 Success:** `update_block_metadata(id, BlockMetadataPatch)` correctly updates specified fields and leaves others untouched
- **v3-memory-rework.AC4.5 Success:** `undo_redo(label, UndoRedoOp)` + `history_depth(label)` produce equivalent behavior to the four removed methods
- **v3-memory-rework.AC4.6 Success:** `search(SearchScope)` correctly scopes to persona / project / constellation
- **v3-memory-rework.AC4.7 Success:** All existing MemoryCache impl tests pass against the new sync trait surface
- **v3-memory-rework.AC4.8 Failure:** `async_trait` dep is not removed from pattern_core (other 8 traits still use it); `cargo check -p pattern_core` still imports `async_trait`
- **v3-memory-rework.AC4.9 Edge:** `MemoryStore::delete_archival` method is retained in the trait but is not reachable via any agent SDK effect. Verified via a `trybuild` compile-fail test at `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs` that attempts to construct the removed SDK request variant and confirms the compile error. The Haskell-side `Pattern.Memory.Archive.delete` symbol is removed from the SDK module; existing agent programs invoking it fail at Tidepool compile-time with a 'symbol not found' diagnostic.

### v3-memory-rework.AC5: Eval worker simplification + async callsite migration

- **v3-memory-rework.AC5.1 Success:** `eval_worker.rs` no longer constructs a per-session `tokio::runtime::Builder::new_multi_thread()`
- **v3-memory-rework.AC5.2 Success:** Worker thread is spawned via `std::thread::spawn` with `std::sync::mpsc::channel` for request intake
- **v3-memory-rework.AC5.3 Success:** Zero `Handle::current().block_on(...)` call sites remain in memory, recall, search, or scope effect handlers
- **v3-memory-rework.AC5.4 Success:** Session::step caller-visible signature unchanged (still `async`)
- **v3-memory-rework.AC5.5 Success:** Pre-existing `spawn_blocking`-related search bug is resolved (regression test passes)
- **v3-memory-rework.AC5.6 Success:** Async callsites that invoke `MemoryStore` DB operations use `tokio::task::spawn_blocking`; cheap sync operations (metadata reads from in-memory caches) call directly
- **v3-memory-rework.AC5.7 Failure:** Running a stream of 100 eval requests against the sync worker completes without `cannot start a runtime from within a runtime` panics
- **v3-memory-rework.AC5.8 Edge:** On eval worker thread panic, a user-visible error surfaces; session becomes unusable (does not silently deadlock)

### v3-memory-rework.AC6: Canonical file serialization round-trips

- **v3-memory-rework.AC6.1 Success:** Text block round-trip: write text, emit `.md`, parse `.md`, import into loro, frontier equals original
- **v3-memory-rework.AC6.2 Success:** Map block round-trip via KDL: write map fields, emit `.kdl`, parse, re-import, loro state equals original (property-tested with proptest)
- **v3-memory-rework.AC6.3 Success:** List block round-trip via KDL: nested lists, ordered correctly, survives round-trip
- **v3-memory-rework.AC6.4 Success:** Log block round-trip via JSONL: entries serialize line-per-entry; parsed back in same order
- **v3-memory-rework.AC6.5 Success:** Composite block round-trip: sections serialize as top-level KDL nodes; section boundaries preserved
- **v3-memory-rework.AC6.6 Failure:** LoroValue containing a type kdl cannot represent (if any are discovered) produces a typed `KdlConversionError`; no silent data loss
- **v3-memory-rework.AC6.7 Edge:** KDL numeric precision: large integers (i128 boundary), floats with special values (#inf, #nan) round-trip exactly per the kdl crate's preservation contract
- **v3-memory-rework.AC6.8 Edge:** Strings with embedded newlines, quotes, and unicode round-trip correctly through KDL

### v3-memory-rework.AC7: Loro-native subscribers + external edit merge

- **v3-memory-rework.AC7.1 Success:** Write a block, observe emitted file matching block content within 100ms (50ms debounce + overhead)
- **v3-memory-rework.AC7.2 Success:** Subscriber emits FTS5 row update matching block content
- **v3-memory-rework.AC7.3 Success:** Subscriber queues vector re-embed only when content hash changes; no spurious re-embeds
- **v3-memory-rework.AC7.4 Success:** External edit to `.md` via text editor: notify detects, loro merges, re-emission produces canonical content
- **v3-memory-rework.AC7.5 Success:** Self-emit-echo suppression: write block → observe single emission (not an infinite loop)
- **v3-memory-rework.AC7.6 Failure:** Invalid KDL from human edit: parse fails, `metrics::counter!("memory.kdl.parse_failed")` increments, no loro merge attempted, prior valid content re-emitted
- **v3-memory-rework.AC7.7 Failure:** Subscriber panic: supervisor detects heartbeat timeout within 30s, logs ERROR, restarts worker, increments restart counter
- **v3-memory-rework.AC7.8 Edge:** Concurrent human edit + agent write: loro CRDT merges both; final state reflects both changes

### v3-memory-rework.AC8: jj CLI adapter + pre-commit quiesce

- **v3-memory-rework.AC8.1 Success:** `JjAdapter::detect` returns `Some` on systems with `jj` in PATH and supported version
- **v3-memory-rework.AC8.2 Success:** All ~15-18 adapter functions execute their jj subcommand and parse JSON-templated output correctly
- **v3-memory-rework.AC8.3 Success:** `quiesce()` drains all sync_workers, calls `wal_checkpoint(TRUNCATE)`, and fsyncs emitted files before returning
- **v3-memory-rework.AC8.4 Success:** In Mode A (no jj adapter), `quiesce()` still runs and produces a canonical `memory.db` for host VCS to commit
- **v3-memory-rework.AC8.5 Failure:** `JjAdapter::detect` returns `None` on systems without `jj`; no panic; Mode A continues working
- **v3-memory-rework.AC8.6 Failure:** `jj --version` returning an unsupported version surfaces `JjError::UnsupportedVersion` with clear message
- **v3-memory-rework.AC8.7 Failure:** jj subcommand failure surfaces `JjError::SubprocessFailed` carrying stderr; caller gets typed error, not stringly-typed
- **v3-memory-rework.AC8.8 Edge:** Adapter respects `--color=never` in all invocations; output parsing doesn't choke on ANSI codes

### v3-memory-rework.AC9: Storage modes A + B

- **v3-memory-rework.AC9.1 Success:** Mode A end-to-end: temp host-git repo + mount init + block write + host git commit + verify state on disk
- **v3-memory-rework.AC9.2 Success:** Mode B end-to-end: pattern-jj temp repo + mount init + block write + quiesce → jj commit + verify state
- **v3-memory-rework.AC9.3 Success:** Mode A `messages.db` lives at `~/.pattern/transient/<project-hash>/` (outside the project repo)
- **v3-memory-rework.AC9.4 Success:** Mode B `messages.db` lives at `~/.pattern/projects/<id>/messages/` (outside pattern-jj worktree)
- **v3-memory-rework.AC9.5 Success:** `.pattern.kdl` config parses cleanly for representative configs; malformed configs produce clear diagnostics
- **v3-memory-rework.AC9.6 Success:** `attach(path)` walks upward to find `.pattern.kdl`; sets up subscribers + opens dbs + registers with jj as applicable
- **v3-memory-rework.AC9.7 Failure:** `attach` on a path with no mount produces a clear "no mount found" error with a suggestion to run `pattern mount init`
- **v3-memory-rework.AC9.8 Edge:** `detach` + re-`attach` produces identical state (no leaked workers, clean restart)

### v3-memory-rework.AC10: Mode C spike outcome

- **v3-memory-rework.AC10.1 Success (Mode C ships):** Spike passes 50-op interleaved test (host git ops + pattern jj ops) with zero state divergence; documented in design-plan with 'verified: YYYY-MM-DD' stamp; Mode C implementation ships
- **v3-memory-rework.AC10.2 Failure (Mode C deferred):** Spike fails; fate-marker comment in `pattern_memory::modes` explicitly records the deferral; design-plan updated with findings; `StorageMode::C` enum variant either (a) ships in a documented-only state that explicitly rejects attachment, or (b) is absent from the enum until a future plan
- **v3-memory-rework.AC10.3 Edge:** Spike outcome (pass or fail) produces a note file at `docs/notes/YYYY-MM-DD-mode-c-spike.md` documenting the evidence

### v3-memory-rework.AC11: Messages.db backup + restore + rotation

- **v3-memory-rework.AC11.1 Success:** `pattern backup create` produces a snapshot at `~/.pattern/backups/<project-id>/messages/<iso8601>.sqlite` using rusqlite's backup API
- **v3-memory-rework.AC11.2 Success:** Snapshot is a valid SQLite file that opens cleanly with the same schema as the source
- **v3-memory-rework.AC11.3 Success:** `pattern backup restore <timestamp>` replaces `messages.db` with the snapshot; all messages present + searchable after restore
- **v3-memory-rework.AC11.4 Success:** Pre-restore safety: current state is auto-snapshotted before replacement; label makes it distinguishable as a rollback point
- **v3-memory-rework.AC11.5 Success:** Rotation policy retains last N snapshots + thins older per the configured hourly/daily/monthly bands
- **v3-memory-rework.AC11.6 Failure:** `pattern backup restore <timestamp>` with a non-existent timestamp produces a clear error listing available snapshots
- **v3-memory-rework.AC11.7 Edge:** Concurrent backup + write: snapshot is atomic; no mid-write corruption observable in the snapshot file

### v3-memory-rework.AC12: MemoryScope + isolate_from_persona

- **v3-memory-rework.AC12.1 Success (`None`):** Reads merge persona + project core; writes to shared handles flow bi-directionally; archival search spans both stores
- **v3-memory-rework.AC12.2 Success (`CoreOnly`):** Reads see persona core as read-only + project core as read-write; writes to persona-core from within project scope are denied
- **v3-memory-rework.AC12.3 Success (`Full`):** Persona identity (name, instructions) visible; persona block content not visible; archival search is project-only
- **v3-memory-rework.AC12.4 Success:** `ctx.memory.write_to_persona(...)` effect succeeds when policy is `None`
- **v3-memory-rework.AC12.5 Failure:** `ctx.memory.write_to_persona(...)` returns `MemoryError::IsolationDenied` when policy is `CoreOnly` or `Full`
- **v3-memory-rework.AC12.6 Edge:** Project-level writes in `None` mode default to project scope unless explicit persona-scoped effect is invoked

### v3-memory-rework.AC13: Project-scoped personas

- **v3-memory-rework.AC13.1 Success:** Persona definition at `<mount>/personas/@reviewer.kdl` loads + becomes invokable as `@reviewer` within the project
- **v3-memory-rework.AC13.2 Success:** `scope: project` persona is not visible when attaching a different project
- **v3-memory-rework.AC13.3 Success:** `scope: global` (or unspecified) persona at `~/.pattern/personas/@name/` works across projects subject to isolation policy
- **v3-memory-rework.AC13.4 Failure:** Persona definition missing required fields produces a clear parse error at attach time, not silent misconfiguration
- **v3-memory-rework.AC13.5 Edge:** Global + project-scoped personas with the same name: project-scoped takes precedence within that project; global available elsewhere

### v3-memory-rework.AC14: Project utilities + Pattern.Diagnostics

- **v3-memory-rework.AC14.1 Success:** `<mount>/lib/Project/Foo.hs` compiles cleanly; main agent program `import Project.Foo qualified as Foo` resolves + runs
- **v3-memory-rework.AC14.2 Success:** `<mount>/lib/Project/Bar.hs` with a syntax error is excluded from import path; session opens normally; agent program that doesn't import `Project.Bar` runs fine
- **v3-memory-rework.AC14.3 Success:** `Pattern.Diagnostics.diagnostics` effect returns a list of diagnostic events including the Bar compile failure
- **v3-memory-rework.AC14.4 Failure:** Main program imports `Project.Bar` (broken): session open fails with clear 'module not found (had compile errors)' diagnostic
- **v3-memory-rework.AC14.5 Failure:** Compile errors do not crash pattern or produce uninformative errors; every error has source location + message
- **v3-memory-rework.AC14.6 Edge:** No `lib/` directory on a mount: session opens cleanly; no error; no import path extension

### v3-memory-rework.AC15: End-to-end smoke test

- **v3-memory-rework.AC15.1 Success:** `cargo nextest run -p pattern-memory --test smoke_e2e` passes deterministically in CI
- **v3-memory-rework.AC15.2 Success:** Test exercises: create persona → attach Mode A project → write Core text + Map + Log blocks → verify files emitted with expected format → external .md edit → loro merge → commit via host git → restart → re-attach → read matches committed state → backup messages.db → clear + restore → messages present
- **v3-memory-rework.AC15.3 Success:** `cargo nextest run --workspace` passes across all crates after all phases land
- **v3-memory-rework.AC15.4 Success:** FTS5 + vector regression snapshot suite (insta) is committed and stable across CI runs
- **v3-memory-rework.AC15.5 Failure:** Any step failing in the smoke flow causes the test to fail loudly with a specific error identifying which step failed
- **v3-memory-rework.AC15.6 Edge:** Multi-agent concurrent stress test (N MemoryCache instances doing writes against shared memory.db) completes without deadlock or data loss

## Glossary

- **Tidepool**: Pattern's embedded Haskell evaluation engine, used to compile and run agent programs. Agent behavior is expressed as Haskell programs that call SDK effects; Tidepool handles compilation, import resolution, and execution.
- **persona**: A named agent identity in Pattern, defined by a set of instructions, memory blocks, and configuration. A persona can be global (shared across projects) or project-scoped (defined inside a mount and invisible to other projects).
- **mount**: A directory managed by Pattern as a block store for a specific project. Contains block files, a `memory.db`, a `.pattern.kdl` config, and optionally a `personas/` and `lib/` directory.
- **`.pattern.kdl`**: A new per-mount configuration file in KDL format that specifies the storage mode, persona bindings, isolation policy, and jj options for that mount. Separate from Pattern's existing TOML config files.
- **MemoryStore**: The central synchronous trait defining the contract for reading and writing memory blocks. After this plan, it has 19 methods and no async trait machinery.
- **MemoryScope**: A wrapper type around a `MemoryStore` that routes reads and writes according to an `isolate_from_persona` policy, controlling how much of a persona's memory bleeds into a project context.
- **BlockType**: An enum classifying a memory block as either `Core` (always in context) or `Working` (loaded on demand). This plan removes the previous `Archival` and `Log` variants, which were conflations of tier and schema.
- **BlockSchema**: The structural shape of a block's content — `Text`, `Map`, `List`, `Log`, or `Composite`. Orthogonal to `BlockType`; a `Log`-schema block can live in either the `Core` or `Working` tier.
- **archival entry**: An immutable record in `memory.db`'s archival table, searchable via FTS5 and vector similarity. Distinct from memory blocks — not a tier, not editable by agents. Agents can insert and search; only human operators can delete.
- **loro**: A Rust CRDT (Conflict-free Replicated Data Type) library used to store block content in-process. Loro documents support concurrent edits that merge without conflict. In this plan, the Loro document is the canonical write target; files on disk and SQLite indexes are derived from it.
- **LoroDoc / LoroValue**: `LoroDoc` is a Loro document instance; `LoroValue` is the Rust value type representing data within it (maps, lists, scalars). The plan hand-writes conversion between `LoroValue` and the KDL document type.
- **rusqlite**: A synchronous Rust bindings crate for SQLite. Used here with the `bundled-full` feature, which compiles SQLite directly into the binary and removes the need for a system SQLite install or a `libsqlite3-sys` pin.
- **r2d2-sqlite**: A connection pool adapter that pairs the `r2d2` connection pool with `rusqlite`. Used to manage a pool of SQLite connections for async callers that go through `spawn_blocking`.
- **sqlx**: The async, compile-time-verified SQL crate being replaced by rusqlite. Previously used across ~339 queries in `pattern_db`.
- **rusqlite_migration**: A library for managing SQLite schema migrations under rusqlite, replacing the `sqlx-cli prepare` / `sqlx::migrate!` workflow.
- **sqlite-vec**: A SQLite extension providing vector similarity search (`vec0` virtual tables, KNN queries). Loaded at runtime via `Connection::load_extension`. Used for hybrid search alongside FTS5.
- **FTS5**: SQLite's built-in full-text search engine (version 5). Used in Pattern for BM25-scored keyword search over memory blocks and messages. Provides `highlight()` and `snippet()` functions for result excerpts.
- **BM25**: A probabilistic text ranking algorithm used by FTS5. SQLite exposes it via the `rank` column in FTS5 queries. Scores are negative in SQLite's implementation (`rank / -10` normalizes them for fusion).
- **CRDT**: Conflict-free Replicated Data Type. A data structure designed so that concurrent edits from multiple sources can always be merged deterministically without coordination. Loro implements CRDTs for in-process use.
- **WAL**: Write-Ahead Logging. A SQLite journal mode that improves concurrency by writing changes to a separate log file before applying them to the main database. Pattern enables WAL for all connections via `PRAGMA journal_mode=WAL`.
- **ATTACH DATABASE**: A SQLite statement that connects a second SQLite file to an existing connection under a named schema alias. Pattern uses this to attach `messages.db` as schema `msg` on every connection, enabling cross-database queries without opening a second connection.
- **quiesce**: A pre-commit step that drains all in-flight subscriber tasks, checkpoints the WAL (`PRAGMA wal_checkpoint(TRUNCATE)`), and fsyncs emitted files. Ensures the on-disk state is canonical and resumable before a VCS commit.
- **sync_worker**: A per-LoroDoc OS thread that receives commit notifications from Loro's `subscribe_local_update` callback via a crossbeam channel, debounces them 50ms, and then emits the canonical file, updates FTS5 indexes, and queues re-embedding. Supervised for liveness by an async tokio task. (`subscribe_local_update` replaced `subscribe_root` in the implementation because it delivers the update bytes directly, eliminating a redundant export step.)
- **jj**: Jujutsu, a modern version control system used by Pattern to manage history for memory state (Modes B and C). Pattern shells out to the `jj` CLI rather than embedding jj-lib.
- **jj workspace**: A jj concept analogous to a git worktree — a working copy checked out from a jj repository. The jj CLI adapter uses workspace operations to support subagent fork semantics in future plans.
- **kdl**: The KDL Document Language — a human-readable, typed data format used for Pattern's `.pattern.kdl` config files and for serializing `Map`, `List`, and `Composite` block schemas to disk. The `kdl` Rust crate provides parsing and round-trip-faithful serialization.
- **notify**: A Rust file-system watching library. Pattern uses `notify 8.2` with `notify-debouncer-full` (500ms debounce) to detect external edits to canonical block files and trigger loro CRDT merges.
- **insta**: A Rust snapshot testing library. Used in this plan to capture and lock FTS5 BM25 scoring output and vector KNN ordering so that regressions in search behavior are caught automatically.
- **proptest**: A property-based testing library for Rust. Used to verify that file format round-trips (KDL, JSONL, Markdown) are correct across a large space of generated inputs, not just hand-picked examples.
- **Mode A / B / C**: The three storage modes for a Pattern mount. Mode A stores block state inside the project repo and delegates history to the host VCS (git or jj). Mode B stores block state in a Pattern-managed directory tracked by a Pattern-owned jj repo. Mode C is an experimental "sidecar" mode where a Pattern-owned jj repo coexists with a host git repo in the same working directory; its viability is gated on a validation spike.
- **isolate_from_persona**: A policy (`None`, `CoreOnly`, `Full`) controlling how much of a persona's memory is visible within a project context. `None` merges persona and project memory fully; `Full` exposes only the persona's name and instructions, not its memory history.
- **Pattern.Diagnostics**: A new SDK effect exposed to agent programs that returns accumulated session diagnostics, including compilation errors from project-local Haskell library modules in `<mount>/lib/`.
- **eval worker**: The component in `pattern_runtime` that runs Tidepool (the Haskell evaluator) as a separate OS thread. This plan simplifies it from a thread with its own tokio runtime to a plain OS thread using `std::sync::mpsc` channels.

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
  │     SqliteConnectionManager::with_init(|conn| init_connection(conn, &messages_path))
  │     — r2d2 invokes init_connection on EVERY newly-opened connection
  │     (ATTACH state is per-connection; we apply it on creation, not on checkout)
  │
  └── dedicated_connection() -> rusqlite::Connection
        for eval worker; owned by the worker for its session lifetime; not pool-managed
        same init_connection hook applied

fn init_connection(conn: &mut Connection, messages_path: &Path) -> Result<()>:
  PRAGMA journal_mode=WAL, foreign_keys=ON, busy_timeout=5000
  PRAGMA cache_size=-65536 (64 MiB), mmap_size=268435456 (256 MiB)
  unsafe { conn.load_extension_enable(); sqlite_vec::load(conn); conn.load_extension_disable(); }
  conn.execute("ATTACH DATABASE ? AS msg", params![messages_path.display()])
```

**Messages.db creation and migration semantics:**

- On first session open for a project, `messages.db` does not yet exist. `ATTACH DATABASE` against a non-existent path creates the file automatically (SQLite's standard behavior when the attach target doesn't exist).
- **Migration runner strategy**: `rusqlite_migration` operates on a single connection but does not have first-class support for attached databases. The design splits migrations into two directories: `pattern_db/migrations/memory/` and `pattern_db/migrations/messages/`. At `ConstellationDb::open`, the memory migrations run against the main connection, then the messages migrations run via a temporarily-opened direct connection to `messages.db` (outside the pool). Both migration runs are complete before the pool hands out any connections.
- sqlite-vec extensions loaded via `load_extension_enable` apply to all attached databases on that connection, so vector indexes work in both `memory.db` and `messages.db` schemas.

`MemoryStore` is sync-ified. 28 original methods consolidate to 19:

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
  2. doc.commit()  ─────────────┐  fires doc.subscribe_local_update callbacks
  3. persist loro delta         │
     to memory.db updates log   │
  4. return to caller           │
                                ▼
                   sync_worker per loaded doc
                     (OS thread; supervised by async task)
                     ├── debounce 50ms
                     ├── borrow pool connection
                     ├── emit canonical file (md/kdl/jsonl)
                     ├── update FTS5 row for block
                     ├── queue vector re-embed if hash changed
                     ├── release connection
                     └── heartbeat to supervisor
```

Key properties:

- **Lazy spawn**: sync_worker is spawned on the first write to a doc, not at doc-load time. Docs loaded read-only (e.g., during context assembly) never pay task overhead until the first write arrives.
- **Lifecycle tied to LoroDoc**: each sync_worker holds a cancel token owned by its doc; when `MemoryCache.drop_doc(label)` fires (cache eviction, project detach, explicit unload), the cancel token fires and the worker exits cleanly. No leaked tasks across attach/detach.
- **Per-doc parallelism**: N actively-written docs = N sync_worker tasks. No central queue contention.
- **Debounce at the subscriber**: rapid writes (streaming text updates, multiple committed fields) coalesce into a single file emission within 50ms. Loro's commit cadence provides the natural event boundary; the subscriber batches further.
- **Bounded channels with backpressure**: each sync_worker's event channel has a bounded capacity (64-128). If writes outpace the worker, commits block briefly on channel send rather than causing unbounded memory growth — caller observes a slower write, not a silent backlog.
- **Pool-borrow per work unit**: workers don't hold connections while idle between events.
- **Idempotent**: on crash, restart emits current doc state. Loro is the truth; files are derived.
- **Supervisor**: one supervisor per `MemoryCache` instance watches all sync_worker tasks. 30s heartbeat timeout → log ERROR, restart worker, increment `metrics::counter!("memory.sync_worker.restart")`. `metrics::gauge!("memory.sync_worker.active")` exposes active subscriber count for observability and scaling data.

**Why OS threads, not tokio tasks** (2026-04 implementation note): sync_worker workload is sync-dominant — rusqlite FTS5 updates, file I/O, blake3 hashing. A tokio task wrapping `spawn_blocking` for every step would be needless overhead for a 50-sub-1000 active-worker scale. Loro's `subscribe_local_update` callback is already synchronous. The supervisor that watches heartbeats is async (tokio task) because it naturally multiplexes across N workers; the workers themselves are plain `std::thread::spawn`ed with `crossbeam-channel` intake + `tokio_util::sync::CancellationToken` for cross-thread cancel. The library-first survey (`docs/implementation-plans/2026-04-19-v3-memory-rework/phase_04.md` -- Task 5's library-first audit block) confirmed no single focused crate wraps this pattern; we compose stdlib threads + crossbeam + tokio-util + a hand-rolled ~60-line supervisor.

**Scale expectations**: pattern's typical workload is 10-50 active personas x 5-20 loaded blocks = 50-1000 potentially-subscribable docs. Per-thread memory is ~8KB (OS thread stack + channel + debounce timer + doc Arc), giving total subscriber overhead of ~400KB-8MB. Thread count well within OS limits. A future pool-of-workers optimization is possible if observability data shows thread count becoming meaningful, but it's not part of this plan.

### LoroValue ↔ KDL serialization policy

`LoroValue` variants do not all map trivially to KDL. Policy per variant:

| LoroValue variant | KDL representation | Round-trip strategy |
|---|---|---|
| `Null` | KDL `null` keyword | exact |
| `Bool` | `#true` / `#false` | exact |
| `Double`, `I64` | KDL number | kdl crate preserves numeric representation |
| `String` | quoted string | exact |
| `List` | KDL list node with child entries | recursive |
| `Map` | KDL map node with keyed entries | recursive |
| `Binary` | typed annotation `(binary)"base64..."` | base64 encode/decode; verification spike in Phase 1 confirms pattern doesn't actually use Binary in memory blocks — if confirmed, converter rejects Binary loudly with `KdlConversionError::UnsupportedBinary` rather than silently base64-encoding |
| `Container` (counter) | plain KDL number reflecting current counter value | on external edit: `increment_counter(new - old)` applied via loro's commutative increment semantics; concurrent agent writes merge correctly via CRDT |
| `Container` (other: LoroMap, LoroList, LoroText) | typed annotation `(container)"🦜:cid:..."` carrying the ContainerID string | ContainerID preserved on round-trip; nested container state is opaque to external file edits (nested state only edited via pattern's own block APIs) |

**Binary usage verification**: Phase 1 (extraction) includes an audit for `LoroValue::Binary` usage across existing `StructuredDocument` call sites. Expected outcome: no usage found, policy is "reject loudly." If usage is found, base64 serialization ships with the converter.

**Counter semantics on external edit**: when a human edits `my_counter 42` to `my_counter 45` in a .kdl file, the file-ingest path computes the delta (3) and calls `increment_counter(3)` on the loro counter. This matches human intuition (they "set" a value, result is a relative increment) and preserves CRDT merge correctness for concurrent agent increments.

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
- **`MemoryStore` becomes sync**. `async_trait` removed from the trait; 28 async methods become sync; trait surface audited from 28 → 19 via consolidation.
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
### Phase 2: Rusqlite migration + DB split + sqlite-vec spike + BlockType cleanup

**Goal:** pattern_db runs on rusqlite with pooled connections. `memory.db` and `messages.db` split via ATTACH. sqlite-vec loads cleanly under rusqlite's bundled SQLite. `BlockType::Archival` and `BlockType::Log` variants removed with explicit call-site handling.

**Structured as three sub-tasks with their own verification gates. Implementor can pause between sub-tasks for review.**

**Sub-task 2a (blocking spike): sqlite-vec compatibility validation**

- Deliverable: `crates/pattern_db/tests/sqlite_vec_smoke.rs` — open in-memory db via rusqlite with `bundled-full` + `load_extension`, create vec0 virtual table with 384-dim floats, insert 100 test vectors, run KNN query, assert expected ordering on a known similarity structure
- PASS = bundled SQLite 3.51.3 + sqlite-vec 0.1.7-alpha.2 compatible. Documented in `docs/notes/YYYY-MM-DD-sqlite-vec-spike.md`. Proceed to 2b.
- FAIL = pause implementation. Research fallback: (a) pin sqlite-vec to a version compatible with rusqlite's bundled SQLite, (b) downgrade rusqlite to match sqlite-vec's tested SQLite version, or (c) build sqlite-vec against rusqlite's SQLite ourselves. Document decision before 2b begins.

**Gate:** Task 2a PASS documented; spike test committed to the repo.

**Sub-task 2b: Rusqlite migration + DB split + messages.db creation**

- Cargo dep swap in `pattern_db/Cargo.toml`: drop `sqlx`, drop `libsqlite3-sys` direct pin; add `rusqlite 0.39` with features `["bundled-full", "load_extension", "jiff", "serde_json"]`; add `r2d2`; add `r2d2-sqlite`; add `rusqlite_migration 1.0`
- `crates/pattern_db/src/connection.rs`: `ConstellationDb` rewrite with `r2d2::Pool<SqliteConnectionManager>`; `SqliteConnectionManager::with_init(init_connection)` applies pragmas + sqlite-vec load + `ATTACH DATABASE msg` per new connection; `dedicated_connection()` method returns a non-pool-managed `Connection` with the same init hook for the eval worker
- Split migration directories: `crates/pattern_db/migrations/memory/` + `crates/pattern_db/migrations/messages/`. At `ConstellationDb::open`, memory migrations run against main connection, messages migrations run via a temporarily-opened direct connection to `messages.db`.
- Port 236 queries across `queries/*.rs` and `fts.rs` / `vector.rs` — each moves from `sqlx::query!` / `sqlx::query_as!` to rusqlite statement + inherent `fn from_row(row) -> Result<Self>` on the row struct
- `FromSql` / `ToSql` impls for domain scalar types (`BlockType`, `BlockPermission`, JSON-blob columns as `serde_json::Value`)
- Port 3 explicit transaction sites in `queries/memory.rs` — `update_block_config`, `insert_memory_block_update`, `consolidate_checkpoint` — to `rusqlite::Transaction`
- Messages extraction: create messages.db schema via fresh messages/ migrations. Existing data migration deferred to v2→v3 migrator plan — this sub-task ships the split on new data only

**Gate:** `cargo check --workspace` passes; `cargo nextest run -p pattern-db` passes every existing integration test; FTS5 BM25 snapshot tests (insta) committed; vector KNN regression test passes; concurrent pool stress test passes; transaction atomicity tests pass.

**Sub-task 2c: BlockType cleanup across call sites**

Removing `BlockType::Archival` and `BlockType::Log` variants touches 12 files across 5 crates. Explicit handling per file group:

- **`pattern_core/src/memory/types.rs`** — remove enum variants; update `Display`, `FromStr`, `From<pattern_db::MemoryBlockType>` impls
- **`pattern_core/src/export/letta_convert.rs` + `export/tests.rs`** — update Letta interop conversions; legacy Letta exports with Archival/Log variants translate to ArchivalEntry insertion (for Archival) or Working-tier Log-schema (for Log)
- **`pattern_runtime/src/session.rs` + `agent_loop.rs`** — remove match arms for removed variants; any code that special-cased Archival routes through archival entry APIs instead
- **`pattern_runtime/src/sdk/requests/memory.rs`** — remove `BlockTypeReq::Archival` and `BlockTypeReq::Log` variants from the FromCore enum; corresponding Haskell GADT constructors removed from Pattern.Memory SDK module (agent programs referencing them fail to compile with clear diagnostic pointing to the migration)
- **`pattern_runtime/src/sdk/handlers/memory.rs`** — update dispatch match; handlers for removed variants replaced with typed error surfacing to the agent
- **`pattern_provider/src/compose/pseudo_messages.rs` + `compose/current_state.rs`** — these render blocks into the cache layout's segment 3. Current implementation filters or labels by tier. Updated rendering treats only `Core` and `Working` tiers; archival entries surface separately (already handled in a different code path; verify). **This is the highest-risk file set for this sub-task** — the compose pipeline's correctness directly affects cache behavior from the foundation plan.
- **`pattern_cli/src/commands/builder/agent.rs` + `builder/group.rs` + `debug.rs`** — builder UI + debug commands currently expose tier selection; remove Archival + Log options; debug command may expose ArchivalEntry surface separately
- Schema migration in `migrations/memory/`: ALTER the `memory_blocks` table's tier-classification column type; existing rows with `block_type = 'archival'` convert to ArchivalEntry rows (data migration); rows with `block_type = 'log'` convert to `block_type = 'working'` with `block_schema` updated to reflect Log schema

**Gate:** `cargo check --workspace` produces zero errors or warnings referencing removed variants; migration round-trip test passes (old-schema test fixture migrates cleanly to new schema); compose pipeline snapshot tests unchanged (no rendering regressions in segment 3 output).

**Dependencies:** Phase 1 (pattern_memory crate exists)

**Done when:** all three sub-task gates pass. Covers: `v3-memory-rework.AC2.*`, `v3-memory-rework.AC3.*`
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: MemoryStore sync + surface audit + eval worker simplification + async callsite migration

**Goal:** `MemoryStore` is sync, consolidated down to 19 methods. Eval worker runs on a plain OS thread. Async callsites use `spawn_blocking` only for DB ops. Session::step's internal path no longer uses `block_on`.

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
- Eliminate `Handle::current().block_on` in MemoryStore-backed handlers: `handlers/memory.rs` (~8-9 pairs), `handlers/recall.rs` (~3 pairs), `handlers/search.rs` (~1-2 pairs). Call sites replaced with direct sync calls against the sync `MemoryStore` trait.
- **`handlers/message.rs` block_on sites are NOT addressed in this phase**: they dispatch to the async `MessageRouter` (network I/O to endpoints), which stays async. Document this explicitly in the phase commit message so reviewers don't mistake it for a missed conversion.
- `handlers/scope.rs`: currently has zero `block_on` sites; verified during Phase 2 BlockType audit. No changes required in this file from the sync-ification work.
- Async callsite updates: `pattern_cli` commands wrap `MemoryStore` calls in `tokio::task::spawn_blocking`. Non-DB sync ops (cache metadata, handle validation) call directly.
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
- **Task 6a (spike)**: Mode C validation spike with explicit operation taxonomy.

  **Operation taxonomy** (50 interleaved ops drawn from these categories):
  - Host git operations: `git add`, `git commit`, `git checkout <branch>`, `git merge`, `git stash pop`
  - Pattern memory writes (agent-driven): block put, block metadata update, archival insert
  - Pattern jj operations: `jj commit`, `jj bookmark set`, `jj log`, `jj workspace update-stale`
  - Pattern attach/detach: mount a project, write some blocks, detach, re-attach
  - External .md edits (simulating human editing outside pattern)

  **Divergence check procedure:**
  - After each host git operation: verify pattern-jj can run `jj log` without errors and sees an up-to-date view (after `jj workspace update-stale` where needed)
  - After each pattern jj commit: verify host git status is clean with respect to tracked files (pattern-jj's `.jj/` is gitignored)
  - At checkpoints: compare `memory.db` content + emitted block files against loro's current state — all three must agree
  - At attach/detach boundaries: verify no leaked subscriber tasks + no stale locks
  - Final check: run `git log --all --oneline` and `jj log` on their respective views; verify both histories are internally consistent (no orphaned commits, no corrupt refs)

  **Pass criteria**: zero divergence events across all 50 operations; every host git operation followed by one `jj workspace update-stale` produces a clean consistent state; no manual intervention required for any standard developer workflow (pull, merge, checkout, commit).

  **Fail criteria**: any corruption observed; any state divergence that requires manual repair; any scenario where gitignore alone is insufficient and additional config is required by the user.

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
- `pattern_memory/src/backup/scheduler.rs` — tokio task spawned by `MountedStore::new` and tied to mount lifecycle (dropped when mount detaches); runs a scheduling loop

**Scheduler behavior specification:**

- **Scheduler lives in `MountedStore`** (the attached store returned from `attach(path)`); its task cancel-token is owned by the mount, tied to the mount's lifecycle
- **"Active use" trigger** defined concretely as: at least one message has been written to messages.db in the current scheduling interval
- **Interval check**: scheduler wakes every `snapshot_interval` (default 1 hour, configurable via `.pattern.kdl`). On wake: query messages.db for "any row added since last snapshot timestamp?" — if yes, create snapshot + apply rotation; if no, skip silently
- **Startup behavior**: on mount attach, check if last snapshot is older than `snapshot_interval`; if yes, take a snapshot immediately; this catches the case where pattern was offline for a long period
- **Crash behavior**: between-snapshot crashes are acceptable — messages.db is still the live authoritative store; worst case is losing the last interval's increment before it was snapshotted. Snapshot creation is atomic (sqlite backup API guarantees consistent snapshot even while writers active); no cross-file coordination required.
- **Manual trigger**: `pattern backup create` CLI invocation runs a snapshot out-of-band immediately; resets the scheduler's last-snapshot-time

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

**Structured as two sub-tasks with their own gates. Sub-task 8a is self-contained (MemoryScope is a pure data-transformation layer); sub-task 8b layers on top of an orthogonal concern (agent-program compile path). Bugs in 8b won't block 8a from landing.**

**Sub-task 8a: MemoryScope + isolate_from_persona**

- `pattern_memory/src/scope.rs` — `MemoryScope<S: MemoryStore>` wrapper + `ScopeBinding` + `IsolatePolicy` enum
- Policy enforcement per read/write call; `MemoryScope` is a pure data-transformation over the underlying MemoryStore
- `ctx.memory.write_to_persona` agent SDK effect: explicit persona write-back, errors unless `isolate_policy == IsolatePolicy::None`; new handler dispatch in `pattern_runtime/src/sdk/handlers/memory.rs`

**Gate:** MemoryScope policy tests pass for all three `IsolatePolicy` values (None / CoreOnly / Full) across read + write scenarios; `write_to_persona` authorization test passes (succeeds with None, errors with CoreOnly and Full).

**Sub-task 8b: Project-scoped personas + project utilities + Pattern.Diagnostics**

- `pattern_memory/src/persona.rs` — load + validate persona configs from both `~/.pattern/personas/` (global) and `<mount>/personas/` (project-scoped); project-scoped takes precedence within a mount
- `pattern_runtime/src/sdk/location.rs` modifications: `resolve_import_paths(sdk_location, project_mount) -> Vec<PathBuf>` extends the Tidepool import search path with `<mount>/lib/` when present
- Lib compile isolation: wrap each `lib/*.hs` module's compile attempt; broken modules excluded from search path; diagnostics captured into session state
- `pattern_runtime/src/sdk/requests/diagnostics.rs` + `pattern_runtime/src/sdk/handlers/diagnostics.rs` — new `Pattern.Diagnostics` effect; `diagnostics :: Effect [Diagnostic]` returns accumulated session diagnostics

**Gate:** Project-scoped persona load test passes (place `@reviewer.kdl` in mount, attach, invoke → correct persona instantiated); global vs project-scoped precedence test passes; project-lib compile test passes (broken + working modules coexist; session opens; diagnostics queryable); import-broken-lib-fails test passes with clear diagnostic.

**Dependencies:** Phase 6 (mount structure known)

**Done when:** both sub-task gates pass. Covers: `v3-memory-rework.AC12.*`, `v3-memory-rework.AC13.*`, `v3-memory-rework.AC14.*`
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

**pattern_macros absent from workspace:**

`pattern_macros` is not in the workspace as of plan-writing — this is status quo, not a divergence introduced by this plan. The from_row pattern for rusqlite row structs is intentionally hand-written (explicit, auditable, no proc-macro overhead). If the hand-written boilerplate ever becomes painful at scale, a derive crate can be added in a future focused pass — at which point the shape will be informed by real usage data.

**Intermediate code-state policy (carryover from foundation plan):**

Fate markers (`// MOVING TO:`, `// REPLACED BY:`, `// MOVING WITHIN CRATE:`) apply to any transitional code during this plan. Cruft (undefined fate, commented-out code, orphaned `unimplemented!()`) fails the intermediate-state audit run at each phase boundary. The port-list doc tracks any temporary exclusions.

**Cross-phase dependency audit:**

Phase 1's extraction surfaces all `BlockType::Archival` and `BlockType::Log` usage sites (12 files across 5 crates identified in pre-plan investigation). Phase 2's sub-task 2c resolves them via schema migration + variant removal with explicit call-site handling, with particular care for the `pattern_provider::compose` pipeline's tier-filtered rendering. Phases 4, 5, 6 build on each other's outputs in a clear chain. Phases 7 and 8 are nominally parallelizable but sequenced serially for review simplicity.

**Subscriber task scaling posture:**

The per-doc subscriber model ships with lazy-spawn + lifecycle-tied-to-LoroDoc semantics, bounded channels with backpressure, and observability metrics (`memory.sync_worker.active` gauge). Expected scale (50-1000 subscribable docs) is well within tokio's operating range. A pool-of-workers refactor is explicitly deferred as a future optimization — only to be considered if observability data shows task count becoming a meaningful cost. Do not pre-optimize.

**delete_archival SDK removal migration:**

Removing `Pattern.Memory.Archive.delete` from the agent-facing SDK has an explicit migration story: the Haskell symbol is removed in Phase 3. Existing agent programs that invoke it fail at Tidepool compile time with a clear 'symbol not found' diagnostic. This is intentional — surfacing to the agent is preferable to silent no-op or runtime error. The corresponding Rust-side `RecallReq::Delete` variant is removed from the GADT bridge (verified via `trybuild` compile-fail test). Human operators continue to have access via `MemoryStore::delete_archival` through CLI tools (`pattern-test-cli` or the eventual human-ops TUI).

**Naming: sync_worker vs eval worker:**

These are distinct components with superficially similar names. Clarified here for implementor clarity:
- **eval worker** (singular, per-session): the OS thread in `pattern_runtime::agent_loop::eval_worker` that runs Tidepool's Haskell evaluator. Sync thread, `std::sync::mpsc` intake, no tokio runtime. Runs agent turn-loops.
- **sync_worker** (plural, per-LoroDoc): OS threads in `pattern_memory::subscriber` that receive loro commit events via crossbeam channels, debounce, and emit canonical files + index updates. Sync threads, crossbeam channels, borrow from the r2d2 pool per work unit. Supervised by an async tokio task that watches heartbeats. Run storage sync.

Commit messages and comments should prefer the full names (`eval worker` and `sync_worker`) to avoid confusion.

**LoroValue `Binary` usage verification (Phase 1 sub-task):**

Phase 1 includes a grep pass across all `StructuredDocument` call sites + schema templates for uses of `LoroValue::Binary`. Expected outcome: zero usage; the `Binary` variant is defined in loro but pattern memory blocks do not contain raw bytes. If confirmed, the KDL converter rejects `LoroValue::Binary` loudly with `KdlConversionError::UnsupportedBinary` instead of silently base64-encoding (prevents introducing binary-in-block usage accidentally). If usage is found, base64 serialization ships with the converter.
