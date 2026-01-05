-- Add missing columns to events, tasks, and file_passages tables
-- Also adds event_occurrences table

-- ============================================================================
-- Events table additions
-- ============================================================================

-- All-day flag for events (vs specific time)
ALTER TABLE events ADD COLUMN all_day INTEGER NOT NULL DEFAULT 0;

-- Event location (physical or virtual)
ALTER TABLE events ADD COLUMN location TEXT;

-- External calendar sync fields
ALTER TABLE events ADD COLUMN external_id TEXT;
ALTER TABLE events ADD COLUMN external_source TEXT;

-- ============================================================================
-- Tasks table additions (ADHD features)
-- ============================================================================

-- Tags for categorization (JSON array)
ALTER TABLE tasks ADD COLUMN tags JSON;

-- Time estimation and tracking
ALTER TABLE tasks ADD COLUMN estimated_minutes INTEGER;
ALTER TABLE tasks ADD COLUMN actual_minutes INTEGER;

-- Additional notes/context
ALTER TABLE tasks ADD COLUMN notes TEXT;

-- ============================================================================
-- File passages additions
-- ============================================================================

-- Chunk index within file for ordering
ALTER TABLE file_passages ADD COLUMN chunk_index INTEGER NOT NULL DEFAULT 0;

-- ============================================================================
-- Event occurrences (for recurring events)
-- ============================================================================

CREATE TABLE event_occurrences (
    id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL,

    starts_at TEXT NOT NULL,
    ends_at TEXT,

    status TEXT NOT NULL DEFAULT 'scheduled',  -- 'scheduled', 'active', 'completed', 'skipped', 'snoozed', 'cancelled'
    notes TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY (event_id) REFERENCES events(id) ON DELETE CASCADE
);

CREATE INDEX idx_occurrences_event ON event_occurrences(event_id);
CREATE INDEX idx_occurrences_starts ON event_occurrences(starts_at);
CREATE INDEX idx_occurrences_status ON event_occurrences(status);
