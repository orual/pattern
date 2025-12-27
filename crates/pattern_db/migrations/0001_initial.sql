-- Pattern v2 Initial Schema
-- One database per constellation - this creates the full constellation schema

-- ============================================================================
-- Agents
-- ============================================================================

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

CREATE INDEX idx_agents_name ON agents(name);
CREATE INDEX idx_agents_status ON agents(status);

-- ============================================================================
-- Agent Groups
-- ============================================================================

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

CREATE TABLE group_members (
    group_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    role TEXT,  -- 'supervisor', 'worker', etc. (pattern-specific)
    joined_at TEXT NOT NULL,
    PRIMARY KEY (group_id, agent_id),
    FOREIGN KEY (group_id) REFERENCES agent_groups(id) ON DELETE CASCADE,
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);

-- ============================================================================
-- Memory Blocks
-- ============================================================================

CREATE TABLE memory_blocks (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT NOT NULL,
    
    block_type TEXT NOT NULL,  -- 'core', 'working', 'archival', 'log'
    char_limit INTEGER NOT NULL DEFAULT 5000,
    permission TEXT NOT NULL DEFAULT 'read_write',  -- 'read_only', 'partner', 'human', 'append', 'read_write', 'admin'
    pinned INTEGER NOT NULL DEFAULT 0,
    
    -- Loro document stored as blob
    loro_snapshot BLOB NOT NULL,
    
    -- Quick access without deserializing
    content_preview TEXT,
    
    -- Additional metadata
    metadata JSON,
    
    -- Embedding model used (if embedded)
    embedding_model TEXT,
    
    -- Soft delete
    is_active INTEGER NOT NULL DEFAULT 1,
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    
    UNIQUE(agent_id, label)
    -- No FK on agent_id: allows constellation-owned blocks with '_constellation_'
);

CREATE INDEX idx_memory_blocks_agent ON memory_blocks(agent_id);
CREATE INDEX idx_memory_blocks_type ON memory_blocks(agent_id, block_type);
CREATE INDEX idx_memory_blocks_active ON memory_blocks(agent_id, is_active);

-- Checkpoint history for memory blocks
CREATE TABLE memory_block_checkpoints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT NOT NULL,
    snapshot BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updates_consolidated INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id) ON DELETE CASCADE
);

CREATE INDEX idx_checkpoints_block ON memory_block_checkpoints(block_id, created_at DESC);

-- Shared blocks (blocks that multiple agents can access)
CREATE TABLE shared_block_agents (
    block_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    permission TEXT NOT NULL DEFAULT 'read_only',
    attached_at TEXT NOT NULL,
    PRIMARY KEY (block_id, agent_id),
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id) ON DELETE CASCADE,
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);

-- ============================================================================
-- Archival Entries
-- ============================================================================

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
    
    -- No FK on agent_id: allows constellation-level archival entries
    FOREIGN KEY (parent_entry_id) REFERENCES archival_entries(id) ON DELETE CASCADE
);

CREATE INDEX idx_archival_agent ON archival_entries(agent_id, created_at DESC);
CREATE INDEX idx_archival_parent ON archival_entries(parent_entry_id);

-- ============================================================================
-- Messages
-- ============================================================================

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,

    -- Snowflake-based ordering
    position TEXT NOT NULL,  -- Snowflake ID as string for sorting
    batch_id TEXT,  -- Groups request/response cycles
    sequence_in_batch INTEGER,

    -- Message content
    role TEXT NOT NULL,  -- 'user', 'assistant', 'system', 'tool'

    -- Content stored as JSON to support all MessageContent variants:
    -- - Text(String)
    -- - Parts(Vec<ContentPart>)
    -- - ToolCalls(Vec<ToolCall>)
    -- - ToolResponses(Vec<ToolResponse>)
    -- - Blocks(Vec<ContentBlock>)
    content_json JSON NOT NULL,

    -- Text preview for FTS and quick access (extracted from content_json)
    content_preview TEXT,

    -- Batch type: 'user_request', 'agent_to_agent', 'system_trigger', 'continuation'
    batch_type TEXT,

    -- Metadata
    source TEXT,  -- 'cli', 'discord', 'bluesky', 'api', etc.
    source_metadata JSON,  -- Channel ID, message ID, etc.

    -- Status
    is_archived INTEGER NOT NULL DEFAULT 0,

    created_at TEXT NOT NULL,

    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_agent_position ON messages(agent_id, position DESC);
CREATE INDEX idx_messages_agent_batch ON messages(agent_id, batch_id);
CREATE INDEX idx_messages_archived ON messages(agent_id, is_archived, position DESC);

