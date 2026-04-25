# Export System (CAR v3)

Pattern uses Content-Addressable Records (CAR) format for portable agent backup and migration. Version 3 is designed for the SQLite-backed architecture with Loro CRDT memory.

## Overview

The export system supports three granularities:
- **Agent**: Single agent with all data
- **Group**: Group configuration plus member agents
- **Constellation**: Full user constellation with deduplication

Format features:
- DAG-CBOR encoding for IPLD compatibility
- Content-addressed blocks with CID references
- Large data chunking (Loro snapshots, messages)
- Streaming writes for memory efficiency
- Letta/MemGPT .af file import support

## Format Structure

### Manifest (Root Block)

Every CAR file starts with an `ExportManifest`:

```rust
pub struct ExportManifest {
    pub version: u32,           // Currently 3
    pub exported_at: DateTime<Utc>,
    pub export_type: ExportType, // Agent, Group, or Constellation
    pub stats: ExportStats,
    pub data_cid: Cid,          // Points to AgentExport/GroupExport/ConstellationExport
}
```

### Agent Export

```rust
pub struct AgentExport {
    pub agent: AgentRecord,
    pub message_chunk_cids: Vec<Cid>,
    pub memory_block_cids: Vec<Cid>,
    pub archival_entry_cids: Vec<Cid>,
    pub archive_summary_cids: Vec<Cid>,
}
```

### Group Export

Two variants:
- **Full**: Includes complete agent data
- **Thin**: Configuration only, references existing agents by ID

```rust
// Full export
pub struct GroupExport {
    pub group: GroupRecord,
    pub members: Vec<GroupMemberExport>,
    pub agent_exports: Vec<AgentExport>,
    pub shared_memory_cids: Vec<Cid>,
    pub shared_attachment_exports: Vec<SharedBlockAttachmentExport>,
}

// Thin export (config only)
pub struct GroupConfigExport {
    pub group: GroupRecord,
    pub member_agent_ids: Vec<String>,
}
```

### Constellation Export

```rust
pub struct ConstellationExport {
    pub version: u32,
    pub owner_id: String,
    pub exported_at: DateTime<Utc>,
    pub agent_exports: HashMap<String, Cid>,  // Deduplicated pool
    pub group_exports: Vec<GroupExportThin>,
    pub standalone_agent_cids: Vec<Cid>,      // Agents not in any group
    pub all_memory_block_cids: Vec<Cid>,
    pub shared_attachments: Vec<SharedBlockAttachmentExport>,
}
```

## Data Chunking

### Message Chunks

Messages are grouped into chunks based on size and count limits:

```rust
pub struct MessageChunk {
    pub chunk_index: u32,
    pub start_position: String,  // Snowflake ID
    pub end_position: String,
    pub messages: Vec<MessageExport>,
    pub message_count: u32,
}
```

Default limits:
- `TARGET_CHUNK_BYTES`: 900KB (leaves headroom under 1MB block limit)
- `DEFAULT_MAX_MESSAGES_PER_CHUNK`: 1000

### Loro Snapshot Chunks

Large Loro snapshots (>900KB) are split into linked chunks:

```rust
pub struct SnapshotChunk {
    pub index: u32,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub next_cid: Option<Cid>,  // Forward link
}
```

Chunks are written in reverse order so each links forward, enabling streaming reconstruction.

### Memory Block Export

Memory blocks reference their snapshot chunks:

```rust
pub struct MemoryBlockExport {
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub block_type: MemoryBlockType,
    pub permission: MemoryPermission,
    // ... other metadata
    pub snapshot_chunk_cids: Vec<Cid>,  // Links to chunks
    pub total_snapshot_bytes: u64,
}
```

## Usage

### CLI Commands

```bash
# Export single agent
pattern export agent my-agent
pattern export agent my-agent -o backup.car

# Export group with all member agents
pattern export group my-group
pattern export group my-group -o group.car

# Export full constellation
pattern export constellation
pattern export constellation -o backup.car

# Import from CAR file
pattern import car backup.car

# Import with rename
pattern import car backup.car --rename-to NewAgentName

# Import preserving original IDs (may conflict)
pattern import car backup.car --preserve-ids

# Convert and import Letta/MemGPT .af file
pattern import letta agent.af
pattern import letta agent.af -o converted.car
```

