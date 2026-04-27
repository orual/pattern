-- Phase 6 of v3-multi-agent: extend `agents` for the persona registry.
--
-- Adds:
--   * config_path          — absolute path to the persona's KDL config file
--                            (NULL allowed for legacy rows + system-default
--                            persona that has no on-disk config).
--   * project_attachments  — JSON array of project paths the persona
--                            participates in. Queried via SQLite's json_each
--                            extension (bundled with rusqlite's
--                            `bundled-full` feature).
--
-- The existing `status` column is reused; `PersonaStatus` (active/draft/inactive)
-- is enforced at the application layer (SQLite does not enforce enum CHECKs
-- without explicit constraints, and we want to evolve the value set without
-- migration churn).
--
-- `idx_agents_status` already exists from `0001_initial.sql:34` — do not
-- re-create.

ALTER TABLE agents ADD COLUMN config_path TEXT;
ALTER TABLE agents ADD COLUMN project_attachments TEXT NOT NULL DEFAULT '[]';
