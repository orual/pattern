# Pattern v2: pattern_db Implementation Status

## Overview

`pattern_db` is the new SQLite-based storage backend for Pattern v2, replacing the SurrealDB-based system. It uses sqlx with compile-time query checking.

**Current State**: All model types implemented, vector search and FTS5 full-text search working, ready for integration.

## What's Implemented

### Crate Structure

```
crates/pattern_db/
├── src/
│   ├── lib.rs              # Public exports
│   ├── connection.rs       # ConstellationDb, pool management
│   ├── error.rs            # DbError, DbResult types
│   ├── fts.rs              # FTS5 full-text search
│   ├── search.rs           # Hybrid search (FTS + vector)
│   ├── vector.rs           # sqlite-vec integration, KNN search
│   ├── models/
│   │   ├── mod.rs          # Re-exports
│   │   ├── agent.rs        # Agent, AgentGroup, GroupMember, ModelRoutingConfig
│   │   ├── coordination.rs # ActivityEvent, AgentSummary, ConstellationSummary, etc.
│   │   ├── event.rs        # Event, EventOccurrence (calendar/reminders)
│   │   ├── folder.rs       # Folder, FolderFile, FilePassage, FolderAttachment
│   │   ├── memory.rs       # MemoryBlock, MemoryBlockCheckpoint, ArchivalEntry, etc.
│   │   ├── message.rs      # Message, ArchiveSummary (with chaining), MessageRole
│   │   ├── migration.rs    # MigrationAudit, MigrationLog, MigrationIssue
│   │   ├── source.rs       # DataSource, AgentDataSource, SourceType
│   │   └── task.rs         # Task (ADHD), TaskSummary, UserTaskStatus/Priority
│   └── queries/
│       ├── mod.rs          # Re-exports
│       ├── agent.rs        # CRUD for agents, groups, members
│       ├── memory.rs       # CRUD for memory blocks
│       ├── message.rs      # Message queries with batching
│       └── coordination.rs # Activity events, summaries, tasks, handoffs
├── migrations/
│   ├── 0001_initial.sql    # Full schema creation
│   └── 0002_fts5.sql       # FTS5 tables and triggers
├── .env                    # DATABASE_URL for dev
├── .gitignore              # Ignores dev.db
└── Cargo.toml
```

### Models

All models use `sqlx::FromRow` with proper type mappings:

| Model | Key Features |
|-------|--------------|
| `Agent` | JSON fields for config, tools, rules; `ModelRoutingConfig` for dynamic model selection |
| `AgentGroup` | Coordination patterns stored as JSON |
| `MemoryBlock` | Loro snapshots as `Vec<u8>` blobs, permissions system |
| `Message` | Snowflake ID ordering, batch tracking, tool call/response pairing |
| `ArchiveSummary` | Summary chaining via `previous_summary_id` and `depth` for recursive compression |
| `ActivityEvent` | Importance levels as sqlx enum |
| `ConstellationSummary` | JSON arrays for key_decisions, open_threads |
| `DataSource` | Comprehensive `SourceType` enum (VCS, LSP, GroupChat, Email, MCP, etc.) |
| `Task` | ADHD-aware task management with status, priority, hierarchy, time estimates |
| `Event` | Calendar events with iCal RRULE recurrence, reminders |
| `Folder` + `FilePassage` | File access with chunked passages for semantic search |
| `MigrationAudit` | v1→v2 migration tracking with detailed logs |

#### Model Routing

`ModelRoutingConfig` enables dynamic model selection:
- Fallback models for rate limits or context overflow
- Rules based on: cost threshold, context length, tool calls, time windows, source type
- Per-rule temperature/max_tokens overrides

### Queries

Most queries use compile-time checked macros (`sqlx::query!`, `sqlx::query_as!`):

- **agent.rs**: Full CRUD, group management, status updates
- **memory.rs**: Block CRUD, shared blocks, delta storage, checkpoints, consolidation
- **message.rs**: Message insert/query, batch operations, archival
- **coordination.rs**: Activity events, summaries, tasks, handoffs
- **event.rs**: Full CRUD for events, occurrences, upcoming events queries
- **task.rs**: Full CRUD with hierarchy, due date filtering, status queries
- **folder.rs**: Full CRUD for folders, files, passages, attachments
- **source.rs**: Full CRUD for data sources, agent subscriptions

