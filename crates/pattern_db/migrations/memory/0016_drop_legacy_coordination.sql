-- Phase 6 of v3-multi-agent: drop legacy coordination tables.
--
-- `agent_groups` + `group_members` were the staging-era coordination schema.
-- v3-multi-agent replaces them with two distinct concepts:
--   * `persona_relationships` (migration 0015) — directed edges encoding
--     supervisor / specialist / peer / observer roles. Used by routing and
--     discovery.
--   * `persona_groups` + `persona_group_members` (migration 0015) —
--     organisational rosters only. NOT a coordination mechanism. Group
--     membership no longer gates cross-agent search permission.
--
-- `coordination_tasks` was already dropped by migration 0011
-- (`0011_task_block_index.sql` dropped it as part of the tasks-as-index
-- schema introduction). Not repeated here.
--
-- Legacy data is intentionally not migrated — v3 breaks the data format
-- and the staging-era group semantics do not map cleanly onto the new
-- relationship + organisational-group split.

DROP TABLE IF EXISTS group_members;
DROP TABLE IF EXISTS agent_groups;
