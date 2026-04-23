# CLAUDE.md - Pattern Memory

Memory subsystem implementation crate. Owns `MemoryCache` (the canonical
`MemoryStore` implementation), `SharedBlockManager`, and schema template
constructors. `StructuredDocument` lives in `pattern_core::memory::document`
(it appears in `MemoryStore` trait signatures; moving it here would create a
circular dependency).

## Dependency rule

`pattern_memory` depends on `pattern_core` and `pattern_db`. Nothing flows
back: `pattern_core` must never depend on `pattern_memory`.

## Testing

- Unit tests: in-file `#[cfg(test)] mod tests` blocks.
- Integration tests: `tests/` directory.
- Run: `cargo nextest run -p pattern-memory`.

## jj adapter (`src/jj/`)

Thin wrapper over the `jj` CLI. Shells out via `std::process::Command`;
serializes workspace mutations via an internal `Mutex` to avoid the
concurrent-workspace-add hazard documented in jj-vcs/jj#9314. Version range
is `MIN_SUPPORTED_VERSION` (0.38.0) to `MAX_TESTED_VERSION` (0.40.0);
`detect()` refuses older versions loudly.

**Why CLI, not jj-lib:** on-disk format drift risk in InRepo and Sidecar modes
is worse than template fragility. See
`docs/implementation-plans/2026-04-19-v3-memory-rework/phase_05.md` for the full
decision record.

**Template shape (jj 0.40.0):** all commands use `json(self) ++ "\n"` which
outputs the full self object as NDJSON. Serde deserialization is forgiving
(unknown fields tolerated). Key shapes:

- `jj log`: `{"commit_id":..., "change_id":..., "description":..., ...}`
- `jj workspace list`: `{"name":..., "target":{"commit_id":..., ...}}`
- `jj bookmark list`: `{"name":..., "target":["<commit_id>", ...]}`
  (`target` is an array — conflict-aware representation)

**Mitigations for CLI fragility:**

- Minimal template fields per call (reduces breakage on jj upgrades).
- Forgiving serde parse (unknown fields tolerated; missing fields flagged).
- `--color=never` universally applied via `JjAdapter::cmd()`.
- `init_repo()` uses `--no-colocate` so the backing git repo stays inside
  `.jj/repo/` (no top-level `.git/` created). Required for Sidecar mode to avoid
  host git treating the mount as a nested repository.

**`JjAdapter::detect()` return values:**

- `Ok(Some(_))` — supported jj found.
- `Ok(None)` — jj not on PATH; InRepo mode continues without it.
- `Err(UnsupportedVersion)` — jj found but too old.

**Entry point:** `pattern_memory::jj::JjAdapter`

## quiesce (`src/quiesce.rs`)

Universal pre-commit step, invoked regardless of storage mode. Uses a
flush-pause-resume model to avoid killing worker threads (which would create
a write-loss window). Runs in four ordered steps:

1. **Pause subscribers** — calls `MemoryCache::pause_subscribers()`, which
   sets the `paused` flag on each worker. Each worker drains its channel,
   imports pending updates into disk_doc, renders the canonical file, records
   version vectors for both memory_doc and disk_doc, then parks on a condvar.
   Workers stay alive — subscriptions and channels remain intact.

2. **WAL checkpoint** — calls `MemoryCache::wal_checkpoint()`, which delegates
   to `ConstellationDb::checkpoint()` running `PRAGMA wal_checkpoint(TRUNCATE)`
   on `memory.db`. This is a hard error — without a successful checkpoint the
   on-disk DB is not canonical.

3. **fsync emitted files** — calls `File::sync_all()` on each path in the
   caller-supplied `emitted_file_paths`. Individual fsync failures are
   non-fatal: logged at WARN, counted in `QuiesceOutcome::fsync_failures`, but
   do not abort the call.

4. **Resume subscribers** — calls `MemoryCache::resume_subscribers()`, which
   wakes each parked worker. Workers reconcile writes from the pause window
   via version-vector diff (catching both agent writes to memory_doc and
   external edits to disk_doc), render once, then return to the normal loop.

`drain_subscribers()` is retained for `drop_doc` and cache shutdown where
workers genuinely need to be killed.

**Entry point:** `pattern_memory::quiesce::quiesce(&cache, &paths)`

**When to call:**
- InRepo mode: caller invokes `quiesce` before the host VCS commit.
- Standalone / Sidecar modes: `JjAdapter::commit` invokes `quiesce` as its first step.

## storage modes (`src/modes.rs`, `src/modes/`)

`StorageMode` enum describing how Pattern manages VCS history for a mount.

