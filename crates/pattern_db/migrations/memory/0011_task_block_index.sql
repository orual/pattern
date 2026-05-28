-- Migration: task block index tables (tasks + task_edges + tasks_fts).
--
-- Retiring coordination_tasks: the "coordination" framing was pre-v3. Task
-- management is now handled via TaskList loro blocks indexed into the `tasks`
-- and `task_edges` tables below.
--
-- This migration aligns the `tasks` table with the Rust TaskItem shape,
-- creates the `task_edges` single-direction edges table derived from loro
-- `blocks` fields, and adds an FTS5 virtual table for keyword search (AC5.3).

-- ---------------------------------------------------------------------------
-- Drop coordination_tasks indexes BEFORE the table (AC2.6 edge case).
-- `DROP INDEX IF EXISTS` is safe even if they don't exist.
-- ---------------------------------------------------------------------------

DROP INDEX IF EXISTS idx_tasks_status;
DROP INDEX IF EXISTS idx_tasks_assigned;

-- coordination_tasks: strict subset of the new tasks-as-index schema.
-- "Coordination" framing will be rebuilt on task blocks in Plan 3 (v3-subagents).
DROP TABLE IF EXISTS coordination_tasks;

-- ---------------------------------------------------------------------------
-- Extend the existing tasks table with block-provenance + comments columns,
-- and align nomenclature with the Rust TaskItem.subject field.
-- ---------------------------------------------------------------------------

-- Rename title → subject so the whole stack agrees:
-- Rust TaskItem.subject, SQL column, FTS5 column.
ALTER TABLE tasks RENAME COLUMN title TO subject;

-- Block provenance: which TaskList block and which item within that block
-- sourced this row. NULL for legacy/manually-created tasks.
ALTER TABLE tasks ADD COLUMN block_handle TEXT;
ALTER TABLE tasks ADD COLUMN task_item_id TEXT;

-- Owning agent for the task item (distinct from `agent_id` which is the
-- legacy "responsible agent" field). NULL if unassigned.
ALTER TABLE tasks ADD COLUMN owner_agent_id TEXT;

-- JSON array of comment objects. NOT NULL with empty-array default so
-- callers never need to handle NULL here.
ALTER TABLE tasks ADD COLUMN comments_json TEXT NOT NULL DEFAULT '[]';

-- Index for block-provenance lookups (reconcile and BFS queries).
CREATE INDEX idx_tasks_block ON tasks(block_handle, task_item_id);

-- Index for owner + status filtering (AC5.2 list_tasks_filtered).
CREATE INDEX idx_tasks_owner ON tasks(owner_agent_id, status);

-- Drop unused legacy column. SQLite 3.35+ supports DROP COLUMN.
-- `priority` has no indexes, foreign keys, or triggers per pre-migration audit.
-- (idx_tasks_status was on coordination_tasks, not on tasks; idx_tasks_agent
-- on tasks(agent_id, status) doesn't cover priority.)
ALTER TABLE tasks DROP COLUMN priority;

-- ---------------------------------------------------------------------------
-- Single-direction edges table (derived from loro task `blocks` fields).
--
-- NOTE: we deliberately DO NOT use WITHOUT ROWID — SQLite requires an explicit
-- PRIMARY KEY on WITHOUT ROWID tables, and the natural key here (source_block +
-- source_item + target_block + target_item-with-NULL-collapse) can't be a
-- straight PRIMARY KEY because NULL is not equal to NULL under PK constraints.
-- The unique expression index `idx_task_edges_pk` below provides the dedup
-- guarantee. WITHOUT ROWID would give marginal storage savings not worth the
-- constraint-ergonomics cost.
-- ---------------------------------------------------------------------------

CREATE TABLE task_edges (
    source_block TEXT NOT NULL,
    source_item  TEXT NOT NULL,
    target_block TEXT NOT NULL,
    -- NULL means the edge targets the block itself, not a specific item within it.
    -- This supports block-level dependency references (AC2.4, AC2.7).
    target_item  TEXT
);

-- Unique expression index serves as the effective primary key, distinguishing
-- block-level targets (NULL → '<block>' sentinel) from item-level targets.
-- '<block>' is not a valid snowflake/base32 id, so collision is impossible.
-- This prevents duplicate edges for both NULL and non-NULL target_item (AC2.5).
CREATE UNIQUE INDEX idx_task_edges_pk ON task_edges(
    source_block, source_item, target_block, COALESCE(target_item, '<block>')
);

-- Lookup index: given a source task item, find all its outgoing edges.
CREATE INDEX idx_task_edges_source ON task_edges(source_block, source_item);

-- Lookup index: given a target block/item, find all incoming edges (reverse).
CREATE INDEX idx_task_edges_target ON task_edges(target_block, target_item);

-- ---------------------------------------------------------------------------
-- FTS5 virtual table for keyword filtering (AC5.3 in Phase 3).
-- content= + content_rowid= creates a "content table" FTS5 index that
-- stores only the index, not the content itself; triggers below keep it
-- in sync with the base table.
-- ---------------------------------------------------------------------------

CREATE VIRTUAL TABLE tasks_fts USING fts5(
    subject,
    description,
    comments_json,
    content='tasks',
    content_rowid='rowid'
);

-- Keep tasks_fts in sync with tasks via INSERT / DELETE / UPDATE triggers.
-- Trigger shape follows the convention established in 0002_fts5.sql.

CREATE TRIGGER tasks_fts_insert AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts(rowid, subject, description, comments_json)
    VALUES (new.rowid, new.subject, new.description, new.comments_json);
END;

CREATE TRIGGER tasks_fts_delete AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, subject, description, comments_json)
    VALUES ('delete', old.rowid, old.subject, old.description, old.comments_json);
END;

CREATE TRIGGER tasks_fts_update AFTER UPDATE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, subject, description, comments_json)
    VALUES ('delete', old.rowid, old.subject, old.description, old.comments_json);
    INSERT INTO tasks_fts(rowid, subject, description, comments_json)
    VALUES (new.rowid, new.subject, new.description, new.comments_json);
END;
