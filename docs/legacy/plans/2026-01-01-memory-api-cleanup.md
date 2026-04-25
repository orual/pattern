# MemoryStore API cleanup

**Date**: 2026-01-01
**Status**: Design
**Authors**: Human + Claude
**Related**: [Persistent undo design](./2026-01-01-persistent-undo-design.md) (benefits from this cleanup but doesn't depend on it)

## Summary

Clean up the MemoryStore API by embedding block metadata in StructuredDocument, changing return types, and deprecating convenience methods that obscure the get→mutate→persist pattern.

## Motivation

The current API has several pain points that make code awkward and inefficient:

### Problem 1: Metadata/document split causes double DB hits

`get_block()` returns `Option<StructuredDocument>` and `get_block_metadata()` returns `Option<BlockMetadata>`. Many callsites need both:

```rust
// Current pattern - two round trips
let metadata = memory.get_block_metadata(agent_id, label).await?;
let doc = memory.get_block(agent_id, label).await?;
```

### Problem 2: create_block returns ID, but you usually want the document

```rust
// Current pattern - create then immediately fetch
let block_id = memory.create_block(agent_id, label, desc, block_type, schema, limit).await?;
let doc = memory.get_block(agent_id, label).await?.unwrap();
doc.set_text("initial content", true)?;
memory.persist_block(agent_id, label).await?;
```

### Problem 3: Convenience methods obscure the data flow

`update_block_text`, `append_to_block`, `replace_in_block` exist for convenience but:
- Hide the get→mutate→persist pattern
- Don't compose well with StructuredDocument's richer operations
- Make features like checkpointing awkward to integrate

## Design

### Embed metadata in StructuredDocument

Currently StructuredDocument has:
```rust
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,
    permission: MemoryPermission,
    label: String,
    accessor_agent_id: Option<String>,
}
```

Expand to include full block metadata:
```rust
pub struct StructuredDocument {
    doc: LoroDoc,

    // Schema and permission (already present)
    schema: BlockSchema,
    permission: MemoryPermission,

    // Block identity (partially present)
    label: String,
    accessor_agent_id: Option<String>,

    // NEW: Full metadata
    id: String,
    agent_id: String,
    description: String,
    block_type: BlockType,
    char_limit: usize,
    pinned: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
```

Add accessors:
```rust
impl StructuredDocument {
    /// Get block metadata as a struct (clones string fields)
    pub fn metadata(&self) -> BlockMetadata {
        BlockMetadata {
            id: self.id.clone(),
            agent_id: self.agent_id.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            block_type: self.block_type,
            schema: self.schema.clone(),
            char_limit: self.char_limit,
            permission: self.permission,
            pinned: self.pinned,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    // Direct accessors (no allocation)
    pub fn id(&self) -> &str { &self.id }
    pub fn agent_id(&self) -> &str { &self.agent_id }
    pub fn description(&self) -> &str { &self.description }
    pub fn block_type(&self) -> BlockType { self.block_type }
    pub fn char_limit(&self) -> usize { self.char_limit }
    pub fn is_pinned(&self) -> bool { self.pinned }
}
```

### Update MemoryStore signatures

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    /// Create a new memory block, returning the document ready for editing.
    /// The returned document includes all metadata.
    async fn create_block(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
        block_type: BlockType,
        schema: BlockSchema,
        char_limit: usize,
    ) -> MemoryResult<StructuredDocument>;  // Was: MemoryResult<String>

    /// Get a block's document (includes all metadata).
    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>>;  // Unchanged signature, but doc now has metadata

    /// Get block metadata without loading the Loro document.
    /// Useful for listing/filtering when you don't need the content.
    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>>;  // Keep for lightweight queries

    // ... other methods unchanged ...
}
```

### Deprecate convenience methods

```rust
// In MemoryStore trait:

#[deprecated(
    since = "0.x.0",
    note = "Use get_block() then doc.set_text() then persist_block()"
)]
async fn update_block_text(
    &self,
    agent_id: &str,
    label: &str,
    new_content: &str,
) -> MemoryResult<()>;

#[deprecated(
    since = "0.x.0",
    note = "Use get_block() then doc.append_text() then persist_block()"
)]
async fn append_to_block(
    &self,
    agent_id: &str,
    label: &str,
    content: &str,
) -> MemoryResult<()>;

