-- Migration 0013: fronting_set + routing_rules tables.
--
-- Persists the `FrontingSet` for a runtime instance so routing configuration
-- survives daemon restarts (AC8.1). There is always at most one row in
-- `fronting_set` (the "default" singleton), which simplifies load/save to a
-- single-row upsert.
--
-- `routing_rules` is an ON DELETE CASCADE child of `fronting_set` so clearing
-- the fronting set via a single DELETE on the parent atomically removes all
-- associated rules.

CREATE TABLE fronting_set (
    id                TEXT PRIMARY KEY,     -- singleton row; id = "default"
    active_personas   TEXT NOT NULL,        -- JSON array of PersonaId strings
    fallback_persona  TEXT,                 -- nullable PersonaId
    updated_at        TEXT NOT NULL         -- jiff::Timestamp as RFC 3339
);

CREATE TABLE routing_rules (
    id              TEXT PRIMARY KEY,
    set_id          TEXT NOT NULL REFERENCES fronting_set(id) ON DELETE CASCADE,
    pattern         TEXT NOT NULL,          -- JSON-serialized MessagePattern
    target_persona  TEXT NOT NULL,          -- PersonaId
    priority        INTEGER NOT NULL,
    created_at      TEXT NOT NULL           -- jiff::Timestamp as RFC 3339
);

CREATE INDEX idx_routing_rules_priority ON routing_rules(set_id, priority DESC);