- `StorageMode::InRepo { mount_path, project_root }` — in-repo; host VCS owns history. No jj.
- `StorageMode::Standalone { mount_path, project_id }` — separate Pattern-owned jj repo.
- `StorageMode::Sidecar { mount_path }` — sidecar jj alongside host git. Validated by Phase 6 spike (2026-04-20, 38 ops, PASS).

Key method: `requires_jj()` — returns `true` for `Standalone` and `Sidecar`; `false` for `InRepo`.

`.pattern.kdl` config accepts both the canonical names (`"in-repo"`, `"standalone"`, `"sidecar"`) and the legacy single-letter aliases (`"A"`, `"B"`, `"C"`) for backward compatibility.

Submodules:

- `modes::in_repo` — InRepo mode init (`init(project_root)` creates `.pattern/shared/` layout + `.pattern.kdl` + `.gitignore` entry).
- `modes::standalone` — Standalone mode init (`init(project_id, &jj_adapter)` creates `~/.pattern/projects/<id>/shared/` + jj repo).
- `modes::sidecar` — Sidecar mode init (`init(project_root, &jj_adapter)` creates `.pattern/shared/` layout + jj repo + `.gitignore` entries). Sidecar jj inside host git project; validated by Phase 6 spike.
- `modes::gitignore` — idempotent `.gitignore` append helper.
- `modes::error` — `ModeError` type.

**Entry point:** `pattern_memory::modes::StorageMode`

## mount (`src/mount.rs`, `src/mount/`)

`MountedStore` is the runtime handle returned from `attach(start_path)`.
Owns `MemoryCache`, `ConstellationDb`, subscriber supervisor, `MountWatcher`,
and optional `ReembedQueue` for the mount's lifetime. `detach()` drains
subscribers, stops the watcher, drops the reembed queue, and releases DB
references.

**ReembedQueue wiring:** `attach()` calls `ReembedQueue::spawn(None, db)` when
a tokio runtime is available (provider=None means silent drain until Phase 8
wires the embedding pipeline). When no runtime is available (sync-only test
contexts), the receiver is dropped and workers handle SendError gracefully.

- `find_mount(start)` — walk upward for `.pattern/shared/.pattern.kdl`.
- `attach(start)` — find mount, parse config, resolve DB paths, open DBs, build cache with subscribers, start watcher, spawn reembed queue. Returns `MountedStore`.
- `MountedStore::detach(self)` — sync teardown: stop watcher, drain subscribers, drop reembed queue, drop resources.

Submodules:

- `mount::attach` — the `attach()` function.
- `mount::error` — `MountError` type (with `NotFound` diagnostic hinting `pattern mount init`).

**Entry point:** `pattern_memory::mount::attach`

## backup (`src/backup.rs`, `src/backup/`)

Atomic `messages.db` snapshot, GFS-style rotation, and safe restore. All
functions are pure library — no global state, no process-level assumptions.

### Key invariants

- **Pre-restore safety**: `restore_snapshot` always copies the current
  `messages.db` to a `.pre-restore-<ns>` file (using nanosecond timestamps to
  guarantee uniqueness even across rapid successive restores) before any swap.
- **WAL strip**: every snapshot and restore destination runs
  `PRAGMA journal_mode = DELETE` after the Backup API finishes, so files are
  clean single-file SQLite databases that do not create a `-wal` sidecar on
  next open.
- **Pool-closed requirement**: `restore_snapshot` must be called with no active
  r2d2 pool open on `messages.db`. Production: CLI runs in a separate one-shot
  process. Tests: `drop(db)` before calling restore.

### Public entry points

- `backup::snapshot::create_snapshot(source, paths, project_id)` — atomic
  snapshot via rusqlite Backup API; returns `SnapshotInfo`.
- `backup::rotation::list_snapshots(paths, project_id)` — `Vec<SnapshotInfo>`,
  newest-first; skips non-sqlite and non-timestamp-named files.
- `backup::rotation::select_deletions(snapshots, policy, now)` — GFS keep set:
  keep-N + hourly/daily/monthly bands. Always keeps ≥1.
- `backup::rotation::apply_rotation(paths, project_id, policy)` — list +
  select + delete; returns deleted count.
- `backup::restore::restore_snapshot(messages_db_path, snapshot_path)` —
  integrity-check + safety-copy + atomic swap; returns pre-restore path.
- `backup::restore::resolve_snapshot(paths, project_id, spec)` — resolves
  `"latest"`, exact stem, or `YYYY-MM-DD` prefix to a `SnapshotInfo`.

### Filename format

`YYYY-MM-DDTHHMMSSZ` (e.g. `2026-04-19T120000Z`). No colons — Windows-safe.
Pre-restore safety copies use nanosecond decimal suffixes (not this format) so
`list_snapshots` skips them cleanly.

**Entry point:** `pattern_memory::backup`

