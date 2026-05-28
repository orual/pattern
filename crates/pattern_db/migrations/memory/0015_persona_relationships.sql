-- Phase 6 of v3-multi-agent: persona relationship edges + organisational groups.
--
-- `persona_relationships` is a directed edge table replacing the legacy
-- `agent_groups` / `group_members` coordination schema (which is dropped in
-- migration 0016). Each row encodes one relationship of a `RelationshipKind`
-- (snake_case: `supervisor_of`, `specialist_for`, `peer_with`, `observer_of`)
-- between two personas.
--
-- `persona_groups` + `persona_group_members` are organisational only — they
-- give humans roster views and bulk operations, but Phase 6's dispatch path
-- does NOT consult them. Coordination patterns from the staging-era
-- `CoordinationPattern` enum are intentionally not carried forward.
--
-- All timestamps are RFC 3339 text (`jiff::Timestamp`).

CREATE TABLE persona_relationships (
    id           TEXT PRIMARY KEY,
    from_persona TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    to_persona   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,                -- RelationshipKind (snake_case)
    metadata     TEXT NOT NULL DEFAULT '{}',   -- Json<serde_json::Value>
    created_at   TEXT NOT NULL,
    UNIQUE(from_persona, to_persona, kind)
);

CREATE INDEX idx_persona_relationships_from ON persona_relationships(from_persona, kind);
CREATE INDEX idx_persona_relationships_to   ON persona_relationships(to_persona, kind);

CREATE TABLE persona_groups (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    project_id TEXT,                           -- nullable: global groups allowed
    metadata   TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    UNIQUE(name, project_id)
);

CREATE TABLE persona_group_members (
    group_id   TEXT NOT NULL REFERENCES persona_groups(id) ON DELETE CASCADE,
    persona_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    joined_at  TEXT NOT NULL,
    PRIMARY KEY (group_id, persona_id)
);

CREATE INDEX idx_persona_group_members_persona ON persona_group_members(persona_id);
