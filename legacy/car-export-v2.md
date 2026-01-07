# CAR Export Format v2

This document describes the export/import format introduced in EXPORT_VERSION=2.

## Summary of Changes

- Slim agent metadata: `AgentRecordExport` replaces embedding full `AgentRecord` in the CAR.
- Chunked data: Messages and memories are stored in `MessageChunk` and `MemoryChunk` blocks.
- Hard block cap: Every block is strictly limited to 1,000,000 bytes for compatibility with IPLD tools such as go-car.
- Linkage: `next_chunk` links each chunk to the next for streaming traversal; ordered CID lists are also provided.
- Manifest root: A single `ExportManifest` is the CAR root; `data_cid` points to the export payload (Agent/Group/Constellation block).

## Block Types

- `ExportManifest { version: 2, export_type, data_cid, stats, exported_at }`
- `AgentExport { agent_cid, message_chunk_cids, memory_chunk_cids }`
- `AgentRecordExport { id, name, …, message_chunks: Vec<Cid>, memory_chunks: Vec<Cid> }`
- `MessageChunk { chunk_id, start_position, end_position, messages, next_chunk }`
- `MemoryChunk { chunk_id, memories, next_chunk }`
- `GroupExport { group, member_agent_cids: Vec<(AgentId, Cid)> }`
- `ConstellationExport { constellation, groups: Vec<GroupExport>, agent_export_cids: Vec<(AgentId, Cid)> }`

## Size Limits and Chunking

- Max block size: 1,000,000 bytes (hard error if exceeded)
- Message chunk nominal size: 1000 messages; final size adjusted to stay under cap
- Memory chunk nominal size: 100 items; final size adjusted to stay under cap

## Import Strategy

1. Read `ExportManifest` from CAR root.
2. Load `data_cid` for the export payload (Agent/Group/Constellation).
3. For agents, decode `AgentExport → AgentRecordExport`, then load all referenced chunk CIDs and reconstruct a full `AgentRecord`.
  4. For groups/constellations, iterate member agent export CIDs and import agents first; then restore group/constellation relationships.

### ID Preservation (default)

- By default, import preserves original IDs (`preserve_ids = true`).
- Set `merge_existing = true` to update existing records with the same IDs, or set `preserve_ids = false` to generate new IDs to avoid conflicts.

## Backward Compatibility

- The importer still supports legacy CARs where the root is an `AgentRecord` or where `AgentExport` embeds a full `AgentRecord`.
- However, older CAR files produced by pre-v2 code are known to be unreadable by external tools due to oversized blocks.

## Verification

- All blocks should be ≤ 1MB; otherwise the exporter fails early.
- `go-car` and other IPLD tools should be able to enumerate and read blocks.
- Import should reconstruct identical counts for messages and memories.

## Notes

- `CompressionSettings` is currently unused; compression at the block level is not applied to preserve compatibility with standard IPLD tooling.
- `next_chunk` linkage enables future streaming import with minimal refactor.
