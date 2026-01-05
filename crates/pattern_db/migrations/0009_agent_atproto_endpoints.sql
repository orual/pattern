-- Agent ATProto endpoint configuration
-- Links agents to their ATProto identity (DID stored in auth.db)
-- Note: No foreign key - agent_id is a soft reference, DID can be shared across agents
CREATE TABLE agent_atproto_endpoints (
    agent_id TEXT NOT NULL,
    did TEXT NOT NULL,                   -- References session in auth.db
    endpoint_type TEXT NOT NULL,         -- 'bluesky_post', 'bluesky_firehose', etc.
    config TEXT,                         -- JSON endpoint-specific config
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (agent_id, endpoint_type)
);

CREATE INDEX idx_agent_atproto_endpoints_did ON agent_atproto_endpoints(did);
