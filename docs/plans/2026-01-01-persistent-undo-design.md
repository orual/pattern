# Persistent undo for Loro-backed memory blocks

**Date**: 2026-01-01
**Status**: Design
**Authors**: Human + Claude

## Summary

Add persistent undo capability to Pattern's CRDT-based memory system. Leverage Loro's versioning primitives (`VersionVector`, `fork_at`) to enable agents and users to revert block edits that survive process restarts.

## Background

Pattern uses [Loro](https://loro.dev/) CRDTs for memory blocks via `StructuredDocument`. Currently we track `last_persisted_frontier` for delta exports but don't expose any undo/time-travel features to agents or tools. The design doc from 2025-12-27 mentioned `UndoManager` integration but it was never implemented.

### Current architecture

- **StructuredDocument** (`memory/document.rs`): Wraps `LoroDoc` with schema-aware operations (Text, Map, List, Log, Composite). Handles permissions. No undo support.
- **MemoryCache** (`memory/cache.rs`): Stores `CachedBlock` instances, loads from DB with snapshot + deltas, persists changes. Tracks `last_persisted_frontier: Option<VersionVector>`.
- **FileSource** (`data_source/file_source.rs`): Manages local files as Loro-backed blocks. Maintains `disk_doc` (fork) and `memory_doc` (clone), tracks `last_saved_frontier`.
- **BlockEditTool** (`tool/builtin/block_edit.rs`): Agent-facing tool for edits (append, replace, patch, set_field, edit_range). No undo.

## Options considered

### Option A: Loro UndoManager (session-only)

Loro provides `UndoManager` for per-peer undo/redo:

```rust
let undo = UndoManager::new(&doc);
doc.get_text("content").insert(0, "hello")?;
doc.commit();
undo.undo();  // Reverts the insert
```

**Pros:**
- Simple API, works out of the box
- Merge interval for batching rapid edits
- `group_start()`/`group_end()` for explicit grouping

**Cons:**
- **In-memory only** - doesn't survive process restarts
- Per-peer scoped (fine for us since agents own their blocks)
- Would need to create/store managers per block

**Verdict:** Rejected as primary solution. Could be used alongside persistent undo for quick session-based undo, but we need persistence.

### Option B: Time travel with checkout

Loro's `checkout(&frontiers)` rewinds document state:

```rust
let old_frontiers = doc.state_frontiers();
// ... make edits ...
doc.checkout(&old_frontiers);  // Document now read-only at old state
doc.attach();  // Return to latest
```

**Pros:**
- Can view any historical state
- Precise control over versions

**Cons:**
- Document becomes detached (read-only) after checkout
- Need `set_detached_editing(true)` to allow edits at historical points
- Doesn't cleanly support "undo and continue editing from there"

**Verdict:** Useful for viewing history but not ideal for undo workflow.

### Option C: Fork-based restore (selected)

Use `fork_at(&frontiers)` to create a new document at a historical state:

```rust
let checkpoint_vv = VersionVector::decode(&stored_bytes)?;
let frontiers = doc.vv_to_frontiers(&checkpoint_vv);
let restored = doc.fork_at(&frontiers);  // New doc at old state, ready for editing
```

**Pros:**
- Clean mental model: undo creates fresh doc at old state
- New doc is immediately editable (no detached mode)
- VersionVector is serializable (`encode()`/`decode()`) for persistence
- Discards operations after checkpoint (true undo, not just viewing)

**Cons:**
- Loses history after checkpoint (by design for undo)
- Need to manage checkpoint storage

**Verdict:** Selected. Clean, persistent, matches undo semantics.

### Option D: Hybrid (UndoManager + checkpoints)

Combine session-based UndoManager for quick undo with persisted checkpoints for named save points.

**Verdict:** Could layer this on later. Start with Option C.

## Design

### Checkpoint storage

New table in constellation database:

```sql
CREATE TABLE memory_block_checkpoints (
    id TEXT PRIMARY KEY DEFAULT (uuid_generate_v4()),
    block_id TEXT NOT NULL REFERENCES memory_blocks(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    version_blob BLOB NOT NULL,  -- VersionVector.encode()
    operation TEXT,              -- What triggered this ("replace", "append", etc.)
    seq INTEGER NOT NULL,        -- Ordering within block
    UNIQUE(block_id, seq)
);

CREATE INDEX idx_checkpoints_block_seq ON memory_block_checkpoints(block_id, seq DESC);
```

### Checkpoint creation (throttled)

Checkpoints are created at **persist time**, not edit time. This captures the state before the pending batch of changes:

1. Block loaded at V1, `last_persisted_frontier = V1`
2. Agent makes edits (doc now at V2)
3. `persist_block()` called
4. Check: time since last checkpoint > threshold (default 30s)?
5. If yes → store checkpoint with `version = V1.encode()` (the "before" state)
6. Persist delta V1→V2
7. Update `last_persisted_frontier = V2`

This approach:
- Requires no hooks in StructuredDocument mutation methods
- Batches rapid edits naturally
- Checkpoints represent "last saved state" (intuitive mental model)

### CachedBlock additions

```rust
pub struct CachedBlock {
    // ... existing fields ...

    /// When we last created a checkpoint (for throttling)
    pub last_checkpoint_at: Option<std::time::Instant>,
}
```

### MemoryStore trait additions

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    // ... existing methods ...

    /// Undo the last checkpointed change to a block.
    /// Returns true if undo was performed, false if no checkpoints available.
    async fn undo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool>;

    /// Get number of available undo checkpoints for a block.
    async fn undo_count(&self, agent_id: &str, label: &str) -> MemoryResult<usize>;
}
```

### MemoryCache implementation

```rust
impl MemoryCache {
    /// Checkpoint interval - only create checkpoint if this much time has passed
    const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);

    /// Maximum checkpoints to load when block is accessed
    const MAX_LOADED_CHECKPOINTS: usize = 20;
}

