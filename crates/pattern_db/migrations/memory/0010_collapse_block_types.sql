-- Migration: collapse BlockType::Archival and BlockType::Log.
--
-- Archival-tier memory_blocks rows are copied into archival_entries
-- (the canonical archival storage table from migration 0001).
-- Log-tier memory_blocks rows are reclassified as working-tier with
-- a {"kind": "log"} marker in their metadata JSON.
--
-- After this migration, only block_type IN ('core', 'working') exists
-- in memory_blocks. The application layer's FromSql impl rejects stale
-- "archival"/"log" values with a clear error pointing here.

-- 1. Copy archival-tier memory blocks into archival_entries.
--    content_preview is the best available text representation
--    (loro_snapshot is a binary CRDT blob, not extractable in SQL).
INSERT INTO archival_entries (id, agent_id, content, metadata, chunk_index, parent_entry_id, created_at)
SELECT
    id,
    agent_id,
    COALESCE(content_preview, '') AS content,
    COALESCE(metadata, '{}') AS metadata,
    0 AS chunk_index,
    NULL AS parent_entry_id,
    created_at
FROM memory_blocks
WHERE block_type = 'archival'
ON CONFLICT(id) DO NOTHING;

-- 2. Delete the migrated archival rows from memory_blocks.
DELETE FROM memory_blocks WHERE block_type = 'archival';

-- 3. Reclassify log-tier blocks as working + log-kind metadata marker.
--    Preserves any existing metadata fields while adding "kind": "log".
UPDATE memory_blocks
SET block_type = 'working',
    metadata = json_set(
        COALESCE(metadata, '{}'),
        '$.kind', 'log'
    )
WHERE block_type = 'log';
