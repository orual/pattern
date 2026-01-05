# Data Source v2 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement the new DataStream and DataBlock traits with source management integrated into RuntimeContext, enabling event-driven and document-oriented data sources with Loro-backed versioning.

**Architecture:**
- Two trait families: `DataStream` (events) and `DataBlock` (documents)
- Source management lives on `RuntimeContext` (no separate coordinator)
- `SourceManager` trait exposes source operations, `RuntimeContext` implements it
- `ToolContext` gains `sources() -> &dyn SourceManager` for tool/source access
- `AgentRuntime` holds `Weak<RuntimeContext>` to delegate source operations
- Sources receive `Arc<dyn ToolContext>` just like tools—full access to memory, model, router, sources

**Tech Stack:** Rust, tokio broadcast channels, Loro CRDT (existing), serde_json for flexible payloads, globset for path matching.

**Scope Boundaries:**
- ✅ In scope: Traits, types, SourceManager, RuntimeContext integration, helper utilities
- ⚠️ Possibly separate: Full tool implementations (block, block_edit, source, recall)
- ❌ Out of scope: Specific source implementations (Bluesky, LSP, File), shell hook integration

---

## Task 1: Core Types Module

**Files:**
- Create: `crates/pattern_core/src/data_source/types.rs`
- Modify: `crates/pattern_core/src/data_source/mod.rs` (replace v1 with new types)

**Step 1: Stub out old code, create new module structure**

First, rename any existing files that will break:
```bash
# Stub out old implementations by renaming
mv crates/pattern_core/src/data_source/traits.rs crates/pattern_core/src/data_source/traits.rs.old
mv crates/pattern_core/src/data_source/coordinator.rs crates/pattern_core/src/data_source/coordinator.rs.old
# etc. for any files that won't compile with new structure
```

Then update the module:
```rust
// crates/pattern_core/src/data_source/mod.rs
//! Data Sources - Event-driven (DataStream) and Document-oriented (DataBlock) sources.
//!
//! Key design points:
//! - No generics on traits (type safety at source boundary)
//! - Sources receive `Arc<dyn ToolContext>` - same access as tools
//! - Channel-based notifications with BlockRef references
//! - Loro-backed versioning for DataBlock sources
//! - Source management lives on RuntimeContext

mod types;
mod stream;
mod block;
mod manager;

pub use types::*;
pub use stream::DataStream;
pub use block::DataBlock;
pub use manager::{SourceManager, StreamSourceInfo, BlockSourceInfo, EditFeedback, BlockEdit};
```

**Step 2: Write core types**

```rust
// crates/pattern_core/src/data_source/types.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::memory::BlockSchema;
use crate::Message;
use crate::SnowflakePosition;

/// Reference to a block in memory store
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct BlockRef {
    /// Human-readable label for context display
    pub label: String,
    /// Database block ID
    pub block_id: String,
    /// Owner agent ID, defaults to "_constellation_" for shared blocks
    pub agent_id: String,
}

impl BlockRef {
    pub fn new(label: impl Into<String>, block_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            block_id: block_id.into(),
            agent_id: "_constellation_".to_string(),
        }
    }

    pub fn owned_by(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }
}

/// Notification delivered to agent via broadcast channel
#[derive(Debug, Clone)]
pub struct Notification {
    /// Full Message type - supports text, images, multi-modal content
    pub message: Message,
    /// Blocks to load for this batch (already exist in memory store)
    pub block_refs: Vec<BlockRef>,
    /// Batch to associate these blocks with
    pub batch_id: SnowflakePosition,
}

/// Opaque cursor for pull-based stream access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamCursor(pub String);

impl StreamCursor {
    pub fn new(cursor: impl Into<String>) -> Self {
        Self(cursor.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Schema specification for blocks a source creates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSchemaSpec {
    /// Label pattern: exact "lsp_diagnostics" or templated "bluesky_user_{handle}"
    pub label_pattern: String,
    /// Schema definition
    pub schema: BlockSchema,
    /// Human-readable description
    pub description: String,
    /// Whether blocks are created pinned (always in context) or ephemeral
    pub pinned: bool,
}

impl BlockSchemaSpec {
    pub fn pinned(label: impl Into<String>, schema: BlockSchema, description: impl Into<String>) -> Self {
        Self {
            label_pattern: label.into(),
            schema,
            description: description.into(),
            pinned: true,
        }
    }

    pub fn ephemeral(label_pattern: impl Into<String>, schema: BlockSchema, description: impl Into<String>) -> Self {
        Self {
            label_pattern: label_pattern.into(),
            schema,
            description: description.into(),
            pinned: false,
        }
    }
}

/// Internal event from streaming source (before formatting)
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub event_type: String,
    pub payload: serde_json::Value,
    pub cursor: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub source_id: String,
}

/// Tool rule for dynamic registration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRule {
    pub tool_name: String,
    pub allowed_operations: Option<Vec<String>>,
    pub metadata: Option<serde_json::Value>,
}

impl ToolRule {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            allowed_operations: None,
            metadata: None,
        }
    }

    pub fn with_operations(mut self, ops: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_operations = Some(ops.into_iter().map(Into::into).collect());
        self
    }
}
```

**Step 3: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS (no compilation errors)

**Step 4: Commit**

```bash
git add crates/pattern_core/src/data_source/
git add crates/pattern_core/src/data_source/mod.rs
git commit -m "feat(data_source): add core types module

Adds BlockRef, Notification, StreamCursor, BlockSchemaSpec, StreamEvent,
and ToolRule types for the new data source architecture."
```

---

## Task 2: DataStream Trait

**Files:**
- Create: `crates/pattern_core/src/data_source/stream.rs`
- Modify: `crates/pattern_core/src/data_source/mod.rs` (already exports)

**Step 1: Write the DataStream trait**

