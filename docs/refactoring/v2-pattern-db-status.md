# Pattern v2: pattern_db Implementation Status

## Overview

`pattern_db` is the new SQLite-based storage backend for Pattern v2, replacing the SurrealDB-based system. It uses sqlx with compile-time query checking.

**Current State**: Core implementation complete, ready for integration.

## What's Implemented

### Crate Structure

```
crates/pattern_db/
├── src/
│   ├── lib.rs              # Public exports
│   ├── connection.rs       # ConstellationDb, pool management
│   ├── error.rs            # DbError, DbResult types
│   ├── models/
│   │   ├── mod.rs          # Re-exports
│   │   ├── agent.rs        # Agent, AgentGroup, GroupMember, AgentStatus
│   │   ├── memory.rs       # MemoryBlock, MemoryBlockUpdate, BlockType, etc.
│   │   ├── message.rs      # Message, ArchiveSummary, MessageRole
│   │   └── coordination.rs # ActivityEvent, AgentSummary, ConstellationSummary, etc.
│   └── queries/
│       ├── mod.rs          # Re-exports
│       ├── agent.rs        # CRUD for agents, groups, members
│       ├── memory.rs       # CRUD for memory blocks
│       ├── message.rs      # Message queries with batching
│       └── coordination.rs # Activity events, summaries, tasks, handoffs
├── migrations/
│   └── 0001_initial.sql    # Full schema creation
├── .env                    # DATABASE_URL for dev
├── .gitignore              # Ignores dev.db
└── Cargo.toml
```

### Models

All models use `sqlx::FromRow` with proper type mappings:

| Model | Key Features |
|-------|--------------|
| `Agent` | JSON fields via `sqlx::types::Json<T>` for config, tools, rules |
| `AgentGroup` | Coordination patterns stored as JSON |
| `MemoryBlock` | Loro snapshots as `Vec<u8>` blobs |
| `Message` | Snowflake ID ordering, batch tracking, tool call/response pairing |
| `ActivityEvent` | Importance levels as sqlx enum |
| `ConstellationSummary` | JSON arrays for key_decisions, open_threads |

### Queries

Most queries use compile-time checked macros (`sqlx::query!`, `sqlx::query_as!`):

- **agent.rs**: Full CRUD, group management, status updates
- **memory.rs**: Block CRUD, shared blocks, updates, history
- **message.rs**: Message insert/query, batch operations, archival
- **coordination.rs**: Activity events, summaries, tasks, handoffs (uses runtime queries)

### Schema

The migration creates all tables from v2-database-design.md:

- Core: `agents`, `agent_groups`, `group_members`
- Memory: `memory_blocks`, `memory_block_updates`, `shared_block_agents`, `memory_block_history`
- Messages: `messages`, `archive_summaries`
- Coordination: `activity_events`, `agent_summaries`, `constellation_summaries`, `notable_events`, `coordination_state`, `coordination_tasks`, `handoff_notes`

**Not yet implemented** (requires sqlite-vec extension):
- `memory_embeddings` (virtual table)
- `message_embeddings` (virtual table)
- `memory_fts`, `message_fts` (FTS5)

### Build System

- **Offline mode**: `.sqlx/` contains cached query metadata for CI builds
- **Dev database**: `dev.db` in crate root for compile-time checking
- **Editor integration**: Set `SQLX_OFFLINE=true` in environment to silence LSP false positives

## Technical Decisions

### JSON Fields

Using `sqlx::types::Json<T>` wrapper instead of `#[sqlx(json)]` attribute:

```rust
pub struct Agent {
    pub config: Json<AgentConfig>,
    pub enabled_tools: Json<Vec<String>>,
    pub tool_rules: Option<Json<ToolRules>>,
}
```

### SQLite Integer Types

All integer columns use `i64` (SQLite INTEGER is always 64-bit internally).

### Column Type Annotations

For compile-time macros with nullable or custom-typed columns:

```rust
sqlx::query_as!(
    Agent,
    r#"SELECT 
        id as "id!",
        status as "status!: AgentStatus",
        config as "config!: _"
    FROM agents WHERE id = ?"#,
    id
)
```

### Connection Management

`ConstellationDb` wraps a `SqlitePool` with:
- WAL mode for concurrent reads
- 5 max connections (SQLite is single-writer)
- Automatic migration on open
- Pragmas for performance (cache_size, mmap_size, etc.)

## What's Missing

### High Priority

1. **Integration with pattern_core** - Replace SurrealDB calls with pattern_db
2. **sqlite-vec extension** - Vector tables for semantic search
3. **FTS5 setup** - Full-text search tables and triggers
4. **More tests** - Query function unit tests, integration tests

### Medium Priority

1. **coordination.rs macro conversion** - Currently uses runtime queries
2. **Archival entries table/queries** - Defined in schema, not in models
3. **Data sources table/queries** - Defined in schema, not in models
4. **Folders/file passages** - Defined in schema, not in models

### Lower Priority

1. **Global database** - User accounts, constellation registry (server mode)
2. **Migration tooling** - CAR import to new schema
3. **Backup/vacuum commands** - CLI integration

## Usage

### Development

```bash
# Create/update dev database
cd crates/pattern_db
sqlx database create
sqlx migrate run

# Regenerate offline data after query changes
DATABASE_URL="sqlite:crates/pattern_db/dev.db" cargo sqlx prepare --workspace

# Build (uses offline data)
SQLX_OFFLINE=true cargo check -p pattern_db
```

### In Code

```rust
use pattern_db::{ConstellationDb, queries, models::*};

// Open database (creates if missing, runs migrations)
let db = ConstellationDb::open("path/to/constellation.db").await?;

// Create an agent
let agent = Agent { /* ... */ };
queries::agent::create_agent(db.pool(), &agent).await?;

// Query messages with batch ordering
let messages = queries::message::get_recent_messages(db.pool(), "agent_id", 50).await?;
```

## Files Changed Since Last Session

- `src/connection.rs` - Removed unused `DbError` import
- `.sqlx/*.json` - Generated offline query data (40+ files)

## Next Steps

1. **Integration spike** - Try using pattern_db from pattern_core for one agent operation
2. **sqlite-vec** - Research bundling strategy, create virtual table migration
3. **Test coverage** - Add tests for critical query paths