#[deprecated(
    since = "0.x.0",
    note = "Use get_block() then doc.replace_text() then persist_block()"
)]
async fn replace_in_block(
    &self,
    agent_id: &str,
    label: &str,
    old: &str,
    new: &str,
) -> MemoryResult<bool>;
```

Keep them working during transition, remove in a later release.

### CachedBlock simplification

With metadata in StructuredDocument, CachedBlock becomes a cache-tracking wrapper:

```rust
pub struct CachedBlock {
    /// The document with embedded metadata
    pub doc: StructuredDocument,

    /// Cache management state
    pub last_seq: i64,
    pub last_persisted_frontier: Option<VersionVector>,
    pub dirty: bool,
    pub last_accessed: DateTime<Utc>,

    /// For checkpoint throttling (see persistent-undo-design.md)
    pub last_checkpoint_at: Option<Instant>,
}
```

The duplicated metadata fields (`id`, `agent_id`, `label`, etc.) can be removed - access via `doc.id()`, `doc.agent_id()`, etc.

## Callsites to update

Based on grep, ~30+ locations use the convenience methods or need signature updates:

**CLI** (`pattern_cli/src/commands/`):
- `debug.rs:689` - `update_block_text`
- `builder/group.rs:1086,1088` - `update_block_text`
- `builder/agent.rs:996` - `update_block_text`

**Tools** (`pattern_core/src/tool/builtin/`):
- `block_edit.rs` - tests use `update_block_text` extensively
- `block.rs:776` - `update_block_text`
- `file.rs:455,778,1050,1246,1296` - all three convenience methods

**Data sources** (`pattern_core/src/data_source/`):
- `file_source.rs:955,1182,1201,1319,1842,1896` - `update_block_text`
- `helpers.rs:139` - `update_block_text`

**Runtime/context** (`pattern_core/src/runtime/`, `context/`):
- Various delegate implementations that just forward calls

**Tests throughout**

## Implementation plan

### Step 1: Expand StructuredDocument
1. Add new fields to struct
2. Add `metadata()` and accessor methods
3. Update all constructors (`new`, `new_with_identity`, `new_with_permission`, `from_snapshot`, etc.)
4. Ensure Clone still works correctly (LoroDoc is Arc-shared)

### Step 2: Update MemoryCache
1. Modify `load_from_db` to populate metadata in returned doc
2. Change `create_block` impl to return `StructuredDocument`
3. Simplify CachedBlock (remove duplicated fields, access via doc)
4. Update cache hit path in `get()` to ensure metadata is current

### Step 3: Update MemoryStore trait
1. Change `create_block` signature
2. Add `#[deprecated]` to convenience methods
3. Update trait implementations (MemoryCache, any mocks)

### Step 4: Update callsites
1. Fix all `create_block` callsites (now returns doc, not ID)
2. Migrate convenience method calls to get→mutate→persist pattern
3. Remove double-fetch patterns where both metadata and doc were fetched
4. Update tests

### Step 5: Cleanup
1. Remove deprecated method implementations (later release)
2. Remove unused CachedBlock fields
3. Update documentation

## Future: Auto-persistence

The explicit `persist_block()` call is easy to forget. Future options:

**Periodic flush:**
```rust
impl MemoryCache {
    /// Persist all dirty blocks
    pub async fn flush_dirty(&self) -> MemoryResult<()>;

    /// Start background task that flushes periodically
    pub async fn start_auto_flush(&self, interval: Duration);
}
```

**RAII guard:**
```rust
// Persist automatically when guard drops
let mut block = memory.get_block_mut(agent_id, label).await?;
block.set_text("new content", false)?;
// persist on drop
```

This is out of scope for this cleanup but worth considering.

## Testing

- Ensure all existing tests pass after migration
- Add tests for new accessors
- Verify no performance regression from metadata in every doc

## References

- Current MemoryStore trait: `crates/pattern_core/src/memory/store.rs`
- Current StructuredDocument: `crates/pattern_core/src/memory/document.rs`
- Current MemoryCache: `crates/pattern_core/src/memory/cache.rs`