// In persist():
async fn persist(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
    // ... existing logic to get entry and check dirty ...

    let should_checkpoint = entry.dirty && match entry.last_checkpoint_at {
        None => true,
        Some(last) => last.elapsed() > Self::CHECKPOINT_INTERVAL,
    };

    if should_checkpoint {
        if let Some(frontier) = &entry.last_persisted_frontier {
            pattern_db::queries::store_checkpoint(
                self.dbs.constellation.pool(),
                &entry.id,
                &frontier.encode(),
                "edit",  // Could be more specific based on context
            ).await?;
            entry.last_checkpoint_at = Some(Instant::now());
        }
    }

    // ... rest of persist logic ...
}

// Undo implementation:
async fn undo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool> {
    let block_id = /* get from DB */;

    let checkpoint = pattern_db::queries::pop_checkpoint(
        self.dbs.constellation.pool(),
        &block_id,
    ).await?;

    match checkpoint {
        Some(cp) => {
            let vv = VersionVector::decode(&cp.version)
                .map_err(|e| MemoryError::Other(format!("Invalid checkpoint: {}", e)))?;
            let frontiers = entry.doc.inner().vv_to_frontiers(&vv);
            let restored_loro = entry.doc.inner().fork_at(&frontiers);

            // Reconstruct StructuredDocument with same metadata
            let restored = StructuredDocument::from_loro_doc(
                restored_loro,
                entry.doc.schema().clone(),
                entry.doc.permission(),
                entry.doc.label().to_string(),
                entry.doc.accessor_agent_id().map(String::from),
            );

            // Replace in cache
            entry.doc = restored;
            entry.dirty = true;
            entry.last_persisted_frontier = Some(vv);

            // Persist immediately
            drop(entry);  // Release lock
            self.persist(agent_id, label).await?;

            Ok(true)
        }
        None => Ok(false),
    }
}
```

### pattern_db queries

```rust
/// Store a checkpoint for a block
pub async fn store_checkpoint(
    pool: &SqlitePool,
    block_id: &str,
    version_blob: &[u8],
    operation: &str,
) -> Result<i64, DbError>;

/// Pop (retrieve and delete) the most recent checkpoint
pub async fn pop_checkpoint(
    pool: &SqlitePool,
    block_id: &str,
) -> Result<Option<Checkpoint>, DbError>;

/// Get recent checkpoints without removing them
pub async fn get_recent_checkpoints(
    pool: &SqlitePool,
    block_id: &str,
    limit: usize,
) -> Result<Vec<Checkpoint>, DbError>;

