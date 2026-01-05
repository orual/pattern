-- Add is_deleted column for soft deletes (tombstones)
-- Unlike is_archived (which is for compression/summarization), is_deleted marks
-- messages that should be treated as if they no longer exist.

ALTER TABLE messages ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0;

-- Index for efficient filtering of non-deleted messages
CREATE INDEX idx_messages_deleted ON messages(agent_id, is_deleted, position DESC);
