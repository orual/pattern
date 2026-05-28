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

### Existing infrastructure

Most of the infrastructure for undo already exists in `pattern_db`:

- **`MemoryBlockUpdate`**: Stores incremental Loro deltas with `seq` ordering
- **`MemoryBlockCheckpoint`**: Stores full Loro snapshots (for consolidation, not currently used)
- **`get_checkpoint_and_updates()`**: Loads checkpoint + replays deltas to reconstruct state

Currently, consolidation (`consolidate_checkpoint()`) is implemented but **not wired up**—updates accumulate indefinitely. This means we already have full edit history; we just need to expose it for undo.

### Required changes

**1. Add version vector to updates**

Add `frontier: Option<Vec<u8>>` to `MemoryBlockUpdate` to store the version vector after each update:

```sql
ALTER TABLE memory_block_updates ADD COLUMN frontier BLOB;
```

```rust
// In store_update():
pub async fn store_update(
    pool: &SqlitePool,
    block_id: &str,
    update_blob: &[u8],
    frontier: Option<&[u8]>,  // NEW: version vector after this update
    source: Option<&str>,
) -> Result<i64, DbError>;
```

**2. Undo = replay to previous seq**

To undo to before update N:
1. Load latest checkpoint (if any)
2. Replay updates with `seq < N`
3. Use `fork_at()` to get clean doc at that state

```rust
// Undo implementation:
async fn undo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool> {
    let block_id = self.get_block_id(agent_id, label).await?
        .ok_or_else(|| MemoryError::not_found(agent_id, label))?;

    // Get the update we want to revert (most recent)
    let latest_update = pattern_db::queries::get_latest_update(
        self.dbs.constellation.pool(),
        &block_id,
    ).await?;

    let target_seq = match latest_update {
        Some(u) if u.seq > 0 => u.seq - 1,
        _ => return Ok(false), // Nothing to undo
    };

    // Load checkpoint + updates up to target_seq
    let (checkpoint, updates) = pattern_db::queries::get_checkpoint_and_updates_until(
        self.dbs.constellation.pool(),
        &block_id,
        target_seq,
    ).await?;

    // Reconstruct doc at target state
    let doc = reconstruct_doc_at(checkpoint, updates)?;
    let frontier = doc.current_version();

    // Fork to get clean doc (discards ops after target)
    let restored = doc.inner().fork_at(&doc.inner().oplog_frontiers());

    // Update cache and persist
    // ... (update CachedBlock, mark dirty, persist)

    Ok(true)
}
```

**3. New queries**

```rust
/// Get the most recent update for a block
pub async fn get_latest_update(
    pool: &SqlitePool,
    block_id: &str,
) -> Result<Option<MemoryBlockUpdate>, DbError>;

/// Get checkpoint + updates up to (inclusive) a seq number
pub async fn get_checkpoint_and_updates_until(
    pool: &SqlitePool,
    block_id: &str,
    max_seq: i64,
) -> Result<(Option<MemoryBlockCheckpoint>, Vec<MemoryBlockUpdate>), DbError>;

/// Delete updates after a seq number (for committing undo)
pub async fn delete_updates_after(
    pool: &SqlitePool,
    block_id: &str,
    after_seq: i64,
) -> Result<u64, DbError>;
```

### MemoryStore trait additions

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    // ... existing methods ...

    /// Undo the last persisted change to a block.
    /// Returns true if undo was performed, false if no history available.
    async fn undo_block(&self, agent_id: &str, label: &str) -> MemoryResult<bool>;

    /// Get number of available undo steps for a block.
    async fn undo_depth(&self, agent_id: &str, label: &str) -> MemoryResult<usize>;
}
```

### Undo window and consolidation

Currently: unlimited undo (no consolidation running).

When consolidation is added, it should preserve recent updates for undo:

```rust
pub struct ConsolidationConfig {
    /// Keep at least this many recent updates for undo
    pub keep_recent_updates: usize,  // e.g., 50

    /// Keep updates from at least this long ago
    pub keep_recent_duration: Duration,  // e.g., 7 days

