# Pattern v2: Database Design

## Overview

Moving from SurrealDB to SQLite + sqlx. One database file per constellation provides physical isolation.

## Why SQLite

### Problems with SurrealDB

1. **Immature ecosystem** - Constant API changes, missing features, weird edge cases
2. **Live query limitations** - Can't use parameters in WHERE clauses, forces string interpolation
3. **Type system friction** - `chrono::DateTime` vs `surrealdb::Datetime`, bracket-wrapped IDs
4. **No row-level security in practice** - We couldn't get permissions working reliably
5. **Custom entity macro** - We built a 1000+ line proc macro just to work around serialization issues
6. **Debugging difficulty** - Hard to inspect what's actually in the database

### SQLite Benefits

1. **Battle-tested** - Decades of production use, known behavior
2. **Extensions for everything** - sqlite-vec for vectors, FTS5 for full-text search
3. **One file = one database** - Natural isolation per constellation
4. **sqlx is solid** - Compile-time checked queries, good async support
5. **Easy debugging** - Can open in any SQLite browser
6. **Portable** - Database files are self-contained, easy backup/restore

## Architecture

### One Database Per Constellation

```
data/
├── constellations/
│   ├── {constellation_id}/
│   │   ├── pattern.db           # Main SQLite database
│   │   ├── pattern.db-shm       # SQLite shared memory (temp)
│   │   ├── pattern.db-wal       # Write-ahead log (temp)
│   │   └── exports/             # CAR exports
│   └── {another_constellation_id}/
│       └── ...
└── global.db                    # User accounts, constellation registry
```

**Benefits:**
- No cross-constellation data leaks possible at DB level
- SQLite's single-writer is fine (one constellation = sequential operations)
- Easy to backup/restore/migrate individual constellations
- Can run multiple constellations in parallel (separate DB connections)

### Global Database

Small database for cross-constellation concerns:

```sql
-- User accounts (for server mode)
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT UNIQUE NOT NULL,
    email TEXT UNIQUE,
    password_hash TEXT,  -- NULL for OAuth-only users
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- OAuth identities
CREATE TABLE user_identities (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    provider TEXT NOT NULL,  -- 'atproto', 'discord', etc.
    provider_id TEXT NOT NULL,
    access_token TEXT,
    refresh_token TEXT,
    expires_at TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(provider, provider_id),
    FOREIGN KEY (user_id) REFERENCES users(id)
);

-- Constellation registry
CREATE TABLE constellations (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    name TEXT NOT NULL,
    db_path TEXT NOT NULL,  -- Relative path to constellation DB
    created_at TEXT NOT NULL,
    last_accessed_at TEXT NOT NULL,
    FOREIGN KEY (owner_id) REFERENCES users(id)
);
```

## Constellation Database Schema

### Core Tables

```sql
-- Agents in this constellation
CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    description TEXT,
    
    -- Model configuration
    model_provider TEXT NOT NULL,  -- 'anthropic', 'openai', 'google'
    model_name TEXT NOT NULL,
    
    -- System prompt and config
    system_prompt TEXT NOT NULL,
    config JSON NOT NULL,  -- Temperature, max tokens, etc.
    
    -- Tool configuration
    enabled_tools JSON NOT NULL,  -- Array of tool names
    tool_rules JSON,  -- Tool-specific rules
    
    -- Status
    status TEXT NOT NULL DEFAULT 'active',  -- 'active', 'hibernated', 'archived'
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Agent groups for coordination
CREATE TABLE agent_groups (
    id TEXT PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    description TEXT,
    
    -- Coordination pattern
    pattern_type TEXT NOT NULL,  -- 'round_robin', 'dynamic', 'supervisor', etc.
    pattern_config JSON NOT NULL,
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Group membership
CREATE TABLE group_members (
    group_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    role TEXT,  -- 'supervisor', 'worker', etc. (pattern-specific)
    joined_at TEXT NOT NULL,
    PRIMARY KEY (group_id, agent_id),
    FOREIGN KEY (group_id) REFERENCES agent_groups(id),
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);
```

### Memory Tables

