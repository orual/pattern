# sqlx → rusqlite migration evaluation

**Date:** 2026-04-18
**Phase:** 6 (design task D)
**Status:** Decision pending — recommendation inside

## Background

The eval worker (`pattern_runtime/src/agent_loop/eval_worker.rs`) spawns a
multi-thread tokio runtime with two worker threads solely so that async `sqlx`
calls inside effect handlers can be bridged back to the synchronous
`compile_and_run` execution context via `Handle::current().block_on(...)`. The
`block_in_place` call around `run_eval` relocates other tasks off the tokio
worker thread before blocking it, sidestepping the "cannot start a runtime from
within a runtime" panic.

This is functional but architecturally uncomfortable: we're carrying a ~2+
thread tokio runtime per open session purely to drive blocking SQLite calls
through an async interface. The question is whether dropping that interface
in favour of `rusqlite` (synchronous) would justify the migration cost.

---

## 1. Current state analysis

### sqlx callsite surface

The `pattern_db` crate is the entire sqlx boundary. A rough audit:

| Query file | `sqlx` macro invocations |
|---|---|
| `queries/memory.rs` | ~110 |
| `queries/message.rs` | ~40 |
| `queries/agent.rs` | ~57 |
| `queries/coordination.rs` | ~49 |
| `queries/folder.rs` | ~37 |
| `queries/task.rs` | ~25 |
| `queries/event.rs` | ~24 |
| `queries/source.rs` | ~27 |
| `queries/queue.rs` | ~9 |
| `queries/stats.rs` | ~9 |
| `queries/atproto_endpoints.rs` | ~11 |
| `fts.rs`, `vector.rs`, `search.rs` | ~29 |
| **Total** | **≈ 427 macro-verified queries** |

All queries use `sqlx::query!` or `sqlx::query_as!` macros, which are
compile-time verified against an offline `.sqlx/` cache. Every query is `async`
and takes a `&SqlitePool`.

The `MemoryStore` trait (`pattern_core/src/traits/memory_store.rs`) exposes 28
`async` methods (plus one sync `mark_dirty`). The canonical implementation,
`MemoryCache` in `pattern_core/src/memory/cache.rs`, delegates to these queries.

### Effect handler `block_on` sites

Every handler that touches `MemoryStore` bridges the async/sync boundary via
`Handle::current().block_on(...)`. A direct count from the source:

- `handlers/memory.rs`: 12 `block_on` calls (covering `Memory.Get`, `Memory.Put`,
  `Memory.Append`, `Memory.Archive`, `Memory.LoadFromArchival`, `Memory.Swap`,
  `Memory.Create`, plus helper calls)
- `handlers/recall.rs`: 5 `block_on` calls (`Recall.Insert`, `Recall.Search`,
  `Recall.Get`, `Recall.Delete`, and scope resolution)
- `handlers/search.rs`: 3 `block_on` calls (`Search.*` scoped variants plus scope
  resolution)
- `handlers/message.rs`: 2 `block_on` calls (message routing)

That's **22 `Handle::current().block_on` call sites** in production handler code,
all of which exist purely because the handlers run synchronously inside
`compile_and_run` but need to drive async store operations.

### Which callers are async-native vs. sync-preferring

**Async-native callers** — have a running tokio runtime and want `await`:

- `pattern_cli` commands: 33-34 `.await` usages per command file. Thin wrappers
  over tokio async code; CLI entry points run inside `tokio::main`.
- `pattern_discord`: minimal `MemoryStore` surface so far (one import in
  `slash_commands.rs`), but Discord bots are event-driven async by nature.
- `pattern_server`: currently does not touch `MemoryStore` directly (no
  occurrences found in `src/`), though it runs on axum which is async-first.
- `pattern_runtime` — the agent loop itself (`orchestrate`, `drive_step`) is
  async and uses `.await` throughout. Only the eval worker's _inner_ path
  (`run_eval`) is synchronous.

**Sync-preferring callers** — run inside `compile_and_run`, which is blocking:

- `EvalWorker::run_eval` — the entire handler dispatch tree during a Haskell
  eval. This is the motivating case. The 22 `block_on` sites listed above all
  exist here.

The division is clean: one synchronous island (eval worker) inside an otherwise
async sea.

### Migration surface summary

To fully migrate `pattern_db` to rusqlite and make `MemoryStore` sync:

- Rewrite ~427 sqlx macro queries in `pattern_db` to rusqlite statements.
  The `query!`/`query_as!` macro annotations are non-trivial: they annotate
  column nullability and enum casting that would need to be re-expressed via
  rusqlite's `FromRow`-equivalent plus manual type coercions.
- Rework `ConstellationDb` (connection management): sqlx provides
  `SqlitePool` with built-in connection pooling; rusqlite uses a single
  `Connection` or requires `r2d2`/`deadpool-sqlite` for pooling.
- Remove `async_trait` from `MemoryStore`; all 28 async methods become sync.
- Update `MemoryCache` (the concrete store implementation in `pattern_core`).
- All async callers (`pattern_cli`, `pattern_discord`) would need `spawn_blocking`
  or `block_on` wrappers at each call site to bridge back to async.
- The sqlx offline query cache (`.sqlx/` directory) and `sqlx prepare` workflow
  in `pattern_db/CLAUDE.md` would be replaced by rusqlite's approach (no
  compile-time verification; just runtime errors).
- `sqlite-vec` integration: currently uses `sqlite3_auto_extension` via
  `libsqlite3-sys` pinned to match sqlx's bundled SQLite. With rusqlite,
  the extension registration path exists but needs re-validation — rusqlite
  exposes `Connection::load_extension` and the `rusqlite` crate's `bundled`
  feature bundles its own SQLite, creating the same version-coordination
  concern as today.
- FTS5 queries currently leverage sqlx typed rows; rusqlite would need manual
  deserialization for the BM25-scored results in `fts.rs` and hybrid score
  fusion in `search.rs`.

---

## 2. Proposed sync `MemoryStore` trait shape

Under the migration, the trait would become:

**Before (current):**

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String>;

    // ... 26 more async methods
    fn mark_dirty(&self, agent_id: &str, label: &str); // already sync
}
```

**After:**

```rust
pub trait MemoryStore: Send + Sync + fmt::Debug {
    fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;

    fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String>;

    // ... 26 more sync methods
    fn mark_dirty(&self, agent_id: &str, label: &str); // unchanged
}
```

`async_trait` disappears; `async_trait` is no longer a dependency of
`pattern_core`. The `MemoryResult<T>` type is unchanged; errors remain the same.

The eval worker handlers (`handlers/memory.rs`, `handlers/recall.rs`,
`handlers/search.rs`) shed all their `Handle::current().block_on(...)` calls
and call methods directly. `run_eval` becomes simpler.

Methods that would **stay sync** under this model: all 28 trait methods. The
`mark_dirty` method already is.

Methods that would need **async wrappers at async callsites**: all 28. Every
`pattern_cli` command that awaits a store method would instead call
`tokio::task::spawn_blocking(|| store.get_block(...))`.await. This is the
principal callsite impact discussed in section 3.

---

## 3. Callsite impact assessment

### pattern_core (trait consumer + concrete impl)

`MemoryCache` implements `MemoryStore` and would need to swap all internal sqlx
async calls for rusqlite sync calls. The implementation is substantial —
`memory/cache.rs` manages an Arc-shared LoroDoc cache layered over the DB.

The LoroDoc cache itself is in-memory and already sync (DashMap); only the DB
persistence calls change. This is a mechanical rewrite but a large one: every
`db::queries::memory::*` call in `MemoryCache` becomes a rusqlite call, with
manual row mapping replacing the `query_as!` macro annotations.

`pattern_core` would drop its transitive dependency on `tokio` (currently pulled
in through sqlx). This is a modest win: the core traits crate becomes truly
sync, which is architecturally cleaner.

**Verdict:** large mechanical effort; no new concepts; no design risk.

### pattern_runtime eval_worker (the motivating case)

The clear winner. `run_eval` drops the multi-thread tokio runtime entirely.

```rust
// Before: tokio runtime + block_in_place
let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_all()
    .build()?;
rt.block_on(async move {
    while let Some(req) = rx.recv().await {
        let outcome = tokio::task::block_in_place(|| run_eval(...));
        ...
    }
});