    /// Consolidate when update count exceeds this
    pub consolidate_threshold: usize,  // e.g., 100
}
```

Consolidation would then:
1. Create new checkpoint with merged state
2. Delete updates older than `max(keep_recent_updates, keep_recent_duration)`
3. Preserve recent updates for undo capability

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

### Phase 1: Database schema + queries
1. Add migration: `ALTER TABLE memory_block_updates ADD COLUMN frontier BLOB`
2. Update `store_update()` to accept and store frontier
3. Implement `get_latest_update()`, `get_checkpoint_and_updates_until()`, `delete_updates_after()`
4. Run `cargo sqlx prepare` in pattern_db

### Phase 2: MemoryCache undo integration
1. Update `persist()` to pass frontier to `store_update()`
2. Implement `undo_block()` - replay to previous seq, fork, persist
3. Implement `undo_depth()` - count of available updates
4. Add to `MemoryStore` trait

### Phase 3: Tool + testing
1. Create `BlockUndoTool` (or add `undo` operation to BlockEditTool)
2. Register in tool registry
3. Add integration tests for undo scenarios
4. Test edge cases: empty block, no history, rapid edits, undo multiple times

### Phase 4: FileSource integration
1. Ensure file blocks store updates like regular blocks
2. Document the memory-vs-disk behavior after undo
3. Consider adding explicit "revert to disk" operation

### Future: Consolidation with undo preservation
1. Wire up `consolidate_checkpoint()` with configurable retention
2. Add `ConsolidationConfig` to `MemoryCache`
3. Background task or on-persist trigger for consolidation
4. Respect `keep_recent_updates` / `keep_recent_duration` for undo window

## Auto-persist design

### Motivation

The current API requires callers to:
1. Get a `StructuredDocument` from `MemoryStore`
2. Mutate the document
3. Call `memory.mark_dirty(agent_id, label)`
4. Call `memory.persist_block(agent_id, label)`

This is error-prone—callers can forget to mark dirty or persist, leading to data loss. Since `LoroDoc` is Arc-shared internally, mutations to the returned document already propagate back to the cached version. We can leverage Loro's subscription system to detect edits and auto-persist.

### Design: subscription-based auto-persist

When a block is loaded, we subscribe to the underlying `LoroDoc`. When changes are detected (via the subscription callback), we spawn a debounced persist operation.

```
┌─────────────────┐     edit      ┌──────────────┐
│ StructuredDoc   │──────────────▶│   LoroDoc    │
│   (returned)    │               │  (Arc-shared)│
└─────────────────┘               └──────┬───────┘
                                         │
                                         │ commit()
                                         ▼
                                  ┌──────────────┐
                                  │ Subscription │
                                  │   callback   │
                                  └──────┬───────┘
                                         │
                                         │ signals
                                         ▼
                              ┌────────────────────┐
                              │  AutoPersistHandle │
                              │  (debounce timer)  │
                              └──────────┬─────────┘
                                         │
                                         │ after debounce
                                         ▼
                              ┌────────────────────┐
                              │   persist_block()  │
                              └────────────────────┘
```

### CachedBlock additions (revised)

```rust
pub struct CachedBlock {
    /// The Loro-backed document.
    pub doc: StructuredDocument,

    /// Last sequence number from DB updates table.
    pub last_seq: i64,

    /// Version vector at last persist (for delta exports).
    pub last_persisted_frontier: Option<VersionVector>,

    /// Whether there are unpersisted changes.
    pub dirty: bool,

    /// When this block was last accessed.
    pub last_accessed: DateTime<Utc>,

    /// When we last created a checkpoint (for throttling).
    pub last_checkpoint_at: Option<std::time::Instant>,

    // === Auto-persist fields ===

    /// Subscription handle (keeps subscription alive).
    /// Dropped when block is evicted from cache.
    pub(crate) subscription: Option<loro::Subscription>,

    /// Channel sender to signal the debounce task.
    /// None if auto-persist is disabled for this block.
    pub(crate) persist_trigger: Option<tokio::sync::mpsc::Sender<()>>,

    /// Handle to the background debounce task.
    /// Aborted when block is evicted.
    pub(crate) persist_task: Option<tokio::task::JoinHandle<()>>,
}
```

### AutoPersist configuration

```rust
/// Configuration for auto-persist behavior
#[derive(Debug, Clone)]
pub struct AutoPersistConfig {
    /// Enable auto-persist globally. Defaults to true.
    pub enabled: bool,

    /// Debounce interval before persisting after the last edit.
    /// Defaults to 500ms.
    pub debounce_ms: u64,

    /// Maximum time to wait before forcing a persist, even if edits continue.
    /// Prevents infinite debounce with rapid continuous edits.
    /// Defaults to 5 seconds.
    pub max_debounce_ms: u64,
}

impl Default for AutoPersistConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_ms: 500,
            max_debounce_ms: 5000,
        }
    }
}
```

### MemoryCache additions

```rust
impl MemoryCache {
    /// Auto-persist configuration
    auto_persist_config: AutoPersistConfig,

