# CLAUDE.md - Pattern Constellation database

Updated 2026-04-19 in v3-memory-rework Phase 2.

Main datastore for Pattern constellations.

## Purpose

This crate owns two per-constellation SQLite databases:

- **memory.db** - agents, memory blocks, archival entries, coordination, tasks, events, folders, data sources
- **messages.db** - messages, queued messages, message tombstones (attached via `ATTACH DATABASE ... AS msg`)

## Stack

- **rusqlite 0.39** (`bundled-full`) for synchronous SQLite access.
- **r2d2 / r2d2_sqlite** for connection pooling.
- **rusqlite_migration 2.5** for schema migrations.
- **sqlite-vec 0.1.9** for vector search (registered process-global via `sqlite3_auto_extension`).

## Conventions

- Always use the `rust-coding-style` skill.
- Queries use `rusqlite::Connection::prepare` with inherent `fn from_row` on each row struct (no derive macros, no helper trait - explicit and auditable).
- Migrations live in `migrations/memory/` and `migrations/messages/`, applied by `rusqlite_migration 2.5`.
- No compile-time query macro; no `.sqlx/` cache.

## Testing

```bash
cargo nextest run -p pattern-db
```