```sql
-- Memory blocks (see v2-memory-system.md for details)
CREATE TABLE memory_blocks (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT NOT NULL,
    
    block_type TEXT NOT NULL,  -- 'core', 'working', 'archival', 'log'
    char_limit INTEGER NOT NULL DEFAULT 5000,
    read_only INTEGER NOT NULL DEFAULT 0,
    
    -- Loro document stored as blob
    loro_snapshot BLOB NOT NULL,
    
    -- Quick access without deserializing
    content_preview TEXT,
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    
    UNIQUE(agent_id, label),
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

-- Pending Loro updates (between snapshots)
CREATE TABLE memory_block_updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT NOT NULL,
    loro_update BLOB NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id) ON DELETE CASCADE
);

-- Shared blocks (blocks that multiple agents can access)
CREATE TABLE shared_block_agents (
    block_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    write_access INTEGER NOT NULL DEFAULT 0,
    attached_at TEXT NOT NULL,
    PRIMARY KEY (block_id, agent_id),
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id) ON DELETE CASCADE,
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

-- Block history metadata (for UI, supplements Loro's internal history)
CREATE TABLE memory_block_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT NOT NULL,
    version_frontiers TEXT NOT NULL,  -- JSON: Loro frontiers
    change_summary TEXT,
    changed_by TEXT NOT NULL,  -- 'agent:{id}', 'user', 'system'
    timestamp TEXT NOT NULL,
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id) ON DELETE CASCADE
);

-- Archival memory entries (separate from blocks)
-- These are individual searchable entries the agent can store/retrieve
CREATE TABLE archival_entries (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    
    -- Content
    content TEXT NOT NULL,
    metadata JSON,  -- Optional structured metadata
    
    -- For chunked large content
    chunk_index INTEGER DEFAULT 0,
    parent_entry_id TEXT,  -- Links chunks together
    
    created_at TEXT NOT NULL,
    
    FOREIGN KEY (agent_id) REFERENCES agents(id),
    FOREIGN KEY (parent_entry_id) REFERENCES archival_entries(id)
);

CREATE INDEX idx_archival_agent ON archival_entries(agent_id);
CREATE INDEX idx_archival_parent ON archival_entries(parent_entry_id);
```

### Message Tables

```sql
-- Messages (conversation history)
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    
    -- Snowflake-based ordering
    position TEXT NOT NULL,  -- Snowflake ID as string for sorting
    batch_id TEXT,  -- Groups request/response cycles
    sequence_in_batch INTEGER,
    
    -- Message content
    role TEXT NOT NULL,  -- 'user', 'assistant', 'system', 'tool'
    content TEXT,
    
    -- For tool messages
    tool_call_id TEXT,
    tool_name TEXT,
    tool_args JSON,
    tool_result JSON,
    
    -- Metadata
    source TEXT,  -- 'cli', 'discord', 'bluesky', 'api', etc.
    source_metadata JSON,  -- Channel ID, message ID, etc.
    
    -- Status
    is_archived INTEGER NOT NULL DEFAULT 0,
    
    created_at TEXT NOT NULL,
    
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

CREATE INDEX idx_messages_agent_position ON messages(agent_id, position);
CREATE INDEX idx_messages_agent_batch ON messages(agent_id, batch_id);
CREATE INDEX idx_messages_archived ON messages(agent_id, is_archived, position);

-- Archive summaries
CREATE TABLE archive_summaries (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    
    summary TEXT NOT NULL,
    
    -- What messages this summarizes
    start_position TEXT NOT NULL,
    end_position TEXT NOT NULL,
    message_count INTEGER NOT NULL,
    
    created_at TEXT NOT NULL,
    
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);
```

### Vector Search (sqlite-vec)

```sql
-- Vector table for semantic search over memories
CREATE VIRTUAL TABLE memory_embeddings USING vec0(
    embedding float[384],  -- Adjust dimension for your model
    +block_id TEXT,
    +chunk_index INTEGER,  -- For blocks split into chunks
    +content_hash TEXT     -- Detect if content changed
);

-- Vector table for message search
CREATE VIRTUAL TABLE message_embeddings USING vec0(
    embedding float[384],
    +message_id TEXT,
    +content_hash TEXT
);
```

