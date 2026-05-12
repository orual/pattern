-- Persist wake-condition registrations across daemon restarts.
--
-- WakeRegistry lives in process memory; without this table, agents have to
-- re-register every restart, and the wake itself is what would otherwise
-- remind them. The session-open path replays rows from this table back into
-- the in-memory registry so registered IDs are stable across restarts.
--
-- condition_json is `serde_json::to_string(&WireWakeCondition)`. WakeCustom
-- variants carry their program text + period and recompile on restore via
-- the same path that runs at original-register time.

CREATE TABLE wake_registrations (
    wake_id        TEXT PRIMARY KEY,
    agent_id       TEXT NOT NULL,
    condition_json TEXT NOT NULL,
    created_at     TEXT NOT NULL
);

CREATE INDEX idx_wake_registrations_agent_id
    ON wake_registrations(agent_id);
