# Pattern v3 Memory Rework — Phase 2 Implementation Plan

**Goal:** Migrate `pattern_db` from sqlx 0.8 to rusqlite 0.39 (synchronous, bundled SQLite 3.51.3) with `r2d2_sqlite` pooling, split `messages.db` out of `memory.db` via `ATTACH DATABASE`, and remove the `BlockType::Archival` and `BlockType::Log` enum variants with clean call-site handling and a data migration.

**Architecture:** `ConstellationDb` is rewired around `r2d2::Pool<SqliteConnectionManager>` with a `with_init` hook that configures pragmas, loads sqlite-vec 0.1.9 (source-compiled, source-linked against rusqlite's bundled SQLite), and attaches `messages.db` as schema `msg`. The eval worker continues to use a dedicated non-pool connection for its session lifetime (the deeper eval-worker rework is Phase 3). Migrations run under `rusqlite_migration 2.5.0` with split directories: `migrations/memory/` executes on the main connection; `migrations/messages/` executes on a temporarily-opened direct `Connection` to `messages.db`. All 236 queries across `pattern_db/src/` port to rusqlite prepared statements + inherent `fn from_row(row) -> rusqlite::Result<Self>` on row structs (no derive macros, no helper trait — explicit and auditable). `BlockType::Archival` rows migrate into the existing `archival_entries` table (distinct schema already present). `BlockType::Log` rows reclassify to `Working` tier with `BlockSchema::Log`.

**Tech Stack:** rusqlite 0.39 (features `bundled-full` + `load_extension` + `jiff` + `serde_json` + `i128`), r2d2 0.8, r2d2_sqlite 0.33, rusqlite_migration 2.5, sqlite-vec 0.1.9, `insta` for FTS5/KNN regression snapshots, `tempfile` for integration tests.

**Scope:** Phase 2 of 8 (Phase 2 from the design).

**Codebase verified:** 2026-04-19 (codebase-investigator agent a4090320d2615f3b2).

**External compat verified:** 2026-04-19 empirical spike — built a standalone Rust project with the full Phase 2 pins (rusqlite 0.39 + bundled-full + sqlite-vec 0.1.9), created a `vec0` virtual table, inserted 384-dim-equivalent vectors, ran KNN, got correct ordering. Independently verified `ATTACH DATABASE 'path' AS msg` auto-creates the file when it doesn't exist (standard SQLite behavior). Spike transcript kept in this plan's companion notes (not committed to the repo).

**Execution posture:** Hybrid — large mechanical work (query port) delegated to subagents with main-executor sign-off at two checkpoints (end of sub-task 2b; end of sub-task 2c). No blocking spike gate (design's sub-task 2a is empirically resolved — see below).

---

## Acceptance Criteria Coverage

This phase implements and tests:

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

---

## Codebase verification findings

Relevant findings from the Phase 2 investigation that inform the task breakdown:

- ✓ Current pattern_db has **236 query-macro invocations**: 105 `sqlx::query!`, 88 `sqlx::query_as!`, 7 `sqlx::query_scalar!`, 17 `sqlx::query`, 18 `sqlx::query_as`, 1 `sqlx::query_scalar`. Per-domain breakdown (in `queries/*.rs`): memory.rs 59, agent.rs 28, coordination.rs 24, message.rs 19, folder.rs 18, source.rs 13, event.rs 12, task.rs 14, stats.rs 6, atproto_endpoints.rs 5, queue.rs 4. The remaining ~34 macros live in `fts.rs` (557 lines) and `vector.rs` (525 lines).
- ✓ `ConstellationDb` lives at `crates/pattern_db/src/connection.rs:15-66`. Current pool: 5 max connections (1 for in-memory). Current init calls `crate::vector::init_sqlite_vec()` before pool creation to register sqlite-vec via `sqlite3_auto_extension` — empirically, this same registration pattern works under rusqlite 0.39.
- ✗ Design names the third transaction site `insert_memory_block_update`. Reality: it's `store_update` at `queries/memory.rs:729-772` (2 queries inside). The other two sites are correctly named: `update_block_config` (lines 403-473, 3 queries) and `consolidate_checkpoint` (lines 892-954, 4 queries). Plan uses the real names; `AC2.5`'s intent (three transaction sites port atomically) is unaffected.
- ✓ `libsqlite3-sys = "=0.30.1"` pinned at `crates/pattern_db/Cargo.toml:47`. Only occurrence in the workspace. Dropping this pin is safe.
- ✓ `BlockType` call sites: 12 files across 5 crates, matching design's estimate. Distribution:
  - `pattern_core/src/memory/types.rs` (enum def + Display + FromStr + From impls — but post-Phase-1 this file is gone; enum lives at `pattern_core::types::memory_types::core_types`).
  - `pattern_core/src/export/letta_convert.rs` (line 832 string match + line 933 variant match), `pattern_core/src/export/tests.rs` (line 509 fixture).
  - `pattern_runtime/src/session.rs` (line 730), `pattern_runtime/src/agent_loop.rs` (lines 412-413 string match, 699 filter), `pattern_runtime/src/sdk/requests/memory.rs` (lines 36-37 `BlockTypeReq → BlockType`), `pattern_runtime/src/sdk/handlers/memory.rs` (line 305 explicit construction).
  - `pattern_provider/src/compose/current_state.rs` (lines 112-120 render), `pattern_provider/src/compose/pseudo_messages.rs` (lines 280-286 render).
  - `pattern_cli/src/commands/builder/agent.rs` (line 1049), `pattern_cli/src/commands/builder/group.rs` (line 1061), `pattern_cli/src/commands/debug.rs` (lines 359-360 routing to `log_blocks`/`archival_blocks` vecs).
- ✓ Compose pipeline filter is two-layer: (1) the `render_block_type` fn at `pseudo_messages.rs:280-286` and mirror at `current_state.rs:112-120` that maps each variant to a string label; (2) `agent_loop.rs:699` filter `BlockType::Archival | BlockType::Log => false` that excludes those tiers from default snapshot selection. Post-cleanup, `render_block_type` maps only `Core`/`Working`; the exclusion filter collapses (no variants to exclude — archival lives in `archival_entries`, Log lives as a schema on Working-tier blocks).
- ✓ Archival data already lives in a distinct `archival_entries` table (migration `0001_initial.sql`). The data migration in sub-task 2c converts `memory_blocks` rows with `block_type = 'archival'` into `archival_entries` rows — clear target schema exists.
- ✓ Messages currently share `constellation.db` (single-file SQLite). No prior messages.db split. Phase 2 creates the split cleanly: memory.db holds zero message tables; messages.db is the sole home for messages, queued_messages, message_tombstones. No transitional duplication. v2 → v3 data transfer is out of scope here AND out of scope for a future DB-migrator plan: the migration path from pre-rewrite deployments is CAR-file export from `main` branch + import via a standalone converter into fresh v3 databases. No on-the-wire schema compat to maintain.
- ✓ Zero `insta` snapshots in pattern_db today. FTS5 tests at `fts.rs:386-540` (7 tests) and vector tests at `vector.rs:352-500` (6 tests) use `assert_eq!` on concrete values but lack snapshot regression coverage. Phase 2 adds insta snapshots as part of the port.
- ✓ `pattern_db/CLAUDE.md` documents the old sqlx `sqlx database reset` / `sqlx migrate run` / `cargo sqlx prepare` workflow. Needs updating for rusqlite + rusqlite_migration.
- ✓ `.sqlx/` directory present at `crates/pattern_db/.sqlx/` (sqlx prepare cache). Must be deleted as part of the swap; update `.gitignore` if it was tracked.
- ✓ rusqlite 0.39, r2d2_sqlite 0.33, rusqlite_migration 2.5.0 are the latest releases (verified via crates.io query 2026-04-19). Design's "rusqlite_migration 1.0" reference is stale — no stable 1.0 exists; the crate jumped pre-1.0 alphas → 2.x.
- ✓ rusqlite 0.39 disables `u64`/`usize` ToSql/FromSql unless the `i128` feature is enabled. The feature is enabled in the pins below. Port still audits `params!` sites for unsafe casts.
- ✓ sqlite-vec 0.1.9 upgrade from the pinned 0.1.7-alpha.2 brings in the DELETE-operations-on-long-metadata bug fix (#274) plus general stabilization.

---

## Dependency changes (overview — task-by-task below)

`crates/pattern_db/Cargo.toml` ends Phase 2 with (elisions match current deps that are unchanged):

```toml
[dependencies]
# runtime
tokio = { workspace = true }

# sqlite
rusqlite = { version = "0.39", features = ["bundled-full", "load_extension", "jiff", "serde_json", "i128"] }
r2d2 = "0.8"
r2d2_sqlite = "0.33"
rusqlite_migration = "2.5"
sqlite-vec = "0.1.9"

# domain (unchanged)
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
miette = { workspace = true }
tracing = { workspace = true }
chrono = { workspace = true, features = ["serde"] }
uuid = { workspace = true }
loro = "1.6"
zerocopy = { version = "0.8", features = ["derive"] }

# REMOVED:
# sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "migrate", "json", "chrono"] }
# libsqlite3-sys = "=0.30.1"

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tempfile = "3"
insta = { version = "1", features = ["yaml"] }
```

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-5) -->

### Subcomponent A — sub-task 2a/2b merged: Dep swap + ConstellationDb rewrite + init_connection + migration runner + messages.db split

The original design had sub-task 2a as a blocking sqlite-vec spike with its own gate. That spike is empirically resolved (rusqlite 0.39 + bundled-full + sqlite-vec 0.1.9 + KNN all work; `ATTACH DATABASE` auto-creates missing files). So 2a collapses into "pin the versions" and merges into 2b. The **gate at end of Subcomponent A** requires main-executor sign-off before Subcomponent B (query port) begins.

<!-- START_TASK_1 -->
### Task 1: Swap Cargo deps in `pattern_db` and delete sqlx prepare cache

**Verifies:** v3-memory-rework.AC2.8 (`libsqlite3-sys` dep removed)

**Files:**
- Modify: `crates/pattern_db/Cargo.toml` (swap per "Dependency changes" above)
- Delete: `crates/pattern_db/.sqlx/` (entire directory; sqlx prepare cache no longer needed)
- Modify: `.gitignore` (if it references `.sqlx/`, replace with a comment noting the directory was removed post-sqlx; otherwise leave alone)
- Modify: `crates/pattern_db/CLAUDE.md` (replace the sqlx workflow paragraph with a rusqlite + rusqlite_migration blurb: "queries are `rusqlite::Connection::prepare` with inherent `fn from_row` on each row struct; migrations live in `migrations/memory/` and `migrations/messages/`, applied by `rusqlite_migration 2.5`; no compile-time macro + no `.sqlx/` cache; tests use `cargo nextest run -p pattern-db`")

**Implementation:**

1. Replace the entire `[dependencies]` and `[dev-dependencies]` sections in `pattern_db/Cargo.toml` with the block under "Dependency changes" above.
2. `rm -r crates/pattern_db/.sqlx/`.
3. `cargo check -p pattern_db` will fail — that's expected; remaining tasks make it compile.
4. Freshen `pattern_db/CLAUDE.md` with the new guidance; include a freshness date `Updated YYYY-MM-DD in v3-memory-rework Phase 2.` at the top.

**Testing:** Operational only (no code to test).

**Verification:**

Run: `ls crates/pattern_db/.sqlx/ 2>&1`
Expected: `No such file or directory`.

Run: `grep -n "sqlx\|libsqlite3-sys" crates/pattern_db/Cargo.toml`
Expected: no matches.

**Commit:** `[pattern-db] swap sqlx → rusqlite 0.39 deps; drop libsqlite3-sys pin; delete .sqlx cache`

<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Rewrite `ConstellationDb` around `r2d2::Pool<SqliteConnectionManager>` with `init_connection` hook

**Verifies:** v3-memory-rework.AC2.1 (partial — connection layer compiles), AC2.10 (ATTACH messages.db works)

**Files:**
- Modify: `crates/pattern_db/src/connection.rs` (full rewrite, ~150-180 lines)
- Modify: `crates/pattern_db/src/error.rs` (add rusqlite + r2d2 error variants; keep `#[non_exhaustive]`)
- Create: `crates/pattern_db/src/connection/init.rs` (the `init_connection` hook module — split out for testability)

**Implementation:**

1. Design a new `ConstellationDb` struct:

   ```rust
   pub struct ConstellationDb {
       pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
       memory_path: PathBuf,
       messages_path: PathBuf,
   }
   ```

   Public surface:
   - `pub fn open(memory_path: impl Into<PathBuf>, messages_path: impl Into<PathBuf>) -> Result<Self>` — runs memory migrations on the main connection AND messages migrations on a temp direct connection (see Task 3), then builds the pool.
   - `pub fn open_in_memory() -> Result<Self>` — both schemas in `:memory:`; messages.db uses `file::memory:?cache=shared` or similar.
   - `pub fn get(&self) -> Result<r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>>` — delegates to `self.pool.get()`.
   - `pub fn dedicated_connection(&self) -> Result<rusqlite::Connection>` — opens a fresh non-pool connection with the same `init_connection` hook applied. Used by the eval worker (Phase 3) for its session lifetime; exposed now so Phase 3 doesn't need to re-touch this file.
   - `pub fn memory_path(&self) -> &Path` / `pub fn messages_path(&self) -> &Path` — accessors.

2. Pool configuration:

   ```rust
   let manager = SqliteConnectionManager::file(&memory_path)
       .with_init(move |conn| init::init_connection(conn, &messages_path_clone));

   let pool = r2d2::Pool::builder()
       .max_size(10)
       .min_idle(Some(2))
       .connection_timeout(std::time::Duration::from_secs(30))
       .build(manager)?;
   ```

   Rationale: 10 max / 2 min_idle is higher than the existing 5 because rusqlite access is synchronous — pool is more heavily used under concurrent async callers via `spawn_blocking`.

3. **sqlite-vec registration happens once at `ConstellationDb::open`, NOT per-connection.** `sqlite3_auto_extension` is process-global state; placing it in `init_connection` would be architecturally misleading (it would appear connection-scoped but actually registers globally). Call it once during `ConstellationDb::open`, before building the pool:

   ```rust
   impl ConstellationDb {
       pub fn open(memory_path: impl Into<PathBuf>, messages_path: impl Into<PathBuf>) -> Result<Self> {
           // Process-global sqlite-vec registration. After this call, every
           // subsequently-opened connection (pool or dedicated) auto-loads
           // sqlite-vec. Idempotent: repeated calls are no-ops, so it's safe
           // to call even if another part of the process already registered.
           unsafe {
               use rusqlite::ffi::sqlite3_auto_extension;
               sqlite3_auto_extension(Some(std::mem::transmute(
                   sqlite_vec::sqlite3_vec_init as *const (),
               )));
           }

           // ... proceed to build the pool with init_connection hook ...
       }
   }
   ```

4. `init::init_connection(conn: &mut rusqlite::Connection, messages_path: &Path) -> rusqlite::Result<()>` — per-connection pragmas + ATTACH only:

   ```rust
   pub fn init_connection(conn: &mut Connection, messages_path: &Path) -> rusqlite::Result<()> {
       // Pragmas on main connection.
       conn.execute_batch("
           PRAGMA foreign_keys = ON;
           PRAGMA journal_mode = WAL;
           PRAGMA busy_timeout = 5000;
           PRAGMA cache_size = -65536;        -- 64 MiB
           PRAGMA mmap_size = 268435456;      -- 256 MiB
           PRAGMA temp_store = MEMORY;
       ")?;

       // sqlite-vec is already loaded process-wide (see ConstellationDb::open).
       // Every new connection inherits it automatically; no per-connection
       // load_extension call needed.

       // Attach messages.db and apply pragmas to it.
       conn.execute(
           "ATTACH DATABASE ?1 AS msg",
           rusqlite::params![messages_path.to_string_lossy()],
       )?;
       conn.execute_batch("
           PRAGMA msg.journal_mode = WAL;
           PRAGMA msg.foreign_keys = ON;
       ")?;
       Ok(())
   }
   ```

   **Important details** empirically verified in the 2026-04-19 spike:
   - `ATTACH DATABASE '<path>' AS msg` auto-creates the file if absent; no separate `Connection::open` needed to materialize.
   - `PRAGMA journal_mode = WAL` is per-database; must be set on both `main` and `msg`.
   - `sqlite3_auto_extension` is process-global; registered once at `ConstellationDb::open`, not per-connection.

4. Expand `pattern_db::error::DbError` (or whatever the existing error enum is named) with new `#[non_exhaustive]` variants for rusqlite + r2d2 + rusqlite_migration errors. Convert each with `#[from]` via `thiserror::Error`. Add a variant for extension-load failures surfacing as `ConnectionInitError::ExtensionLoadFailed(String)` (per design's error-handling policy).

**Testing:**

Unit tests colocated in `connection.rs` and `connection/init.rs`:

- `init_connection` sets expected pragmas on both `main` and `msg` (query `PRAGMA main.journal_mode` / `PRAGMA msg.journal_mode`, assert `"wal"`).
- `open` on fresh temp paths materializes both DB files.
- `open_in_memory` succeeds without panics and returns a pool that hands out working connections.
- Pool stress (integration test at `tests/pool_stress.rs`): spawn 20 tokio tasks each doing `spawn_blocking(move || { let conn = db.get()?; conn.query_row("SELECT 1", [], |r| r.get::<_,i64>(0)) })`; assert no deadlock within 10s wall clock, no pool exhaustion (all 20 succeed).

**Verification:**

Run: `cargo check -p pattern_db`
Expected: connection.rs and init.rs compile; other files still broken (query port is next task).

Run: `cargo nextest run -p pattern_db --test pool_stress` (after the test is written in this task)
Expected: passes.

**Commit:** `[pattern-db] rewrite ConstellationDb around r2d2_sqlite pool; wire init_connection + ATTACH messages.db`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Split migrations into `memory/` and `messages/` directories; wire `rusqlite_migration 2.5` runners

**Verifies:** v3-memory-rework.AC2.1, AC2.10

**Files:**
- Move: `crates/pattern_db/migrations/*.sql` → `crates/pattern_db/migrations/memory/*.sql` (13 existing files)
- Create: `crates/pattern_db/migrations/messages/` (directory)
- Create: `crates/pattern_db/migrations/messages/M000001__messages_init.sql` — schema for `messages`, `queued_messages`, `message_tombstones`, their indexes, their FTS5 virtual tables, and the triggers wiring FTS5 to the content tables. LIFT this content out of the current `migrations/memory/0001_initial.sql` + related memory-side migrations (0005_queued_messages.sql, 0007_message_tombstones.sql, 0012_queued_message_full_content.sql) into the single new `M000001__messages_init.sql`. Any message-FTS statements from `0002_fts5.sql` and `0006_archival_fts_metadata.sql` that reference message tables also move to the messages side.
- Modify: `crates/pattern_db/migrations/memory/0001_initial.sql` — DELETE all message-related CREATE TABLE / INDEX / TRIGGER / VIEW statements. memory.db has zero message tables after this migration.
- Modify: `crates/pattern_db/migrations/memory/0002_fts5.sql` — DELETE message-FTS statements; keep memory/archival FTS.
- Modify / delete: `crates/pattern_db/migrations/memory/0005_queued_messages.sql`, `0007_message_tombstones.sql`, `0012_queued_message_full_content.sql` — if a migration file becomes entirely empty, delete it and renumber downstream migrations accordingly. **Renumbering caveat**: rusqlite_migration tracks migration index via `user_version`; on a fresh v3 database there's no prior index to preserve, so renumbering is safe. Never delete or renumber migrations on a database that's already applied them — but v3 is greenfield (no extant deployments), so this applies cleanly.
- Create: `crates/pattern_db/src/migrations.rs` (runners module)
- Modify: `crates/pattern_db/src/connection.rs` (call migration runners from `open` before building the pool)
- Modify: `crates/pattern_db/src/lib.rs` (add `mod migrations;`)

**Implementation:**

1. Move current migrations: `git mv crates/pattern_db/migrations/*.sql crates/pattern_db/migrations/memory/` (or `jj move`). The 13 files (`0001_initial.sql` through `0013_update_frontiers.sql`) go into `migrations/memory/`.

2. Author `crates/pattern_db/migrations/messages/M000001__messages_init.sql`. CUT (don't copy) the `messages`, `queued_messages`, `message_tombstones` CREATE TABLE + index + FTS5 virtual table + trigger statements from the current memory-side migrations (`0001_initial.sql`, `0002_fts5.sql`, `0005_queued_messages.sql`, `0007_message_tombstones.sql`, `0012_queued_message_full_content.sql`) and assemble them into a single initial migration for messages.db. Memory-side migrations lose these statements entirely — memory.db has zero message tables post-Phase-2.

   Rationale for the clean split (no transitional duplication): v3 is greenfield. No pre-v3 databases will ever run v3 migrations. The migration path from pre-rewrite Pattern deployments (on the `main` branch) is CAR-file export plus a standalone converter into fresh v3 databases — not an in-place schema migration. So there is no compatibility window to bridge.

   After the cut, `migrations/memory/0005_queued_messages.sql`, `0007_message_tombstones.sql`, and `0012_queued_message_full_content.sql` are entirely empty. Delete those three files and renumber any trailing migrations (0009, 0010, 0011, 0013) to fill the gaps, maintaining sequential order. Again: safe because no extant database has applied these yet under the v3 schema; `rusqlite_migration` will simply see the renumbered sequence as canonical.

3. `crates/pattern_db/src/migrations.rs`:

   ```rust
   use rusqlite_migration::{Migrations, M};
   use std::sync::LazyLock;

   // Compile-time include of SQL file contents.
   static MEMORY_MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
       Migrations::new(vec![
           M::up(include_str!("../migrations/memory/0001_initial.sql")),
           M::up(include_str!("../migrations/memory/0002_fts5.sql")),
           // ... through 0013
       ])
   });

   static MESSAGES_MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
       Migrations::new(vec![
           M::up(include_str!("../migrations/messages/M000001__messages_init.sql")),
       ])
   });

   pub fn run_memory_migrations(conn: &mut rusqlite::Connection) -> Result<(), rusqlite_migration::Error> {
       MEMORY_MIGRATIONS.to_latest(conn)
   }

   pub fn run_messages_migrations(conn: &mut rusqlite::Connection) -> Result<(), rusqlite_migration::Error> {
       MESSAGES_MIGRATIONS.to_latest(conn)
   }
   ```

   Using `include_str!` means migration files are compile-time constants; no filesystem access needed at runtime. `rusqlite_migration 2.5` recommends this pattern for applications that embed their migrations.

4. In `ConstellationDb::open`, before building the pool:

   ```rust
   // Run memory migrations on a temporary direct connection to memory.db.
   {
       let mut mem_conn = rusqlite::Connection::open(&memory_path)?;
       migrations::run_memory_migrations(&mut mem_conn)?;
   }
   // Run messages migrations on a temporary direct connection to messages.db.
   {
       let mut msg_conn = rusqlite::Connection::open(&messages_path)?;
       migrations::run_messages_migrations(&mut msg_conn)?;
   }
   // Both databases are now at latest schema. Build the pool; init_connection
   // will ATTACH messages.db into every pooled connection.
   ```

**Testing:**

- Migration round-trip test (`tests/migrations_roundtrip.rs`): open fresh temp paths, call `ConstellationDb::open`, verify tables present in both schemas via `SELECT name FROM sqlite_master WHERE type='table'` on main connection, and `SELECT name FROM msg.sqlite_master WHERE type='table'` on the same connection.
- Idempotent test: call `open` twice on the same path; `user_version` pragma reports latest; no migration is re-applied.
- Compile-time include check: changing a .sql file triggers rebuild (implicit; no explicit test needed).

**Verification:**

Run: `cargo check -p pattern_db`
Expected: compiles.

Run: `cargo nextest run -p pattern_db --test migrations_roundtrip`
Expected: passes.

**Commit:** `[pattern-db] split memory/messages migrations; wire rusqlite_migration 2.5 runners`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Add `FromSql`/`ToSql` impls for domain scalar types

**Verifies:** v3-memory-rework.AC2.1 (partial — scalar types roundtrip through rusqlite)

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/core_types.rs` (add `rusqlite::types::{FromSql, ToSql}` impls for `BlockType`)
- Modify: `crates/pattern_core/src/types/memory_types/metadata.rs` (same for `BlockPermission` if it exists and is used as a SQLite column)
- Modify: whichever file now holds `BlockType` post-Phase-1 (confirm via `grep -rn "pub enum BlockType" crates/pattern_core/src/`)
- Modify: any other domain scalar types used as SQLite columns. Enumerate via `grep -rn "sqlx::Type\|FromRow\|sqlx::sqlite" crates/pattern_core/src/` and `crates/pattern_db/src/` — every sqlx-derived column type needs a rusqlite equivalent. Candidates to look for: ID newtypes (e.g., `AgentId`, `BlockId`), enum columns, any JSON-blob typed columns.
- Create: `crates/pattern_db/src/sql_types.rs` (home for any rusqlite-specific `FromSql`/`ToSql` impls that belong in pattern_db rather than pattern_core)

**Implementation:**

1. For each domain enum stored as a string in SQLite (e.g., `BlockType`), implement:

   ```rust
   use rusqlite::types::{FromSql, FromSqlResult, FromSqlError, ToSql, ToSqlOutput, ValueRef};

   impl ToSql for BlockType {
       fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
           // Reuse existing Display impl if present; otherwise explicit match.
           Ok(ToSqlOutput::from(self.to_string()))
       }
   }

   impl FromSql for BlockType {
       fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
           let s = value.as_str()?;
           s.parse::<BlockType>()
               .map_err(|e| FromSqlError::Other(Box::new(e)))
       }
   }
   ```

2. For JSON-blob columns (columns storing `serde_json::Value` or serde-serialized structs), implement using rusqlite's `serde_json` feature (which provides blanket `FromSql`/`ToSql` for `serde_json::Value`):

   ```rust
   // For a Foo stored as JSON TEXT:
   impl ToSql for Foo {
       fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
           serde_json::to_string(self)
               .map(ToSqlOutput::from)
               .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
       }
   }

   impl FromSql for Foo {
       fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
           let s = value.as_str()?;
           serde_json::from_str(s)
               .map_err(|e| FromSqlError::Other(Box::new(e)))
       }
   }
   ```

3. For newtype ID wrappers (UUIDs), prefer storing as TEXT (UUID string) and implementing a thin `FromSql`/`ToSql` via `uuid::Uuid`'s existing round-trip if pattern already uses TEXT uuids. Confirm the current storage format by reading a sample of existing queries (e.g., `grep -A2 "FROM agents WHERE id" crates/pattern_db/src/queries/agent.rs`).

4. Where the impls naturally live in `pattern_core` (because the type lives there), put them there. Where they depend on pattern_db-specific adapters (e.g., a custom wrapper), put them in `pattern_db::sql_types`. Prefer pattern_core home; minimize the pattern_db surface.

5. For the `i128` / `u64` concern: rusqlite 0.39 disables `u64`/`usize` ToSql/FromSql by default. The `i128` feature (pinned in Task 1) re-enables them via wider-integer support. Audit `params!` sites in the port tasks (5-8 below) for any `u64`/`usize` binding — they will compile with `i128`, no explicit cast needed. If the feature were NOT enabled, each would need `as i64`. Ensure the pins stay.

**Testing:**

Unit tests in each domain-scalar-type file:

- Round-trip: construct value, bind to a memory-backed rusqlite Connection via an `INSERT` + `SELECT`, assert equal.
- Error paths: insert garbage bytes, assert `FromSqlError::Other` with a useful message.

Unit tests for each JSON-blob column type: round-trip a rich instance through INSERT + SELECT.

**Verification:**

Run: `cargo nextest run -p pattern_core --lib` (tests for domain types live in pattern_core)
Expected: all FromSql/ToSql round-trip tests pass.

**Commit:** `[pattern-core] [pattern-db] FromSql/ToSql impls for domain scalar types`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Port FTS5 + vector modules (`fts.rs`, `vector.rs`) with insta snapshot regression coverage

**Verifies:** v3-memory-rework.AC2.1, AC2.3 (FTS5 BM25 snapshots), AC2.4 (KNN ordering), AC2.9 (sqlite-vec spike test)

**Files:**
- Modify: `crates/pattern_db/src/fts.rs` (port all sqlx calls to rusqlite; 557 lines → probably similar after port)
- Modify: `crates/pattern_db/src/vector.rs` (port sqlx calls; re-wire sqlite-vec integration; 525 lines → similar)
- Create: `crates/pattern_db/src/vector/init.rs` (move the sqlite3_auto_extension registration here — sub-module of vector, scoped)
- Create: `crates/pattern_db/tests/sqlite_vec_smoke.rs` (the 100-vector KNN integration test AC2.9 calls for)
- Create: `crates/pattern_db/tests/snapshots/` (directory; insta creates .snap files here)
- Create: `crates/pattern_db/tests/fts5_regression.rs` (AC2.3 insta snapshot suite)
- Create: `crates/pattern_db/tests/vector_regression.rs` (AC2.4 insta snapshot suite for KNN ordering)

**Implementation:**

1. Port `fts.rs`:
   - Every `sqlx::query!` and `sqlx::query_as!` becomes `conn.prepare(sql)` followed by `stmt.query_map(params, |row| { ... from_row(row) ... })`.
   - Row structs gain inherent `fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self>` rather than deriving `FromRow`. Explicit column index mapping; this is intentionally boilerplate-heavy for auditability per the guidance doc.
   - BM25 SELECTs keep their `rank` column; ordering stays ascending (lower = better match in SQLite's sign convention).
   - `highlight()` and `snippet()` SQL-level calls stay verbatim — rusqlite passes them through transparently.

2. Port `vector.rs`:
   - `init_sqlite_vec` moves into the pool's `init_connection` (Task 2 already did this); the legacy function can stay as a thin wrapper that no-ops after first call, or delete it and update callers.
   - `CREATE VIRTUAL TABLE IF NOT EXISTS embeddings USING vec0(...)` stays identical.
   - KNN SELECT with `WHERE rowid IN (...)` syntax stays identical.
   - The `zerocopy` encoding for float vectors (FLOAT32[1536] → bytes) stays unchanged.

3. **FTS5 regression snapshots** (AC2.3):

   ```rust
   // tests/fts5_regression.rs
   #[test]
   fn bm25_scoring_matches_canonical_corpus() {
       let db = ConstellationDb::open_in_memory().unwrap();
       insert_canonical_corpus(&db);

       let conn = db.get().unwrap();
       let mut stmt = conn.prepare("
           SELECT m.id, m.content_preview, bm25(messages_fts) AS rank
           FROM messages_fts
           JOIN messages m ON messages_fts.rowid = m.rowid
           WHERE messages_fts MATCH ?1
           ORDER BY rank
           LIMIT 20
       ").unwrap();
       let results: Vec<(String, String, f64)> = stmt
           .query_map(params!["memory blocks"], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
           .unwrap()
           .collect::<Result<_, _>>()
           .unwrap();

       insta::assert_yaml_snapshot!(results);
   }
   ```

   `insert_canonical_corpus` seeds a fixed set of ~50 messages with known content. Snapshot is hand-reviewed on first run; future changes blocked by insta review.

4. **Vector KNN regression snapshots** (AC2.4): similar pattern. 100 synthetic vectors with a known nearest-neighbor structure (e.g., cluster around 3 centroids in 384-d space); assert nearest-k ordering snapshot.

5. **Sqlite-vec smoke test** (AC2.9): self-contained integration test at `tests/sqlite_vec_smoke.rs` that opens an in-memory db via `ConstellationDb::open_in_memory`, creates a vec0 virtual table with 384-dim floats, inserts 100 test vectors, runs a KNN query, asserts expected ordering. Design's pass criteria for the old 2a spike — lifted into a permanent regression test.

**Testing:**

Per above. Inline unit tests in `fts.rs`/`vector.rs` that already exist (7 + 6 tests) carry forward with rewritten bodies.

**Verification:**

Run: `cargo check -p pattern_db`
Expected: compiles.

Run: `cargo nextest run -p pattern_db --test sqlite_vec_smoke --test fts5_regression --test vector_regression`
Expected: all pass.

Run: `cargo nextest run -p pattern_db --lib`
Expected: inline FTS/vector tests pass.

Run: `cargo insta review`
Expected: on first run, accept the generated .snap files; subsequent runs require the snapshots to match exactly.

**Commit:** `[pattern-db] port FTS5 + vector modules to rusqlite; add BM25 + KNN regression snapshots + sqlite-vec smoke`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_A -->

**GATE (main-executor sign-off required):**

Before Subcomponent B, the main executor reviews the Subcomponent A output:

- `cargo check -p pattern_db` passes.
- `cargo nextest run -p pattern_db` passes every Subcomponent A test (pool stress, migrations roundtrip, sqlite-vec smoke, FTS5 regression, vector regression).
- FTS5 + KNN insta snapshots committed to the repo.
- `pattern_db/CLAUDE.md` freshened.

Open questions a reviewer asks:
- Does `init_connection` handle connection-recreation correctly (pool evicts → new connection → init fires again)? Evidence: re-run the pool stress test with a small `max_size=2` and many more concurrent callers to force eviction.
- Are the canonical corpus + synthetic vector fixtures stable across machines? Evidence: run on two different devices and compare .snap files.

If the gate passes, continue to Subcomponent B.

---

<!-- START_SUBCOMPONENT_B (tasks 6-9) -->

### Subcomponent B — sub-task 2b continued: Port 202 queries across `queries/*.rs`

Bulk mechanical work: convert every `sqlx::query!` / `sqlx::query_as!` / `sqlx::query_scalar!` + runtime variants to rusqlite. Split into four tasks so each is reviewable and commits atomically.

<!-- START_TASK_6 -->
### Task 6: Port `queries/memory.rs` (59 queries, 3 explicit transaction sites)

**Verifies:** v3-memory-rework.AC2.1, AC2.2 (existing tests pass), AC2.5 (transaction sites port atomically), AC2.6 (transaction rollback semantics)

**Files:**
- Modify: `crates/pattern_db/src/queries/memory.rs` (full port of 59 queries)
- Create: `crates/pattern_db/tests/transaction_atomicity.rs` (tests for AC2.6)

**Implementation:**

1. Port each query. Pattern:

   ```rust
   // sqlx version:
   let block = sqlx::query_as!(MemoryBlock, "SELECT ... WHERE id = ?", id)
       .fetch_one(&pool).await?;

   // rusqlite version:
   let block = conn.query_row(
       "SELECT ... WHERE id = ?1",
       rusqlite::params![id],
       MemoryBlock::from_row,
   )?;
   ```

   `MemoryBlock::from_row(row: &rusqlite::Row) -> rusqlite::Result<Self>` is an inherent method (not a derive).

2. Transaction sites port to `rusqlite::Transaction`:

   ```rust
   let tx = conn.transaction()?;
   tx.execute("UPDATE memory_blocks SET ...", params![...])?;
   tx.execute("INSERT INTO memory_block_updates ...", params![...])?;
   tx.commit()?;
   ```

   The three sites: `update_block_config` (3 queries), `consolidate_checkpoint` (4 queries), `store_update` (2 queries). Each retains its current query sequence verbatim — only the transaction wrapper changes.

3. `tests/transaction_atomicity.rs`:

   - Test: start a transaction, run the first query, inject a rusqlite error on the second, ensure `tx.commit()` is never reached → no changes visible post-block (AC2.6).
   - Test: happy path commits all queries in one tx; verify visibility.

**Testing:** Per above. Existing unit + integration tests for queries/memory.rs carry forward (port the bodies of those tests too).

**Verification:**

Run: `cargo nextest run -p pattern_db --lib queries::memory`
Run: `cargo nextest run -p pattern_db --test transaction_atomicity`
Expected: all pass.

**Commit:** `[pattern-db] port queries/memory.rs to rusqlite; preserve transaction atomicity (AC2.5, AC2.6)`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Port remaining queries/*.rs — batch 1: agent, coordination, message, folder

**Verifies:** v3-memory-rework.AC2.1, AC2.2

**Files:**
- Modify: `crates/pattern_db/src/queries/agent.rs` (28 queries)
- Modify: `crates/pattern_db/src/queries/coordination.rs` (24 queries)
- Modify: `crates/pattern_db/src/queries/message.rs` (19 queries; note these now write to `msg.messages` rather than `main.messages`)
- Modify: `crates/pattern_db/src/queries/folder.rs` (18 queries)

**Implementation:**

Same port pattern as Task 6. `message.rs` specifically updates table references from `messages` to `msg.messages` (and same for `queued_messages` → `msg.queued_messages`, `message_tombstones` → `msg.message_tombstones`). All existing tests (inline + integration) port their fixtures to use the new connection API; otherwise assertions are unchanged.

**Testing:** Existing tests.

**Verification:**

Run: `cargo nextest run -p pattern_db --lib queries::agent queries::coordination queries::message queries::folder`
Expected: passes.

**Commit:** `[pattern-db] port queries/{agent,coordination,message,folder}.rs to rusqlite`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Port remaining queries/*.rs — batch 2: event, task, source, atproto_endpoints, stats, queue

**Verifies:** v3-memory-rework.AC2.1, AC2.2

**Files:**
- Modify: `crates/pattern_db/src/queries/event.rs` (12 queries)
- Modify: `crates/pattern_db/src/queries/task.rs` (14 queries)
- Modify: `crates/pattern_db/src/queries/source.rs` (13 queries)
- Modify: `crates/pattern_db/src/queries/atproto_endpoints.rs` (5 queries)
- Modify: `crates/pattern_db/src/queries/stats.rs` (6 queries)
- Modify: `crates/pattern_db/src/queries/queue.rs` (4 queries; note: `queued_messages` table reference becomes `msg.queued_messages`)

**Implementation:** Per Task 7's pattern.

**Verification:**

Run: `cargo nextest run -p pattern_db --lib queries::event queries::task queries::source queries::atproto_endpoints queries::stats queries::queue`
Expected: passes.

**Commit:** `[pattern-db] port remaining queries/*.rs to rusqlite`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Port top-level `search.rs` + `lib.rs` + public surface; full-workspace check

**Verifies:** v3-memory-rework.AC2.1 (workspace clean), AC2.2 (all integration tests pass)

**Files:**
- Modify: `crates/pattern_db/src/search.rs` (702 lines — unified search orchestration)
- Modify: `crates/pattern_db/src/lib.rs` (re-exports; drop any sqlx-specific types)
- Create: `crates/pattern_db/tests/cross_db_query.rs` (AC2.10 explicit test for `main.X JOIN msg.Y`)
- Create: `crates/pattern_db/tests/pool_stress_20.rs` (AC2.7 explicit 20-caller stress test; scale up from the smaller one added in Task 2)

**Implementation:**

1. Port `search.rs` using the same patterns.
2. `cross_db_query` test: insert a memory_blocks row and a msg.messages row with related IDs; run a cross-DB JOIN; assert correct row returned.
3. Pool stress: 20 tokio tasks, each doing 50 `spawn_blocking` round-trips, asserts all complete within 30s wall clock.

**Testing:** Per above.

**Verification:**

Run: `cargo check --workspace`
Expected: clean.

Run: `cargo nextest run --workspace`
Expected: all tests pass (pattern_db, plus downstream crates that depend on it — nothing outside pattern_db should have broken, since consumers talk through `MemoryStore` trait which is unchanged in this phase).

Run: `cargo test --doc --workspace`
Expected: doctests pass.

**Commit:** `[pattern-db] port search.rs + public surface; add cross-DB join test + full pool stress`
<!-- END_TASK_9 -->

<!-- END_SUBCOMPONENT_B -->

**GATE (main-executor sign-off required):**

- `cargo check --workspace` clean.
- `cargo nextest run --workspace` green.
- Every `sqlx::` import removed from pattern_db (`grep -rn "sqlx" crates/pattern_db/src/` returns zero matches).
- All insta snapshots committed and stable.

Open questions a reviewer asks:
- Any warnings about `u64`/`usize` at `params!` sites? If yes, did the `i128` feature successfully suppress them, or did we need explicit `as i64` casts? Either is fine; document which one was needed.
- Does `cargo check --workspace` produce any `BlockType::Archival | BlockType::Log` non-exhaustive-match warnings? If yes, those are pre-existing — Subcomponent C removes the variants and the warnings collapse.

If the gate passes, continue to Subcomponent C.

---

<!-- START_SUBCOMPONENT_C (tasks 10-13) -->

### Subcomponent C — sub-task 2c: BlockType cleanup across call sites + data migration

Remove `BlockType::Archival` and `BlockType::Log` enum variants; migrate existing data. Compose pipeline is highest-risk: preserve tier exclusion semantics.

<!-- START_TASK_10 -->
### Task 10: Add Phase 2 schema migration + data conversion

**Verifies:** v3-memory-rework.AC3.3 (Log → Working + BlockSchema::Log), AC3.4 (Archival → archival_entries), AC3.5 (clear error on stale records)

**Files:**
- Create: `crates/pattern_db/migrations/memory/0014_collapse_block_types.sql`

**Implementation:**

```sql
-- Migration: collapse BlockType::Archival and BlockType::Log into Core/Working.
-- Archival rows → archival_entries (existing table from migration 0001).
-- Log rows → block_type = 'working' with block_schema updated to reflect Log semantics.

BEGIN;

-- 1. Copy archival-tier memory blocks into archival_entries.
INSERT INTO archival_entries (id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at)
SELECT
    id,
    agent_id,
    value AS content,                      -- memory_blocks column name is 'value'; verify at port
    COALESCE(metadata, '{}') AS metadata,
    0 AS chunk_index,
    NULL AS parent_entry_id,
    created_at
FROM memory_blocks
WHERE block_type = 'archival'
ON CONFLICT(id) DO NOTHING;                 -- idempotent re-run safe

-- 2. Delete the migrated archival rows from memory_blocks.
DELETE FROM memory_blocks WHERE block_type = 'archival';

-- 3. Reclassify log-tier blocks as working + log-schema.
UPDATE memory_blocks
SET block_type = 'working',
    block_schema = json_set(
        COALESCE(block_schema, '{}'),
        '$.kind', 'log'
    )
WHERE block_type = 'log';

-- 4. Guard rail: any remaining block_type outside {'core','working'} is illegal post-migration.
--    Enforced at application layer via the new BlockType enum (only two variants).
--    A belt-and-suspenders CHECK constraint isn't added because it would require
--    a table rebuild — the FromSql impl already fails loudly on unknown values.

COMMIT;
```

**Implementation notes:**

- The exact column name for "block content" on `memory_blocks` needs verification from the current schema (likely `value`, maybe `content`). The migration must reference the real name; implementor reads `migrations/memory/0001_initial.sql` to confirm before writing.
- `block_schema` is already stored as JSON TEXT; `json_set` preserves any existing schema fields while setting `kind` to `"log"`.
- Data loss audit: ANY archival memory_blocks row that can't fit the archival_entries schema (e.g., missing `agent_id` NOT NULL constraint) would fail the INSERT. Guard: a pre-migration audit query is included in the test for Task 13 — count archival rows before and after; counts must match `memory_blocks + archival_entries` totals.

**Testing:** Round-trip migration test (Task 13).

**Verification:**

Run: `cargo nextest run -p pattern_db --test migrations_roundtrip`
Expected: migration 0014 applies cleanly on a fixture with pre-Phase-2 data.

**Commit:** `[pattern-db] migration 0014: collapse BlockType::Archival → archival_entries; Log → Working+log-schema`
<!-- END_TASK_10 -->

<!-- START_TASK_11 -->
### Task 11: Remove `BlockType::Archival` + `BlockType::Log` variants; update enum and impls

**Verifies:** v3-memory-rework.AC3.1 (only Core + Working), AC3.2 (no stale references), AC3.5 (FromStr rejects old variants loudly)

**Files:**
- Modify: `crates/pattern_core/src/types/memory_types/core_types.rs` (remove `Archival` + `Log` variants from `BlockType`)
- Modify: `crates/pattern_core/src/types/memory_types/core_types.rs` (update `Display`, `FromStr`, any `From<MemoryBlockType>` or `From<BlockType>` impls)
- Modify: `crates/pattern_core/src/export/letta_convert.rs` (line 832 string match + line 933 variant match — Letta importer converts "archival"/"archive"/"long_term" strings to an ArchivalEntry insertion; "log" strings to Working+log-schema)
- Modify: `crates/pattern_core/src/export/tests.rs` (line 509 fixture — update to reflect new reality)
- Modify: `crates/pattern_runtime/src/session.rs` (line 730 `MemoryType::Archival => BlockType::Archival` — the MemoryType → BlockType translation collapses; MemoryType::Archival routes through the ArchivalEntry API instead of creating a memory_blocks row)
- Modify: `crates/pattern_runtime/src/agent_loop.rs` (lines 412-413 string match → update; line 699 filter becomes trivially-true — no tier exclusion needed now since archival/log aren't tiers)
- Modify: `crates/pattern_runtime/src/sdk/requests/memory.rs` (remove `BlockTypeReq::Archival` and `BlockTypeReq::Log` variants from the FromCore enum; the corresponding Haskell GADT constructors in the Pattern.Memory SDK are removed in Phase 3 — flag this here with a TODO pointing to Phase 3, but DO NOT add a // TODO comment per guidance; instead, write a concise port-list doc entry: `docs/plans/rewrite-v3-portlist.md` gains a line "BlockTypeReq::Archival/Log Haskell GADT constructors removed in v3-memory-rework Phase 3")
- Modify: `crates/pattern_runtime/src/sdk/handlers/memory.rs` (line 305 explicit `BlockType::Archival` construction — route through archival-entry API instead; the specific caller's intent must be read + replaced)
- Modify: `crates/pattern_cli/src/commands/builder/agent.rs` (line 1049) and `builder/group.rs` (line 1061) — update conversions
- Modify: `crates/pattern_cli/src/commands/debug.rs` (lines 359-360 — `log_blocks` and `archival_blocks` vecs. The debug command previously presented tier-filtered buckets; rework to present Core/Working + a separate ArchivalEntry listing. This is a user-visible CLI change; keep the output roughly equivalent so operators aren't confused.)

**Implementation:**

1. Update the enum:

   ```rust
   #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
   #[non_exhaustive]
   pub enum BlockType {
       Core,
       Working,
   }
   ```

2. Update `FromStr` to **reject** old string values loudly:

   ```rust
   impl FromStr for BlockType {
       type Err = BlockTypeParseError;
       fn from_str(s: &str) -> Result<Self, Self::Err> {
           match s {
               "core" => Ok(BlockType::Core),
               "working" => Ok(BlockType::Working),
               "archival" | "log" => Err(BlockTypeParseError::RemovedVariant(s.to_owned())),
               other => Err(BlockTypeParseError::Unknown(other.to_owned())),
           }
       }
   }

   #[derive(Debug, thiserror::Error)]
   #[non_exhaustive]
   pub enum BlockTypeParseError {
       #[error("block_type {0:?} was removed in v3-memory-rework; rows must be migrated via migration 0014_collapse_block_types.sql")]
       RemovedVariant(String),
       #[error("unknown block_type {0:?}")]
       Unknown(String),
   }
   ```

   AC3.5 verified by unit test: FromStr of `"archival"` or `"log"` returns `RemovedVariant` with a clear message pointing to the migrator.

3. For each consumer file, read the current match arms + string matches and collapse. Two patterns:
   - **Enum match with all variants**: remove `BlockType::Archival` + `BlockType::Log` arms; rely on `#[non_exhaustive]` so future variants don't silently break.
   - **String match on "archival"/"log"**: handlers route to ArchivalEntry / log-schema Working construction. Spec per file:
     - `letta_convert.rs:832`: `"archival" | "archive" | "long_term"` now constructs an `ArchivalEntry` (via a call into `MemoryStore::insert_archival` or equivalent). `"log"` constructs a Working block with `BlockSchema::Log`.
     - `letta_convert.rs:933`: enum-level variant match — remove the Archival and Log arms.
     - `agent_loop.rs:412-413`: same kind of rewrite — route old-string inputs to the new APIs; add a conversion comment in the import surface (docstring on the function) rather than inline comments.

4. **Compose pipeline cleanup** (pseudo_messages.rs, current_state.rs):

   ```rust
   fn render_block_type(bt: BlockType) -> &'static str {
       match bt {
           BlockType::Core => "core",
           BlockType::Working => "working",
       }
   }
   ```

   Because `BlockType` is `#[non_exhaustive]`, the match is exhaustive within the crate but prompts a note if a future variant is added. This is intentional per guidance.

   Current-state rendering code at `current_state.rs:112-120` (previously also rendered `archival` and `log`): update to render only Core + Working tiers from memory_blocks. Archival entries surface via a separate code path (they're already distinct — the pipeline already has an "archival" collection if it's being rendered at all; confirm via grep at implementation time).

5. **Agent loop tier filter** at `agent_loop.rs:699`:

   Previously:
   ```rust
   BlockType::Archival | BlockType::Log => false,
   BlockType::Core | BlockType::Working => true,
   ```

   Becomes:
   ```rust
   BlockType::Core | BlockType::Working => true,
   ```

   Trivially-true — consider collapsing the entire `matches!` check and documenting inline why it was there historically. The code reviewer in finalization will decide whether to fully remove or preserve as a future extension point.

**Testing:**

- Unit test on `BlockType::from_str("archival")` returns `Err(BlockTypeParseError::RemovedVariant("archival".into()))` (AC3.5).
- Compose pipeline snapshot tests continue to pass without regression — re-run the existing snapshot suite; any changed output is reviewed and explicitly accepted (expected: no changes, since the removed variants never appeared in the segment-3 output anyway).

**Verification:**

Run: `cargo check --workspace 2>&1 | grep -iE "archival|log" | grep -iE "warning|error"`
Expected: no matches — no stale references.

Run: `cargo nextest run --workspace`
Expected: all tests pass.

Run: `grep -rn "BlockType::Archival\|BlockType::Log" crates/ --include="*.rs"`
Expected: zero matches.

**Commit:** `[pattern-core] [pattern-runtime] [pattern-provider] [pattern-cli] remove BlockType::Archival and BlockType::Log variants; route legacy strings through archival_entries / log-schema APIs`
<!-- END_TASK_11 -->

<!-- START_TASK_12 -->
### Task 12: Compose pipeline snapshot regression

**Verifies:** v3-memory-rework.AC3.6 (Log-schema can live on either Core or Working), compose pipeline preserved

**Files:**
- Modify / create: snapshot tests in `pattern_provider` covering `pseudo_messages.rs` and `current_state.rs` rendering — likely tests exist; audit + add insta snapshots if absent
- Create: `crates/pattern_provider/tests/compose_segment3_regression.rs` if no integration test already covers this

**Implementation:**

1. Capture a representative constellation state (fixtures: persona + project agents, a handful of core + working blocks, at least one log-schema working block, at least one archival entry).
2. Render the full segment-3 compose output.
3. Commit as an insta snapshot.
4. AC3.6 verification: set up a fixture with the SAME block content loaded once at Core and once at Working; both render correctly with log-schema formatting. Snapshot.

**Testing:** Per above.

**Verification:**

Run: `cargo nextest run -p pattern_provider --test compose_segment3_regression`
Expected: passes; insta snapshots accepted on first run, unchanged thereafter.

**Commit:** `[pattern-provider] compose pipeline snapshot regression for segment-3 rendering`
<!-- END_TASK_12 -->

<!-- START_TASK_13 -->
### Task 13: Migration 0014 round-trip test + full workspace green

**Verifies:** v3-memory-rework.AC3.3, AC3.4, AC3.5

**Files:**
- Modify: `crates/pattern_db/tests/migrations_roundtrip.rs` (extend to cover 0014 scenarios)

**Implementation:**

Three test cases in `migrations_roundtrip.rs`:

1. **Pre-Phase-2 fixture → migration 0014 → post-state**: seed a memory.db at migration 0013 (sqlx era) with several memory_blocks rows in each of the 4 old block_type values (core, working, archival, log). Run migrations to 0014. Assert:
   - `SELECT COUNT(*) FROM memory_blocks WHERE block_type = 'archival'` = 0.
   - `SELECT COUNT(*) FROM memory_blocks WHERE block_type = 'log'` = 0.
   - `SELECT COUNT(*) FROM archival_entries` equals the pre-migration archival memory_blocks count.
   - The old Log-typed blocks are now Working-typed with `json_extract(block_schema, '$.kind') = 'log'`.

2. **AC3.5 confirmation via live rusqlite**: after migration, artificially INSERT a row with `block_type = 'archival'` (simulating a corrupt record somehow surviving migration). Attempt to load via the MemoryStore surface; assert the load fails with a clear `BlockTypeParseError::RemovedVariant` error — not a silent decode.

3. **AC3.3, AC3.4 total-count invariant**: pre-migration total (memory_blocks + archival_entries) equals post-migration total (memory_blocks + archival_entries). No data loss.

**Testing:** Per above.

**Verification:**

Run: `cargo nextest run -p pattern_db --test migrations_roundtrip`
Expected: all three cases pass.

Run: `cargo check --workspace && cargo nextest run --workspace && cargo test --doc --workspace`
Expected: all green.

**Commit:** `[pattern-db] migration 0014 round-trip test + stale-record error surfacing`
<!-- END_TASK_13 -->

<!-- END_SUBCOMPONENT_C -->

**GATE (main-executor sign-off required):**

- `cargo check --workspace` clean, zero warnings about removed BlockType variants.
- `cargo nextest run --workspace` green across all crates.
- Migration 0014 round-trip test passes.
- Compose pipeline insta snapshots committed.

Open questions a reviewer asks:
- Did the agent_loop tier-filter collapse change observed agent behavior? Evidence: behavioral regression test or structured review of call sites.
- Were there any unexpected BlockType string references (e.g., hardcoded in config files, test fixtures, log messages)? Evidence: `grep -rn 'archival\|"log"' crates/ docs/` and audit.

If the gate passes, Phase 2 is done.

---

## Phase 2 Done-when recap

- `cargo check --workspace` clean (AC2.1, AC3.1, AC3.2).
- `cargo nextest run --workspace` green, including pre-existing pattern_db integration tests ported verbatim in assertion logic (AC2.2).
- FTS5 BM25 insta snapshots stable (AC2.3); vector KNN insta snapshots stable (AC2.4).
- `rusqlite::Transaction` wraps the three memory.rs transaction sites with rollback semantics proven (AC2.5, AC2.6).
- 20-concurrent-caller pool stress test passes without deadlock or pool exhaustion (AC2.7).
- `libsqlite3-sys` direct pin gone from `pattern_db/Cargo.toml` (AC2.8).
- `sqlite_vec_smoke.rs` test passes (AC2.9).
- `messages.db` split succeeds; `init_connection` ATTACHes it; cross-DB `main.X JOIN msg.Y` works (AC2.10).
- Migration 0014 applies cleanly; archival rows → archival_entries; log rows → Working + log-schema (AC3.3, AC3.4).
- FromStr rejects `"archival"` / `"log"` with a clear typed error pointing to the migrator (AC3.5).
- Log-schema blocks loadable in either Core or Working tier (AC3.6).

## Notes for downstream phases

- **Phase 3** depends on: pool + dedicated-connection surface from this phase, sync-friendly ConstellationDb surface, the `BlockTypeReq::Archival/Log` Haskell GADT removal (recorded in port-list doc during Task 11; actually executed in Phase 3).
- **Phase 4** depends on: pattern_memory is still using pattern_db as its DB layer. The sync_worker subscribers borrow pool connections per work unit; this Phase's pool + init_connection are the foundation. The FTS5 row-update queries the subscriber fires are rusqlite-native.
- **Phase 7** depends on: rusqlite's `backup` API (included in `bundled-full`, confirmed in this phase). Messages.db path configured per mode (Mode A: `~/.pattern/transient/<project-hash>/`; Mode B/C: `~/.pattern/projects/<id>/messages/`). Phase 6 formalizes those paths; this phase's `ConstellationDb::open` takes the paths as arguments so the transition is smooth.
- **CI**: the workspace-wide `cargo nextest run` in Phase 8's capstone depends on Phase 2's green state as the foundation. Any regression introduced by Phases 3-8 that breaks pattern_db's tests reproduces here.