```rust
// crates/pattern_core/src/data_source/stream.rs
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::runtime::ToolContext;
use crate::AgentId;
use crate::error::Result;

use super::{BlockSchemaSpec, Notification, StreamCursor, ToolRule};

/// Event-driven data source that produces notifications and manages state blocks.
///
/// Sources receive `Arc<dyn ToolContext>` on start(), giving them the same access
/// as tools: memory, router, model provider, and source management. This enables
/// sources to create blocks, route messages, classify events with LLM, and even
/// coordinate with other sources.
///
/// # Block Lifecycle
///
/// - **Pinned blocks** (`pinned=true`): Always in agent context while subscribed
/// - **Ephemeral blocks** (`pinned=false`): Loaded for the batch that references them,
///   then drop out of context (but remain in store)
///
/// # Example
///
/// ```ignore
/// impl DataStream for BlueskySource {
///     async fn start(&mut self, ctx: Arc<dyn ToolContext>, owner: AgentId)
///         -> Result<broadcast::Receiver<Notification>>
///     {
///         // Create pinned config block via memory
///         let memory = ctx.memory();
///         let config_id = memory.create_block(&owner, "bluesky_config", ...).await?;
///
///         // Can also access model for classification, router for messages, etc.
///         // let model = ctx.model();
///         // let sources = ctx.sources(); // manage other sources
///
///         // Spawn event processor that sends Notifications
///         let (tx, rx) = broadcast::channel(256);
///         // ... spawn task ...
///         Ok(rx)
///     }
/// }
/// ```
#[async_trait]
pub trait DataStream: Send + Sync {
    /// Unique identifier for this stream source
    fn source_id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    // === Schema Declarations ===

    /// Block schemas this source creates (for documentation/validation)
    fn block_schemas(&self) -> Vec<BlockSchemaSpec>;

