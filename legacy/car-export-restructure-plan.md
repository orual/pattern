# CAR Export Restructure Plan

## Problem Statement

The current CAR export implementation creates blocks that are too large for standard IPLD tools like go-car. This happens because we serialize entire `AgentRecord` objects with all their messages and memories inline, creating blocks that can be many megabytes in size.

Error from go-car:
```
invalid section data, length of read beyond allowable maximum
```

## Current Structure

Currently, when exporting an agent:

1. We load the full `AgentRecord` with all messages and memories
2. We serialize the entire record as a single DAG-CBOR block
3. We separately create chunks for messages and memories
4. The `AgentExport` references both the full agent block AND the chunk CIDs

This creates redundancy and oversized blocks.

## Proposed Structure

### 1. Slim Agent Record

Create a new `AgentRecordExport` type that contains only the essential agent metadata:

```rust
#[derive(Serialize, Deserialize)]
pub struct AgentRecordExport {
    pub id: AgentId,
    pub name: String,
    pub agent_type: AgentType,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_messages: u64,
    pub compression_status: Option<CompressionStatus>,
    pub temperature: Option<f64>,
    pub model: Option<String>,
    pub max_context_tokens: Option<u32>,
    pub system_prompt_tokens: Option<u32>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    // References to data chunks instead of inline data
    pub message_chunks: Vec<Cid>,  // CIDs of MessageChunk blocks
    pub memory_chunks: Vec<Cid>,   // CIDs of MemoryChunk blocks
}
```

### 2. Modified Export Process

#### `export_agent_to_blocks` changes:

```rust
async fn export_agent_to_blocks(
    &self,
    agent: &AgentRecord,
    options: &ExportOptions,
) -> Result<(AgentExport, Vec<(Cid, Vec<u8>)>, ExportStats)> {
    let mut blocks = Vec::new();
    let mut stats = ExportStats::default();
    
    // 1. First create memory chunks (unchanged)
    let memory_chunk_cids = self.create_memory_chunks(
        &agent.memories, 
        &mut blocks, 
        &mut stats
    )?;
    
    // 2. Create message chunks (unchanged)
    let message_chunk_cids = self.create_message_chunks(
        &agent.messages,
        options,
        &mut blocks,
        &mut stats
    )?;
    
    // 3. Create slim agent record with only references
    let agent_export_record = AgentRecordExport {
        id: agent.id.clone(),
        name: agent.name.clone(),
        agent_type: agent.agent_type.clone(),
        // ... other metadata fields ...
        message_chunks: message_chunk_cids.clone(),
        memory_chunks: memory_chunk_cids.clone(),
    };
    
    // 4. Serialize the slim record (will be much smaller)
    let agent_data = encode_dag_cbor(&agent_export_record)?;
    let agent_cid = Self::create_cid(&agent_data)?;
    blocks.push((agent_cid, agent_data));
    
    // 5. Create AgentExport with just the CID reference
    let agent_export = AgentExport {
        agent_cid,  // Just reference the slim agent record
        message_chunk_cids,
        memory_chunk_cids,
    };
    
    Ok((agent_export, blocks, stats))
}
```

### 3. Import Process Updates

The import process will need to:

1. Read the `AgentRecordExport` from its CID
2. Reconstruct the full `AgentRecord` by:
   - Copying metadata fields from `AgentRecordExport`
   - Loading messages from the referenced `MessageChunk` CIDs
   - Loading memories from the referenced `MemoryChunk` CIDs
3. Save the reconstructed `AgentRecord` to the database

### 4. Size Limits and Chunking Strategy

To ensure compatibility with IPLD standards:

- **Maximum block size**: 1MB (conservative) to 2MB (liberal)
- **Message chunks**: Keep current chunking at 1000 messages per chunk
- **Memory chunks**: Consider chunking if > 100 memory blocks
- **Agent metadata**: Should never exceed 64KB

### 5. Benefits

1. **Standards Compliance**: Blocks will be within IPLD size limits
2. **Deduplication**: Message/memory chunks can be referenced by multiple exports
3. **Streaming**: Can load agent metadata without loading all messages
4. **Compatibility**: Works with go-car, ipfs, and other IPLD tools
5. **Efficiency**: Smaller blocks transfer and cache better

## Implementation Steps

### Phase 1: Create New Types
- [x] Define `AgentRecordExport` struct
- [x] Define `GroupExport` struct (similar pattern)
- [x] Define `ConstellationExport` struct (similar pattern)
- [x] Add conversion methods between full and export types

### Phase 2: Update Export Logic
- [x] Modify `export_agent_to_blocks` to use slim records
- [x] Update `export_group` to not include full agents inline
- [x] Update `export_constellation` similarly
- [x] Add block size validation (hard 1MB cap)

### Phase 3: Update Import Logic
- [x] Create `reconstruct_agent_from_export` method
- [x] Update `import_from_car` to handle new structure
- [x] Add backward compatibility for old CAR files

### Phase 4: Testing
- [x] Unit tests for size cap, linkage, reconstruction (block-level)
- [ ] Verify with go-car and other IPLD tools (manual)
- [ ] Benchmark export/import performance
- [ ] Test incremental/partial imports

### Phase 5: Migration
- [x] Document format changes (see car-export-v2.md)
- [ ] Provide migration tool for old CAR files
- [ ] Update CLI help text and examples

## Status

- Implemented in code with `EXPORT_VERSION = 2`.
- Slim agent metadata with chunk references is now the default.
- Hard 1MB cap enforced for all blocks; chunks are size-aware and linked with `next_chunk`.

## Backward Compatibility

To maintain compatibility with existing CAR files:

1. Check the structure of the agent block on import
2. If it has `messages` field (old format), use legacy import
3. If it has `message_chunks` field (new format), use new import
4. Log warnings for old format, suggesting re-export

## Alternative Considerations

### Option: Stream-based Export
Instead of loading all messages into memory, stream them directly to chunks:
- Pro: Lower memory usage
- Con: More complex implementation

### Option: Content-addressed Deduplication
Store each message as its own block, deduplicate identical messages:
- Pro: Maximum deduplication
- Con: Many more blocks, slower import/export

### Option: Compression
Apply compression to blocks before writing:
- Pro: Smaller CAR files
- Con: Not standard IPLD practice, may break compatibility

## Estimated Effort

- Type definitions: 2-3 hours
- Export logic update: 4-6 hours  
- Import logic update: 4-6 hours
- Testing: 3-4 hours
- Documentation: 1-2 hours

**Total: 14-21 hours**

## Success Criteria

1. CAR files can be read by go-car without errors
2. No single block exceeds 2MB
3. Export/import round-trip preserves all data
4. Performance is comparable or better than current implementation
5. Memory usage stays reasonable for large agents
