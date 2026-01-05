-- Add frontier and active flag to memory_block_updates for undo support
-- frontier: Stores the Loro version vector after each update
-- is_active: Marks whether this update is on the active branch (for undo/redo)

ALTER TABLE memory_block_updates ADD COLUMN frontier BLOB;
ALTER TABLE memory_block_updates ADD COLUMN is_active INTEGER NOT NULL DEFAULT 1;

CREATE INDEX idx_updates_active ON memory_block_updates(block_id, is_active, seq);
