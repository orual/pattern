-- Memory block incremental updates
-- Stores Loro deltas between checkpoints for reduced write amplification

-- ============================================================================
-- New table for incremental updates
-- ============================================================================

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
CREATE INDEX idx_updates_block ON memory_block_updates(block_id);

-- ============================================================================
-- Add columns to memory_blocks
-- ============================================================================

-- Loro frontier for version tracking
ALTER TABLE memory_blocks ADD COLUMN frontier BLOB;

-- Last assigned sequence number for updates
ALTER TABLE memory_blocks ADD COLUMN last_seq INTEGER NOT NULL DEFAULT 0;

-- ============================================================================
-- Add frontier to checkpoints
-- ============================================================================

ALTER TABLE memory_block_checkpoints ADD COLUMN frontier BLOB;