// After: plain std::sync::mpsc
let (tx, rx) = std::sync::mpsc::channel::<EvalRequest>();
while let Ok(req) = rx.recv() {
    let outcome = run_eval(...); // just calls sync store methods
    let _ = req.reply.send(outcome);
}
```

The eval worker becomes a plain OS thread with no tokio dependency. The 22
`Handle::current().block_on` calls in handlers disappear. The docstring warning
about deadlock risk from "cannot start a runtime from within a runtime" goes
away.

Thread count per session drops from `1 (worker) + 2 (tokio workers) + N (tokio
blocking pool)` to `1 (worker)`. For a constellation with, say, 5 concurrently
active sessions, this is a meaningful reduction: potentially 15+ threads fewer,
with no risk of starvation between tokio's blocking pool and the eval workers.

The reply channel would also shift: currently uses `tokio::sync::oneshot`
(requires an async runtime to await). Under the sync model, `EvalDispatcher`
itself remains `async` at its interface (because callers like `orchestrate` are
async), but the inner `run_eval` no longer needs tokio. The channel pattern
becomes: `dispatch` spawns the request onto the sync `std::sync::mpsc` channel
and uses `tokio::sync::oneshot` for the reply (the reply future can still be
awaited from async callers; only the worker side is sync). This hybrid is
standard and well-supported.

**Verdict:** this is the clear beneficiary. The simplification is substantial
and directly addresses the documented tech debt in `eval_worker.rs`.

### pattern_runtime other handlers (memory/search/recall effect handlers)

These live entirely inside `run_eval`'s synchronous context. They benefit
fully from the migration: all 22 `block_on` call sites become direct method
calls. The handler code becomes shorter and more readable. No async scaffolding
needed.

The scope resolver (`handlers/scope.rs`) also uses `block_on` for the
`has_shared_blocks_with` and `shares_group_with` checks; these would also
become direct calls.

**Verdict:** pure improvement within the eval path.

### pattern_runtime other code (orchestrate, drive_step, agent loop)

The agent loop outside `run_eval` does not call `MemoryStore` directly during
provider interaction — it uses `MemoryStoreAdapter` at turn boundaries
(draining writes, building snapshot attachments). The adapter's `record_write`
is already sync. The `persist_block` and `create_block` calls at turn close
would need to become `spawn_blocking(...).await` wrappers, since that code is
async-native.

The `MemoryStoreAdapter` itself would need updating: it delegates to
`Arc<dyn MemoryStore>` and currently implements `MemoryStore` with all-async
methods forwarded to `self.inner`. Post-migration those forwarded calls are
sync, so the adapter's `#[async_trait] impl MemoryStore` body becomes trivially
sync-wrapping-sync — except at turn boundary code that legitimately lives in
async context. Those sites would use `spawn_blocking`.

**Verdict:** a few `spawn_blocking` wrappers needed at turn-boundary sites
(persist, compaction decisions); manageable but not zero effort.

### pattern_server

Currently no `MemoryStore` callsites found in `pattern_server/src/`. If it
eventually gains them (API endpoints for memory inspection), they would live
in axum handlers (async), so would need `spawn_blocking` wrappers. Not a
concern today.

**Verdict:** zero immediate impact.

### pattern_discord

Minimal current exposure (one import in `slash_commands.rs`). Discord event
handlers are async (`serenity`/`poise` based); any memory access would need
`spawn_blocking`. Acceptable and idiomatic — Discord events don't need the
microsecond latency of a direct call.

**Verdict:** minor future friction; not a blocker.

### pattern_cli

Currently 33-34 `.await` usages per command file, with `MemoryStore` calls
scattered throughout the agent and group command implementations. Each awaited
store call would become:

```rust
// Before
let block = store.get_block(agent_id, label).await?;

// After
let block = tokio::task::spawn_blocking({
    let store = store.clone();
    move || store.get_block(&agent_id, &label)
}).await??;
```

Or, since the CLI is single-user and the main concern is not throughput but
simplicity, the CLI could instead use a `block_on` wrapper — acceptable in a
CLI context where there's no risk of reactor starvation.

There are approximately 40-50 `MemoryStore` call sites in the CLI (conservatively
estimated from the `MemoryCache` usage pattern across agent.rs, group.rs,
builder/group.rs). Each needs updating.

**Verdict:** mechanical but non-trivial effort. The CLI already carries the
complexity; this shifts it from "async internally" to "sync internally, bridged
at the tokio boundary". The net complexity is similar but the topology changes.

---

## 4. Alternatives considered

### (a) Full migration to rusqlite

Migrate `pattern_db` entirely: replace all sqlx queries with rusqlite, make
`MemoryStore` sync, drop the multi-thread tokio runtime from the eval worker.