    /// Tool rules required while subscribed
    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]
    }

    // === Lifecycle ===

    /// Start the source, returns broadcast receiver for notifications.
    ///
    /// Source receives full ToolContext access - memory, model, router, sources.
    /// The receiver is used by RuntimeContext to route notifications to agents.
    async fn start(
        &mut self,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>>;

    /// Stop the source and cleanup resources
    async fn stop(&mut self) -> Result<()>;

    // === Control ===

    /// Pause notification emission (source may continue processing internally)
    fn pause(&mut self);

    /// Resume notification emission
    fn resume(&mut self);

    /// Check if currently paused
    fn is_paused(&self) -> bool;

    // === Optional Pull Support ===

    /// Whether this source supports on-demand pull (for backfill/history)
    fn supports_pull(&self) -> bool {
        false
    }

    /// Pull notifications on demand
    ///
    /// Returns notifications from history, useful for backfilling or
    /// paginating through past events.
    async fn pull(
        &self,
        _limit: usize,
        _cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>> {
        Ok(vec![])
    }
}
```

**Step 2: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/stream.rs
git commit -m "feat(data_source): add DataStream trait

Event-driven source trait with broadcast channel notifications,
direct memory store access, and optional pull support."
```

---

## Task 3: DataBlock Trait - Permission Types

**Files:**
- Create: `crates/pattern_core/src/data_source/block.rs`

**Step 1: Write permission and file change types**

```rust
// crates/pattern_core/src/data_source/block.rs
use std::path::Path;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::memory::{BlockSchema, MemoryPermission, MemoryStore};
use crate::AgentId;
use crate::error::Result;

use super::{BlockRef, ToolRule};

/// Permission rule for path-based access control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Glob pattern: "*.config.toml", "src/**/*.rs"
    pub pattern: String,
    /// Permission level for matching paths
    pub permission: MemoryPermission,
    /// Operations that require human escalation even with write permission
    pub operations_requiring_escalation: Vec<String>,
}

impl PermissionRule {
    pub fn new(pattern: impl Into<String>, permission: MemoryPermission) -> Self {
        Self {
            pattern: pattern.into(),
            permission,
            operations_requiring_escalation: vec![],
        }
    }

    pub fn with_escalation(mut self, ops: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.operations_requiring_escalation = ops.into_iter().map(Into::into).collect();
        self
    }

    /// Check if a path matches this rule's glob pattern
    pub fn matches(&self, path: &str) -> bool {
        glob_match(&self.pattern, path)
    }
}

/// Type of file change detected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileChangeType {
    Modified,
    Created,
    Deleted,
}

/// File change event from watching or reconciliation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub change_type: FileChangeType,
    /// Block ID if we have a loaded block for this path
    pub block_id: Option<String>,
}

/// Version history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version_id: String,
    pub timestamp: DateTime<Utc>,
    pub description: Option<String>,
}

/// How a conflict was resolved during reconciliation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictResolution {
    /// External (disk) changes won
    DiskWins,
    /// Agent's Loro changes won
    AgentWins,
    /// CRDT merge applied
    Merge,
    /// Could not auto-resolve, needs human decision
    Conflict {
        disk_summary: String,
        agent_summary: String,
    },
}

/// Result of reconciling disk state with Loro overlay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReconcileResult {
    /// Successfully resolved
    Resolved {
        path: String,
        resolution: ConflictResolution,
    },
    /// Needs manual resolution
    NeedsResolution {
        path: String,
        disk_changes: String,
        agent_changes: String,
    },
    /// No changes detected
    NoChange { path: String },
}

/// Simple glob matching (can be replaced with globset for full support)
fn glob_match(pattern: &str, path: &str) -> bool {
    // Basic implementation - handles * and ** patterns
    // For production, use globset crate
    if pattern == "**" {
        return true;
    }

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    glob_match_parts(&pattern_parts, &path_parts)
}

fn glob_match_parts(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            // ** matches zero or more path segments
            if pattern.len() == 1 {
                true
            } else {
                // Try matching rest of pattern at each position
                (0..=path.len()).any(|i| glob_match_parts(&pattern[1..], &path[i..]))
            }
        }
        (Some(p), Some(s)) => {
            if glob_match_segment(p, s) {
                glob_match_parts(&pattern[1..], &path[1..])
            } else {
                false
            }
        }
        _ => false,
    }
}

fn glob_match_segment(pattern: &str, segment: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    // Handle *.ext patterns
    if let Some(suffix) = pattern.strip_prefix('*') {
        return segment.ends_with(suffix);
    }

    // Handle prefix.* patterns
    if let Some(prefix) = pattern.strip_suffix('*') {
        return segment.starts_with(prefix);
    }

    pattern == segment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("foo.rs", "foo.rs"));
        assert!(!glob_match("foo.rs", "bar.rs"));
    }

    #[test]
    fn test_glob_match_star() {
        assert!(glob_match("*.rs", "foo.rs"));
        assert!(glob_match("*.rs", "bar.rs"));
        assert!(!glob_match("*.rs", "foo.txt"));
    }

    #[test]
    fn test_glob_match_doublestar() {
        assert!(glob_match("src/**/*.rs", "src/foo.rs"));
        assert!(glob_match("src/**/*.rs", "src/bar/baz.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/c/d.rs"));
        assert!(!glob_match("src/**/*.rs", "test/foo.rs"));
    }

    #[test]
    fn test_glob_match_all() {
        assert!(glob_match("**", "anything/at/all.txt"));
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p pattern_core glob_match`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/block.rs
git commit -m "feat(data_source): add DataBlock permission types

PermissionRule with glob matching, FileChange, VersionInfo,
ConflictResolution, and ReconcileResult types."
```

---

## Task 4: DataBlock Trait - Full Definition

**Files:**
- Modify: `crates/pattern_core/src/data_source/block.rs`

**Step 1: Add the DataBlock trait definition**

Append to `block.rs`:

```rust
/// Document-oriented data source with Loro-backed versioning.
///
/// Presents files and persistent documents as memory blocks with gated edits,
/// version history, and rollback capabilities. Agent works with these like
/// documents, pulling content when needed.
///
/// # Sync Model
///
/// ```text
/// Agent tools ←→ Loro ←→ Disk ←→ Editor (ACP)
///                  ↑
///             Shell side effects
/// ```
///
/// - **Loro as working state**: Agent's view with full version history
/// - **Disk as canonical**: External changes win via reconcile
/// - **Permission-gated writes**: Glob patterns determine access levels
///
/// # Example
///
/// ```ignore
/// impl DataBlock for FileSource {
///     async fn load(&self, path: &str, ctx: &dyn ToolContext, owner: AgentId)
///         -> Result<BlockRef>
///     {
///         let content = tokio::fs::read_to_string(path).await?;
///         let memory = ctx.memory();
///         let block_id = memory.create_block(&owner, &format!("file:{}", path), ...).await?;
///         memory.update_block_text(&owner, &format!("file:{}", path), &content).await?;
///         Ok(BlockRef::new(format!("file:{}", path), block_id).owned_by(owner))
///     }
/// }
/// ```
#[async_trait]
pub trait DataBlock: Send + Sync {
    /// Unique identifier for this block source
    fn source_id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Schema for content (typically Text for files)
    fn schema(&self) -> BlockSchema;

    /// Permission rules (glob patterns → permission levels)
    fn permission_rules(&self) -> &[PermissionRule];

    /// Tools required when working with this source
    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]
    }

    /// Check if path matches this source's scope
    fn matches(&self, path: &str) -> bool;

    /// Get permission for a specific path
    fn permission_for(&self, path: &str) -> MemoryPermission;

    // === Load/Save Operations ===

    /// Load file content into memory store as a block
    async fn load(
        &self,
        path: &str,
        ctx: &dyn ToolContext,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Create a new file with optional initial content
    async fn create(
        &self,
        path: &str,
        initial_content: Option<&str>,
        ctx: &dyn ToolContext,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Save block back to disk (permission-gated)
    async fn save(
        &self,
        block_ref: &BlockRef,
        ctx: &dyn ToolContext,
    ) -> Result<()>;

    /// Delete file (usually requires escalation)
    async fn delete(
        &self,
        path: &str,
        ctx: &dyn ToolContext,
    ) -> Result<()>;

    // === Watch/Reconcile ===

    /// Start watching for external changes (optional)
    ///
    /// Returns a receiver that emits FileChange events when files are
    /// modified externally (by shell, editor, or other processes).
    fn start_watch(&mut self) -> Option<broadcast::Receiver<FileChange>>;

    /// Stop watching for changes
    fn stop_watch(&mut self);

    /// Reconcile disk state with Loro overlay after external changes
    ///
    /// Called by shell tool after command execution, or by watch handler.
    /// Uses fork-and-compare to determine resolution.
    async fn reconcile(
        &self,
        paths: &[String],
        ctx: &dyn ToolContext,
    ) -> Result<Vec<ReconcileResult>>;

    // === History Operations ===

    /// Get version history for a loaded block
    async fn history(
        &self,
        block_ref: &BlockRef,
        ctx: &dyn ToolContext,
    ) -> Result<Vec<VersionInfo>>;

    /// Rollback to a previous version
    async fn rollback(
        &self,
        block_ref: &BlockRef,
        version: &str,
        ctx: &dyn ToolContext,
    ) -> Result<()>;

    /// Diff between versions or current vs disk
    ///
    /// - `from: None` = disk state
    /// - `to: None` = current Loro state
    async fn diff(
        &self,
        block_ref: &BlockRef,
        from: Option<&str>,
        to: Option<&str>,
        ctx: &dyn ToolContext,
    ) -> Result<String>;
}
```

**Step 2: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/block.rs
git commit -m "feat(data_source): add DataBlock trait

Document-oriented source trait with Loro versioning, permission-gated
writes, file watching, reconciliation, and history operations."
```

---

## Task 5: SourceManager Trait

**Files:**
- Create: `crates/pattern_core/src/data_source/manager.rs`
- Modify: `crates/pattern_core/src/data_source/mod.rs`

**Step 1: Define SourceManager trait**

```rust
// crates/pattern_core/src/data_source/manager.rs
//! SourceManager trait - the interface for source operations exposed to tools and sources.

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::AgentId;
use crate::error::Result;

use super::{
    BlockRef, BlockSchemaSpec, FileChange, Notification, PermissionRule,
    ReconcileResult, StreamCursor, VersionInfo,
};
use crate::memory::BlockSchema;

/// Info about a registered stream source
#[derive(Debug, Clone)]
pub struct StreamSourceInfo {
    pub source_id: String,
    pub name: String,
    pub block_schemas: Vec<BlockSchemaSpec>,
    pub is_running: bool,
    pub is_paused: bool,
    pub supports_pull: bool,
}

/// Info about a registered block source
#[derive(Debug, Clone)]
pub struct BlockSourceInfo {
    pub source_id: String,
    pub name: String,
    pub schema: BlockSchema,
    pub permission_rules: Vec<PermissionRule>,
    pub is_watching: bool,
}

/// Feedback from source after handling a block edit
#[derive(Debug, Clone)]
pub enum EditFeedback {
    /// Edit was applied successfully
    Applied { message: Option<String> },
    /// Edit is pending (async operation)
    Pending { message: Option<String> },
    /// Edit was rejected
    Rejected { reason: String },
}

/// Block edit event for routing to sources
#[derive(Debug, Clone)]
pub struct BlockEdit {
    pub agent_id: AgentId,
    pub block_id: String,
    pub block_label: String,
    pub field: Option<String>,
    pub old_value: Option<serde_json::Value>,
    pub new_value: serde_json::Value,
}

/// Interface for source management operations.
///
/// Implemented by RuntimeContext. Exposed to tools and sources via ToolContext.
#[async_trait]
pub trait SourceManager: Send + Sync {
    // === Stream Source Operations ===

    /// List registered stream sources
    fn list_streams(&self) -> Vec<String>;

    /// Get stream source info
    fn get_stream_info(&self, source_id: &str) -> Option<StreamSourceInfo>;

    /// Pause a stream source (stops notifications, source may continue internally)
    fn pause_stream(&self, source_id: &str) -> Result<()>;

    /// Resume a stream source
    fn resume_stream(&self, source_id: &str) -> Result<()>;

    /// Subscribe agent to a stream source
    async fn subscribe_to_stream(
        &self,
        agent_id: &AgentId,
        source_id: &str,
    ) -> Result<broadcast::Receiver<Notification>>;

    /// Unsubscribe agent from a stream source
    async fn unsubscribe_from_stream(
        &self,
        agent_id: &AgentId,
        source_id: &str,
    ) -> Result<()>;

    /// Pull from a stream source (if supported)
    async fn pull_from_stream(
        &self,
        source_id: &str,
        limit: usize,
        cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>>;

    // === Block Source Operations ===

    /// List registered block sources
    fn list_block_sources(&self) -> Vec<String>;

    /// Get block source info
    fn get_block_source_info(&self, source_id: &str) -> Option<BlockSourceInfo>;

    /// Load a file/document through a block source
    async fn load_block(
        &self,
        source_id: &str,
        path: &str,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Create a new file/document
    async fn create_block(
        &self,
        source_id: &str,
        path: &str,
        content: Option<&str>,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Save block back to external storage
    async fn save_block(
        &self,
        source_id: &str,
        block_ref: &BlockRef,
    ) -> Result<()>;

    /// Reconcile after external changes
    async fn reconcile_blocks(
        &self,
        source_id: &str,
        paths: &[String],
    ) -> Result<Vec<ReconcileResult>>;

    /// Get version history
    async fn block_history(
        &self,
        source_id: &str,
        block_ref: &BlockRef,
    ) -> Result<Vec<VersionInfo>>;

    /// Rollback to previous version
    async fn rollback_block(
        &self,
        source_id: &str,
        block_ref: &BlockRef,
        version: &str,
    ) -> Result<()>;

    /// Diff between versions
    async fn diff_block(
        &self,
        source_id: &str,
        block_ref: &BlockRef,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<String>;

    // === Block Edit Routing ===

    /// Handle a block edit, routing to interested sources
    async fn handle_block_edit(&self, edit: &BlockEdit) -> Result<EditFeedback>;
}
```

**Step 2: Update mod.rs**

Add to `crates/pattern_core/src/data_source/mod.rs`:
```rust
mod manager;
pub use manager::{
    SourceManager, StreamSourceInfo, BlockSourceInfo,
    EditFeedback, BlockEdit,
};
```

**Step 3: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/data_source/manager.rs
git add crates/pattern_core/src/data_source/mod.rs
git commit -m "feat(data_source): add SourceManager trait

Interface for source operations exposed to tools and sources.
Covers stream lifecycle, block operations, and edit routing."
```

---

## Task 6: Extend ToolContext with sources()

**Files:**
- Modify: `crates/pattern_core/src/runtime/tool_context.rs`

**Step 1: Add sources() method to ToolContext trait**

```rust
// Add import at top
use crate::data_source::SourceManager;

// Add to trait definition
#[async_trait]
pub trait ToolContext: Send + Sync {
    // ... existing methods ...

    /// Get the source manager for data source operations
    ///
    /// Returns None if source management is not available (e.g., during tests
    /// or when RuntimeContext is not connected).
    fn sources(&self) -> Option<&dyn SourceManager>;
}
```

**Step 2: Run check**

Run: `cargo check -p pattern_core`
Expected: FAIL - AgentRuntime doesn't implement sources() yet

**Step 3: Commit (partial)**

```bash
git add crates/pattern_core/src/runtime/tool_context.rs
git commit -m "feat(runtime): add sources() to ToolContext trait

Exposes SourceManager to tools and sources via ToolContext."
```

---

## Task 7: Add Weak<RuntimeContext> to AgentRuntime

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs` (AgentRuntime struct)
- Modify: `crates/pattern_core/src/runtime/builder.rs` (if separate)

**Step 1: Add runtime_context field to AgentRuntime**

```rust
use std::sync::Weak;

pub struct AgentRuntime {
    // ... existing fields ...

    /// Weak reference to RuntimeContext for constellation-level operations
    /// Used for source management, cross-agent communication, etc.
    runtime_context: Option<Weak<RuntimeContext>>,
}
```

**Step 2: Add builder method**

```rust
impl AgentRuntimeBuilder {
    /// Set the runtime context (weak reference to avoid cycles)
    pub fn runtime_context(mut self, ctx: Weak<RuntimeContext>) -> Self {
        self.runtime_context = Some(ctx);
        self
    }
}
```

**Step 3: Implement sources() for AgentRuntime**

```rust
impl ToolContext for AgentRuntime {
    // ... existing implementations ...

    fn sources(&self) -> Option<&dyn SourceManager> {
        self.runtime_context
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .map(|arc| arc.as_ref() as &dyn SourceManager)
            // Problem: can't return reference to temporary Arc
    }
}
```

**Note:** The lifetime issue here requires a different approach. Options:
1. Store `Arc<RuntimeContext>` (creates cycle, need care)
2. Return `Option<Arc<dyn SourceManager>>` instead of reference
3. Use interior caching pattern

**Step 3 (revised): Change return type to Arc**

```rust
// In tool_context.rs, change the method signature:
fn sources(&self) -> Option<Arc<dyn SourceManager>>;

// In AgentRuntime implementation:
fn sources(&self) -> Option<Arc<dyn SourceManager>> {
    self.runtime_context
        .as_ref()
        .and_then(|weak| weak.upgrade())
        .map(|arc| arc as Arc<dyn SourceManager>)
}
```

**Step 4: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS (after RuntimeContext implements SourceManager)

**Step 5: Commit**

```bash
git add crates/pattern_core/src/runtime/
git commit -m "feat(runtime): add runtime_context to AgentRuntime

Weak<RuntimeContext> enables source management access via ToolContext.
Returns Arc<dyn SourceManager> to handle lifetime correctly."
```

---

## Task 8: RuntimeContext Source Storage

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

**Step 1: Add source storage fields to RuntimeContext**

```rust
use dashmap::DashMap;
use crate::data_source::{DataStream, DataBlock, BlockSchemaSpec};

// Add to RuntimeContext struct:
pub struct RuntimeContext {
    // ... existing fields ...

    /// Registered stream sources
    stream_sources: DashMap<String, StreamHandle>,

    /// Registered block sources
    block_sources: DashMap<String, BlockHandle>,

    /// Agent stream subscriptions: agent_id -> source_ids
    stream_subscriptions: DashMap<String, Vec<String>>,

    /// Agent block subscriptions: agent_id -> source_ids
    block_subscriptions: DashMap<String, Vec<String>>,

    /// Block edit subscribers: label_pattern -> source_ids
    block_edit_subscribers: DashMap<String, Vec<String>>,
}

struct StreamHandle {
    source: Box<dyn DataStream>,
    is_running: bool,
}

struct BlockHandle {
    source: Box<dyn DataBlock>,
    is_watching: bool,
}
```

**Step 2: Add registration methods**

```rust
impl RuntimeContext {
    /// Register a stream source
    pub fn register_stream(&self, source: Box<dyn DataStream>) {
        let source_id = source.source_id().to_string();
        self.stream_sources.insert(source_id, StreamHandle {
            source,
            is_running: false,
        });
    }

    /// Register a block source
    pub fn register_block_source(&self, source: Box<dyn DataBlock>) {
        let source_id = source.source_id().to_string();
        self.block_sources.insert(source_id, BlockHandle {
            source,
            is_watching: false,
        });
    }
}
```

**Step 3: Update builder to initialize new fields**

**Step 4: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/runtime/context.rs
git commit -m "feat(runtime): add source storage to RuntimeContext

DashMap storage for stream and block sources with subscription tracking."
```

---

## Task 9: RuntimeContext Implements SourceManager

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

**Step 1: Implement SourceManager for RuntimeContext**

```rust
use crate::data_source::{
    SourceManager, StreamSourceInfo, BlockSourceInfo,
    EditFeedback, BlockEdit, BlockRef, Notification,
    ReconcileResult, StreamCursor, VersionInfo,
};

#[async_trait]
impl SourceManager for RuntimeContext {
    fn list_streams(&self) -> Vec<String> {
        self.stream_sources.iter().map(|e| e.key().clone()).collect()
    }

    fn get_stream_info(&self, source_id: &str) -> Option<StreamSourceInfo> {
        self.stream_sources.get(source_id).map(|h| StreamSourceInfo {
            source_id: source_id.to_string(),
            name: h.source.name().to_string(),
            block_schemas: h.source.block_schemas(),
            is_running: h.is_running,
            is_paused: h.source.is_paused(),
            supports_pull: h.source.supports_pull(),
        })
    }

    fn pause_stream(&self, source_id: &str) -> Result<()> {
        let mut handle = self.stream_sources
            .get_mut(source_id)
            .ok_or_else(|| CoreError::NotFound {
                resource: "stream_source".into(),
                identifier: source_id.into(),
            })?;
        handle.source.pause();
        Ok(())
    }

    fn resume_stream(&self, source_id: &str) -> Result<()> {
        let mut handle = self.stream_sources
            .get_mut(source_id)
            .ok_or_else(|| CoreError::NotFound {
                resource: "stream_source".into(),
                identifier: source_id.into(),
            })?;
        handle.source.resume();
        Ok(())
    }

    async fn subscribe_to_stream(
        &self,
        agent_id: &AgentId,
        source_id: &str,
    ) -> Result<broadcast::Receiver<Notification>> {
        let mut handle = self.stream_sources
            .get_mut(source_id)
            .ok_or_else(|| CoreError::NotFound {
                resource: "stream_source".into(),
                identifier: source_id.into(),
            })?;

        // Start source if not running, passing self as ToolContext
        if !handle.is_running {
            let ctx: Arc<dyn ToolContext> = Arc::new(self.clone()); // or appropriate reference
            let rx = handle.source.start(ctx, agent_id.clone()).await?;
            handle.is_running = true;

            // Track subscription
            self.stream_subscriptions
                .entry(agent_id.to_string())
                .or_default()
                .push(source_id.to_string());

            return Ok(rx);
        }

        // Source already running - for multi-subscriber we'd need stored sender
        Err(CoreError::InvalidState {
            message: "Stream already running, multi-subscriber not yet implemented".into(),
        })
    }

    // ... implement remaining methods similarly ...

    async fn handle_block_edit(&self, edit: &BlockEdit) -> Result<EditFeedback> {
        // Find subscribers interested in this block label
        let subscribers = self.find_edit_subscribers(&edit.block_label);

        if subscribers.is_empty() {
            return Ok(EditFeedback::Applied { message: None });
        }

        // Route to subscribers (simplified - first handler wins)
        for source_id in subscribers {
            tracing::debug!(
                source_id = %source_id,
                block_label = %edit.block_label,
                "Block edit routed to source"
            );
        }

        Ok(EditFeedback::Applied { message: None })
    }
}

impl RuntimeContext {
    fn find_edit_subscribers(&self, block_label: &str) -> Vec<String> {
        let mut result = Vec::new();
        for entry in self.block_edit_subscribers.iter() {
            if label_matches_pattern(block_label, entry.key()) {
                result.extend(entry.value().clone());
            }
        }
        result
    }
}

/// Pattern matching for block labels (e.g., "bluesky_user_{handle}")
fn label_matches_pattern(label: &str, pattern: &str) -> bool {
    if !pattern.contains('{') {
        return label == pattern;
    }

    let mut parts = pattern.splitn(2, '{');
    let prefix = parts.next().unwrap_or("");
    if !label.starts_with(prefix) {
        return false;
    }

    if let Some(rest) = parts.next() {
        if let Some(close_idx) = rest.find('}') {
            let suffix = &rest[close_idx + 1..];
            if !label.ends_with(suffix) {
                return false;
            }
            let middle_len = label.len() - prefix.len() - suffix.len();
            return middle_len > 0;
        }
    }
    false
}
```

**Step 2: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/runtime/context.rs
git commit -m "feat(runtime): implement SourceManager for RuntimeContext

Full source management implementation including stream lifecycle,
block operations, and edit routing with pattern matching."
```

---

## Task 10: Helper Utilities - Block Creation

**Files:**
- Create: `crates/pattern_core/src/data_source/helpers.rs`
- Modify: `crates/pattern_core/src/data_source/mod.rs`

**Step 1: Write block creation helpers**

```rust
// crates/pattern_core/src/data_source/helpers.rs
//! Helper utilities for implementing DataStream and DataBlock sources.

use std::sync::Arc;

use crate::memory::{BlockSchema, BlockType, MemoryStore};
use crate::AgentId;
use crate::error::Result;
use crate::SnowflakePosition;

use super::{BlockRef, BlockSchemaSpec, Notification};
use crate::Message;

/// Builder for creating blocks in a memory store
pub struct BlockBuilder<'a> {
    memory: &'a dyn MemoryStore,
    owner: AgentId,
    label: String,
    description: Option<String>,
    schema: BlockSchema,
    block_type: BlockType,
    char_limit: usize,
    pinned: bool,
    initial_content: Option<String>,
}

impl<'a> BlockBuilder<'a> {
    /// Create a new block builder
    pub fn new(memory: &'a dyn MemoryStore, owner: AgentId, label: impl Into<String>) -> Self {
        Self {
            memory,
            owner,
            label: label.into(),
            description: None,
            schema: BlockSchema::Text,
            block_type: BlockType::Working,
            char_limit: 4096,
            pinned: false,
            initial_content: None,
        }
    }

    /// Set block description
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set block schema
    pub fn schema(mut self, schema: BlockSchema) -> Self {
        self.schema = schema;
        self
    }

    /// Set block type
    pub fn block_type(mut self, block_type: BlockType) -> Self {
        self.block_type = block_type;
        self
    }

    /// Set character limit
    pub fn char_limit(mut self, limit: usize) -> Self {
        self.char_limit = limit;
        self
    }

    /// Set block as pinned (always in context)
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Set initial text content
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.initial_content = Some(content.into());
        self
    }

    /// Build the block and return a BlockRef
    pub async fn build(self) -> Result<BlockRef> {
        let description = self.description.unwrap_or_else(|| self.label.clone());

        let block_id = self.memory.create_block(
            &self.owner.to_string(),
            &self.label,
            &description,
            self.block_type,
            self.schema,
            self.char_limit,
        ).await?;

        // Set initial content if provided
        if let Some(content) = &self.initial_content {
            self.memory.update_block_text(
                &self.owner.to_string(),
                &self.label,
                content,
            ).await?;
        }

        // Set pinned flag if requested
        if self.pinned {
            self.memory.set_block_pinned(
                &self.owner.to_string(),
                &self.label,
                true,
            ).await?;
        }

        Ok(BlockRef::new(&self.label, block_id).owned_by(&self.owner.to_string()))
    }
}

/// Builder for creating notifications
pub struct NotificationBuilder {
    message: Option<Message>,
    block_refs: Vec<BlockRef>,
    batch_id: Option<SnowflakePosition>,
}

impl NotificationBuilder {
    /// Create a new notification builder
    pub fn new() -> Self {
        Self {
            message: None,
            block_refs: Vec::new(),
            batch_id: None,
        }
    }

    /// Set the message (from text)
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.message = Some(Message::user(text.into()));
        self
    }

    /// Set the message directly
    pub fn message(mut self, message: Message) -> Self {
        self.message = Some(message);
        self
    }

    /// Add a block reference
    pub fn block(mut self, block_ref: BlockRef) -> Self {
        self.block_refs.push(block_ref);
        self
    }

    /// Add multiple block references
    pub fn blocks(mut self, refs: impl IntoIterator<Item = BlockRef>) -> Self {
        self.block_refs.extend(refs);
        self
    }

    /// Set the batch ID
    pub fn batch_id(mut self, id: SnowflakePosition) -> Self {
        self.batch_id = Some(id);
        self
    }

    /// Build the notification
    pub fn build(self) -> Notification {
        Notification {
            message: self.message.unwrap_or_else(|| Message::user("".to_string())),
            block_refs: self.block_refs,
            batch_id: self.batch_id.unwrap_or_else(SnowflakePosition::generate),
        }
    }
}

impl Default for NotificationBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Utility for managing ephemeral blocks with get-or-create semantics
pub struct EphemeralBlockCache {
    /// Map of external ID to block info
    cache: dashmap::DashMap<String, CachedBlockInfo>,
}

#[derive(Clone)]
struct CachedBlockInfo {
    block_id: String,
    label: String,
    owner: String,
}

impl EphemeralBlockCache {
    pub fn new() -> Self {
        Self {
            cache: dashmap::DashMap::new(),
        }
    }

    /// Get or create an ephemeral block
    ///
    /// Uses external_id as cache key (e.g., "did:plc:abc123" for a user).
    /// If block exists, returns reference. Otherwise creates it.
    pub async fn get_or_create<F, Fut>(
        &self,
        external_id: &str,
        label_fn: impl FnOnce(&str) -> String,
        create_fn: F,
    ) -> Result<BlockRef>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<BlockRef>>,
    {
        // Check cache first
        if let Some(info) = self.cache.get(external_id) {
            return Ok(BlockRef {
                label: info.label.clone(),
                block_id: info.block_id.clone(),
                agent_id: info.owner.clone(),
            });
        }

        // Create new block
        let label = label_fn(external_id);
        let block_ref = create_fn(label.clone()).await?;

        // Cache it
        self.cache.insert(external_id.to_string(), CachedBlockInfo {
            block_id: block_ref.block_id.clone(),
            label: block_ref.label.clone(),
            owner: block_ref.agent_id.clone(),
        });

        Ok(block_ref)
    }

    /// Remove a block from cache
    pub fn invalidate(&self, external_id: &str) {
        self.cache.remove(external_id);
    }

    /// Clear all cached entries
    pub fn clear(&self) {
        self.cache.clear();
    }
}

impl Default for EphemeralBlockCache {
    fn default() -> Self {
        Self::new()
    }
}
```

**Step 2: Update mod.rs exports**

Add to `crates/pattern_core/src/data_source/mod.rs`:
```rust
mod helpers;
pub use helpers::{BlockBuilder, NotificationBuilder, EphemeralBlockCache};
```

**Step 3: Run check**

Run: `cargo check -p pattern_core`
Expected: May fail if MemoryStore doesn't have set_block_pinned - note for next task

**Step 4: Commit (if passes, otherwise defer)**

```bash
git add crates/pattern_core/src/data_source/helpers.rs
git add crates/pattern_core/src/data_source/mod.rs
git commit -m "feat(data_source): add helper utilities for source implementations

BlockBuilder for easy block creation, NotificationBuilder for
notifications, EphemeralBlockCache for get-or-create patterns."
```

---

## Task 11: MemoryStore Extension - Pinned Blocks

**Files:**
- Modify: `crates/pattern_core/src/memory/store.rs` (or trait location)

**Step 1: Identify current MemoryStore trait location**

Search for `trait MemoryStore` in pattern_core to find exact file.

**Step 2: Add set_block_pinned method**

Add to MemoryStore trait:
```rust
    /// Set the pinned flag on a block
    ///
    /// Pinned blocks are always loaded into agent context while subscribed.
    /// Unpinned (ephemeral) blocks only load when referenced by a notification.
    async fn set_block_pinned(
        &self,
        agent_id: &str,
        label: &str,
        pinned: bool,
    ) -> MemoryResult<()>;
```

**Step 3: Implement for SurrealMemoryStore**

Add implementation that updates the block's pinned field in the database.

**Step 4: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory/
git commit -m "feat(memory): add set_block_pinned to MemoryStore trait

Enables setting pinned flag on blocks for context loading behavior."
```

---

## Task 12: Context Builder Integration

**Files:**
- Modify: `crates/pattern_core/src/context/builder.rs` (or equivalent)

**Step 1: Find ContextBuilder location**

Search for `struct ContextBuilder` or context building logic.

**Step 2: Add batch block filtering**

Add method and modify build logic:
```rust
impl<'a> ContextBuilder<'a> {
    /// Set block IDs to keep loaded for this batch (even if unpinned)
    pub fn with_batch_blocks(mut self, block_ids: Vec<String>) -> Self {
        self.batch_block_ids = Some(block_ids);
        self
    }
}

// In build() or equivalent, filter Working blocks:
let working_blocks: Vec<BlockMetadata> = owned_working_blocks
    .into_iter()
    .filter(|b| {
        b.pinned || self.batch_block_ids
            .as_ref()
            .map(|ids| ids.contains(&b.id))
            .unwrap_or(false)
    })
    .collect();
```

**Step 3: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/context/
git commit -m "feat(context): add batch block filtering to ContextBuilder

with_batch_blocks method enables loading ephemeral blocks for
specific notification batches."
```

---

## Task 13: Integration Tests

**Files:**
- Create: `crates/pattern_core/tests/data_source_test.rs`

**Step 1: Write basic integration test**

```rust
// crates/pattern_core/tests/data_source_test.rs
use pattern_core::data_source::*;
use pattern_core::memory::{BlockSchema, BlockType};

#[test]
fn test_block_ref_creation() {
    let block_ref = BlockRef::new("test_label", "block_123");
    assert_eq!(block_ref.label, "test_label");
    assert_eq!(block_ref.block_id, "block_123");
    assert_eq!(block_ref.agent_id, "_constellation_");

    let owned = block_ref.owned_by("agent_456");
    assert_eq!(owned.agent_id, "agent_456");
}

#[test]
fn test_block_schema_spec() {
    let pinned = BlockSchemaSpec::pinned(
        "config",
        BlockSchema::Text,
        "Configuration block",
    );
    assert!(pinned.pinned);
    assert_eq!(pinned.label_pattern, "config");

    let ephemeral = BlockSchemaSpec::ephemeral(
        "user_{id}",
        BlockSchema::Text,
        "User profile",
    );
    assert!(!ephemeral.pinned);
    assert_eq!(ephemeral.label_pattern, "user_{id}");
}

#[test]
fn test_notification_builder() {
    let notification = NotificationBuilder::new()
        .text("Hello, world!")
        .block(BlockRef::new("label1", "id1"))
        .build();

    assert_eq!(notification.block_refs.len(), 1);
    assert_eq!(notification.block_refs[0].label, "label1");
}

#[test]
fn test_permission_rule_matching() {
    use pattern_core::memory::MemoryPermission;

    let rule = PermissionRule::new("src/**/*.rs", MemoryPermission::ReadWrite);
    assert!(rule.matches("src/main.rs"));
    assert!(rule.matches("src/lib/mod.rs"));
    assert!(!rule.matches("tests/test.rs"));
}

#[test]
fn test_stream_cursor() {
    let cursor = StreamCursor::new("cursor_abc");
    assert_eq!(cursor.as_str(), "cursor_abc");

    let json = serde_json::to_string(&cursor).unwrap();
    let parsed: StreamCursor = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.as_str(), "cursor_abc");
}
```

**Step 2: Run tests**

Run: `cargo test -p pattern_core data_source`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/tests/data_source_test.rs
git commit -m "test(data_source): add integration tests

Tests for BlockRef, BlockSchemaSpec, NotificationBuilder,
PermissionRule, and StreamCursor."
```

---

## Task 14: Documentation

**Files:**
- Modify: `crates/pattern_core/src/data_source/mod.rs`

**Step 1: Add module documentation**

Expand the module doc at the top:

```rust
//! # Data Sources - Event and Document Sources
//!
//! This module provides the data source architecture with two trait families:
//!
//! ## DataStream - Event-Driven Sources
//!
//! For sources that produce notifications and/or maintain state blocks:
//! - Bluesky firehose, Discord events, LSP diagnostics, sensors
//! - Sources receive `Arc<dyn ToolContext>` - same access as tools
//! - Channel-based notifications with `BlockRef` references
//!
//! ```ignore
//! impl DataStream for MySource {
//!     async fn start(&mut self, ctx: Arc<dyn ToolContext>, owner: AgentId)
//!         -> Result<broadcast::Receiver<Notification>>
//!     {
//!         // Access memory, model, router, sources via ctx
//!         // Create blocks, spawn processing task, return receiver
//!     }
//! }
//! ```
//!
//! ## DataBlock - Document-Oriented Sources
//!
//! For persistent documents with versioning and permission-gated edits:
//! - Files, configs, structured documents
//! - Loro-backed with full version history and rollback
//! - Disk as canonical with reconciliation after external changes
//!
//! ```ignore
//! impl DataBlock for FileSource {
//!     async fn load(&self, path: &str, ctx: &dyn ToolContext, owner: AgentId)
//!         -> Result<BlockRef>
//!     {
//!         // Access memory via ctx.memory(), read file, create block
//!     }
//! }
//! ```
//!
//! ## Source Management
//!
//! `RuntimeContext` implements `SourceManager` for source lifecycle:
//! - Registration and lifecycle
//! - Subscription management per agent
//! - Block edit routing to interested sources
//! - Access via `ToolContext::sources()`
//!
//! ## Helpers
//!
//! Utilities for implementing sources:
//! - `BlockBuilder` - Fluent block creation
//! - `NotificationBuilder` - Fluent notification creation
//! - `EphemeralBlockCache` - Get-or-create for ephemeral blocks
```

**Step 2: Run doc check**

Run: `cargo doc -p pattern_core --no-deps`
Expected: PASS with no warnings

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/mod.rs
git commit -m "docs(data_source): add comprehensive module documentation

Documents DataStream, DataBlock, SourceManager, and helper utilities."
```

---

## Task 15: Export from pattern_core

**Files:**
- Modify: `crates/pattern_core/src/lib.rs`

**Step 1: Add public exports**

Add to lib.rs exports:
```rust
// In the data_source section
pub use data_source::{
    DataStream, DataBlock,
    BlockRef, Notification, StreamCursor, BlockSchemaSpec,
    PermissionRule, FileChange, FileChangeType, VersionInfo,
    ConflictResolution, ReconcileResult,
    BlockBuilder, NotificationBuilder, EphemeralBlockCache,
    ToolRule as DataSourceToolRule,  // Avoid conflict with existing ToolRule
    SourceManager, StreamSourceInfo, BlockSourceInfo,
    BlockEdit, EditFeedback,
};
```

**Step 2: Run check**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/lib.rs
git commit -m "feat(pattern_core): export data source types

Public API for DataStream, DataBlock, SourceManager, and helpers."
```

---

## Task 16: Tool Stubs (Optional - May Warrant Separate Plan)

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/block_tool.rs`
- Create: `crates/pattern_core/src/tool/builtin/source_tool.rs`

**Decision Point:** The tool implementations (block, block_edit, source, recall) are substantial. This task creates minimal stubs. Full implementation should be a separate plan.

**Step 1: Create block tool stub**

```rust
// crates/pattern_core/src/tool/builtin/block_tool.rs
//! Block lifecycle management tool
//!
//! Operations: load, pin, unpin, archive, info
//!
//! TODO: Full implementation in separate plan

use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum BlockOperation {
    Load,
    Pin,
    Unpin,
    Archive,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockToolInput {
    pub label: String,
    pub operation: BlockOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockToolOutput {
    pub success: bool,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
}

// TODO: Implement AiTool trait
```

**Step 2: Create source tool stub**

```rust
// crates/pattern_core/src/tool/builtin/source_tool.rs
//! Data source control tool
//!
//! Operations: pause, resume, status
//!
//! TODO: Full implementation in separate plan

use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum SourceOperation {
    Pause,
    Resume,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceToolInput {
    pub source_id: String,
    pub operation: SourceOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceToolOutput {
    pub success: bool,
    pub message: Option<String>,
    pub status: Option<String>,
}

// TODO: Implement AiTool trait
```

**Step 3: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/block_tool.rs
git add crates/pattern_core/src/tool/builtin/source_tool.rs
git commit -m "feat(tools): add block and source tool stubs

Minimal type definitions for block lifecycle and source control tools.
Full implementation deferred to separate plan."
```

---

## Summary

This plan implements the core data source infrastructure:

1. **Tasks 1-4**: Core types and trait definitions (DataStream, DataBlock)
2. **Tasks 5-7**: SourceManager trait and ToolContext integration
3. **Tasks 8-9**: RuntimeContext source storage and SourceManager implementation
4. **Tasks 10-11**: Helper utilities and MemoryStore extension
5. **Task 12**: Context builder integration
6. **Tasks 13-15**: Tests, documentation, exports
7. **Task 16**: Optional tool stubs

**Key architectural decisions:**
- Source management lives on RuntimeContext (no separate coordinator)
- ToolContext gains `sources() -> Arc<dyn SourceManager>`
- AgentRuntime holds `Weak<RuntimeContext>` to implement sources()
- Sources receive `Arc<dyn ToolContext>` - same access as tools

**Not in scope** (require separate plans):
- Full tool implementations (block, block_edit, source, recall)
- Specific source implementations (Bluesky, LSP, File)
- Shell hook integration for permission enforcement
- Conflict resolution UI for Human permission escalation

**Estimated commits**: 14-15 focused commits
**Dependencies**: Existing pattern_core memory and tool infrastructure

## Post-Implementation TODOs

- Add deregistration methods to RuntimeContext (unregister_stream, unregister_block_source)
- Consider adding unregistration to ToolRegistry as well for parity