## scope (`src/scope/`)

`MemoryScope` is a `MemoryStore`-wrapping layer that routes reads and writes
between persona and project scopes according to an `IsolatePolicy`.

- `ScopeBinding` — config struct: `persona_id`, optional `project_id`, `policy`.
  `passthrough(persona_id)` creates a no-op binding.
- `MemoryScope` — implements `MemoryStore`; wraps an inner store and applies
  policy-based routing. Under `IsolatePolicy::None`, all calls pass through.
  Under `CoreOnly`, persona core blocks are read-only from project context.
  Under `Full`, persona memory is not carried over at all.
- `WriteToPersona` (SDK effect in pattern_runtime) is only allowed when
  `IsolatePolicy::None` is active; otherwise returns `MemoryError::IsolationDenied`.

**Entry point:** `pattern_memory::scope::MemoryScope`

## subscriber (`src/subscriber/`)

Loro-native CRDT sync between in-memory `MemoryCache` docs and on-disk files.
Each document gets a dedicated OS thread (`SyncWorker`) that watches for
mutations via `crossbeam-channel`, debounces, renders the canonical file format,
and updates FTS5 indexes.

- `subscriber::worker` — `SyncWorker` with two-doc model (memory_doc + disk_doc).
- `subscriber::supervisor` — respawns crashed workers automatically.
- `subscriber::event` — `SyncEvent` enum for the channel protocol.

Workers support pause/resume for quiesce (see above) and drain for shutdown.

**Entry point:** `pattern_memory::subscriber`

## config (`src/config/`)

Typed parsing of `.pattern.kdl` config files via `knus` (KDL derive decoder).
Validates storage mode, project identity, isolation policy, and backup schedule.

**Entry point:** `pattern_memory::config::pattern_kdl::PatternConfig`

## persona (`src/persona/`)

Persona discovery: scans a mount's `personas/` directory for `.kdl` files,
validates each against the persona loader, and returns a manifest of available
personas with their paths and metadata.

**Entry point:** `pattern_memory::persona::discover::discover_personas`

## reembed (`src/reembed.rs`)

Background re-embedding queue. `ReembedQueue::spawn()` creates a tokio task
that drains embedding requests from a channel. Provider is `Option` — when
`None`, requests are silently drained (placeholder until the embedding pipeline
is wired).

**Entry point:** `pattern_memory::reembed::ReembedQueue`

## Status

Last verified: 2026-04-20

Created 2026-04-19 during v3-memory-rework Phase 1; populated incrementally
in Phases 1-8. All 8 phases complete.

Phase 5 subcomponent A (jj adapter + error types + all adapter functions):
completed 2026-04-20.

Phase 5 subcomponent B (`StorageMode` enum, `quiesce()`, CI canary):
completed 2026-04-20.

Phase 6 subcomponent B (InRepo + Standalone init, MountedStore attach/detach,
CLI subcommands): completed 2026-04-20.

Phase 6 task 7 (Sidecar init, attach, CLI `--mode sidecar`, validation spike):
completed 2026-04-20. See `docs/notes/2026-04-20-mode-c-spike.md` for
spike results. Spike expanded 2026-04-20 to 38 ops including attach/detach
cycles, MemoryStore writes, and external .md edits.

Storage modes renamed 2026-04-23: `Mode A/B/C` → `InRepo/Standalone/Sidecar`.
The `ModeKind` KDL parser keeps the legacy single-letter strings as aliases.

Phase 6 code review fixes (2026-04-20):
- `attach()` now spawns `ReembedQueue` when tokio runtime is available.
- `MountedStore.reembed_queue` field stores the queue handle.
- Standalone mode tests use `PatternPaths::with_base(tempdir)` (no unsafe env var, no real `~/.pattern/` writes).
- `PatternPaths` struct replaced free path functions; `default_paths()` for production, `with_base()` for tests.
- `attach_with_paths()` accepts injectable `PatternPaths` for test isolation.
- `persist()` uses version-vector comparison instead of dirty flag — prevents silent data loss.
- `ModeKind` parse error falls back to `Standalone` (safer than `InRepo` — stays in `~/.pattern/` and cannot accidentally pollute a project directory).
- `IsolateSection.policy` validated as one of "none"/"core-only"/"full".
- CLI integration tests in `crates/pattern_cli/tests/cli_mount.rs`.

Phase 7 subcomponent A (backup::snapshot, backup::rotation, backup::restore):
completed 2026-04-20. AC11.1–11.7 implemented and passing (23 tests: 13 unit
+ 10 integration). Root bug fixed: pre-restore safety copies used second-
precision timestamps causing name collision when rollback restore happened
in the same second; switched to nanosecond decimal suffix.