    /// Create with auto-persist enabled (default)
    pub fn with_auto_persist(mut self, config: AutoPersistConfig) -> Self {
        self.auto_persist_config = config;
        self
    }

    /// Setup auto-persist for a cached block.
    /// Called when block is loaded into cache.
    fn setup_auto_persist(&self, block_id: &str, entry: &mut CachedBlock) {
        if !self.auto_persist_config.enabled {
            return;
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<()>(16);

        // Clone what we need for the debounce task
        let block_id = block_id.to_string();
        let agent_id = entry.doc.agent_id().to_string();
        let label = entry.doc.label().to_string();
        let debounce = Duration::from_millis(self.auto_persist_config.debounce_ms);
        let max_debounce = Duration::from_millis(self.auto_persist_config.max_debounce_ms);
        let cache = self.clone(); // MemoryCache needs to be Clone (Arc internally)

        // Spawn debounce task
        let task = tokio::spawn(async move {
            debounce_persist_loop(rx, debounce, max_debounce, cache, agent_id, label).await;
        });

        // Subscribe to document changes
        let tx_clone = tx.clone();
        let subscription = entry.doc.subscribe_root(Arc::new(move |_event| {
            // Non-blocking send - if channel is full, the task will persist soon anyway
            let _ = tx_clone.try_send(());
        }));

        entry.subscription = Some(subscription);
        entry.persist_trigger = Some(tx);
        entry.persist_task = Some(task);
    }
}

/// Background task that debounces persist signals.
async fn debounce_persist_loop(
    mut rx: tokio::sync::mpsc::Receiver<()>,
    debounce: Duration,
    max_debounce: Duration,
    cache: MemoryCache,
    agent_id: String,
    label: String,
) {
    let mut first_signal_at: Option<Instant> = None;

    loop {
        // Wait for first signal
        if rx.recv().await.is_none() {
            // Channel closed, block evicted
            break;
        }

        // Record when we first got a signal in this batch
        let batch_start = Instant::now();
        if first_signal_at.is_none() {
            first_signal_at = Some(batch_start);
        }

        // Debounce: wait for debounce duration or until max_debounce exceeded
        loop {
            let elapsed_since_first = first_signal_at.unwrap().elapsed();
            if elapsed_since_first >= max_debounce {
                // Max debounce exceeded, persist now
                break;
            }

            let remaining_max = max_debounce - elapsed_since_first;
            let timeout = debounce.min(remaining_max);

            match tokio::time::timeout(timeout, rx.recv()).await {
                Ok(Some(())) => {
                    // Got another signal, restart debounce (unless max exceeded)
                    continue;
                }
                Ok(None) => {
                    // Channel closed
                    return;
                }
                Err(_) => {
                    // Timeout expired, debounce complete
                    break;
                }
            }
        }

        // Persist
        cache.mark_dirty(&agent_id, &label);
        if let Err(e) = cache.persist(&agent_id, &label).await {
            tracing::error!(
                agent_id = %agent_id,
                label = %label,
                error = %e,
                "Auto-persist failed"
            );
        } else {
            tracing::debug!(
                agent_id = %agent_id,
                label = %label,
                "Auto-persisted block"
            );
        }

        first_signal_at = None;
    }
}
```

### Subscription lifecycle

1. **On load**: `setup_auto_persist()` creates subscription and debounce task
2. **On edit + commit**: Subscription fires, signals debounce task
3. **On debounce timeout**: Task calls `persist()`
4. **On eviction**: Dropping `CachedBlock` drops `subscription` (unsubscribes) and aborts `persist_task`

Important: Loro subscriptions only fire on `commit()`. Callers must call `doc.commit()` after edits for auto-persist to work. This is already the expected pattern for CRDT semantics.

### Commit convenience

To make auto-persist transparent, mutation methods on `StructuredDocument` could auto-commit:

```rust
impl StructuredDocument {
    /// Set text content and commit (triggers subscriptions).
    pub fn set_text_and_commit(&self, content: &str, is_system: bool) -> Result<(), DocumentError> {
        self.set_text(content, is_system)?;
        self.commit();
        Ok(())
    }

    // Similar for append_text_and_commit, set_field_and_commit, etc.
}
```

Or provide a guard pattern:

```rust
impl StructuredDocument {
    /// Create an auto-committing scope.
    /// Commit is called when the guard is dropped.
    pub fn editing(&self) -> EditGuard<'_> {
        EditGuard { doc: self }
    }
}

pub struct EditGuard<'a> {
    doc: &'a StructuredDocument,
}

