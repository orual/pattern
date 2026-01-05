-- Expand FTS indexes to include more searchable fields
-- 1. Archival entries: add metadata (includes labels)
-- 2. Memory blocks: add label and description

-- ============================================================================
-- Archival entries FTS: add metadata column
-- ============================================================================

DROP TRIGGER IF EXISTS archival_ai;
DROP TRIGGER IF EXISTS archival_ad;
DROP TRIGGER IF EXISTS archival_au;

DROP TABLE IF EXISTS archival_fts;

CREATE VIRTUAL TABLE archival_fts USING fts5(
    content,
    metadata,
    content='archival_entries',
    content_rowid='rowid'
);

CREATE TRIGGER archival_ai AFTER INSERT ON archival_entries BEGIN
    INSERT INTO archival_fts(rowid, content, metadata)
    VALUES (new.rowid, new.content, new.metadata);
END;

CREATE TRIGGER archival_ad AFTER DELETE ON archival_entries BEGIN
    INSERT INTO archival_fts(archival_fts, rowid, content, metadata)
    VALUES('delete', old.rowid, old.content, old.metadata);
END;

CREATE TRIGGER archival_au AFTER UPDATE ON archival_entries BEGIN
    INSERT INTO archival_fts(archival_fts, rowid, content, metadata)
    VALUES('delete', old.rowid, old.content, old.metadata);
    INSERT INTO archival_fts(rowid, content, metadata)
    VALUES (new.rowid, new.content, new.metadata);
END;

-- Rebuild archival FTS with existing data
INSERT INTO archival_fts(rowid, content, metadata)
SELECT rowid, content, metadata FROM archival_entries;

-- ============================================================================
-- Memory blocks FTS: add label and description columns
-- ============================================================================

DROP TRIGGER IF EXISTS memory_blocks_ai;
DROP TRIGGER IF EXISTS memory_blocks_ad;
DROP TRIGGER IF EXISTS memory_blocks_au;

DROP TABLE IF EXISTS memory_blocks_fts;

CREATE VIRTUAL TABLE memory_blocks_fts USING fts5(
    label,
    description,
    content_preview,
    content='memory_blocks',
    content_rowid='rowid'
);

CREATE TRIGGER memory_blocks_ai AFTER INSERT ON memory_blocks BEGIN
    INSERT INTO memory_blocks_fts(rowid, label, description, content_preview)
    VALUES (new.rowid, new.label, new.description, new.content_preview);
END;

CREATE TRIGGER memory_blocks_ad AFTER DELETE ON memory_blocks BEGIN
    INSERT INTO memory_blocks_fts(memory_blocks_fts, rowid, label, description, content_preview)
    VALUES('delete', old.rowid, old.label, old.description, old.content_preview);
END;

CREATE TRIGGER memory_blocks_au AFTER UPDATE ON memory_blocks BEGIN
    INSERT INTO memory_blocks_fts(memory_blocks_fts, rowid, label, description, content_preview)
    VALUES('delete', old.rowid, old.label, old.description, old.content_preview);
    INSERT INTO memory_blocks_fts(rowid, label, description, content_preview)
    VALUES (new.rowid, new.label, new.description, new.content_preview);
END;

-- Rebuild memory blocks FTS with existing data
INSERT INTO memory_blocks_fts(rowid, label, description, content_preview)
SELECT rowid, label, description, content_preview FROM memory_blocks;