### Vector Search (sqlite-vec)

**Fully implemented** via `src/vector.rs`:

- **Global extension registration** via `libsqlite3-sys::sqlite3_auto_extension`
- **vec0 virtual table** for embeddings with auxiliary columns
- **KNN search** with optional content type filtering (post-query filter)
- **CRUD operations**: insert, delete, update embeddings
- **Stats**: embedding counts by content type
- **Zerocopy** for efficient `Vec<f32>` to bytes conversion

Key functions:
- `init_sqlite_vec()` - Register extension globally (called automatically on DB open)
- `ensure_embeddings_table(pool, dimensions)` - Create vec0 virtual table
- `insert_embedding()`, `delete_embeddings()`, `update_embedding()`
- `knn_search()`, `search_similar()` - Vector similarity search
- `verify_sqlite_vec()` - Health check returning version string

**Note**: Vector queries use runtime `sqlx::query_as()` instead of compile-time macros because:
1. vec0 KNN syntax (`WHERE embedding MATCH ? AND k = ?`) isn't standard SQL
2. Table created at runtime, not in migrations
3. Dynamic dimensions per constellation

### Full-Text Search (FTS5)

**Fully implemented** via `src/fts.rs`:

- **Built into SQLite** - No extension loading required (unlike sqlite-vec)
- **External content tables** - FTS indexes existing tables without duplicating data
- **Automatic sync** - Triggers keep FTS indexes updated on INSERT/UPDATE/DELETE
- **BM25 ranking** - Results sorted by relevance using FTS5's built-in ranking

Tables indexed:
- `messages_fts` - Indexes `messages.content`
- `memory_blocks_fts` - Indexes `memory_blocks.content_preview`
- `archival_fts` - Indexes `archival_entries.content`

Key functions:
- `search_messages()` - Search message content with optional agent filter
- `search_memory_blocks()` - Search memory block previews
- `search_archival()` - Search archival entries
- `search_all()` - Search across all content types, merged by rank
- `rebuild_*_fts()` - Rebuild indexes after bulk imports
- `get_fts_stats()` - Get index statistics
- `validate_fts_query()` - Basic query syntax validation

FTS5 query syntax supported:
- Basic: `word1 word2` (AND)
- Phrase: `"exact phrase"`
- OR: `word1 OR word2`
- NOT: `word1 NOT word2`
- Prefix: `prefix*`

**Note**: FTS queries use runtime `sqlx::query_as()` because they join FTS virtual tables
with source tables and use bm25() ranking, which is easier to express at runtime.

### Hybrid Search

**Fully implemented** via `src/search.rs`:

Combines FTS5 keyword search with vector similarity search for best-of-both-worlds retrieval.

Search modes:
- `FtsOnly` - Fast keyword search, good for exact matches
- `VectorOnly` - Semantic search, good for conceptual similarity  
- `Hybrid` - Combines both (default), best for most use cases

Fusion methods:
- **RRF (Reciprocal Rank Fusion)** - Default, combines based on rank positions, parameter-free
- **Linear combination** - Weighted average of normalized scores, tunable weights

Key types:
- `HybridSearchBuilder` - Fluent builder for constructing searches
- `SearchResult` - Unified result with combined score and breakdown
- `ContentFilter` - Filter by content type and/or agent
- `ScoreBreakdown` - Individual FTS/vector scores for debugging

Example usage:
```rust
use pattern_db::search::{search, ContentFilter, SearchMode};

// Hybrid search (text + embedding)
let results = search(pool)
    .text("ADHD task management")
    .embedding(query_embedding)
    .filter(ContentFilter::messages(Some("agent_1")))
    .limit(10)
    .execute()
    .await?;

// FTS-only search
let results = search(pool)
    .text("deadline reminder")
    .mode(SearchMode::FtsOnly)
    .execute()
    .await?;

// Vector-only search  
let results = search(pool)
    .embedding(query_embedding)
    .mode(SearchMode::VectorOnly)
    .max_vector_distance(0.5)
    .execute()
    .await?;
```

