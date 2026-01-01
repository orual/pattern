# Streaming CAR Export Design

## Problem
Need to export large agents/constellations without loading everything into memory.

## Current Approach
1. Generate all blocks in memory
2. Create manifest with data CID
3. Write manifest as root, then all blocks

## Insights from rsky Implementation

The rsky project uses a clever duplex stream approach:

```rust
// Create a duplex pipe for streaming data
let (writer, mut reader) = tokio::io::duplex(8 * 1024); // 8KB buffer

// CAR writer writes to one end
let car_writer = CarWriter::new(header, writer);

// Reader streams from the other end
// This allows writing and reading to happen concurrently
```

Key patterns:
- Uses `tokio::io::duplex()` for bidirectional streaming with small buffer (8KB)
- Error propagation through oneshot channels
- Async stream generation that yields chunks as they're written
- No need to buffer entire CAR file in memory

## Proposed Streaming Approach with Multiple Roots

### Option 1: Manifest + All Data Blocks as Roots
```rust
// Pseudocode
let mut root_cids = vec![];
let mut stats = ExportStats::default();

// Stream and write agent block
let agent_cid = write_agent_block(&agent, &mut writer)?;
root_cids.push(agent_cid);

// Stream and write memory chunks
for memory_chunk in stream_memory_chunks(&agent) {
    let cid = write_block(memory_chunk, &mut writer)?;
    root_cids.push(cid);
    stats.memory_count += chunk.len();
}

// Stream and write message chunks  
for message_chunk in stream_message_chunks(&agent) {
    let cid = write_block(message_chunk, &mut writer)?;
    root_cids.push(cid);
    stats.message_count += chunk.len();
}

// Create manifest
let manifest = ExportManifest {
    version: EXPORT_VERSION,
    exported_at: Utc::now(),
    export_type: ExportType::Agent,
    stats,
    data_cid: agent_cid, // Points to main agent block
};

let manifest_cid = write_block(manifest, &mut writer)?;
root_cids.insert(0, manifest_cid); // Manifest first

// Create header with all roots
let header = CarHeader::new_v1(root_cids);
```

### Option 2: Streaming with Temporary Buffer
1. Use a temporary file or buffer for the CAR body
2. Write all blocks to temp, collecting CIDs
3. Create manifest with correct CIDs
4. Write final CAR with manifest as single root, copying temp blocks

### Option 3: Fixed Manifest Pattern
```rust
// Reserve known structure
struct StreamingManifest {
    version: u32,
    exported_at: DateTime<Utc>,
    export_type: ExportType,
    // Instead of stats, use markers
    blocks_start_marker: [u8; 32], // Known pattern
    blocks_end_marker: [u8; 32],   // Known pattern
}
```

## Proposed Pattern Streaming Implementation

Based on rsky's approach, we could implement streaming for Pattern like this:

```rust
pub fn export_agent_stream(
    agent_id: AgentId,
    options: ExportOptions,
) -> impl Stream<Item = Result<Vec<u8>>> + Send + 'static {
    let (writer, mut reader) = tokio::io::duplex(8 * 1024);
    
    tokio::spawn(async move {
        // First pass: write data blocks and collect CIDs
        let mut block_cids = Vec::new();
        let mut car_writer = CarWriter::new(CarHeader::new_v1(vec![]), writer);
        
        // Write agent block
        let agent = load_agent(agent_id).await?;
        let agent_data = encode_dag_cbor(&agent)?;
        let agent_cid = create_cid(&agent_data)?;
        car_writer.write(agent_cid, agent_data).await?;
        block_cids.push(agent_cid);
        
        // Stream memory chunks
        for chunk in stream_memory_chunks(&agent).await {
            let data = encode_dag_cbor(&chunk)?;
            let cid = create_cid(&data)?;
            car_writer.write(cid, data).await?;
            block_cids.push(cid);
        }
        
        // Create and write manifest as final block
        let manifest = ExportManifest {
            version: EXPORT_VERSION,
            exported_at: Utc::now(),
            export_type: ExportType::Agent,
            stats,
            data_cid: agent_cid,
        };
        
        let manifest_data = encode_dag_cbor(&manifest)?;
        let manifest_cid = create_cid(&manifest_data)?;
        car_writer.write(manifest_cid, manifest_data).await?;
        
        // Note: We'd need to handle the root CID issue here
        // Either use multiple roots or a two-pass approach
        
        car_writer.finish().await?;
        Ok(())
    });
    
    // Stream chunks as they're written
    stream! {
        let mut buf = [0; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok(buf[..n].to_vec()),
                Err(e) => {
                    yield Err(e.into());
                    break;
                }
            }
        }
    }
}
```

## Recommendation

The rsky duplex stream approach is elegant for true streaming. However, it still has the challenge that CAR files need their root CID in the header, which we don't know until we've generated all blocks.

Options:
1. **Two-pass with temp file**: Write blocks to temp file, then create final CAR with correct header
2. **Multiple roots**: Use all significant CIDs as roots (manifest + data blocks)
3. **Deferred header**: Some CAR implementations might support writing header after blocks
4. **Current approach**: Keep the in-memory approach for now, which works well for reasonable sizes

For Pattern's current needs, the in-memory approach is sufficient. When we need to handle truly massive exports (thousands of agents, millions of messages), we can implement the streaming approach with temporary files.

## Hybrid Approach for Message-Heavy Agents

For agents with millions of messages, a hybrid approach using BlockMap would be ideal:

```rust
// Keep small data in memory, stream large collections
async fn export_agent_hybrid(agent: &AgentRecord) -> Result<(AgentExport, BlockMap)> {
    let mut message_blocks = BlockMap::new();
    let mut message_chunk_cids = Vec::new();
    
    // Stream messages in chunks, writing to BlockMap
    let message_stream = stream_messages_from_db(&agent.id);
    let mut chunk_buffer = Vec::with_capacity(1000);
    
    while let Some(msg) = message_stream.next().await {
        chunk_buffer.push(msg);
        
        if chunk_buffer.len() >= 1000 {
            let chunk = create_message_chunk(&chunk_buffer);
            let cid = write_to_blockmap(&mut message_blocks, chunk)?;
            message_chunk_cids.push(cid);
            chunk_buffer.clear();
        }
    }
    
    // Handle remaining messages
    if !chunk_buffer.is_empty() {
        let chunk = create_message_chunk(&chunk_buffer);
        let cid = write_to_blockmap(&mut message_blocks, chunk)?;
        message_chunk_cids.push(cid);
    }
    
    let agent_export = AgentExport {
        agent: agent.clone(),
        message_chunk_cids,
        memory_chunk_cids: vec![], // Small enough to handle in memory
    };
    
    Ok((agent_export, message_blocks))
}
```

### Agent Structure Caching Considerations

When implementing this, we'll also need to reconsider how agents cache messages:

1. **Current**: `AgentRecord` contains `Vec<(Message, AgentMessageRelation)>`
2. **Future**: Consider lazy-loading or streaming interface for messages
3. **Options**:
   - Message cursor/iterator instead of Vec
   - Separate message cache with size limits
   - Virtual message list that fetches from DB on demand
   - Keep recent messages in memory, older ones on disk

This would prevent agents with long conversation histories from consuming excessive memory during normal operations, not just during export.