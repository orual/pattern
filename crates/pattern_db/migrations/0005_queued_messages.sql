-- Queued messages for agent-to-agent communication
CREATE TABLE IF NOT EXISTS queued_messages (
    id TEXT PRIMARY KEY NOT NULL,
    target_agent_id TEXT NOT NULL,
    source_agent_id TEXT,
    content TEXT NOT NULL,
    origin_json TEXT,  -- JSON serialized MessageOrigin
    metadata_json TEXT,  -- JSON for extra metadata
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    processed_at TEXT,  -- NULL until processed
    FOREIGN KEY (target_agent_id) REFERENCES agents(id)
);

CREATE INDEX IF NOT EXISTS idx_queued_messages_target ON queued_messages(target_agent_id, processed_at);
CREATE INDEX IF NOT EXISTS idx_queued_messages_priority ON queued_messages(priority DESC, created_at);
