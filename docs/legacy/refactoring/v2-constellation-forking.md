# Pattern v2.1: Constellation Forking (Future)

> **Status**: Concept for future implementation. Not part of v2 initial release.

## Overview

Explicit forking and merging of constellations (or subsets thereof) for isolated work with optional reintegration.

## Use Cases

1. **Experimental work** - Try something risky without polluting main constellation history
2. **Focused task** - Spin off agents for a specific project, less noise from other activity
3. **Collaboration** - Fork, share with someone, merge their contributions back
4. **Rollback** - If the fork goes badly, just discard it
5. **Templates** - Fork a "clean" constellation as starting point for new projects

## Fork Specification

```rust
pub struct ForkSpec {
    /// Source constellation
    pub source: ConstellationId,
    
    /// Which agents to include (None = all)
    pub agents: Option<Vec<AgentId>>,
    
    /// How much history to bring
    pub history: HistorySpec,
    
    /// Which memory blocks to copy
    pub memory: MemorySpec,
    
    /// Include shared resources (folders, coordination state)?
    pub include_shared: bool,
}

pub enum HistorySpec {
    /// No history, just current memory state
    None,
    /// Last N messages per agent
    Recent(usize),
    /// Everything since timestamp
    Since(DateTime<Utc>),
    /// Full history
    Full,
}

pub enum MemorySpec {
    /// Core blocks only (persona, human)
    CoreOnly,
    /// Core + working
    CoreAndWorking,
    /// Everything including archival
    Full,
    /// Specific blocks by label
    Specific(Vec<String>),
}
```

## Merge Specification

```rust
pub struct MergeSpec {
    /// Fork to merge from
    pub source: ConstellationId,
    
    /// Target constellation
    pub target: ConstellationId,
    
    /// What to merge
    pub merge: MergeContent,
    
    /// Conflict resolution strategy
    pub conflicts: ConflictResolution,
}

pub enum MergeContent {
    /// Just memories (learnings), not conversation history
    MemoriesOnly,
    /// Memories + LLM-generated summary of what happened
    MemoriesAndSummary,
    /// Full history appended as delineated section
    FullHistory,
}

pub enum ConflictResolution {
    /// Fork wins (overwrite target)
    PreferFork,
    /// Target wins (keep target, discard fork conflicts)
    PreferTarget,
    /// Interactive resolution (prompt user)
    Interactive,
    /// Keep both versions (create separate blocks)
    KeepBoth,
}
```

## Memory Block Merge Semantics

Loro CRDT enables automatic merging of memory blocks:

```rust
async fn merge_memory_block(
    source_block: &MemoryBlock,
    target_block: &MemoryBlock,
) -> Result<MemoryBlock> {
    // Loro documents can merge!
    let mut merged_doc = target_block.document.clone();
    merged_doc.merge(&source_block.document)?;
    
    // Result contains both histories, conflicts auto-resolved
    Ok(MemoryBlock {
        document: merged_doc,
        ..target_block.clone()
    })
}
```

Merge rules:
- **Block only in fork** → Copy to target
- **Block only in target** → Keep as-is
- **Block in both** → Loro CRDT merge (or interactive if structural conflict)

## History Merge Options

### Option 1: Append as Fork Session

Clearly delineated in history:

```
[normal history]
--- Fork: refactor-task (3 days) ---
[fork history]
--- End Fork ---
[continues]
```

### Option 2: Summarize (Recommended Default)

LLM generates summary of fork activity:

```
[normal history]
[System: During "refactor-task" fork, coder refactored the memory system 
 and entropy broke it into 12 subtasks. Key learnings were saved to archival.]
[continues]
```

### Option 3: Memories Only

Don't merge history at all, just memory block updates. Fork history is discarded.

## CLI Interface

```bash
# Create fork
$ pattern fork my-constellation --agents coder entropy --name refactor-task
Created fork: refactor-task (2 agents, core+working memory, recent 100 messages)

# Work in fork
$ pattern chat --constellation refactor-task --agent coder

# Check fork status
$ pattern fork status refactor-task
Fork: refactor-task
Source: my-constellation
Created: 2 days ago
Agents: coder (847 new messages), entropy (234 new messages)
Memory changes: 12 blocks modified, 3 new archival entries

# Merge back
$ pattern merge refactor-task --into my-constellation --summarize
Merging fork...
- 12 memory blocks merged (Loro CRDT)
- 3 archival entries copied
- Summary generated and inserted into history
Done.

# Or discard
$ pattern fork delete refactor-task
Deleted fork: refactor-task
```

## Dialect Integration

Agents could fork/merge via dialect:

```
/fork agents coder entropy as "refactor-task"
/fork status "refactor-task"
/merge "refactor-task" summarize
/fork delete "refactor-task"
```

This would require appropriate permissions - probably partner-only.

## Implementation Considerations

1. **Storage** - Fork creates new constellation DB (same as regular constellation)
2. **Tracking** - Source constellation ID stored in fork metadata
3. **Divergence tracking** - Record "fork point" for merge operations
4. **Shared resources** - Folders, data sources need explicit handling
5. **Coordination state** - Probably shouldn't be forked (or reset to clean state)

## Relationship to Existing Concurrent Processing

Pattern already handles concurrent message processing via batch isolation:

```
Timeline (actual):
  t=0: msg A arrives, batch A starts processing
  t=1: msg B arrives, batch B starts processing  
  t=5: batch B completes (faster response)
  t=8: batch A completes

Agent's history (reconstructed):
  [prior history]
  batch A messages (t=0)  
  batch B messages (t=1)
```

This is **implicit, automatic forking** for concurrent requests. The explicit forking described in this document is for **intentional, longer-lived divergence** with controlled merge back.

## Open Questions

1. **Nested forks** - Can you fork a fork?
2. **Partial merge** - Merge some agents/blocks but not others?
3. **Fork sharing** - Export fork for someone else to work on?
4. **Long-lived forks** - At what point is it just a new constellation?
5. **Sync during fork** - Should fork see updates from source? (Probably not - that's merge)
