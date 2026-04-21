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

**Why CLI, not jj-lib:** on-disk format drift risk in Modes A+C is worse than
template fragility. See `docs/implementation-plans/2026-04-19-v3-memory-rework/phase_05.md`
for the full decision record.

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

**`JjAdapter::detect()` return values:**

- `Ok(Some(_))` — supported jj found.
- `Ok(None)` — jj not on PATH; Mode A continues without it.
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
- Mode A: caller invokes `quiesce` before the host VCS commit.
- Modes B/C: `JjAdapter::commit` invokes `quiesce` as its first step.

## storage modes (`src/modes.rs`)

`StorageMode` enum describing how Pattern manages VCS history for a mount.
Phase 5 introduces the skeleton; Phase 6 adds per-mode path resolution,
`.pattern.kdl` config parsing, and attach/detach logic.

- `StorageMode::A { mount_path }` — in-repo; host VCS owns history. No jj.
- `StorageMode::B { mount_path, project_id }` — separate Pattern-owned jj repo.
- `StorageMode::C { mount_path }` — sidecar jj alongside host git. Phase 6 spike.

Key method: `requires_jj()` — returns `true` for B and C; `false` for A.

**Entry point:** `pattern_memory::modes::StorageMode`

## Status

Created 2026-04-19 during v3-memory-rework Phase 1; populated incrementally
in Phases 1-8.

Phase 5 subcomponent A (jj adapter + error types + all adapter functions):
completed 2026-04-20.

Phase 5 subcomponent B (`StorageMode` enum, `quiesce()`, CI canary):
completed 2026-04-20.
