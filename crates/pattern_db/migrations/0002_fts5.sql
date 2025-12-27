-- FTS5 Full-Text Search Tables
-- These are external content tables that index existing tables

-- Message full-text search (using content_preview extracted from content_json)
CREATE VIRTUAL TABLE messages_fts USING fts5(
    content_preview,
    content='messages',
    content_rowid='rowid'
);

-- Triggers to keep FTS index in sync with messages table
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content_preview) VALUES (new.rowid, new.content_preview);
END;

CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content_preview) VALUES('delete', old.rowid, old.content_preview);
END;

CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content_preview) VALUES('delete', old.rowid, old.content_preview);
    INSERT INTO messages_fts(rowid, content_preview) VALUES (new.rowid, new.content_preview);
END;

-- Memory block full-text search (on the preview text)
CREATE VIRTUAL TABLE memory_blocks_fts USING fts5(
    content_preview,
    content='memory_blocks',
    content_rowid='rowid'
);

CREATE TRIGGER memory_blocks_ai AFTER INSERT ON memory_blocks BEGIN
    INSERT INTO memory_blocks_fts(rowid, content_preview) VALUES (new.rowid, new.content_preview);
END;

CREATE TRIGGER memory_blocks_ad AFTER DELETE ON memory_blocks BEGIN
    INSERT INTO memory_blocks_fts(memory_blocks_fts, rowid, content_preview) VALUES('delete', old.rowid, old.content_preview);
END;

CREATE TRIGGER memory_blocks_au AFTER UPDATE ON memory_blocks BEGIN
    INSERT INTO memory_blocks_fts(memory_blocks_fts, rowid, content_preview) VALUES('delete', old.rowid, old.content_preview);
    INSERT INTO memory_blocks_fts(rowid, content_preview) VALUES (new.rowid, new.content_preview);
END;

-- Archival entries full-text search
CREATE VIRTUAL TABLE archival_fts USING fts5(
    content,
    content='archival_entries',
    content_rowid='rowid'
);

CREATE TRIGGER archival_ai AFTER INSERT ON archival_entries BEGIN
    INSERT INTO archival_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER archival_ad AFTER DELETE ON archival_entries BEGIN
    INSERT INTO archival_fts(archival_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;

CREATE TRIGGER archival_au AFTER UPDATE ON archival_entries BEGIN
    INSERT INTO archival_fts(archival_fts, rowid, content) VALUES('delete', old.rowid, old.content);
    INSERT INTO archival_fts(rowid, content) VALUES (new.rowid, new.content);
END;
