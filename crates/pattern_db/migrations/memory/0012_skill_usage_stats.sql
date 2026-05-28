-- Skill usage statistics (per-local-install observability).
--
-- Rows are keyed on the canonical block handle (SmolStr from pattern_core).
-- WITHOUT ROWID: the block_handle TEXT PRIMARY KEY is the physical row key;
-- no integer rowid column is created. Suitable for frequent point lookups and
-- upserts by handle without a secondary B-tree.
--
-- Rows are orphan-tolerant: deleting a Skill block does NOT cascade to this
-- table. Stale rows are harmless; future cleanup (cascade on block delete) is
-- a Phase 5 concern.
CREATE TABLE skill_usage_stats (
    block_handle   TEXT PRIMARY KEY NOT NULL,
    last_used      TEXT,                              -- ISO-8601 / RFC 3339 timestamp, nullable
    last_used_by   TEXT,                              -- AgentId, nullable
    use_count      INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
