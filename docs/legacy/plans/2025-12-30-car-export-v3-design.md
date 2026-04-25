# CAR Export v3 Design

## Overview

New import/export system for pattern_core using CAR (Content Addressable aRchive) files. Replaces the v1/v2 format from SurrealDB era with a clean implementation matching the new SQLite-backed architecture.

## Format Version

- **Version 3** - Signals evolution from v1/v2, allows old readers to reject
- v1/v2 files → use converter in pattern_surreal_compat
- v3+ files → handled by pattern_core directly

## Export Scopes

1. **Agent** - Single agent with all its data
2. **Group** - Group config + member agents (or thin: config only)
3. **Constellation** - All agents and groups with deduplication

## Root Structure

```
ExportManifest (CAR root block)
├── version: 3
├── exported_at: DateTime<Utc>
├── export_type: Agent | Group | Constellation
├── stats: ExportStats
│   ├── agent_count, group_count
│   ├── message_count, memory_block_count
│   ├── archival_entry_count
│   └── total_bytes
└── data_cid: Cid → AgentExport | GroupExport | ConstellationExport
```

## Agent Export

```
AgentExport
├── agent: AgentRecord (inline - small)
│   ├── id, name, description
│   ├── model_provider, model_name
│   ├── system_prompt
│   ├── config: serde_json::Value (full AgentConfig)
│   ├── enabled_tools, tool_rules
│   ├── status, created_at, updated_at
├── message_chunk_cids: Vec<Cid>
├── memory_block_cids: Vec<Cid>
├── archival_entry_cids: Vec<Cid>
└── archive_summary_cids: Vec<Cid>
```

## Memory Blocks

Memory blocks use Loro CRDT snapshots which may exceed CAR block size limits.

```
MemoryBlockExport (head block)
├── id, agent_id, label, description
├── block_type, char_limit, permission, pinned
├── content_preview: String (rendered text for inspection)
├── metadata, is_active
├── frontier, last_seq
├── created_at, updated_at
├── snapshot_cids: Vec<Cid>  // chunks if > 1MB
└── total_snapshot_bytes: u64

SnapshotChunk
├── index: u32
├── data: Vec<u8>
└── next_cid: Option<Cid>  // optional for streaming
```

## Archival Entries

Exported directly (text content, reasonable size):

```
ArchivalEntryExport
├── id, agent_id, content
├── metadata: Option<serde_json::Value>
├── chunk_index, parent_entry_id
└── created_at
```

## Messages

Chunked by size with message count ceiling:

```
MessageChunk
├── chunk_index: u32
├── start_position: String  // Snowflake ID
├── end_position: String
├── messages: Vec<MessageRecord>
│   ├── id, agent_id, position, batch_id, sequence_in_batch
│   ├── role, content_json
│   ├── batch_type, source, source_metadata
│   ├── is_archived, is_deleted
│   └── created_at
└── message_count: u32
```

**Chunking strategy:**
- Target ~900KB per chunk (headroom under 1MB limit)
- Cap at 1000 messages per chunk
- Size check before adding each message
- Finalize chunk when either limit would be exceeded

## Archive Summaries

```
ArchiveSummaryExport
├── id, agent_id
├── summary: String
├── start_position, end_position
├── message_count: i64
├── previous_summary_id: Option<String>
├── depth: i64
└── created_at
```

## Group Export

```
GroupExport
├── group: GroupRecord
│   ├── id, name, description
│   ├── pattern_type, pattern_config
│   └── created_at, updated_at
├── members: Vec<GroupMemberRecord>
│   ├── agent_id, role, capabilities, joined_at
├── agent_exports: Vec<AgentExport>  // inline for standalone
└── shared_memory_cids: Vec<Cid>

GroupConfigExport (thin variant)
├── group: GroupRecord
└── member_agent_ids: Vec<AgentId>
```

## Constellation Export

Deduplicates agents across groups:

```
ConstellationExport
├── version: 3
├── owner_id: String
├── exported_at: DateTime<Utc>
├── agent_exports: HashMap<AgentId, Cid>  // shared pool
├── group_exports: Vec<GroupExportThin>
│   ├── group: GroupRecord
│   ├── members: Vec<GroupMemberRecord>
│   ├── agent_cids: Vec<Cid>  // references into pool
│   └── shared_memory_cids: Vec<Cid>
└── standalone_agent_cids: Vec<Cid>
```

## Export Options

```rust
pub struct ExportOptions {
    pub target: ExportTarget,
    pub include_messages: bool,
    pub include_archival: bool,
    pub max_chunk_bytes: usize,      // default 900_000
    pub max_messages_per_chunk: usize, // default 1000
}

pub enum ExportTarget {
    Agent(AgentId),
    Group { id: GroupId, thin: bool },
    Constellation,
}
```

## Import Options

```rust
pub struct ImportOptions {
    pub owner_id: UserId,
    pub rename: Option<String>,
    pub preserve_ids: bool,
    pub include_messages: bool,
    pub include_archival: bool,
}
```

## Import Flow

1. Read manifest, validate version == 3
2. Load export data block
3. For each agent:
   - Insert Agent row
   - Stream message chunks → insert
   - Reconstruct Loro snapshots → insert memory blocks
   - Insert archival entries
   - Queue embedding regeneration
4. For groups: insert group + members, link agents
5. Return imported entity IDs

## Converter (pattern_surreal_compat)

Handles migration from old format:

```
Sources:
├── Live SurrealDB connection
└── v1/v2 CAR files

Flow:
1. Load old data (surreal query or CAR parse)
2. Map old types → new pattern_db types
3. Call pattern_core export functions
4. Output v3 CAR file
```

## Implementation Location

- **pattern_core**: New export/import module
- **pattern_surreal_compat**: Converter using pattern_core

## Encoding

- DAG-CBOR for all blocks
- CIDs: Blake3-256 + DAG-CBOR codec (0x71)
- Max block size: 1MB (IPLD compatibility)

## Notes

- No embeddings stored - regenerate on import
- Loro snapshots are full exports (not deltas)
- Archive summaries included alongside original messages
