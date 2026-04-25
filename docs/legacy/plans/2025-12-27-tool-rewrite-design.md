# Tool Rewrite & FileSource Design

## Overview

This design describes a new tool taxonomy for the v2 data source system, replacing the current `context` and `recall` tools with a cleaner separation of concerns. It also specifies FileSource as the first concrete DataBlock implementation.

## Tool Taxonomy

Five tools replacing the current system:

| Tool | Purpose | Operations |
|------|---------|------------|
| `block` | Block lifecycle | load, pin, unpin, archive, info |
| `block_edit` | Block content | append, replace, patch, set_field |
| `recall` | Archival entries | insert, search |
| `source` | Data source control | pause, resume, status, list |
| `file` | File disk I/O + edits | load, save, create, delete, append, replace |

### Key Relationships

- `file load` creates a Working block from disk, then `block`/`block_edit` or `file` edit ops work on it
- `block archive` changes BlockType (Working → Archival), does NOT create an archival entry
- `recall insert` creates immutable archival entry (distinct from Archival-typed blocks)
- `source` controls streams (pause/resume), shows status for all source types

### Permission Model

Two layers that compose:
1. **FileSource rules** - Glob patterns → MemoryPermission (what's possible)
2. **Agent tool rules** - `AllowedOperations` (what's allowed for this agent)

### Deprecation Path

- `context` tool remains but becomes outmoded
- New agents use `block`/`block_edit`
- Existing agents migrate over time

---

## Tool Specifications

### `block` Tool

**Purpose:** Manage block lifecycle - loading, pinning, archiving, querying info.

**Operations:**

| Op | Parameters | Description |
|----|------------|-------------|
| `load` | `label`, `source_id?` | Load block into working context. If source_id provided, delegates to source's load. |
| `pin` | `label` | Set `pinned=true`, block stays in context across batches |
| `unpin` | `label` | Set `pinned=false`, block becomes ephemeral |
| `archive` | `label` | Change BlockType from Working → Archival |
| `info` | `label` | Return block metadata: schema, type, permission, size, pinned status |

**Input schema:**
```rust
pub struct BlockInput {
    pub op: BlockOp,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

pub enum BlockOp {
    Load,
    Pin,
    Unpin,
    Archive,
    Info,
}
```

**Tool rules:**
- `ContinueLoop` - conversation continues after any operation
- Operations gated via `AllowedOperations` at agent level

**Implementation notes:**
- Uses `ToolContext::memory()` for block access
- `load` with `source_id` calls `SourceManager::load_block()`
- `info` returns `BlockMetadata` formatted for agent consumption

---

### `block_edit` Tool

**Purpose:** Edit block content with schema-aware operations.

**Operations:**

| Op | Parameters | Description |
|----|------------|-------------|
| `append` | `label`, `content` | Append text to block (Text, List schemas) |
| `replace` | `label`, `old`, `new` | Find and replace text (Text schema initially, expandable) |
| `patch` | `label`, `patch` | Apply diff/patch (advanced capability) |
| `set_field` | `label`, `field`, `value` | Set specific field (Map, Composite schemas) |

**Input schema:**
```rust
pub struct BlockEditInput {
    pub op: BlockEditOp,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

pub enum BlockEditOp {
    Append,
    Replace,
    Patch,
    SetField,
}
```

**Schema compatibility:**
- `append` → Text, List
- `replace` → Text (expandable to other schemas later)
- `set_field` → Map, Composite (respects `read_only` field flag)
- `patch` → Text (requires AdvancedEdit capability)

**Permission enforcement:**
- Checks `MemoryPermission` on target block
- `read_only` fields reject `set_field`
- May trigger consent request for Human/Partner permission levels

**Tool rules:**
- `ContinueLoop`
- `AllowedOperations` gates which ops agent can use

---

### `recall` Tool

**Purpose:** Create and search immutable archival entries.

**Operations:**

| Op | Parameters | Description |
|----|------------|-------------|
| `insert` | `content`, `metadata?` | Create new immutable archival entry |
| `search` | `query`, `limit?` | Full-text search over archival entries |

**Input schema:**
```rust
pub struct RecallInput {
    pub op: RecallOp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

pub enum RecallOp {
    Insert,
    Search,
}
```

**Output:**
- `insert` returns entry ID and confirmation
- `search` returns `Vec<ArchivalSearchResult>` with label, content, timestamps, relevance score

**Notes:**
- Entries are immutable once created - no update/delete ops
- Uses existing `insert_archival` / `search_archival` on MemoryStore
- Archival-typed blocks may also surface in search results (TBD)
- Simpler than current `recall` which has Append/Read/Delete

**Tool rules:**
- `ContinueLoop`

---

### `source` Tool

**Purpose:** Control data sources and view status.

**Operations:**

| Op | Parameters | Description |
|----|------------|-------------|
| `pause` | `source_id` | Pause notifications from a stream source |
| `resume` | `source_id` | Resume notifications |
| `status` | `source_id?` | Get status of one source or all |
| `list` | - | List all registered sources |

**Input schema:**
```rust
pub struct SourceInput {
    pub op: SourceOp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

pub enum SourceOp {
    Pause,
    Resume,
    Status,
    List,
}
```

**Output:**
- `pause`/`resume` - confirmation message
- `status` - `StreamSourceInfo` or `BlockSourceInfo` (type, name, status, schemas)
- `list` - all sources with type indicator (stream vs block)

**Stream vs Block behavior:**
- `pause`/`resume` only valid for streams
- For block sources, return informative message with current state (watching status, loaded blocks)
- `status`/`list` work for both source types

**Implementation:**
- Uses `ToolContext::sources()` → `SourceManager`
- Delegates to `pause_stream()`, `resume_stream()`, etc.

**Tool rules:**
- `ContinueLoop`

---

### `file` Tool

**Purpose:** File disk I/O and editing operations for FileSource.

**Operations:**

| Op | Parameters | Description |
|----|------------|-------------|
| `load` | `path` | Load file from disk into Working block |
| `save` | `path` or `label` | Persist block content back to disk |
| `create` | `path`, `content?` | Create new file (and block) |
| `delete` | `path` | Delete file from disk (escalation likely) |
| `append` | `path`, `content` | Append to file (operates on block) |
| `replace` | `path`, `old`, `new` | Find/replace in file |

**Input schema:**
```rust
pub struct FileInput {
    pub op: FileOp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
}

pub enum FileOp {
    Load,
    Save,
    Create,
    Delete,
    Append,
    Replace,
}
```

**Block label convention:** `file:{hash8}:{relative_path}`
- `hash8` is truncated hash of absolute path
- Avoids collisions with similarly named files in different locations
- Agent sees relative path, internally resolves unambiguously

**Permission enforcement:**
- FileSource checks glob rules before any operation
- `delete` typically requires escalation
- Agent tool rules can further restrict

**Sync model (v1):**
- `load` reads disk → creates/updates block
- `save` writes block → disk
- No auto-reconcile; agent manages sync explicitly
- Clear error if disk changed since load (mtime check)

**Implementation:**
- Registered by FileSource when active
- Uses FileSource methods: `load()`, `save()`, `create()`, `delete()`
- Edit ops delegate to underlying block via MemoryStore

---

## FileSource Implementation

**Purpose:** First concrete DataBlock implementation - local filesystem with Loro overlay.

### Core Structure

```rust
pub struct FileSource {
    source_id: String,
    base_path: PathBuf,
    permission_rules: Vec<PermissionRule>,

    // Loaded file tracking
    loaded_blocks: DashMap<PathBuf, LoadedFileInfo>,
}

struct LoadedFileInfo {
    block_id: String,
    label: String,
    disk_mtime: SystemTime,
    disk_hash: u64,  // For conflict detection
}
```

### DataBlock Trait Implementation

- `load()` - Read file, create Text block with Loro backing, track mtime/hash
- `save()` - Check mtime, write block content to disk, update tracking
- `create()` - Create file + block
- `delete()` - Permission check (likely escalation), remove file, cleanup block
- `reconcile()` - Compare disk vs Loro, apply resolution strategy

### Conflict Detection (v1 On-Demand)

- `save` checks if `disk_mtime` changed since load
- If changed: error with message "file modified externally, use `file load` to refresh"
- No auto-merge in v1 - agent decides

### Permission Integration

- `permission_for(path)` matches against glob rules
- Returns `MemoryPermission` level
- Checked before load/save/create/delete

### Tool Registration

- FileSource registers `file` tool when started
- Tool rules derived from permission_rules

---

## Integration

### ToolContext Integration

All new tools receive `Arc<dyn ToolContext>` (same pattern as existing):
- Access memory via `ctx.memory()`
- Access sources via `ctx.sources()` → `SourceManager`
- Permission broker via `ctx.permission_broker()`

### BuiltinTools Update

```rust
impl BuiltinTools {
    pub fn v2_tools(ctx: Arc<dyn ToolContext>) -> Self {
        Self {
            block_tool: BlockTool::new(ctx.clone()),
            block_edit_tool: BlockEditTool::new(ctx.clone()),
            recall_tool: RecallTool::new(ctx.clone()),
            source_tool: SourceTool::new(ctx.clone()),
            // file tool registered dynamically by FileSource
        }
    }
}
```

### Dynamic Tool Registration

- FileSource registers `file` tool via `ToolRegistry` when started
- Other DataBlock sources can register their own tools
- DataStream sources may also register source-specific tools

### Migration Path

1. Implement new tools alongside existing
2. New agents configured with v2 tools
3. Existing agents continue using `context`/`recall`
4. Gradually migrate as agents are updated
5. Eventually deprecate old tools

---

## Open Questions / Future Work

1. **Watch-driven reconciliation** - Background file watching with auto-reconcile (v2)
2. **Shell hook integration** - Pre/post hooks for permission enforcement
3. **Dynamic config updates** - Hot-reload of permission rules
4. **`replace` expansion** - Support other schema types if parseable
5. **Archival block search** - Whether `recall search` also surfaces Archival-typed blocks

## Not In Scope

- BlueskySource migration (separate effort, different atproto lib)
- DiscordSource migration
- MCP integration with sources