### Full-Text Search (FTS5)

```sql
-- Full-text search over memories
CREATE VIRTUAL TABLE memory_fts USING fts5(
    block_id,
    label,
    content,
    content='memory_blocks',
    content_rowid='rowid'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER memory_fts_insert AFTER INSERT ON memory_blocks BEGIN
    INSERT INTO memory_fts(rowid, block_id, label, content)
    VALUES (new.rowid, new.id, new.label, new.content_preview);
END;

CREATE TRIGGER memory_fts_delete AFTER DELETE ON memory_blocks BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, block_id, label, content)
    VALUES ('delete', old.rowid, old.id, old.label, old.content_preview);
END;

CREATE TRIGGER memory_fts_update AFTER UPDATE ON memory_blocks BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, block_id, label, content)
    VALUES ('delete', old.rowid, old.id, old.label, old.content_preview);
    INSERT INTO memory_fts(rowid, block_id, label, content)
    VALUES (new.rowid, new.id, new.label, new.content_preview);
END;

-- Full-text search over messages
CREATE VIRTUAL TABLE message_fts USING fts5(
    message_id,
    content,
    content='messages',
    content_rowid='rowid'
);
```

### Tasks and Events

```sql
-- Tasks (ADHD support)
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    agent_id TEXT,  -- NULL for constellation-level tasks
    
    title TEXT NOT NULL,
    description TEXT,
    
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'in_progress', 'completed', 'cancelled'
    priority TEXT NOT NULL DEFAULT 'medium',  -- 'low', 'medium', 'high', 'urgent'
    
    -- Optional scheduling
    due_at TEXT,
    scheduled_at TEXT,
    completed_at TEXT,
    
    -- Hierarchy
    parent_task_id TEXT,
    
    -- Embedding for semantic search
    -- (stored in vec table, linked by task_id)
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    
    FOREIGN KEY (agent_id) REFERENCES agents(id),
    FOREIGN KEY (parent_task_id) REFERENCES tasks(id)
);

-- Events/reminders
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    agent_id TEXT,
    
    title TEXT NOT NULL,
    description TEXT,
    
    starts_at TEXT NOT NULL,
    ends_at TEXT,
    
    -- Recurrence (iCal RRULE format)
    rrule TEXT,
    
    -- Reminder settings
    reminder_minutes INTEGER,  -- Minutes before event
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);
```

### Data Sources

```sql
-- External data source configurations
CREATE TABLE data_sources (
    id TEXT PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    source_type TEXT NOT NULL,  -- 'file', 'bluesky', 'discord', 'rss', etc.
    config JSON NOT NULL,
    
    -- Polling/sync state
    last_sync_at TEXT,
    sync_cursor TEXT,  -- Source-specific position marker
    
    enabled INTEGER NOT NULL DEFAULT 1,
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Which agents receive from which sources
CREATE TABLE agent_data_sources (
    agent_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    
    -- How to handle incoming data
    notification_template TEXT,
    
    PRIMARY KEY (agent_id, source_id),
    FOREIGN KEY (agent_id) REFERENCES agents(id),
    FOREIGN KEY (source_id) REFERENCES data_sources(id)
);
```

### Folders (File Access)

```sql
-- File folders for agent access
CREATE TABLE folders (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    path_type TEXT NOT NULL,  -- 'local', 'virtual', 'remote'
    path_value TEXT,          -- filesystem path or URL
    embedding_model TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Files within folders
CREATE TABLE folder_files (
    id TEXT PRIMARY KEY,
    folder_id TEXT NOT NULL,
    name TEXT NOT NULL,
    content_type TEXT,
    size_bytes INTEGER,
    content BLOB,             -- for virtual folders
    uploaded_at TEXT NOT NULL,
    indexed_at TEXT,
    UNIQUE(folder_id, name),
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE
);

-- File passages (chunks with embeddings)
CREATE TABLE file_passages (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    content TEXT NOT NULL,
    start_line INTEGER,
    end_line INTEGER,
    created_at TEXT NOT NULL,
    FOREIGN KEY (file_id) REFERENCES folder_files(id) ON DELETE CASCADE
);

-- Passage embeddings (sqlite-vec)
CREATE VIRTUAL TABLE file_passage_embeddings USING vec0(
    embedding float[384],
    +passage_id TEXT,
    +file_id TEXT,
    +folder_id TEXT
);

-- Folder attachments to agents
CREATE TABLE folder_attachments (
    folder_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    access TEXT NOT NULL,  -- 'read', 'read_write'
    attached_at TEXT NOT NULL,
    PRIMARY KEY (folder_id, agent_id),
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE,
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);
```

