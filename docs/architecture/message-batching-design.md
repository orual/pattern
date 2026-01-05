# Message Batching System

## Overview

Pattern uses an implicit message batching system based on Snowflake IDs to maintain consistency and ordering in agent conversations. This system ensures that related messages (user requests, assistant responses, tool calls, and tool results) are grouped together as atomic units that can be processed and compressed without breaking logical boundaries.

## Core Concepts

### Snowflake IDs
Every message has a unique, time-ordered Snowflake ID in the `position` field that provides:
- Absolute ordering across all messages for an agent
- Millisecond-precision timestamps embedded in the ID
- Guaranteed uniqueness even for concurrent operations

### Message Batches
Messages are grouped into batches representing complete request/response cycles:
- Each batch starts with a triggering message (user, agent-to-agent, or system)
- All subsequent messages in that processing cycle share the same `batch` ID
- Tool calls and their responses are kept together within batches
- Heartbeat continuations extend the original batch rather than creating new ones

## Message Structure

```rust
pub struct Message {
    pub id: MessageId,                   // UUID for database identity
    pub position: Option<SnowflakeId>,   // Absolute ordering position
    pub batch: Option<SnowflakeId>,      // ID of first message in batch
    pub sequence_num: Option<u32>,       // Position within the batch
    pub batch_type: Option<BatchType>,   // Type of processing cycle
    // ... other fields
}

pub enum BatchType {
    UserRequest,      // User-initiated interaction
    AgentToAgent,     // Inter-agent communication  
    SystemTrigger,    // System-initiated (e.g., scheduled task)
    Continuation,     // Continuation of previous batch
}
```

## How Batching Works

### New Request Flow
1. A triggering message arrives (user, system, or agent)
2. System generates a new Snowflake ID that serves as both `position` and `batch` ID
3. All subsequent messages in the processing cycle:
   - Get unique `position` IDs
   - Share the same `batch` ID
   - Increment `sequence_num` within the batch
4. Batch completes when assistant sends final response without tool calls

### Continuation Flow
1. Heartbeat or continuation request includes existing `batch` ID
2. New messages maintain the same `batch` ID
3. Sequence numbers continue incrementing
4. Original batch extends across async boundaries

### Tool Call Pairing
Within each batch:
- Tool calls are immediately followed by their responses
- Message compression respects these pairs
- Reordering ensures responses come after their calls
- No tool call/response pairs are split across batches

## Context Building

The context builder uses batching to ensure consistency:

1. **Queries messages ordered by `position`** - Provides chronological ordering
2. **Groups messages by `batch` ID** - Identifies related message sets
3. **Validates batch completeness** - Checks for:
   - Balanced tool calls and responses
   - Final assistant message without pending tools
   - No continuation markers
4. **Includes complete batches** - All finished processing cycles
5. **Includes current active batch** - Even if incomplete, for context continuity
6. **Drops orphaned batches** - Incomplete batches from crashed processes

## Benefits

### Consistency
- Tool calls never separated from responses
- Request/response cycles remain atomic
- Heartbeat continuations stay with original context

### Compression Safety
- Batch boundaries prevent breaking logical units
- Tool pairs compressed together
- Archive summaries respect batch boundaries

### Recovery
- Orphaned incomplete batches easily identified
- Clean recovery points at batch boundaries
- Migration can reconstruct batches from message history

### Debugging
- Clear processing cycle boundaries
- Easy to trace full request/response flows
- Batch type indicates trigger source

## Implementation Notes

- Batches are implicit - no separate batch table needed
- All batch metadata reconstructed from messages at runtime
- Migration successfully applied to all existing agent histories
- Snowflake ID generation includes small delays to ensure uniqueness
- Position field serves dual purpose as ordering and timestamp