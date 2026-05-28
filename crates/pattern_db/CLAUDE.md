# CLAUDE.md - Pattern Constellation database

Last verified: 2026-04-24

Main datastore for Pattern constellations.

## Purpose

This crate owns two per-constellation SQLite databases:

- **memory.db** - agents, memory blocks, archival entries, coordination, tasks, events, folders, data sources (12 migrations)
- **messages.db** - messages, queued messages, message tombstones (1 migration; attached as `msg` schema via `ATTACH DATABASE`)

## Stack

- **rusqlite 0.39** (`bundled-full`) for synchronous SQLite access (replaced sqlx in v3-memory-rework Phase 2).
- **r2d2 / r2d2_sqlite** for connection pooling.
- **rusqlite_migration 2.5** for schema migrations.
- **sqlite-vec 0.1.9** for vector search (registered process-global via `sqlite3_auto_extension`).
- **jiff** for message timestamp handling (`jiff::Timestamp` stored as RFC 3339 text).

## Conventions

- Queries use `rusqlite::Connection::prepare` with inherent `fn from_row` on each row struct (no derive macros, no helper trait - explicit and auditable).
- Migrations live in `migrations/memory/` and `migrations/messages/`, applied by `rusqlite_migration 2.5`.
- No compile-time query macro; no `.sqlx/` cache.
- SQL type conversions (`FromSql`/`ToSql` impls) live in `sql_types.rs`.
- The `json_wrapper` module provides a `Json<T>` wrapper for serde-based JSON columns.

## Key decisions

- **rusqlite over sqlx**: Sync API matches the desynced `MemoryStore` trait. Eliminates compile-time macro overhead. All 202 queries ported in Phase 2.
- **BlockType collapse (migration 0010)**: `Archival` and `Log` block types removed. Archival entries use `archival_entries` table; log blocks use `Working` type with `log-schema` schema.
- **Skill usage stats are sqlite-only (migration 0012)**: Per-local-install observability (`last_used`, `last_used_by`, `use_count`) lives in `skill_usage_stats` (WITHOUT ROWID, keyed on `block_handle`). Never replicated via CRDT. This keeps the canonical `.md` content-hash stable across load events. Query surface: `queries::skill_usage::{record_usage, get_usage_stats, get_usage_stats_batch}`.

## Notable migrations

- `0011_task_block_index.sql` — `tasks`, `task_edges`, `tasks_fts` tables for TaskList block indexing.
- `0012_skill_usage_stats.sql` — `skill_usage_stats` WITHOUT ROWID table for per-install skill load observability.

## Testing

```bash
cargo nextest run -p pattern-db
```

Notable test suites: `transaction_atomicity`, `cross_db_query`, `migrations_roundtrip`, `fts5_regression`, `vector_regression`.
