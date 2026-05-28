-- Allow caller-supplied wake ids that don't have to be globally unique:
-- two agents can both register a wake named "social-check" without
-- colliding. Composite PK (agent_id, wake_id) matches the per-agent
-- listing semantics already used by list_for_agent.
--
-- sqlite can't ALTER the primary key in place; rebuild via tmp table.

CREATE TABLE wake_registrations_new (
    agent_id       TEXT NOT NULL,
    wake_id        TEXT NOT NULL,
    condition_json TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (agent_id, wake_id)
);

INSERT INTO wake_registrations_new (agent_id, wake_id, condition_json, created_at)
    SELECT agent_id, wake_id, condition_json, created_at FROM wake_registrations;

DROP TABLE wake_registrations;
ALTER TABLE wake_registrations_new RENAME TO wake_registrations;

-- The old agent_id index from 0018 is dropped along with the old table;
-- the new composite PK on (agent_id, wake_id) already serves the
-- list_for_agent prefix lookup, so no separate index needed.