### Schema

The migration creates all tables from v2-database-design.md:

- Core: `agents`, `agent_groups`, `group_members`
- Memory: `memory_blocks`, `memory_block_updates`, `memory_block_checkpoints`, `shared_block_agents`, `archival_entries`
- Messages: `messages`, `archive_summaries`
- Coordination: `activity_events`, `agent_summaries`, `constellation_summaries`, `notable_events`, `coordination_state`, `coordination_tasks`, `handoff_notes`
- Vectors: `embeddings` (vec0 virtual table, created at runtime)
- FTS5: `messages_fts`, `memory_blocks_fts`, `archival_fts` (external content tables with sync triggers)

### Build System

- **Offline mode**: `.sqlx/` contains cached query metadata for CI builds
- **Dev database**: `dev.db` in crate root for compile-time checking
- **Editor integration**: Set `SQLX_OFFLINE=true` in environment to silence LSP false positives

## Technical Decisions

### sqlite-vec Integration

Uses `libsqlite3-sys` directly to call `sqlite3_auto_extension()`:

```rust
// In vector.rs - the ONE place with unsafe code
pub fn init_sqlite_vec() {
    INIT.call_once(|| {
        unsafe {
            let init_fn = sqlite_vec::sqlite3_vec_init as *const ();
            let init_fn: unsafe extern "C" fn(...) = std::mem::transmute(init_fn);
            libsqlite3_sys::sqlite3_auto_extension(Some(init_fn));
        }
    });
}
```

**Dependencies**:
- `sqlite-vec = "0.1.7-alpha.2"` - Bundles C source, compiles via `cc`
- `libsqlite3-sys = "=0.30.1"` - Pinned to match sqlx (semver-exempt linkage)
- `zerocopy = "0.8"` - Zero-copy `Vec<f32>` to bytes

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
- sqlite-vec registered globally before first connection
- WAL mode for concurrent reads
- 5 max connections (SQLite is single-writer)
- Automatic migration on open
- Pragmas for performance (cache_size, mmap_size, etc.)

## What's Missing

### High Priority

1. **Integration with pattern_core** - Replace SurrealDB calls with pattern_db
2. **More tests** - Query function unit tests, integration tests

### Medium Priority

1. **Global database** - User accounts, constellation registry (server mode)
2. **Migration tooling** - CAR import to new schema

### Lower Priority

1. **Backup/vacuum commands** - CLI integration

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
use pattern_db::{ConstellationDb, queries, vector, models::*};

// Open database (creates if missing, runs migrations, registers sqlite-vec)
let db = ConstellationDb::open("path/to/constellation.db").await?;

// Verify sqlite-vec is loaded
let version = vector::verify_sqlite_vec(db.pool()).await?;
println!("sqlite-vec {}", version);

// Create embeddings table (384 dimensions for bge-small)
vector::ensure_embeddings_table(db.pool(), 384).await?;

// Insert an embedding
vector::insert_embedding(
    db.pool(),
    vector::ContentType::Message,
    "msg_123",
    &embedding_vec,  // Vec<f32>
    None,            // chunk_index
    Some("hash123"), // content_hash for staleness detection
).await?;

// KNN search
let results = vector::knn_search(
    db.pool(),
    &query_embedding,
    10,  // k
    Some(vector::ContentType::Message),  // optional filter
).await?;