**Pros:**
- Eval worker becomes dramatically simpler.
- 22 `block_on` hacks eliminated.
- `pattern_core` drops tokio as a transitive dependency.
- SQLite is genuinely synchronous; the async wrapper is architectural fiction.
- Connection management becomes explicit rather than hidden in a pool.
- Matches the actual workload: agent evals are single-threaded Haskell, one
  eval per session at a time; pool concurrency buys nothing for the eval path.

**Cons:**
- ~427 queries rewritten without compile-time verification (sqlx's killer
  feature). rusqlite has no `query!` macro; type safety becomes runtime.
- `sqlite-vec` and FTS5 integration need re-validation under rusqlite.
- Connection pooling must be explicit (via `r2d2-sqlite` or similar) for
  async callers; another dependency.
- async callsites (`pattern_cli`, turn-boundary code) all need
  `spawn_blocking` wrappers.
- Migration churn is very high: ~427 queries + 28 trait methods + adapter +
  all async callsites. Risk of introducing subtle regressions in type coercions.

### (b) Keep sqlx, drive it differently

Keep the async `MemoryStore` trait and sqlx as-is. Reduce the runtime overhead
by replacing the per-session multi-thread runtime with a shared application-level
tokio runtime. All eval workers block_on the shared runtime rather than spinning
up their own.

This avoids query rewrites and preserves compile-time SQL verification, but
does not eliminate the fundamental `block_on` awkwardness. It also introduces
shared-runtime contention: a blocked eval thread on runtime A that spawns
blocking tasks can starve other sessions if the shared pool is small. The tokio
documentation advises against running multiple tokio runtimes concurrently (it's
not prohibited but risks signal handler conflicts and poor resource partitioning).

A shared runtime would reduce per-session thread count but not eliminate the
`block_in_place` / `block_on` complexity. The architectural friction remains.

**Verdict:** reduces overhead without reducing complexity. Not recommended as the
permanent state.

### (c) Dual-trait approach: sync store inside eval worker only

Define a `SyncMemoryStore` trait alongside `MemoryStore`. The eval worker
handlers would use `SyncMemoryStore`; all other callers use the existing async
`MemoryStore`. A concrete implementation (`MemoryCacheSync`) would wrap the
rusqlite backend for the sync path; `MemoryCache` would retain the sqlx backend
for the async path.

This sounds like it contains the migration but actually duplicates the entire
store surface: two traits, two backends, two sets of query implementations.
Any schema change touches both. The maintenance burden doubles. The performance
pressure motivating the sync path (SQLite is blocking anyway) applies equally
to the async path; running two separate implementations of the same queries
is difficult to justify.

Additionally, it creates a correctness hazard: the sync and async implementations
could diverge in subtle ways (different transaction semantics, different error
handling, different JSON coercions) that are hard to catch in testing.

**Verdict:** appealing on paper, fragile in practice. Not recommended.

### (d) Status quo — accept the tokio-in-eval-worker as permanent

Document the multi-thread tokio runtime in `EvalWorker` as the accepted
architectural pattern and stop treating it as tech debt. The code works; the
runtime overhead is bounded (~2 extra threads + a blocking pool, per session);
the `block_in_place` pattern is documented with explanation.

This is the honest choice when the migration cost is high and the runtime cost
is low. With modest constellation sizes (a few active sessions concurrently),
the thread overhead is not material — it is roughly equivalent to running a
small background service.

The cost is architectural honesty: we are doing async-to-sync bridging that
exists only because `sqlx` requires it, not because the operations benefit from
async execution. This violates the principle that the type system should
encode correct constraints. A `MemoryStore` trait that is `async` implies that
implementations are non-blocking; `sqlx`'s SQLite backend is in fact blocking
under a `spawn_blocking` wrapper, so the async signature is a polite fiction.

---

## 5. Recommendation

**Migrate — but not yet. Commit to rusqlite as the target; schedule it as a
dedicated Phase 7 task (not part of Phase 6).**

The case for migration is sound: SQLite is fundamentally synchronous, the async
wrapper is overhead without benefit, and the eval worker's multi-thread tokio
runtime is the most visible symptom of the mismatch. Eliminating 22
`Handle::current().block_on` call sites and the per-session tokio runtime is a
meaningful simplification.

