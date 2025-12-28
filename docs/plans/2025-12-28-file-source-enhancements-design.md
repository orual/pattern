# FileSource & Block Editing Enhancements Design

## Overview

This design enhances the FileSource implementation and block editing utilities to provide:
- Automatic bidirectional sync between memory blocks and disk via Loro subscriptions
- File watching with permission-based conflict resolution
- Smarter text editing using Loro's native operations
- Unified diff format for input and output

## Core Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      Agent                              │
│                        │                                │
│                   FileTool                              │
│          (load/save/list/status/diff/patch)             │
└─────────────────────────────────────────────────────────┘
                         │
┌─────────────────────────────────────────────────────────┐
│                    FileSource                           │
│  ┌──────────────────────────────────────────────────┐   │
│  │              LoadedFileInfo                      │   │
│  │  ┌──────────────┐      ┌──────────────┐          │   │
│  │  │  memory_doc  │◄──►│   disk_doc   │          │   │
│  │  │  (LoroDoc)   │      │  (forked)    │          │   │
│  │  └──────────────┘      └──────┬───────┘          │   │
│  │         │                     │                  │   │
│  │         │ subscribe_local_update (bidirectional) │   │
│  │         │                     │                  │   │
│  │         ▼                    ▼                 │   │
│  │    Agent edits ──────► Auto-save to disk        │   │
│  └──────────────────────────────────────────────────┘   │
│                                                         │
│  ┌──────────────────────────────────────────────────┐   │
│  │              File Watcher (notify)               │   │
│  │   Disk changes ──► update disk_doc ──► memory  │   │
│  └──────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

## Key Insight: Fork + Subscribe

Rather than manual reconciliation, we leverage Loro's CRDT capabilities:

1. On load, fork the memory doc to create a disk doc (shared history)
2. Subscribe to local updates on both docs
3. Memory changes → import to disk_doc → auto-save to filesystem
4. Disk changes detected → update disk_doc → import to memory_doc

`subscribe_local_update` only fires on local changes, not imports, preventing loops.

---

## Data Structures

### LoadedFileInfo

```rust
struct LoadedFileInfo {
    block_id: String,
    label: String,
    disk_mtime: SystemTime,
    disk_size: u64,

    // Loro docs - forked, kept in sync via subscriptions
    disk_doc: LoroDoc,

    // Keep subscriptions alive
    _subscriptions: (Subscription, Subscription),

    // For explicit save tracking (if needed)
    last_saved_frontier: VersionVector,
}
```

### FileSource

```rust
pub struct FileSource {
    source_id: String,
    base_path: PathBuf,
    permission_rules: Vec<PermissionRule>,
    loaded_blocks: DashMap<PathBuf, LoadedFileInfo>,
    status: AtomicU8,

    // File watching
    watcher: Option<Arc<RecommendedWatcher>>,
    change_tx: Option<broadcast::Sender<FileChange>>,
}
```

---

## FileTool Operations

### Extended FileOp Enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOp {
    // Existing
    Load,
    Save,
    Create,
    Delete,
    Append,
    Replace,

    // New
    List,    // List files in source
    Status,  // Check sync status
    Diff,    // Show changes vs disk
    Reload,  // Discard changes, re-read from disk
}
```

### Extended FileInput (Flat Structure)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileInput {
    pub op: FileOp,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,

    // New fields
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,  // Glob for list op
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_loaded: Option<bool>,  // For list op
}
```

### Operation Details

**list** - Discover files in source
- `pattern`: Optional glob (e.g., `"**/*.rs"`)
- Returns: Array of `{path, size, permission, loaded, dirty}`

**status** - Check sync state
- `path`: Optional, specific file or all loaded
- Returns: Array of `{path, label, sync_status, disk_mtime}`

**diff** - Show changes between block and disk
- `path`: Required
- Returns: Unified diff with metadata header (in `message` field)

**reload** - Discard block changes, re-read from disk
- `path`: Required
- Returns: Confirmation

---

## Diff Output Format

Unified diff with metadata header:

```
[file:abc123:src/lib.rs]
status: block_modified
disk_mtime: 2025-12-28T10:30:00Z
block_version: a1b2c3d4
---
--- disk
+++ block
@@ -10,3 +10,4 @@
 context
-old line
+new line
+added line
```

---

## Smarter Replace Operation

Current implementation reads all content, does string replace, writes all content. This loses CRDT granularity.

### Position Type Conversion

**Critical**: Loro's `delete()` and `insert()` use **Unicode character indices**, not UTF-8 byte indices. Rust's `str::find()` and `str::len()` return byte indices. Use `LoroText::convert_pos()` to convert:

```rust
pub fn convert_pos(
    &self,
    index: usize,
    from: PosType,
    to: PosType,
) -> Option<usize>

pub enum PosType {
    Bytes,      // UTF-8 byte offset (what Rust str methods return)
    Unicode,    // Unicode scalar value count (what Loro uses internally)
    Utf16,      // UTF-16 code unit count
    Event,      // Event-based position
    Entity,     // Entity-based position
}
```