-- Archive summaries
CREATE TABLE archive_summaries (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,

    summary TEXT NOT NULL,

    -- What messages this summarizes
    start_position TEXT NOT NULL,
    end_position TEXT NOT NULL,
    message_count INTEGER NOT NULL,

    -- Summary chaining (for summarizing summaries)
    previous_summary_id TEXT,
    depth INTEGER NOT NULL DEFAULT 0,  -- 0 = direct message summary, 1+ = summary of summaries

    created_at TEXT NOT NULL,

    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (previous_summary_id) REFERENCES archive_summaries(id) ON DELETE SET NULL
);

CREATE INDEX idx_archive_summaries_agent ON archive_summaries(agent_id, start_position);
CREATE INDEX idx_archive_summaries_chain ON archive_summaries(previous_summary_id);

-- ============================================================================
-- Activity Stream & Summaries
-- ============================================================================

CREATE TABLE activity_events (
    id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    agent_id TEXT,  -- NULL for system events
    event_type TEXT NOT NULL,
    details JSON NOT NULL,
    importance TEXT,  -- 'low', 'medium', 'high', 'critical'
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE SET NULL
);

CREATE INDEX idx_activity_timestamp ON activity_events(timestamp DESC);
CREATE INDEX idx_activity_agent ON activity_events(agent_id);
CREATE INDEX idx_activity_type ON activity_events(event_type);
CREATE INDEX idx_activity_importance ON activity_events(importance, timestamp DESC);

-- Per-agent activity summaries (LLM-generated)
CREATE TABLE agent_summaries (
    agent_id TEXT PRIMARY KEY,
    summary TEXT NOT NULL,
    messages_covered INTEGER,
    generated_at TEXT NOT NULL,
    last_active TEXT NOT NULL,
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
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

CREATE INDEX idx_constellation_summaries_period ON constellation_summaries(period_end DESC);

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

-- ============================================================================
-- Coordination
-- ============================================================================

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
    FOREIGN KEY (assigned_to) REFERENCES agents(id) ON DELETE SET NULL
);

CREATE INDEX idx_tasks_status ON coordination_tasks(status, priority DESC);
CREATE INDEX idx_tasks_assigned ON coordination_tasks(assigned_to);

-- Handoff notes between agents
CREATE TABLE handoff_notes (
    id TEXT PRIMARY KEY,
    from_agent TEXT NOT NULL,
    to_agent TEXT,  -- NULL = for any agent
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT,
    FOREIGN KEY (from_agent) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (to_agent) REFERENCES agents(id) ON DELETE SET NULL
);

CREATE INDEX idx_handoff_to ON handoff_notes(to_agent, read_at);
CREATE INDEX idx_handoff_unread ON handoff_notes(to_agent) WHERE read_at IS NULL;

-- ============================================================================
-- Data Sources
-- ============================================================================

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
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (source_id) REFERENCES data_sources(id) ON DELETE CASCADE
);

-- ============================================================================
-- Tasks (ADHD support)
-- ============================================================================

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
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE SET NULL,
    FOREIGN KEY (parent_task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

CREATE INDEX idx_tasks_agent ON tasks(agent_id, status);
CREATE INDEX idx_tasks_due ON tasks(due_at) WHERE due_at IS NOT NULL;
CREATE INDEX idx_tasks_parent ON tasks(parent_task_id);

-- ============================================================================
-- Events/Reminders
-- ============================================================================

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
    
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE SET NULL
);

CREATE INDEX idx_events_starts ON events(starts_at);
CREATE INDEX idx_events_agent ON events(agent_id);

-- ============================================================================
-- Folders (File Access)
-- ============================================================================

CREATE TABLE folders (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    path_type TEXT NOT NULL,  -- 'local', 'virtual', 'remote'
    path_value TEXT,          -- filesystem path or URL
    embedding_model TEXT NOT NULL,
    created_at TEXT NOT NULL
);

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

CREATE TABLE file_passages (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    content TEXT NOT NULL,
    start_line INTEGER,
    end_line INTEGER,
    created_at TEXT NOT NULL,
    FOREIGN KEY (file_id) REFERENCES folder_files(id) ON DELETE CASCADE
);

CREATE INDEX idx_passages_file ON file_passages(file_id);

CREATE TABLE folder_attachments (
    folder_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    access TEXT NOT NULL,  -- 'read', 'read_write'
    attached_at TEXT NOT NULL,
    PRIMARY KEY (folder_id, agent_id),
    FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE CASCADE,
    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
);

-- ============================================================================
-- Migration Audit
-- ============================================================================

CREATE TABLE migration_audit (
    id TEXT PRIMARY KEY,
    imported_at TEXT NOT NULL,
    source_file TEXT NOT NULL,
    source_version INTEGER NOT NULL,
    issues_found INTEGER NOT NULL,
    issues_resolved INTEGER NOT NULL,
    audit_log JSON NOT NULL  -- Full decision log
);
