-- Phase 6 Task 4: separate column for persona lifecycle status.
--
-- The pre-v3 `agents.status` column stores runtime state ('active' /
-- 'hibernated' / 'archived'). PersonaStatus (Active / Draft / Inactive) is a
-- distinct lifecycle dimension (was this persona promoted by a human?), so
-- it gets its own column rather than overloading the existing one.
--
-- Default 'active' is safe for all pre-Phase-6 rows: any agent that exists
-- prior to this migration was created by a privileged path (CLI / direct
-- DB seed) and should be considered promoted.

ALTER TABLE agents ADD COLUMN persona_status TEXT NOT NULL DEFAULT 'active';

CREATE INDEX idx_agents_persona_status ON agents(persona_status);