### New Approach

Use Loro's native `delete()` + `insert()` with proper position conversion:

```rust
pub fn replace_text(
    &self,
    find: &str,
    replace: &str,
    is_system: bool,
) -> Result<bool, DocumentError> {
    self.check_permission(pattern_db::models::MemoryOp::Overwrite, is_system)?;

    let text = self.doc.get_text("content");
    let current = text.to_string();

    if let Some(byte_pos) = current.find(find) {
        // Convert byte indices to Unicode character indices
        let unicode_pos = text.convert_pos(byte_pos, PosType::Bytes, PosType::Unicode)
            .ok_or_else(|| DocumentError::InvalidPosition(byte_pos))?;

        // Calculate find length in bytes, then convert to unicode
        let find_byte_end = byte_pos + find.len();
        let unicode_end = text.convert_pos(find_byte_end, PosType::Bytes, PosType::Unicode)
            .ok_or_else(|| DocumentError::InvalidPosition(find_byte_end))?;
        let unicode_len = unicode_end - unicode_pos;

        // Surgical edit at found position (using Unicode indices)
        // splice(pos, delete_len, insert_str) is atomic
        text.splice(unicode_pos, unicode_len, replace)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
```

### Benefits

- Proper CRDT semantics - each op is discrete in history
- Better merge behavior for concurrent edits
- Accurate attribution tracking
- Foundation for stable cursors
- Correct handling of multi-byte UTF-8 characters (emoji, CJK, etc.)

---

## File Watching

### Watcher Setup

```rust
impl FileSource {
    pub fn start_watching(&mut self) -> Result<broadcast::Receiver<FileChange>> {
        let (tx, rx) = broadcast::channel(256);

        let tx_clone = tx.clone();
        let loaded_blocks = self.loaded_blocks.clone();

        let watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                for path in event.paths {
                    // Only emit for tracked files
                    if loaded_blocks.contains_key(&path) {
                        let change = FileChange {
                            path: path.clone(),
                            change_type: event.kind.into(),
                            block_id: loaded_blocks.get(&path)
                                .map(|info| info.block_id.clone()),
                            timestamp: Some(Utc::now()),
                        };
                        let _ = tx_clone.send(change);
                    }
                }
            }
        })?;

        watcher.watch(&self.base_path, RecursiveMode::Recursive)?;

        self.watcher = Some(Arc::new(watcher));
        self.change_tx = Some(tx);
        self.status.store(STATUS_WATCHING, Ordering::SeqCst);

        Ok(rx)
    }
}
```

### On File Change

```rust
async fn handle_file_change(&self, path: &Path) -> Result<()> {
    let disk_content = tokio::fs::read_to_string(path).await?;

    if let Some(mut info) = self.loaded_blocks.get_mut(path) {
        // Update disk doc - subscription auto-syncs to memory
        let disk_text = info.disk_doc.get_text("content");
        disk_text.update(&disk_content, Default::default())?;
        info.disk_doc.commit();
        info.disk_mtime = self.get_file_metadata(path).await?.0;
    }

    Ok(())
}
```

---

## Load with Fork + Subscribe

```rust
async fn load(&self, path: &Path, ctx: Arc<dyn ToolContext>, owner: AgentId) -> Result<BlockRef> {
    let content = tokio::fs::read_to_string(&abs_path).await?;
    let (mtime, size) = self.get_file_metadata(path).await?;

    // Create/update memory block
    let memory = ctx.memory();
    // ... existing block creation logic ...

    let doc = memory.get_block(agent_id, &label).await?.unwrap();
    let memory_loro = doc.inner().clone();

    // Fork to create disk doc (shares history)
    let disk_doc = memory_loro.fork();

    // Subscribe: memory changes → import to disk_doc → auto-save
    let disk_clone = disk_doc.clone();
    let path_clone = abs_path.clone();
    let mem_sub = memory_loro.subscribe_local_update(Box::new(move |update| {
        let _ = disk_clone.import(&update);
        let content = disk_clone.get_text("content").to_string();
        let path = path_clone.clone();
        tokio::spawn(async move {
            let _ = tokio::fs::write(&path, &content).await;
        });
    }));

    // Subscribe: disk changes → import to memory (no write-back)
    let mem_clone = memory_loro.clone();
    let disk_sub = disk_doc.subscribe_local_update(Box::new(move |update| {
        let _ = mem_clone.import(&update);
    }));

    self.loaded_blocks.insert(abs_path.clone(), LoadedFileInfo {
        block_id: block_id.clone(),
        label: label.clone(),
        disk_mtime: mtime,
        disk_size: size,
        disk_doc,
        _subscriptions: (mem_sub, disk_sub),
        last_saved_frontier: memory_loro.oplog_vv(),
    });

    Ok(block_ref)
}
```

---

## Permission-Based Subscription Setup

Different permissions get different subscription configurations:

