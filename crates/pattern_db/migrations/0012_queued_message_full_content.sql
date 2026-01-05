-- Add full message content support to queued_messages
-- Stores complete MessageContent, MessageMetadata (with block_refs), batch tracking

-- Full MessageContent as JSON (Text, Parts, ToolCalls, etc.)
ALTER TABLE queued_messages ADD COLUMN content_json TEXT;

-- Full MessageMetadata as JSON (includes block_refs, user_id, custom, etc.)
ALTER TABLE queued_messages ADD COLUMN metadata_json_full TEXT;

-- Batch ID for notification batching
ALTER TABLE queued_messages ADD COLUMN batch_id TEXT;

-- Message role (user, assistant, system, tool)
ALTER TABLE queued_messages ADD COLUMN role TEXT NOT NULL DEFAULT 'user';

-- Index for batch queries
CREATE INDEX IF NOT EXISTS idx_queued_messages_batch ON queued_messages(batch_id);