/// Delete checkpoints older than a sequence number
pub async fn delete_checkpoints_before(
    pool: &SqlitePool,
    block_id: &str,
    seq: i64,
) -> Result<u64, DbError>;
```

### Tool: block_undo

Either add `undo` operation to BlockEditTool or create a new tool:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockUndoInput {
    /// Label of the block to undo
    pub label: String,
}

pub struct BlockUndoTool {
    ctx: Arc<dyn ToolContext>,
}

#[async_trait]
impl AiTool for BlockUndoTool {
    type Input = BlockUndoInput;
    type Output = ToolOutput;

    fn name(&self) -> &str { "block_undo" }

    fn description(&self) -> &str {
        "Undo the last change to a memory block, restoring it to its previous state."
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        let memory = self.ctx.memory();
        let agent_id = self.ctx.agent_id();

        let undone = memory.undo_block(agent_id, &input.label).await?;

        if undone {
            Ok(ToolOutput::success(format!(
                "Undid last change to block '{}'",
                input.label
            )))
        } else {
            Err(CoreError::tool_exec_msg(
                "block_undo",
                json!({"label": input.label}),
                "No undo history available for this block",
            ))
        }
    }
}
```

### FileSource considerations

For file-backed blocks, undo is more complex:

1. **Memory side**: Restore memory_doc via `fork_at` (same as regular blocks)
2. **Disk side**: Do NOT automatically revert disk file
   - External editors may have modified the file
   - User should explicitly save to disk if they want to propagate the undo

After undo, file block would be in "memory ahead of disk" state. User/agent can:
- Call save to write reverted content to disk
- Or discard memory changes and reload from disk

This matches the existing conflict model.

## Related: MemoryStore API cleanup

This undo work is related to but separate from a broader API cleanup effort. See [memory-api-cleanup.md](./2026-01-01-memory-api-cleanup.md) for details on:

- Embedding full block metadata in StructuredDocument
- Changing `create_block` to return the document (not just ID)
- Deprecating convenience methods (`update_block_text`, `append_to_block`, `replace_in_block`)
- Pushing callers toward get→mutate→persist pattern

The API cleanup benefits the undo implementation (cleaner document access, simpler CachedBlock) but isn't a strict prerequisite. These can be done in parallel or sequenced either way.

## Implementation plan

### Phase 1: Database schema + queries for checkpoints
1. Add migration for `memory_block_checkpoints` table
2. Implement `store_checkpoint`, `pop_checkpoint`, `get_recent_checkpoints` in pattern_db

### Phase 2: MemoryCache checkpoint integration
1. Add `last_checkpoint_at` field to `CachedBlock`
2. Modify `persist()` to create checkpoints based on time threshold
3. Implement `undo_block()` and `undo_count()` methods
4. Add `StructuredDocument::from_loro_doc()` constructor for restore

### Phase 3: Tool + testing
1. Create `BlockUndoTool` (or add operation to BlockEditTool)
2. Register in tool registry
3. Add integration tests for undo scenarios
4. Test edge cases: empty block, no checkpoints, rapid edits

### Phase 4: FileSource integration
1. Ensure file blocks get checkpointed like regular blocks
2. Document the memory-vs-disk behavior after undo
3. Consider adding explicit "revert to disk" operation

## Open questions

1. **Checkpoint threshold**: 30 seconds seems reasonable. Make configurable?

2. **Redo support**: Not in initial design. Could add by keeping popped checkpoints in a "redo stack" until next edit clears it.

3. **Checkpoint metadata**: Currently just storing operation name. Could store more context (agent_id, tool call ID, etc.) for audit trails.

4. **Garbage collection**: Checkpoints accumulate forever. Add periodic cleanup of very old checkpoints? Age-based or count-based limit per block?

## References

- [Loro documentation](https://loro.dev/)
- [Loro Rust docs - VersionVector](https://docs.rs/loro/latest/loro/struct.VersionVector.html)
- [Loro Rust docs - LoroDoc](https://docs.rs/loro/latest/loro/struct.LoroDoc.html)
- [Loro Undo documentation](https://loro.dev/docs/advanced/undo)
- Prior design: `docs/plans/2025-12-27-block-schema-loro-integration-design.md`