impl Drop for EditGuard<'_> {
    fn drop(&mut self) {
        self.doc.commit();
    }
}

// Usage:
{
    let _guard = doc.editing();
    doc.set_text("hello", true)?;
    doc.append_text(" world", true)?;
} // commit() called here, triggers subscription → auto-persist
```

### Explicit persist still supported

Auto-persist is fire-and-forget. For operations that need guaranteed persistence (e.g., before responding to user), callers can still call:

```rust
memory.persist_block(agent_id, label).await?;
```

This immediately persists, canceling any pending debounce.

### Thread safety considerations

- `LoroDoc` is internally thread-safe (uses Arc)
- Subscription callbacks run synchronously in the thread that calls `commit()`
- `mpsc::Sender::try_send` is non-blocking and safe from any thread
- The debounce task runs on the Tokio runtime
- `MemoryCache` uses `DashMap` for concurrent access

### MemoryCache self-reference

The debounce task needs to call `cache.persist()`, but `MemoryCache` isn't currently `Clone`. Options:

1. **Arc\<MemoryCacheInner\>**: Wrap fields in inner struct, make `Clone` cheap. Cleanest approach since `persist()` has non-trivial logic.

2. **Pass components directly**: Give the task `Arc<DashMap>` + `Arc<ConstellationDatabases>`, refactor persist logic into a free function. More churn but better testability.

3. **Weak reference**: Task holds `Weak<MemoryCache>`, upgrades when needed. Gracefully handles cache being dropped, but adds upgrade ceremony on every persist.

**Recommendation**: Option 1 (Arc\<Inner\>). Standard pattern, minimal API change, keeps persist logic encapsulated.

Note: Even if the debounce task somehow outlives the cache, the mpsc channel closes when `CachedBlock` is dropped (which holds the `Sender`), causing the task to exit cleanly via `rx.recv() -> None`.

### Integration with checkpoints (undo)

Auto-persist integrates naturally with checkpoints:
1. Edit detected → subscription fires → signals debounce
2. Debounce timeout → `persist()` called
3. In `persist()`: check if checkpoint is needed (time threshold)
4. If yes: store checkpoint with previous version
5. Export and store delta
6. Update `last_persisted_frontier`

No changes needed to checkpoint logic—it already runs in `persist()`.

### Disabling auto-persist

For specific blocks (e.g., high-frequency scratch buffers):

```rust
impl MemoryStore {
    /// Get a block without setting up auto-persist.
    /// Caller must manually persist changes.
    async fn get_block_manual_persist(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;
}
```

Or via block metadata:

```rust
// In BlockMetadata
pub auto_persist: bool,  // defaults to true
```

### Implementation phases

This feature slots into the existing implementation plan:

#### Phase 2.5: Auto-persist infrastructure
1. Add `AutoPersistConfig` to `MemoryCache`
2. Add subscription/task fields to `CachedBlock`
3. Implement `setup_auto_persist()` and `debounce_persist_loop()`
4. Wire into `load_from_db()` to setup auto-persist on load
5. Clean up subscription/task on cache eviction

#### Phase 2.6: Commit convenience
1. Add `EditGuard` pattern to `StructuredDocument`
2. Update tool implementations to use guard or explicit commit
3. Document the commit requirement in API docs

## Open questions

1. **Checkpoint threshold**: 30 seconds seems reasonable. Make configurable?

2. **Redo support**: Not in initial design. Could add by keeping popped checkpoints in a "redo stack" until next edit clears it.

3. **Checkpoint metadata**: Currently just storing operation name. Could store more context (agent_id, tool call ID, etc.) for audit trails.

4. **Garbage collection**: Checkpoints accumulate forever. Add periodic cleanup of very old checkpoints? Age-based or count-based limit per block?

5. **Auto-persist debounce tuning**: 500ms default feels right for interactive use. May need adjustment for batch operations. Should this be per-block configurable?

6. **Commit ergonomics**: Should mutation methods auto-commit by default? This changes the current behavior where batched edits are possible. The `EditGuard` pattern preserves batching while ensuring commit happens.

## References

- [Loro documentation](https://loro.dev/)
- [Loro Rust docs - VersionVector](https://docs.rs/loro/latest/loro/struct.VersionVector.html)
- [Loro Rust docs - LoroDoc](https://docs.rs/loro/latest/loro/struct.LoroDoc.html)
- [Loro Undo documentation](https://loro.dev/docs/advanced/undo)
- Prior design: `docs/plans/2025-12-27-block-schema-loro-integration-design.md`