```rust
match permission {
    MemoryPermission::ReadOnly => {
        // Disk → memory only (agent can't edit)
        let mem_clone = memory_loro.clone();
        let disk_sub = disk_doc.subscribe_local_update(Box::new(move |update| {
            let _ = mem_clone.import(&update);
        }));
        // No memory → disk subscription
    }

    MemoryPermission::ReadWrite => {
        // Bidirectional - disk changes win conflicts (CRDT merge)
        // Both subscriptions as shown above
    }

    MemoryPermission::Admin => {
        // Bidirectional - agent changes win conflicts (CRDT merge)
        // Both subscriptions as shown above
        // Agent edits have "priority" in CRDT semantics
    }

    MemoryPermission::Human | MemoryPermission::Partner => {
        // Bidirectional, but conflicts surfaced to agent
        // May need additional conflict detection logic
    }
}
```

---

## Patch Operation (Unified Diff Input)

### BlockEditOp Extension

```rust
pub enum BlockEditOp {
    Append,
    Replace,
    Patch,     // Accepts unified diff
    SetField,
}
```

### Parsing & Application

```rust
async fn handle_patch(&self, label: &str, patch: Option<String>) -> Result<ToolOutput> {
    let patch = patch.ok_or_else(|| /* error */)?;

    let doc = memory.get_block(agent_id, label).await?.ok_or_else(|| /* error */)?;
    let text = doc.inner().get_text("content");
    let current = text.to_string();

    // Parse hunks from unified diff
    let hunks = parse_unified_diff(&patch)?;

    // Apply hunks in reverse order (so line numbers stay valid)
    for hunk in hunks.into_iter().rev() {
        let start_line = hunk.old_start - 1; // 0-indexed

        // Calculate byte offset for the start of the target line
        let byte_offset = line_to_byte_offset(&current, start_line);

        // Calculate byte length of content to delete
        let delete_byte_len: usize = hunk.old_lines.iter()
            .map(|l| l.len() + 1) // +1 for newline
            .sum();

        // Convert byte positions to Unicode character positions
        let unicode_start = text.convert_pos(byte_offset, PosType::Bytes, PosType::Unicode)
            .ok_or_else(|| PatchError::InvalidPosition(byte_offset))?;
        let unicode_end = text.convert_pos(byte_offset + delete_byte_len, PosType::Bytes, PosType::Unicode)
            .ok_or_else(|| PatchError::InvalidPosition(byte_offset + delete_byte_len))?;
        let unicode_delete_len = unicode_end - unicode_start;

        // Atomic splice: delete old content and insert new (using Unicode indices)
        let new_content = hunk.new_lines.join("\n") + "\n";
        text.splice(unicode_start, unicode_delete_len, &new_content)?;
    }

    memory.persist_block(agent_id, label).await?;

    Ok(ToolOutput::success(format!(
        "Applied {} hunks to '{}'",
        hunks.len(),
        label
    )))
}

/// Calculate byte offset for the start of a given line (0-indexed)
fn line_to_byte_offset(content: &str, target_line: usize) -> usize {
    content.lines()
        .take(target_line)
        .map(|l| l.len() + 1) // +1 for newline
        .sum()
}
```

### DiffHunk Struct

```rust
struct DiffHunk {
    old_start: usize,   // 1-indexed line number
    old_count: usize,
    new_start: usize,
    new_count: usize,
    old_lines: Vec<String>,  // Lines from '-' or ' ' prefix
    new_lines: Vec<String>,  // Lines from '+' or ' ' prefix
}
```

---

## Implementation Phases

### Phase 1 - Foundation & Safety

- [X] Add `list` operation to FileTool
- [X] Add `status` operation
- [X] Smarter `replace` using Loro's `splice()` with `convert_pos()`
- [X] Fix auto-load to not overwrite (check if already loaded)

### Phase 2 - Watching & Auto-Sync

- [ ] Fork disk_doc on load
- [ ] Bidirectional subscriptions with auto-save
- [ ] File watching via `notify` crate
- [ ] Permission-based subscription setup
- [ ] `diff` operation with unified format + metadata header
- [ ] `reload` operation

### Phase 3 - Smart Editing

- [ ] `patch` operation accepting unified diff
- [ ] `replace` options: first/all/nth, regex
- [ ] Line-range editing via BlockEditInput

### Phase 4 - History & Undo (Future)

- [ ] Version history via Loro frontiers
- [ ] `history` / `rollback` operations
- [ ] Undo/redo exposure

---

## Open Questions

1. **Line endings** - `lines()` handles `\n` and `\r\n`, but trailing newline handling may need care
2. **Large file handling** - May need streaming for very large files
3. **Binary files** - Currently Text schema only; binary files need different handling
4. **Subscription lifecycle** - Need to clean up subscriptions when blocks are unloaded
5. **Splice error handling** - Verify `splice()` returns appropriate errors for out-of-bounds positions

---

## References

- [Loro Documentation](https://docs.rs/loro/latest/loro/)
- [LoroText API](https://docs.rs/loro/latest/loro/struct.LoroText.html)
- [notify crate](https://docs.rs/notify/latest/notify/)
- [Unified Diff Format](https://www.gnu.org/software/diffutils/manual/html_node/Unified-Format.html)
