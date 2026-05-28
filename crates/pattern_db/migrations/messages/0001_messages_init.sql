-- Messages database initial schema (messages.db)
-- This file is attached as schema `msg` on pooled connections.
-- When run standalone (via rusqlite_migration), tables are created in the
-- default schema of messages.db.

-- ============================================================================
-- Messages
-- ============================================================================

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,

    -- Snowflake-based ordering
    position TEXT NOT NULL,
    batch_id TEXT,
    sequence_in_batch INTEGER,

    -- Message content
    role TEXT NOT NULL,  -- 'user', 'assistant', 'system', 'tool'

    content_json JSON NOT NULL,

    -- Text preview for FTS and quick access
    content_preview TEXT,

    -- Batch type
    batch_type TEXT,

    -- Metadata
    source TEXT,
    source_metadata JSON,

    -- Status
    is_archived INTEGER NOT NULL DEFAULT 0,

    -- Soft delete (tombstone)
    is_deleted INTEGER NOT NULL DEFAULT 0,

    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_agent_position ON messages(agent_id, position DESC);
CREATE INDEX IF NOT EXISTS idx_messages_agent_batch ON messages(agent_id, batch_id);
CREATE INDEX IF NOT EXISTS idx_messages_archived ON messages(agent_id, is_archived, position DESC);
CREATE INDEX IF NOT EXISTS idx_messages_deleted ON messages(agent_id, is_deleted, position DESC);

-- ============================================================================
-- Queued Messages (agent-to-agent communication)
-- ============================================================================

CREATE TABLE IF NOT EXISTS queued_messages (
    id TEXT PRIMARY KEY NOT NULL,
    target_agent_id TEXT NOT NULL,
    source_agent_id TEXT,
    content TEXT NOT NULL,
    origin_json TEXT,
    metadata_json TEXT,
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    processed_at TEXT,

    -- Full message content support
    content_json TEXT,
    metadata_json_full TEXT,
    batch_id TEXT,
    role TEXT NOT NULL DEFAULT 'user'
);

CREATE INDEX IF NOT EXISTS idx_queued_messages_target ON queued_messages(target_agent_id, processed_at);
CREATE INDEX IF NOT EXISTS idx_queued_messages_priority ON queued_messages(priority DESC, created_at);
CREATE INDEX IF NOT EXISTS idx_queued_messages_batch ON queued_messages(batch_id);

-- ============================================================================
-- Message FTS5
-- ============================================================================

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content_preview,
    content='messages',
    content_rowid='rowid'
);

-- Triggers to keep FTS index in sync with messages table
CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content_preview) VALUES (new.rowid, new.content_preview);
END;

CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content_preview) VALUES('delete', old.rowid, old.content_preview);
END;

CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content_preview) VALUES('delete', old.rowid, old.content_preview);
    INSERT INTO messages_fts(rowid, content_preview) VALUES (new.rowid, new.content_preview);
END;