The case against migrating _now_ is practical: 427 queries rewritten without
compile-time verification is a high-risk, high-churn operation. The existing
query suite is correct and well-tested; introducing rusqlite row mapping
manually re-opens every type coercion question that `query_as!` macros close.
FTS5 and `sqlite-vec` integration under rusqlite needs explicit validation
before committing to the approach. And the Phase 6 priority is smoke-test
completeness, not storage layer refactoring.

**Immediate action:** add a `// TECH-DEBT: phase-7` comment in `eval_worker.rs`
documenting that the multi-thread tokio runtime is a consequence of async sqlx
and will be eliminated when `pattern_db` migrates to rusqlite. This prevents
the pattern from being copied elsewhere and keeps the intent visible.

**Phase 7 scoping criteria** (a separate implementation plan, not this doc):
- Validate `sqlite-vec` registration under rusqlite's `bundled` feature
- Validate FTS5 query behaviour (BM25 scoring, `highlight()`, `snippet()`)
- Confirm `r2d2-sqlite` or equivalent pooling satisfies async callers
- Estimate per-query migration cost across a representative sample (~10%)
- Draft the incremental migration order (start with `queries/memory.rs` since
  it is the most exercised path; validate with existing tests before proceeding)

**Accept the tokio-in-eval-worker for Phase 6.** It is documented, bounded,
and not a correctness issue. Phase 6 smoke-test work should not be blocked on
storage layer refactoring.

---

## 6. Open questions and risks

### FTS5 under rusqlite

The `fts.rs` and `search.rs` modules use `sqlx`'s typed row mapping with
custom enum casts for `FtsContentType` and score fusion. rusqlite exposes
FTS5 via the same SQL surface, but row deserialization is manual. The
`highlight()` and `snippet()` FTS5 auxiliary functions are well-supported in
SQLite; the concern is not availability but the tedium of rewriting the typed
result mapping without compile-time checking. A migration plan should validate
a representative FTS5 query early to surface any surprises.

### Connection pooling

`sqlx`'s `SqlitePool` provides connection pooling with per-connection WAL-mode
enforcement and connection lifecycle management. rusqlite's `Connection` is
single-connection; async callers would need `r2d2-sqlite` (sync, thread-pool
based) or `deadpool-sqlite` (async-aware wrapper) to avoid serializing all
operations through a single lock. The eval worker path (which would become
single-threaded) needs no pooling, but async callers (`pattern_cli`,
turn-boundary code) do. This is a solvable dependency problem but must be
validated before committing to the migration.

### Migration tooling and sqlx prepare

`pattern_db`'s `CLAUDE.md` documents a strict `sqlx prepare` workflow for
keeping the offline query cache consistent with the schema. rusqlite has no
equivalent: there is no offline verification step, so regressions from schema
drifts or type mismatches surface at runtime rather than compile time. This is
a meaningful regression in the development feedback loop. A post-migration
test harness that exercises every query against a fresh in-memory SQLite
database (similar to the existing integration tests) would partially compensate,
but it is not equivalent to compile-time verification.

### Transaction semantics

The current codebase uses explicit transactions in several places
(`store_update`, `consolidate_checkpoint`, `update_block_config`). sqlx's
transaction API returns a `Transaction<'_, Sqlite>` that implements `Executor`;
the async `begin`/`commit`/`rollback` pattern is idiomatic. rusqlite's
transaction API is synchronous and similar in structure, but the migration must
ensure that every multi-statement operation that currently uses a transaction
is correctly mapped — losing a transaction boundary would introduce atomicity
bugs that could corrupt Loro CRDT state or the update sequence counter.

### sqlite-vec version pinning

`pattern_db/Cargo.toml` pins `libsqlite3-sys = "=0.30.1"` to match sqlx's
bundled SQLite version, which is required for `sqlite3_auto_extension` to
register `sqlite-vec` globally. Under rusqlite with its own bundled SQLite,
the version to pin against would change. If rusqlite's bundled SQLite version
differs from `sqlite-vec`'s tested version, the extension registration may
fail at runtime. This pin needs re-validation as part of any migration attempt.

### The `query!`-macro investment

The 427 sqlx queries use `query!` and `query_as!` macros, which encode
nullability and type information that was presumably validated against the live
schema at the time each query was written. Rewriting these by hand creates
an opportunity to introduce type errors that the current macro verification
would have caught. The per-query risk is small but the aggregate risk across
427 queries is non-trivial. The migration plan should include a query-level
regression test for each module before proceeding to the next.