### Programmatic Usage

```rust
use pattern_core::export::{Exporter, ExportOptions, ExportTarget, ImportOptions, Importer};

// Export an agent
let exporter = Exporter::new(pool.clone());
let mut output = File::create("agent.car").await?;
let options = ExportOptions {
    target: ExportTarget::Agent("agent-id".into()),
    include_messages: true,
    include_archival: true,
    ..Default::default()
};
let manifest = exporter.export_agent("agent-id", &mut output, &options).await?;

// Export a constellation
let mut output = File::create("constellation.car").await?;
let manifest = exporter.export_constellation("owner-id", &mut output, &options).await?;

// Import
let importer = Importer::new(pool.clone());
let input = File::open("agent.car").await?;
let options = ImportOptions::new("new-owner-id")
    .with_rename("imported-agent")
    .with_preserve_ids(false);
let result = importer.import(input, &options).await?;
```

### Letta/MemGPT Import

Convert and import Letta .af files:

```rust
use pattern_core::export::{convert_letta_to_car, LettaConversionOptions};

// Convert to CAR format
let options = LettaConversionOptions {
    owner_id: "owner-id".into(),
    preserve_timestamps: true,
    ..Default::default()
};
let (car_data, stats) = convert_letta_to_car(&af_bytes, options)?;

// Then import normally
let importer = Importer::new(pool.clone());
let result = importer.import(&car_data[..], &ImportOptions::new("owner-id")).await?;
```

## Export Options

```rust
pub struct ExportOptions {
    pub target: ExportTarget,
    pub include_messages: bool,      // Include message history
    pub include_archival: bool,      // Include archival entries
    pub max_chunk_bytes: usize,      // Max bytes per chunk
    pub max_messages_per_chunk: usize,
}
```

## Import Options

```rust
pub struct ImportOptions {
    pub owner_id: String,            // New owner for imported data
    pub rename: Option<String>,      // Rename the imported entity
    pub preserve_ids: bool,          // Keep original IDs (may conflict)
    pub include_messages: bool,      // Import message history
    pub include_archival: bool,      // Import archival entries
}
```

## Format Details

### Block Encoding

All blocks use DAG-CBOR (IPLD) encoding:
- Deterministic serialization
- CIDs computed from content hash
- Maximum block size: 1MB

```rust
// Create CID from encoded data
fn create_cid(data: &[u8]) -> Cid {
    let hash = Code::Sha2_256.digest(data);
    Cid::new_v1(DAG_CBOR, hash)
}

// Encode a block
fn encode_block<T: Serialize>(value: &T, _type_name: &str) -> Result<(Cid, Vec<u8>)> {
    let data = serde_ipld_dagcbor::to_vec(value)?;
    let cid = create_cid(&data);
    Ok((cid, data))
}
```

### CAR File Structure

```
[CAR Header: version 1, root = manifest_cid]
[manifest_cid → ExportManifest]
[data_cid → AgentExport/GroupExport/ConstellationExport]
[chunk_cids... → MessageChunk, SnapshotChunk, etc.]
[entry_cids... → MemoryBlockExport, ArchivalEntryExport, etc.]
```

### Deduplication

Constellation exports deduplicate agents across groups:
- Each agent exported once
- Groups reference agents by CID
- Shared blocks exported once, referenced via attachments

## Statistics

Export stats track what was included:

```rust
pub struct ExportStats {
    pub agent_count: u64,
    pub group_count: u64,
    pub message_count: u64,
    pub memory_block_count: u64,
    pub archival_entry_count: u64,
    pub archive_summary_count: u64,
    pub chunk_count: u64,
    pub total_blocks: u64,
    pub total_bytes: u64,
}
```

## Migration from v2

Key changes from v2:
- SQLite-native: Works directly with pattern_db models
- Loro integration: Properly chunks and reconstructs CRDT snapshots
- Shared blocks: Exports block sharing relationships
- Groups: Full group export with member agents
- Constellation: Complete backup with deduplication

## Legacy Docs

Previous export format documentation is preserved in `docs/legacy/`:
- `agent-export-design.md` - v2 design notes
- `car-export-v2.md` - v2 format specification
- `streaming-car-export.md` - Streaming design
- `car-export-restructure-plan.md` - v2 to v3 migration plan