### Activity Stream & Shared Context

```sql
-- Activity stream events
CREATE TABLE activity_events (
    id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    agent_id TEXT,  -- NULL for system events
    event_type TEXT NOT NULL,
    details JSON NOT NULL,
    importance TEXT,  -- 'low', 'medium', 'high', 'critical'
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

CREATE INDEX idx_activity_timestamp ON activity_events(timestamp DESC);
CREATE INDEX idx_activity_agent ON activity_events(agent_id);
CREATE INDEX idx_activity_type ON activity_events(event_type);

-- Per-agent activity summaries (LLM-generated)
CREATE TABLE agent_summaries (
    agent_id TEXT PRIMARY KEY,
    summary TEXT NOT NULL,
    messages_covered INTEGER,
    generated_at TEXT NOT NULL,
    last_active TEXT NOT NULL,
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

-- Constellation-wide summaries (periodic roll-ups)
CREATE TABLE constellation_summaries (
    id TEXT PRIMARY KEY,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    summary TEXT NOT NULL,
    key_decisions JSON,  -- array of strings
    open_threads JSON,   -- array of strings
    created_at TEXT NOT NULL
);

-- Notable events (flagged for long-term memory)
CREATE TABLE notable_events (
    id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    description TEXT NOT NULL,
    agents_involved JSON,  -- array of agent IDs
    importance TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_notable_timestamp ON notable_events(timestamp DESC);
CREATE INDEX idx_notable_importance ON notable_events(importance);
```

### Coordination State

```sql
-- Coordination key-value store (flexible shared state)
CREATE TABLE coordination_state (
    key TEXT PRIMARY KEY,
    value JSON NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT  -- agent ID or 'system' or 'user'
);

-- Task assignments (structured coordination)
CREATE TABLE coordination_tasks (
    id TEXT PRIMARY KEY,
    description TEXT NOT NULL,
    assigned_to TEXT,  -- agent ID, NULL = unassigned
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'in_progress', 'completed', 'cancelled'
    priority TEXT NOT NULL DEFAULT 'medium',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (assigned_to) REFERENCES agents(id)
);

-- Handoff notes between agents
CREATE TABLE handoff_notes (
    id TEXT PRIMARY KEY,
    from_agent TEXT NOT NULL,
    to_agent TEXT,  -- NULL = for any agent
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT,
    FOREIGN KEY (from_agent) REFERENCES agents(id),
    FOREIGN KEY (to_agent) REFERENCES agents(id)
);

CREATE INDEX idx_handoff_to ON handoff_notes(to_agent, read_at);
```

### Migration Audit

```sql
-- Records of v1->v2 migration decisions
CREATE TABLE migration_audit (
    id TEXT PRIMARY KEY,
    imported_at TEXT NOT NULL,
    source_file TEXT NOT NULL,
    source_version INTEGER NOT NULL,
    issues_found INTEGER NOT NULL,
    issues_resolved INTEGER NOT NULL,
    audit_log JSON NOT NULL  -- Full decision log
);
```
```

## sqlx Patterns

### No Entity Macro

Instead of the v1 `#[derive(Entity)]` proc macro, we use plain sqlx:

```rust
// Simple struct, derives what sqlx needs
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub model_provider: String,
    pub model_name: String,
    pub system_prompt: String,
    pub config: sqlx::types::Json<AgentConfig>,
    pub enabled_tools: sqlx::types::Json<Vec<String>>,
    pub tool_rules: Option<sqlx::types::Json<ToolRules>>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

// Queries are compile-time checked
pub async fn get_agent(pool: &SqlitePool, id: &str) -> Result<Option<Agent>> {
    sqlx::query_as!(
        Agent,
        r#"SELECT * FROM agents WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn create_agent(pool: &SqlitePool, agent: &Agent) -> Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO agents (id, name, description, model_provider, model_name, 
                           system_prompt, config, enabled_tools, tool_rules, 
                           status, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        agent.id,
        agent.name,
        agent.description,
        agent.model_provider,
        agent.model_name,
        agent.system_prompt,
        agent.config,
        agent.enabled_tools,
        agent.tool_rules,
        agent.status,
        agent.created_at,
        agent.updated_at
    )
    .execute(pool)
    .await?;
    Ok(())
}
```

### Connection Management

```rust
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

pub struct ConstellationDb {
    pool: SqlitePool,
    constellation_id: String,
}

impl ConstellationDb {
    pub async fn open(path: &Path) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)  // SQLite is single-writer anyway
            .connect(&format!("sqlite:{}?mode=rwc", path.display()))
            .await?;
        
        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await?;
        
        // Load sqlite-vec extension
        sqlx::query("SELECT load_extension('vec0')")
            .execute(&pool)
            .await?;
        
        Ok(Self { pool, constellation_id })
    }
    
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
```

### Transactions

```rust
pub async fn create_agent_with_default_blocks(
    db: &ConstellationDb,
    agent: &Agent,
) -> Result<()> {
    let mut tx = db.pool().begin().await?;
    
    // Create agent
    sqlx::query!(/* ... */)
        .execute(&mut *tx)
        .await?;
    
    // Create default memory blocks
    for block in default_blocks(agent.id) {
        sqlx::query!(/* ... */)
            .execute(&mut *tx)
            .await?;
    }
    
    tx.commit().await?;
    Ok(())
}
```

## Migration Strategy

### From v1 SurrealDB

1. Export constellation to CAR file (v1 format)
2. Parse CAR, extract entities
3. Transform to v2 schema (agent_id on memories, etc.)
4. Import into fresh SQLite database
5. Generate Loro documents for memory blocks

See [v2-migration-path.md](./v2-migration-path.md) for details.

### Schema Migrations

Using sqlx migrations in `migrations/` directory:

```
migrations/
├── 20250101000000_initial.sql
├── 20250102000000_add_task_embeddings.sql
└── ...
```

## Performance Considerations

### Indexes

Key indexes for common query patterns:

```sql
-- Agent lookups
CREATE INDEX idx_agents_name ON agents(name);
CREATE INDEX idx_agents_status ON agents(status);

-- Memory block lookups
CREATE INDEX idx_memory_blocks_agent ON memory_blocks(agent_id);
CREATE INDEX idx_memory_blocks_type ON memory_blocks(agent_id, block_type);

-- Message queries (most common)
CREATE INDEX idx_messages_agent_position ON messages(agent_id, position DESC);
CREATE INDEX idx_messages_batch ON messages(batch_id);
```

### Blob Storage

Loro snapshots are stored as BLOBs. For large documents:

```sql
-- Consider separate table for large blobs
CREATE TABLE large_blobs (
    id TEXT PRIMARY KEY,
    data BLOB NOT NULL
);

-- Reference from memory_blocks
ALTER TABLE memory_blocks ADD COLUMN large_blob_id TEXT REFERENCES large_blobs(id);
```

### Connection Pooling

SQLite with WAL mode handles concurrent reads well:

```rust
SqlitePoolOptions::new()
    .max_connections(10)  // Readers can be parallel
    .connect("sqlite:pattern.db?mode=rwc&_journal_mode=WAL")
```

## Design Decisions

### Embeddings

**Decision**: Support multiple embedding models, pursue local model as default.

- Store embedding dimension in config per constellation/folder
- Create vec tables dynamically based on configured dimension
- Default to a local model (e.g., `all-MiniLM-L6-v2` at 384 dims) to avoid cloud costs
- Cloud embeddings (OpenAI, Google) available as opt-in for higher quality
- This matches current v1 approach but with local-first default