for result in results {
    println!("{}: distance={}", result.content_id, result.distance);
}
```

## Files Changed (Hybrid Search Session)

- `src/search.rs` - New module for hybrid FTS + vector search
- `src/lib.rs` - Export search module and types

## Files Changed (FTS5 Session)

- `src/fts.rs` - New module for FTS5 full-text search
- `src/lib.rs` - Export fts module and types
- `migrations/0002_fts5.sql` - FTS5 tables and sync triggers
- `.sqlx/*.json` - Regenerated offline query data

## Files Changed (Vector Session)

- `Cargo.toml` - Added `sqlite-vec`, `libsqlite3-sys`, `zerocopy`
- `src/vector.rs` - New module for sqlite-vec integration
- `src/lib.rs` - Export vector module
- `src/connection.rs` - Call `init_sqlite_vec()` on open
- `src/error.rs` - Added `Extension` error variant
- `src/queries/coordination.rs` - Converted to compile-time macros
- `src/models/coordination.rs` - Fixed `messages_covered` to `i64`
- `.sqlx/*.json` - Regenerated offline query data

## Files Changed (Model Completion Session)

New model files:
- `src/models/source.rs` - DataSource, AgentDataSource, SourceType enum
- `src/models/task.rs` - Task, TaskSummary, UserTaskStatus, UserTaskPriority
- `src/models/event.rs` - Event, EventOccurrence, OccurrenceStatus
- `src/models/folder.rs` - Folder, FolderFile, FilePassage, FolderAttachment
- `src/models/migration.rs` - MigrationAudit, MigrationLog, MigrationIssue

Modified files:
- `src/models/agent.rs` - Added ModelRoutingConfig, ModelRoutingRule, RoutingCondition
- `src/models/message.rs` - Added previous_summary_id and depth to ArchiveSummary
- `src/models/mod.rs` - Export all new types
- `src/lib.rs` - Re-export new types at crate root
- `migrations/0001_initial.sql` - Added summary chaining columns
- `.sqlx/*.json` - Regenerated offline query data

## Files Changed (CRUD Queries Session)

New query files:
- `src/queries/event.rs` - Full CRUD for events and occurrences
- `src/queries/task.rs` - Full CRUD for user tasks with hierarchy
- `src/queries/folder.rs` - Full CRUD for folders, files, passages, attachments
- `src/queries/source.rs` - Full CRUD for data sources and agent subscriptions

Schema updates:
- `migrations/0003_model_fields.sql` - Added missing columns to events, tasks, file_passages tables
  - Events: `all_day`, `location`, `external_id`, `external_source`
  - Tasks: `tags`, `estimated_minutes`, `actual_minutes`, `notes`
  - File passages: `chunk_index`
  - New table: `event_occurrences` for tracking recurring event instances

Modified files:
- `src/queries/mod.rs` - Export new query modules
- `.sqlx/*.json` - Regenerated offline query data

## Files Changed (Memory Block Delta Storage Session)

New migration:
- `migrations/0004_memory_updates.sql` - Incremental Loro delta storage
  - New table `memory_block_updates` for storing Loro deltas
  - Added `frontier` BLOB to `memory_blocks` for Loro version tracking
  - Added `last_seq` INTEGER to `memory_blocks` for monotonic sequencing
  - Added `frontier` BLOB to `memory_block_checkpoints`

Modified models:
- `src/models/memory.rs`:
  - Added `frontier: Option<Vec<u8>>` and `last_seq: i64` to `MemoryBlock`
  - Added `frontier: Option<Vec<u8>>` to `MemoryBlockCheckpoint`
  - Added `MemoryBlockUpdate` struct for delta storage
  - Added `UpdateSource` enum (Agent, Sync, Migration, Manual)
  - Added `UpdateStats` struct for consolidation decisions

Modified queries:
- `src/queries/memory.rs` - All existing queries updated for new fields, plus:
  - `store_update()` - Store a Loro delta blob
  - `get_checkpoint_and_updates()` - Fetch checkpoint + all subsequent deltas
  - `get_updates_since()` - Fetch deltas after a given seq number
  - `has_updates_since()` - Quick check for pending updates
  - `consolidate_checkpoint()` - Atomic checkpoint consolidation
  - `get_pending_update_stats()` - Stats for consolidation decisions
  - `update_block_frontier()` - Update Loro frontier after consolidation
  - `get_block_version_info()` - Fetch (id, last_seq) for version checks

Modified exports:
- `src/models/mod.rs` - Export `MemoryBlockUpdate`, `MemoryGate`, `MemoryOp`, `UpdateSource`, `UpdateStats`

Design documentation:
- `docs/refactoring/v2-database-design.md` - Appended "Memory Block Delta Storage (Detailed Design)" section

## Next Steps

1. **Integration spike** - Try using pattern_db from pattern_core for one agent operation
2. **Test coverage** - Add tests for remaining query paths
3. **Search tuning** - Experiment with RRF k parameter and linear weights for optimal results