### Blob Compression & Loro Snapshots

**Decision**: Compress large snapshots, periodic checkpoints, incremental updates.

Strategy:
1. **Frequent ops**: Store Loro update deltas (small, fast)
2. **Periodic checkpoints**: Full document snapshot that acts as new root
   - Triggered by: update count threshold, total update size, or time interval
   - Old updates before checkpoint can be pruned
3. **Compression**: zstd compress snapshots over N bytes (e.g., 10KB)
4. **History**: Checkpoint acts as "shallow clone" point - history before it is summarized/discarded

```rust
pub struct LoroStorageConfig {
    /// Compress snapshots larger than this
    pub compression_threshold_bytes: usize,  // default: 10KB
    /// Checkpoint after this many updates
    pub checkpoint_update_count: usize,      // default: 100
    /// Checkpoint if updates exceed this size
    pub checkpoint_update_size: usize,       // default: 50KB
    /// Maximum time between checkpoints
    pub checkpoint_interval: Duration,       // default: 1 hour
}
```

### Backup Strategy

**Decision**: CAR files for cold storage/migration, SQLite backup API for hot backups.

- **CAR exports**: Cross-version portable, human-reviewable structure, used for migration and archival
- **SQLite backups**: Fast, consistent snapshots for disaster recovery
- **Recommendation**: Automated SQLite backups (daily), manual CAR exports for major milestones

```bash
# Hot backup (fast, for disaster recovery)
pattern-cli backup --constellation "MyConstellation" -o backup.db

# Cold export (portable, for migration/archival)  
pattern-cli export constellation --name "MyConstellation" -o archive.car
```

### sqlite-vec

**Decision**: Bundle sqlite-vec, explicit migration via CLI.

- sqlite-vec compiled and bundled with pattern binaries
- On startup, check schema version against expected
- If outdated, refuse to run and prompt: `pattern-cli db migrate`
- Migrations are explicit user action, not automatic (safety)

### Multi-Process Access

**Decision**: Single canonical writer process ("server"), one DB per constellation, concurrency via threading.

Architecture:
```
┌─────────────────────────────────────────────────────────────┐
│                    Pattern Server Process                   │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐        │
│  │Constellation│  │Constellation│  │Constellation│        │
│  │  Thread A   │  │  Thread B   │  │  Thread C   │        │
│  │             │  │             │  │             │        │
│  │ agents.db   │  │ agents.db   │  │ agents.db   │        │
│  │ (exclusive) │  │ (exclusive) │  │ (exclusive) │        │
│  └─────────────┘  └─────────────┘  └─────────────┘        │
├─────────────────────────────────────────────────────────────┤
│  Shared: LLM API clients, config, global.db (user accounts) │
└─────────────────────────────────────────────────────────────┘
```

- Each constellation gets its own thread with exclusive DB access
- No SQLite concurrency concerns - single writer per DB
- LLM API is the bottleneck (99.9999% of the time), not SQLite
- CLI in "server mode" fills this role currently; explicit server binary later
- CLI can also run standalone for single-constellation local use

### Vacuum Schedule

**Decision**: Manual trigger, with recommendations.

- `pattern-cli db vacuum --constellation "MyConstellation"`
- Recommend after: migration, bulk deletes, major archival operations
- Could add `--auto-vacuum` flag for background maintenance mode
- SQLite's auto_vacuum pragma is an option but has tradeoffs

## Memory Block Delta Storage (Detailed Design)

### Overview

Incremental update storage for memory blocks to reduce write amplification. The DB layer (pattern_db) stores deltas and provides query primitives; the application layer (pattern_core) handles Loro operations and in-memory caching.

**Key principle**: pattern_db remains Loro-agnostic. It stores blobs and provides efficient "updates since X" queries. pattern_core owns deserialization, merging, and in-memory caching of hot blocks.

### Data Flow

**Write path**:
1. pattern_core applies update to in-memory Loro doc
2. Extracts delta blob from Loro
3. Calls `store_update(block_id, delta, source)`
4. pattern_db assigns next seq, persists update

**Read path** (cache miss):
1. `get_checkpoint_and_updates(block_id)` returns checkpoint + all pending updates
2. pattern_core deserializes checkpoint, applies updates in seq order
3. Caches result keyed by block_id

**Read path** (cache hit):
1. `get_updates_since(block_id, last_seq)` returns new updates only
2. Apply to cached doc, update last_seq

### Schema (replaces basic sketch in Memory Tables section)

**New table** - `memory_block_updates`:

```sql
CREATE TABLE memory_block_updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT NOT NULL REFERENCES memory_blocks(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    update_blob BLOB NOT NULL,
    byte_size INTEGER NOT NULL,
    source TEXT,  -- 'agent', 'sync', 'migration', 'manual'
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_updates_block_seq ON memory_block_updates(block_id, seq);
```

**Additions to `memory_blocks`**:

```sql
ALTER TABLE memory_blocks ADD COLUMN frontier BLOB;
ALTER TABLE memory_blocks ADD COLUMN last_seq INTEGER NOT NULL DEFAULT 0;
```

**Additions to `memory_block_checkpoints`**:

```sql
ALTER TABLE memory_block_checkpoints ADD COLUMN frontier BLOB;
```

### Query API (pattern_db)

```rust
// Store a new update, returns assigned seq
async fn store_update(pool, block_id, update_blob, source) -> DbResult<i64>;

// Get checkpoint + all pending updates for reconstruction
async fn get_checkpoint_and_updates(pool, block_id) -> DbResult<(Option<Checkpoint>, Vec<Update>)>;

// Get only updates after a given seq (for cache refresh)
async fn get_updates_since(pool, block_id, after_seq) -> DbResult<Vec<Update>>;

// Check if updates exist without fetching them
async fn has_updates_since(pool, block_id, after_seq) -> DbResult<bool>;

// Atomic consolidation: delete updates, store new checkpoint
async fn consolidate_checkpoint(pool, block_id, new_snapshot, new_frontier, up_to_seq) -> DbResult<()>;

// Stats for consolidation decisions
async fn get_pending_update_stats(pool, block_id) -> DbResult<UpdateStats>;  // count, total_bytes
```

### Edge Cases & Considerations

**Concurrent writes**: SQLite's single-writer model handles this. `seq` assignment is atomic via `last_seq` update in same transaction as insert.

**Crash recovery**: Updates are durable once `store_update` returns. Worst case: checkpoint is stale, but updates rebuild correct state.

**Consolidation race**: `consolidate_checkpoint` uses `up_to_seq` to only delete updates that were actually merged. New updates arriving during merge are preserved.

**Empty state**: New blocks start with `last_seq = 0`, no checkpoint. First write can be either a full snapshot (checkpoint) or an update—pattern_core decides.

**Orphaned updates**: `ON DELETE CASCADE` cleans up updates when block is deleted.

**Multi-writer**: Rare case (shared blocks). Loro handles merge conflicts; DB just stores the deltas from each writer with their own seqs.

### Out of Scope (for pattern_db)

- **Update compression**: Store raw deltas; Loro's format is already compact.
- **Automatic consolidation triggers**: pattern_core owns policy; DB just provides primitives.
- **In-memory caching**: Lives in pattern_core, not pattern_db.

### Migration Path

1. Add migration `0004_memory_updates.sql` with new table and column additions
2. Existing blocks have `frontier = NULL`, `last_seq = 0` (no pending updates)
3. No data migration needed—existing `loro_snapshot` remains valid as checkpoint

## Remaining Open Questions

1. **Default local embedding model** - Which specific model to bundle/recommend?
   - `all-MiniLM-L6-v2` (384 dims, fast, decent quality)
   - `bge-small-en-v1.5` (384 dims, better quality)
   - `nomic-embed-text-v1` (768 dims, good quality, larger)

2. **Checkpoint history retention** - How many checkpoints to keep?
   - Keep last N checkpoints for rollback?
   - Or just current + one previous?

3. **Server process management** - Systemd service? Docker? Just CLI?
   - For local use: CLI is fine
   - For always-on: need proper service management
