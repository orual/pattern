# Tool Rewrite & FileSource Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement v2 tool taxonomy (block, block_edit, recall, source, file) and FileSource as first DataBlock implementation.

**Architecture:** Five new tools with clean separation - lifecycle vs content editing, archival entries vs blocks, source control. FileSource implements DataBlock trait with Loro-backed blocks and on-demand disk sync.

**Tech Stack:** Rust, tokio, serde, DashMap, Loro CRDT, SQLite (in-memory for tests)

**Design Doc:** `docs/plans/2025-12-27-tool-rewrite-design.md`

---

## Task 1: Shared Types for New Tools

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/types.rs`
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs` (add types module)

**Step 1: Create shared types module**

```rust
// crates/pattern_core/src/tool/builtin/types.rs
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Operations for the `block` tool (lifecycle management)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockOp {
    /// Load a block into working context
    Load,
    /// Pin block to retain across batches
    Pin,
    /// Unpin block (becomes ephemeral)
    Unpin,
    /// Change block type to Archival
    Archive,
    /// Get block metadata
    Info,
}

/// Input for the `block` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockInput {
    /// Operation to perform
    pub op: BlockOp,
    /// Block label
    pub label: String,
    /// Optional source ID for load operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

/// Operations for the `block_edit` tool (content editing)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockEditOp {
    /// Append content to block
    Append,
    /// Find and replace text
    Replace,
    /// Apply diff/patch (advanced)
    Patch,
    /// Set a specific field (Map/Composite schemas)
    SetField,
}

/// Input for the `block_edit` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockEditInput {
    /// Operation to perform
    pub op: BlockEditOp,
    /// Block label
    pub label: String,
    /// Content for append operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Old text for replace operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// New text for replace operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    /// Field name for set_field operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Value for set_field operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Patch content for patch operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

/// Operations for the `recall` tool (archival entries)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecallOp {
    /// Create new archival entry
    Insert,
    /// Search archival entries
    Search,
}

/// Input for the `recall` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecallInput {
    /// Operation to perform
    pub op: RecallOp,
    /// Content for insert operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Metadata for insert operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Query for search operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Limit for search results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Operations for the `source` tool (data source control)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceOp {
    /// Pause stream notifications
    Pause,
    /// Resume stream notifications
    Resume,
    /// Get source status
    Status,
    /// List all sources
    List,
}

/// Input for the `source` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceInput {
    /// Operation to perform
    pub op: SourceOp,
    /// Source ID (required for pause/resume/status on specific source)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

/// Operations for the `file` tool (FileSource operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOp {
    /// Load file from disk into block
    Load,
    /// Save block content to disk
    Save,
    /// Create new file
    Create,
    /// Delete file
    Delete,
    /// Append to file
    Append,
    /// Find/replace in file
    Replace,
}

/// Input for the `file` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileInput {
    /// Operation to perform
    pub op: FileOp,
    /// File path (relative to source base)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Block label (alternative to path for save)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Content for create/append operations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Old text for replace operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// New text for replace operation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
}

/// Standard output for tool operations
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolOutput {
    /// Whether operation succeeded
    pub success: bool,
    /// Human-readable message
    pub message: String,
    /// Optional structured data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ToolOutput {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            data: None,
        }
    }

    pub fn success_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: message.into(),
            data: Some(data),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            data: None,
        }
    }
}
```

**Step 3: Add types module to builtin mod.rs**

In `crates/pattern_core/src/tool/builtin/mod.rs`, add:

```rust
pub mod types;
pub use types::*;
```

**Step 4: Run cargo check**

Run: `cargo check -p pattern_core`
Expected: Compiles with no errors (empty tool modules will be created next)

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/types.rs
git add crates/pattern_core/src/tool/builtin/mod.rs
git commit -m "$(cat <<'EOF'
feat: add shared types for new tool taxonomy

Shared input/output types for new tools:
- BlockOp, BlockInput for block lifecycle
- BlockEditOp, BlockEditInput for content editing
- RecallOp, RecallInput for archival entries
- SourceOp, SourceInput for data source control
- FileOp, FileInput for file operations

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Block Tool Implementation

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/block.rs`
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs` (uncomment block)

**Step 1: Write failing test**

Add to `crates/pattern_core/src/tool/builtin/tests.rs`:

```rust
use super::types::*;
use crate::tool::builtin::test_utils::create_test_context_with_agent;
use crate::tool::{AiTool, ExecutionMeta};
use crate::memory::{BlockType, BlockSchema};

#[tokio::test]
async fn test_block_tool_info_operation() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    // Create a test block
    memory
        .create_block(
            "test-agent",
            "test_block",
            "A test block",
            BlockType::Working,
            BlockSchema::Text,
            1000,
        )
        .await
        .unwrap();

    let tool = BlockTool::new(ctx);
    let input = BlockInput {
        op: BlockOp::Info,
        label: "test_block".to_string(),
        source_id: None,
    };

    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();
    assert!(result.success);
    assert!(result.data.is_some());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core block_tool_info -- --nocapture`
Expected: FAIL - BlockTool not found

**Step 3: Implement BlockTool**

```rust
// crates/pattern_core/src/tool/builtin/block.rs
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde_json::json;

use crate::memory::{BlockType, MemoryStore};
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};

use super::types::{BlockInput, BlockOp, ToolOutput};

/// Block lifecycle management tool.
///
/// Operations:
/// - `load` - Load block into working context
/// - `pin` - Pin block to retain across batches
/// - `unpin` - Unpin block (becomes ephemeral)
/// - `archive` - Change block type to Archival
/// - `info` - Get block metadata
#[derive(Clone)]
pub struct BlockTool {
    ctx: Arc<dyn ToolContext>,
}

impl BlockTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    async fn handle_load(&self, label: &str, source_id: Option<&str>) -> ToolOutput {
        // If source_id provided, delegate to SourceManager
        if let Some(sid) = source_id {
            if let Some(sources) = self.ctx.sources() {
                // For now, just check if source exists
                // Full path-based loading will be in file tool
                if sources.get_stream_info(sid).is_some()
                    || sources.get_block_source_info(sid).is_some()
                {
                    return ToolOutput::error(format!(
                        "Use source-specific tool to load from '{}'. block load without source_id loads existing blocks.",
                        sid
                    ));
                } else {
                    return ToolOutput::error(format!("Source '{}' not found", sid));
                }
            } else {
                return ToolOutput::error("No source manager available");
            }
        }

        // Load existing block by label
        let memory = self.ctx.memory();
        match memory.get_block_metadata(self.ctx.agent_id(), label).await {
            Ok(Some(metadata)) => {
                // Block exists, ensure it's accessible
                ToolOutput::success_with_data(
                    format!("Block '{}' loaded", label),
                    json!({
                        "label": metadata.label,
                        "block_type": format!("{:?}", metadata.block_type),
                        "pinned": metadata.pinned,
                    }),
                )
            }
            Ok(None) => ToolOutput::error(format!("Block '{}' not found", label)),
            Err(e) => ToolOutput::error(format!("Failed to load block: {}", e)),
        }
    }

    async fn handle_pin(&self, label: &str) -> ToolOutput {
        let memory = self.ctx.memory();
        match memory.set_block_pinned(self.ctx.agent_id(), label, true).await {
            Ok(()) => ToolOutput::success(format!("Block '{}' pinned", label)),
            Err(e) => ToolOutput::error(format!("Failed to pin block: {}", e)),
        }
    }

    async fn handle_unpin(&self, label: &str) -> ToolOutput {
        let memory = self.ctx.memory();
        match memory.set_block_pinned(self.ctx.agent_id(), label, false).await {
            Ok(()) => ToolOutput::success(format!("Block '{}' unpinned", label)),
            Err(e) => ToolOutput::error(format!("Failed to unpin block: {}", e)),
        }
    }

    async fn handle_archive(&self, label: &str) -> ToolOutput {
        let memory = self.ctx.memory();

        // Get current block
        match memory.get_block_metadata(self.ctx.agent_id(), label).await {
            Ok(Some(metadata)) => {
                if metadata.block_type == BlockType::Archival {
                    return ToolOutput::error(format!("Block '{}' is already archived", label));
                }
                if metadata.block_type == BlockType::Core {
                    return ToolOutput::error("Cannot archive Core blocks");
                }

                // Change block type to Archival
                match memory
                    .set_block_type(self.ctx.agent_id(), label, BlockType::Archival)
                    .await
                {
                    Ok(()) => ToolOutput::success(format!("Block '{}' archived", label)),
                    Err(e) => ToolOutput::error(format!("Failed to archive block: {}", e)),
                }
            }
            Ok(None) => ToolOutput::error(format!("Block '{}' not found", label)),
            Err(e) => ToolOutput::error(format!("Failed to get block: {}", e)),
        }
    }

    async fn handle_info(&self, label: &str) -> ToolOutput {
        let memory = self.ctx.memory();
        match memory.get_block_metadata(self.ctx.agent_id(), label).await {
            Ok(Some(metadata)) => ToolOutput::success_with_data(
                format!("Block '{}' info", label),
                json!({
                    "label": metadata.label,
                    "description": metadata.description,
                    "block_type": format!("{:?}", metadata.block_type),
                    "schema": format!("{:?}", metadata.schema),
                    "permission": format!("{:?}", metadata.permission),
                    "pinned": metadata.pinned,
                    "char_limit": metadata.char_limit,
                    "created_at": metadata.created_at.to_rfc3339(),
                    "updated_at": metadata.updated_at.to_rfc3339(),
                }),
            ),
            Ok(None) => ToolOutput::error(format!("Block '{}' not found", label)),
            Err(e) => ToolOutput::error(format!("Failed to get block info: {}", e)),
        }
    }
}

#[async_trait]
impl AiTool for BlockTool {
    type Input = BlockInput;
    type Output = ToolOutput;

    fn name(&self) -> &'static str {
        "block"
    }

    fn description(&self) -> &'static str {
        "Manage block lifecycle: load blocks into context, pin/unpin for persistence, archive to long-term storage, or get block info."
    }

    fn usage_rule(&self) -> &'static str {
        "Use to manage which blocks are in your working context. Pin important blocks to keep them available."
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(self.name(), ToolRuleType::ContinueLoop)]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["load", "pin", "unpin", "archive", "info"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output, String> {
        let result = match input.op {
            BlockOp::Load => self.handle_load(&input.label, input.source_id.as_deref()).await,
            BlockOp::Pin => self.handle_pin(&input.label).await,
            BlockOp::Unpin => self.handle_unpin(&input.label).await,
            BlockOp::Archive => self.handle_archive(&input.label).await,
            BlockOp::Info => self.handle_info(&input.label).await,
        };
        Ok(result)
    }
}
```

**Step 4: Add missing MemoryStore methods if needed**

Check if `set_block_pinned` and `set_block_type` exist on MemoryStore trait. If not, add them:

```rust
// In crates/pattern_core/src/memory/store.rs, add to trait:
async fn set_block_pinned(&self, agent_id: &str, label: &str, pinned: bool) -> MemoryResult<()>;
async fn set_block_type(&self, agent_id: &str, label: &str, block_type: BlockType) -> MemoryResult<()>;
```

And implement in MemoryCache.

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core block_tool_info -- --nocapture`
Expected: PASS

**Step 6: Add more tests**

```rust
#[tokio::test]
async fn test_block_tool_pin_unpin() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    memory
        .create_block(
            "test-agent",
            "pin_test",
            "Test block",
            BlockType::Working,
            BlockSchema::Text,
            1000,
        )
        .await
        .unwrap();

    let tool = BlockTool::new(ctx);

    // Pin
    let result = tool
        .execute(
            BlockInput {
                op: BlockOp::Pin,
                label: "pin_test".to_string(),
                source_id: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Verify pinned
    let meta = memory
        .get_block_metadata("test-agent", "pin_test")
        .await
        .unwrap()
        .unwrap();
    assert!(meta.pinned);

    // Unpin
    let result = tool
        .execute(
            BlockInput {
                op: BlockOp::Unpin,
                label: "pin_test".to_string(),
                source_id: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Verify unpinned
    let meta = memory
        .get_block_metadata("test-agent", "pin_test")
        .await
        .unwrap()
        .unwrap();
    assert!(!meta.pinned);
}

#[tokio::test]
async fn test_block_tool_archive() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    memory
        .create_block(
            "test-agent",
            "archive_test",
            "Test block",
            BlockType::Working,
            BlockSchema::Text,
            1000,
        )
        .await
        .unwrap();

    let tool = BlockTool::new(ctx);

    let result = tool
        .execute(
            BlockInput {
                op: BlockOp::Archive,
                label: "archive_test".to_string(),
                source_id: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Verify archived
    let meta = memory
        .get_block_metadata("test-agent", "archive_test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(meta.block_type, BlockType::Archival);
}

#[tokio::test]
async fn test_block_tool_cannot_archive_core() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    memory
        .create_block(
            "test-agent",
            "core_block",
            "Core block",
            BlockType::Core,
            BlockSchema::Text,
            1000,
        )
        .await
        .unwrap();

    let tool = BlockTool::new(ctx);

    let result = tool
        .execute(
            BlockInput {
                op: BlockOp::Archive,
                label: "core_block".to_string(),
                source_id: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.message.contains("Core"));
}
```

**Step 7: Run all block tool tests**

Run: `cargo test -p pattern_core block_tool -- --nocapture`
Expected: All PASS

**Step 8: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/block.rs
git add crates/pattern_core/src/tool/builtin/tests.rs
git add crates/pattern_core/src/memory/  # if modified
git commit -m "$(cat <<'EOF'
feat: implement block tool for lifecycle management

Operations: load, pin, unpin, archive, info
- load: loads existing block or errors if source_id provided
- pin/unpin: control block persistence across batches
- archive: changes Working block to Archival type
- info: returns block metadata

Includes comprehensive tests.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: BlockEdit Tool Implementation

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/block_edit.rs`
- Modify: `crates/pattern_core/src/tool/builtin/tests.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn test_block_edit_append() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    memory
        .create_block(
            "test-agent",
            "edit_test",
            "Test block",
            BlockType::Working,
            BlockSchema::Text,
            1000,
        )
        .await
        .unwrap();
    memory
        .update_block_text("test-agent", "edit_test", "Hello")
        .await
        .unwrap();

    let tool = BlockEditTool::new(ctx);
    let result = tool
        .execute(
            BlockEditInput {
                op: BlockEditOp::Append,
                label: "edit_test".to_string(),
                content: Some(" World".to_string()),
                old: None,
                new: None,
                field: None,
                value: None,
                patch: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

    assert!(result.success);

    let content = memory
        .get_rendered_content("test-agent", "edit_test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(content, "Hello World");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core block_edit_append -- --nocapture`
Expected: FAIL - BlockEditTool not found

**Step 3: Implement BlockEditTool**

**IMPORTANT:** This tool works directly with `StructuredDocument`, NOT high-level MemoryStore methods.

Pattern:
1. `memory.get_block(agent_id, label)` → returns `Option<StructuredDocument>`
2. Call methods on the document: `doc.append_text()`, `doc.replace_text()`, `doc.set_field()`
3. `memory.persist_block(agent_id, label)` to save changes

```rust
// crates/pattern_core/src/tool/builtin/block_edit.rs
use std::sync::Arc;

use async_trait::async_trait;

use crate::memory::BlockSchema;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};
use crate::CoreError;

use super::types::{BlockEditInput, BlockEditOp, ToolOutput};

/// Block content editing tool.
///
/// Operations:
/// - `append` - Append content to block (Text, List)
/// - `replace` - Find and replace text (Text)
/// - `patch` - Apply diff/patch (Text, advanced) - NOT YET IMPLEMENTED
/// - `set_field` - Set specific field (Map, Composite)
#[derive(Clone)]
pub struct BlockEditTool {
    ctx: Arc<dyn ToolContext>,
}

impl BlockEditTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    async fn handle_append(&self, label: &str, content: Option<&str>) -> Result<ToolOutput, String> {
        let content = content.ok_or("append requires 'content' parameter")?;

        let memory = self.ctx.memory();
        let doc = memory
            .get_block(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to get block: {}", e))?
            .ok_or_else(|| format!("Block '{}' not found", label))?;

        doc.append_text(content)
            .map_err(|e| format!("Failed to append: {}", e))?;

        memory
            .persist_block(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to persist: {}", e))?;

        Ok(ToolOutput::success(format!("Appended to block '{}'", label)))
    }

    async fn handle_replace(&self, label: &str, old: Option<&str>, new: Option<&str>) -> Result<ToolOutput, String> {
        let old = old.ok_or("replace requires 'old' parameter")?;
        let new = new.ok_or("replace requires 'new' parameter")?;

        let memory = self.ctx.memory();

        // Check schema is Text
        let metadata = memory
            .get_block_metadata(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to get block: {}", e))?
            .ok_or_else(|| format!("Block '{}' not found", label))?;

        if !matches!(metadata.schema, BlockSchema::Text) {
            return Err(format!(
                "replace currently only supports Text schema, got {:?}",
                metadata.schema
            ));
        }

        let doc = memory
            .get_block(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to get block: {}", e))?
            .ok_or_else(|| format!("Block '{}' not found", label))?;

        let replaced = doc
            .replace_text(old, new)
            .map_err(|e| format!("Failed to replace: {}", e))?;

        if !replaced {
            return Err(format!("Text '{}' not found in block", old));
        }

        memory
            .persist_block(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to persist: {}", e))?;

        Ok(ToolOutput::success(format!("Replaced in block '{}'", label)))
    }

    async fn handle_patch(&self, _label: &str, patch: Option<&str>) -> Result<ToolOutput, String> {
        let _patch = patch.ok_or("patch requires 'patch' parameter")?;
        Err("patch operation not yet implemented".to_string())
    }

    async fn handle_set_field(
        &self,
        label: &str,
        field: Option<&str>,
        value: Option<&serde_json::Value>,
    ) -> Result<ToolOutput, String> {
        let field = field.ok_or("set_field requires 'field' parameter")?;
        let value = value.ok_or("set_field requires 'value' parameter")?;

        let memory = self.ctx.memory();

        // Check schema compatibility and read-only
        let metadata = memory
            .get_block_metadata(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to get block: {}", e))?
            .ok_or_else(|| format!("Block '{}' not found", label))?;

        if let Some(true) = metadata.schema.is_field_read_only(field) {
            return Err(format!("Field '{}' is read-only", field));
        }

        match &metadata.schema {
            BlockSchema::Map { .. } | BlockSchema::Composite { .. } => {}
            _ => {
                return Err(format!(
                    "set_field requires Map or Composite schema, got {:?}",
                    metadata.schema
                ));
            }
        }

        let doc = memory
            .get_block(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to get block: {}", e))?
            .ok_or_else(|| format!("Block '{}' not found", label))?;

        doc.set_field(field, value.clone(), false)
            .map_err(|e| format!("Failed to set field: {}", e))?;

        memory
            .persist_block(self.ctx.agent_id(), label)
            .await
            .map_err(|e| format!("Failed to persist: {}", e))?;

        Ok(ToolOutput::success(format!(
            "Set field '{}' in block '{}'",
            field, label
        )))
    }
}

#[async_trait]
impl AiTool for BlockEditTool {
    type Input = BlockEditInput;
    type Output = ToolOutput;

    fn name(&self) -> &'static str {
        "block_edit"
    }

    fn description(&self) -> &'static str {
        "Edit block content: append text, find/replace, apply patches, or set specific fields in structured blocks."
    }

    fn usage_rule(&self) -> &'static str {
        "Use to modify block content. Available operations depend on block schema."
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(self.name(), ToolRuleType::ContinueLoop)]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["append", "replace", "patch", "set_field"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output, String> {
        match input.op {
            BlockEditOp::Append => self.handle_append(&input.label, input.content.as_deref()).await,
            BlockEditOp::Replace => {
                self.handle_replace(&input.label, input.old.as_deref(), input.new.as_deref())
                    .await
            }
            BlockEditOp::Patch => self.handle_patch(&input.label, input.patch.as_deref()).await,
            BlockEditOp::SetField => {
                self.handle_set_field(&input.label, input.field.as_deref(), input.value.as_ref())
                    .await
            }
        }.map_err(|e| CoreError::tool_exec_msg("block_edit", serde_json::json!({}), &e).to_string())
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core block_edit_append -- --nocapture`
Expected: PASS

**Step 6: Add more tests**

```rust
#[tokio::test]
async fn test_block_edit_replace() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    memory
        .create_block(
            "test-agent",
            "replace_test",
            "Test block",
            BlockType::Working,
            BlockSchema::Text,
            1000,
        )
        .await
        .unwrap();
    memory
        .update_block_text("test-agent", "replace_test", "Hello World")
        .await
        .unwrap();

    let tool = BlockEditTool::new(ctx);
    let result = tool
        .execute(
            BlockEditInput {
                op: BlockEditOp::Replace,
                label: "replace_test".to_string(),
                content: None,
                old: Some("World".to_string()),
                new: Some("Rust".to_string()),
                field: None,
                value: None,
                patch: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

    assert!(result.success);

    let content = memory
        .get_rendered_content("test-agent", "replace_test")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(content, "Hello Rust");
}

#[tokio::test]
async fn test_block_edit_set_field() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    // Create a Map schema block
    let schema = BlockSchema::Map {
        fields: vec![
            crate::memory::FieldDef {
                name: "name".to_string(),
                field_type: crate::memory::FieldType::Text,
                description: Some("User name".to_string()),
                read_only: false,
                default: None,
            },
            crate::memory::FieldDef {
                name: "age".to_string(),
                field_type: crate::memory::FieldType::Number,
                description: Some("User age".to_string()),
                read_only: false,
                default: None,
            },
        ],
    };

    memory
        .create_block(
            "test-agent",
            "map_block",
            "User info",
            BlockType::Working,
            schema,
            1000,
        )
        .await
        .unwrap();

    let tool = BlockEditTool::new(ctx);
    let result = tool
        .execute(
            BlockEditInput {
                op: BlockEditOp::SetField,
                label: "map_block".to_string(),
                content: None,
                old: None,
                new: None,
                field: Some("name".to_string()),
                value: Some(serde_json::json!("Alice")),
                patch: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

    assert!(result.success);
}

#[tokio::test]
async fn test_block_edit_rejects_readonly_field() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    let schema = BlockSchema::Map {
        fields: vec![crate::memory::FieldDef {
            name: "system_id".to_string(),
            field_type: crate::memory::FieldType::Text,
            description: Some("System ID".to_string()),
            read_only: true,
            default: None,
        }],
    };

    memory
        .create_block(
            "test-agent",
            "readonly_block",
            "System info",
            BlockType::Working,
            schema,
            1000,
        )
        .await
        .unwrap();

    let tool = BlockEditTool::new(ctx);
    let result = tool
        .execute(
            BlockEditInput {
                op: BlockEditOp::SetField,
                label: "readonly_block".to_string(),
                content: None,
                old: None,
                new: None,
                field: Some("system_id".to_string()),
                value: Some(serde_json::json!("hacked")),
                patch: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

    assert!(!result.success);
    assert!(result.message.contains("read-only"));
}
```

**Step 7: Run all block_edit tests**

Run: `cargo test -p pattern_core block_edit -- --nocapture`
Expected: All PASS

**Step 8: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/block_edit.rs
git add crates/pattern_core/src/tool/builtin/tests.rs
git add crates/pattern_core/src/memory/  # if modified
git commit -m "$(cat <<'EOF'
feat: implement block_edit tool for content editing

Operations: append, replace, patch (stub), set_field
- append: add content to Text/List blocks
- replace: find/replace in Text blocks (expandable later)
- patch: placeholder for diff application
- set_field: set fields in Map/Composite, respects read_only

Includes permission checking via MemoryACL.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Recall Tool Implementation (Simplified)

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/recall.rs` (rewrite existing)
- Modify: `crates/pattern_core/src/tool/builtin/tests.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn test_recall_insert_and_search() {
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    let tool = RecallTool::new(ctx.clone());

    // Insert
    let result = tool
        .execute(
            RecallInput {
                op: RecallOp::Insert,
                content: Some("Important fact about Rust ownership".to_string()),
                metadata: None,
                query: None,
                limit: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Search
    let result = tool
        .execute(
            RecallInput {
                op: RecallOp::Search,
                content: None,
                metadata: None,
                query: Some("Rust ownership".to_string()),
                limit: Some(10),
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.data.is_some());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core recall_insert_and_search -- --nocapture`
Expected: FAIL - RecallTool not found

**Step 3: Implement RecallTool**

```rust
// crates/pattern_core/src/tool/builtin/recall.rs
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::memory::MemoryStore;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};

use super::types::{RecallInput, RecallOp, ToolOutput};

/// Archival entry management tool (simplified).
///
/// Operations:
/// - `insert` - Create new immutable archival entry
/// - `search` - Full-text search over archival entries
///
/// Note: This operates on archival *entries*, not Archival-typed blocks.
/// Archival entries are immutable once created.
#[derive(Clone)]
pub struct RecallTool {
    ctx: Arc<dyn ToolContext>,
}

impl RecallTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    async fn handle_insert(
        &self,
        content: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> ToolOutput {
        let content = match content {
            Some(c) => c,
            None => return ToolOutput::error("insert requires 'content' parameter"),
        };

        let memory = self.ctx.memory();
        match memory
            .insert_archival(self.ctx.agent_id(), content, metadata.cloned())
            .await
        {
            Ok(entry_id) => ToolOutput::success_with_data(
                "Archival entry created",
                json!({ "entry_id": entry_id }),
            ),
            Err(e) => ToolOutput::error(format!("Failed to insert archival entry: {}", e)),
        }
    }

    async fn handle_search(&self, query: Option<&str>, limit: Option<usize>) -> ToolOutput {
        let query = match query {
            Some(q) => q,
            None => return ToolOutput::error("search requires 'query' parameter"),
        };

        let limit = limit.unwrap_or(10);

        let memory = self.ctx.memory();
        match memory
            .search_archival(self.ctx.agent_id(), query, limit)
            .await
        {
            Ok(results) => {
                let entries: Vec<serde_json::Value> = results
                    .into_iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "content": r.content,
                            "created_at": r.created_at.to_rfc3339(),
                            "score": r.score,
                        })
                    })
                    .collect();

                ToolOutput::success_with_data(
                    format!("Found {} archival entries", entries.len()),
                    json!({ "entries": entries }),
                )
            }
            Err(e) => ToolOutput::error(format!("Search failed: {}", e)),
        }
    }
}

#[async_trait]
impl AiTool for RecallTool {
    type Input = RecallInput;
    type Output = ToolOutput;

    fn name(&self) -> &'static str {
        "recall"
    }

    fn description(&self) -> &'static str {
        "Manage archival memory: insert new entries for long-term storage or search existing entries. Entries are immutable once created."
    }

    fn usage_rule(&self) -> &'static str {
        "Use to store important information for later retrieval. Search when you need to remember something from the past."
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(self.name(), ToolRuleType::ContinueLoop)]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["insert", "search"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output, String> {
        let result = match input.op {
            RecallOp::Insert => {
                self.handle_insert(input.content.as_deref(), input.metadata.as_ref())
                    .await
            }
            RecallOp::Search => {
                self.handle_search(input.query.as_deref(), input.limit).await
            }
        };
        Ok(result)
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core recall_insert_and_search -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/recall.rs
git add crates/pattern_core/src/tool/builtin/tests.rs
git commit -m "$(cat <<'EOF'
feat: implement simplified recall tool for archival entries

Operations: insert, search
- insert: create immutable archival entry with optional metadata
- search: full-text search over archival entries

Simplified from v1 which had append/read/delete operations.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Source Tool Implementation

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/source.rs`
- Modify: `crates/pattern_core/src/tool/builtin/tests.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn test_source_tool_list() {
    let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

    let tool = SourceTool::new(ctx);
    let result = tool
        .execute(
            SourceInput {
                op: SourceOp::List,
                source_id: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

    // Should succeed even with no sources
    assert!(result.success);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core source_tool_list -- --nocapture`
Expected: FAIL - SourceTool not found

**Step 3: Implement SourceTool**

```rust
// crates/pattern_core/src/tool/builtin/source.rs
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::data_source::{SourceManager, StreamStatus, BlockSourceStatus};
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};

use super::types::{SourceInput, SourceOp, ToolOutput};

/// Data source control tool.
///
/// Operations:
/// - `pause` - Pause stream notifications
/// - `resume` - Resume stream notifications
/// - `status` - Get source status (works for both stream and block sources)
/// - `list` - List all registered sources
#[derive(Clone)]
pub struct SourceTool {
    ctx: Arc<dyn ToolContext>,
}

impl SourceTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    fn get_sources(&self) -> Option<Arc<dyn SourceManager>> {
        self.ctx.sources()
    }

    async fn handle_pause(&self, source_id: Option<&str>) -> ToolOutput {
        let source_id = match source_id {
            Some(id) => id,
            None => return ToolOutput::error("pause requires 'source_id' parameter"),
        };

        let sources = match self.get_sources() {
            Some(s) => s,
            None => return ToolOutput::error("No source manager available"),
        };

        // Check if it's a block source first
        if let Some(info) = sources.get_block_source_info(source_id) {
            return ToolOutput::error(format!(
                "Cannot pause block source '{}'. Status: {:?}. Loaded blocks: {:?}. Use 'file' tool to manage file operations.",
                source_id,
                info.status,
                info.loaded_paths.len()
            ));
        }

        // Try to pause stream
        match sources.pause_stream(source_id).await {
            Ok(()) => ToolOutput::success(format!("Paused stream '{}'", source_id)),
            Err(e) => ToolOutput::error(format!("Failed to pause: {}", e)),
        }
    }

    async fn handle_resume(&self, source_id: Option<&str>) -> ToolOutput {
        let source_id = match source_id {
            Some(id) => id,
            None => return ToolOutput::error("resume requires 'source_id' parameter"),
        };

        let sources = match self.get_sources() {
            Some(s) => s,
            None => return ToolOutput::error("No source manager available"),
        };

        // Check if it's a block source first
        if let Some(info) = sources.get_block_source_info(source_id) {
            return ToolOutput::error(format!(
                "Cannot resume block source '{}'. Status: {:?}. Use 'file' tool to manage file operations.",
                source_id,
                info.status
            ));
        }

        // Try to resume stream
        match sources.resume_stream(source_id).await {
            Ok(()) => ToolOutput::success(format!("Resumed stream '{}'", source_id)),
            Err(e) => ToolOutput::error(format!("Failed to resume: {}", e)),
        }
    }

    fn handle_status(&self, source_id: Option<&str>) -> ToolOutput {
        let sources = match self.get_sources() {
            Some(s) => s,
            None => return ToolOutput::error("No source manager available"),
        };

        match source_id {
            Some(id) => {
                // Specific source status
                if let Some(info) = sources.get_stream_info(id) {
                    return ToolOutput::success_with_data(
                        format!("Stream source '{}' status", id),
                        json!({
                            "type": "stream",
                            "source_id": info.source_id,
                            "name": info.name,
                            "status": format!("{:?}", info.status),
                            "supports_pull": info.supports_pull,
                            "block_schemas": info.block_schemas.len(),
                        }),
                    );
                }

                if let Some(info) = sources.get_block_source_info(id) {
                    return ToolOutput::success_with_data(
                        format!("Block source '{}' status", id),
                        json!({
                            "type": "block",
                            "source_id": info.source_id,
                            "name": info.name,
                            "status": format!("{:?}", info.status),
                            "permission_rules": info.permission_rules.len(),
                        }),
                    );
                }

                ToolOutput::error(format!("Source '{}' not found", id))
            }
            None => {
                // All sources status
                let streams = sources.list_streams();
                let blocks = sources.list_block_sources();

                ToolOutput::success_with_data(
                    "All sources status",
                    json!({
                        "streams": streams,
                        "block_sources": blocks,
                    }),
                )
            }
        }
    }

    fn handle_list(&self) -> ToolOutput {
        let sources = match self.get_sources() {
            Some(s) => s,
            None => {
                return ToolOutput::success_with_data(
                    "No source manager - no sources available",
                    json!({
                        "streams": [],
                        "block_sources": [],
                    }),
                )
            }
        };

        let streams: Vec<serde_json::Value> = sources
            .list_streams()
            .into_iter()
            .filter_map(|id| {
                sources.get_stream_info(&id).map(|info| {
                    json!({
                        "source_id": info.source_id,
                        "name": info.name,
                        "type": "stream",
                        "status": format!("{:?}", info.status),
                    })
                })
            })
            .collect();

        let blocks: Vec<serde_json::Value> = sources
            .list_block_sources()
            .into_iter()
            .filter_map(|id| {
                sources.get_block_source_info(&id).map(|info| {
                    json!({
                        "source_id": info.source_id,
                        "name": info.name,
                        "type": "block",
                        "status": format!("{:?}", info.status),
                    })
                })
            })
            .collect();

        ToolOutput::success_with_data(
            format!("Found {} stream(s) and {} block source(s)", streams.len(), blocks.len()),
            json!({
                "streams": streams,
                "block_sources": blocks,
            }),
        )
    }
}

#[async_trait]
impl AiTool for SourceTool {
    type Input = SourceInput;
    type Output = ToolOutput;

    fn name(&self) -> &'static str {
        "source"
    }

    fn description(&self) -> &'static str {
        "Control data sources: pause/resume stream notifications, check status, or list all sources."
    }

    fn usage_rule(&self) -> &'static str {
        "Use to manage data source notifications. Pause streams when you need to focus, resume when ready."
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(self.name(), ToolRuleType::ContinueLoop)]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["pause", "resume", "status", "list"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output, String> {
        let result = match input.op {
            SourceOp::Pause => self.handle_pause(input.source_id.as_deref()).await,
            SourceOp::Resume => self.handle_resume(input.source_id.as_deref()).await,
            SourceOp::Status => self.handle_status(input.source_id.as_deref()),
            SourceOp::List => self.handle_list(),
        };
        Ok(result)
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core source_tool_list -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/source.rs
git add crates/pattern_core/src/tool/builtin/tests.rs
git commit -m "$(cat <<'EOF'
feat: implement source tool for data source control

Operations: pause, resume, status, list
- pause/resume: control stream notifications (informative error for block sources)
- status: get source info (works for both types)
- list: enumerate all registered sources

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: FileSource Implementation

**Files:**
- Create: `crates/pattern_core/src/data_source/file_source.rs`
- Modify: `crates/pattern_core/src/data_source/mod.rs`

**Step 1: Write failing test**

```rust
// In crates/pattern_core/src/data_source/tests.rs
use super::file_source::FileSource;

#[tokio::test]
async fn test_file_source_load_save() {
    use tempfile::TempDir;
    use std::fs;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "Hello World").unwrap();

    let source = FileSource::new(
        "file",
        temp_dir.path().to_path_buf(),
        vec![],  // No permission rules = default ReadWrite
    );

    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    // Load file
    let block_ref = source
        .load(
            Path::new("test.txt"),
            ctx.clone(),
            AgentId::new("test-agent"),
        )
        .await
        .unwrap();

    assert!(block_ref.label.contains("test.txt"));

    // Verify content
    let content = memory
        .get_rendered_content("test-agent", &block_ref.label)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(content, "Hello World");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core file_source_load -- --nocapture`
Expected: FAIL - FileSource not found

**Step 3: Implement FileSource**

```rust
// crates/pattern_core/src/data_source/file_source.rs
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::broadcast;

use crate::memory::{BlockSchema, BlockType, MemoryPermission, MemoryStore};
use crate::runtime::ToolContext;
use crate::AgentId;

use super::block::{
    BlockSourceStatus, ConflictResolution, DataBlock, FileChange, FileChangeType,
    PermissionRule, ReconcileResult, VersionInfo,
};
use super::types::{BlockRef, BlockSchemaSpec};
use super::ToolRule;

/// Tracks a loaded file's state for conflict detection
#[derive(Debug, Clone)]
struct LoadedFileInfo {
    block_id: String,
    label: String,
    disk_mtime: SystemTime,
    disk_size: u64,
}

/// FileSource - DataBlock implementation for local filesystem.
///
/// Provides Loro-backed blocks for files with:
/// - Glob-based permission rules
/// - On-demand disk sync (v1)
/// - Conflict detection via mtime
#[derive(Debug)]
pub struct FileSource {
    source_id: String,
    base_path: PathBuf,
    permission_rules: Vec<PermissionRule>,
    loaded_blocks: DashMap<PathBuf, LoadedFileInfo>,
    status: std::sync::atomic::AtomicU8,
}

impl FileSource {
    pub fn new(
        source_id: impl Into<String>,
        base_path: PathBuf,
        permission_rules: Vec<PermissionRule>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            base_path,
            permission_rules,
            loaded_blocks: DashMap::new(),
            status: std::sync::atomic::AtomicU8::new(BlockSourceStatus::Idle as u8),
        }
    }

    /// Generate block label from path
    fn make_label(&self, path: &Path) -> String {
        let full_path = self.base_path.join(path);
        let mut hasher = DefaultHasher::new();
        full_path.hash(&mut hasher);
        let hash = hasher.finish();
        let hash8 = format!("{:08x}", hash & 0xFFFFFFFF);

        let relative = path.to_string_lossy();
        format!("file:{}:{}", hash8, relative)
    }

    /// Get full path from relative path
    fn full_path(&self, path: &Path) -> PathBuf {
        self.base_path.join(path)
    }

    /// Read file metadata for conflict detection
    async fn get_file_metadata(&self, path: &Path) -> std::io::Result<(SystemTime, u64)> {
        let full_path = self.full_path(path);
        let metadata = tokio::fs::metadata(&full_path).await?;
        Ok((metadata.modified()?, metadata.len()))
    }

    /// Check if file was modified since last load
    async fn check_conflict(&self, path: &Path) -> Result<(), String> {
        let full_path = self.full_path(path);
        if let Some(info) = self.loaded_blocks.get(&full_path) {
            let (mtime, size) = self
                .get_file_metadata(path)
                .await
                .map_err(|e| format!("Failed to check file: {}", e))?;

            if mtime != info.disk_mtime || size != info.disk_size {
                return Err(format!(
                    "File '{}' was modified externally. Use 'file load' to refresh.",
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl DataBlock for FileSource {
    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn name(&self) -> &str {
        "Local Files"
    }

    fn block_schema(&self) -> BlockSchemaSpec {
        BlockSchemaSpec {
            label_pattern: "file:{hash}:{path}".to_string(),
            schema: BlockSchema::Text,
            description: "File content as text block".to_string(),
            pinned: false,
        }
    }

    fn permission_rules(&self) -> &[PermissionRule] {
        &self.permission_rules
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]  // file tool registered separately
    }

    fn matches(&self, path: &Path) -> bool {
        // Check if path is under base_path
        if let Ok(canonical) = path.canonicalize() {
            if let Ok(base_canonical) = self.base_path.canonicalize() {
                return canonical.starts_with(&base_canonical);
            }
        }
        // Fallback: assume relative paths are under base
        !path.is_absolute()
    }

    fn permission_for(&self, path: &Path) -> MemoryPermission {
        let path_str = path.to_string_lossy();

        for rule in &self.permission_rules {
            if glob::Pattern::new(&rule.pattern)
                .map(|p| p.matches(&path_str))
                .unwrap_or(false)
            {
                return rule.permission.clone();
            }
        }

        // Default: ReadWrite
        MemoryPermission::ReadWrite
    }

    async fn load(
        &self,
        path: &Path,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> crate::Result<BlockRef> {
        let full_path = self.full_path(path);
        let label = self.make_label(path);

        // Read file content
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| crate::Error::Io(e.to_string()))?;

        // Get file metadata for conflict detection
        let (mtime, size) = self
            .get_file_metadata(path)
            .await
            .map_err(|e| crate::Error::Io(e.to_string()))?;

        let memory = ctx.memory();

        // Create or update block
        let block_id = match memory.get_block_metadata(&owner.to_string(), &label).await? {
            Some(_) => {
                // Block exists, update content
                memory
                    .update_block_text(&owner.to_string(), &label, &content)
                    .await?;
                memory
                    .get_block_metadata(&owner.to_string(), &label)
                    .await?
                    .unwrap()
                    .id
            }
            None => {
                // Create new block
                let id = memory
                    .create_block(
                        &owner.to_string(),
                        &label,
                        &format!("File: {}", path.display()),
                        BlockType::Working,
                        BlockSchema::Text,
                        content.len() as i64 * 2, // Allow growth
                    )
                    .await?;
                memory
                    .update_block_text(&owner.to_string(), &label, &content)
                    .await?;
                id
            }
        };

        // Track loaded file
        self.loaded_blocks.insert(
            full_path,
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
            },
        );

        Ok(BlockRef {
            label,
            block_id,
            agent_id: owner.to_string(),
        })
    }

    async fn create(
        &self,
        path: &Path,
        initial_content: Option<&str>,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> crate::Result<BlockRef> {
        let full_path = self.full_path(path);

        // Check if file already exists
        if full_path.exists() {
            return Err(crate::Error::AlreadyExists(format!(
                "File '{}' already exists",
                path.display()
            )));
        }

        // Check permission
        let permission = self.permission_for(path);
        if !permission.allows_write() {
            return Err(crate::Error::PermissionDenied(format!(
                "Cannot create file at '{}'",
                path.display()
            )));
        }

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| crate::Error::Io(e.to_string()))?;
        }

        // Write file
        let content = initial_content.unwrap_or("");
        tokio::fs::write(&full_path, content)
            .await
            .map_err(|e| crate::Error::Io(e.to_string()))?;

        // Load the newly created file
        self.load(path, ctx, owner).await
    }

    async fn save(&self, block_ref: &BlockRef, ctx: Arc<dyn ToolContext>) -> crate::Result<()> {
        // Extract path from label
        let path_str = block_ref
            .label
            .split(':')
            .nth(2)
            .ok_or_else(|| crate::Error::InvalidInput("Invalid file label".to_string()))?;
        let path = Path::new(path_str);

        // Check permission
        let permission = self.permission_for(path);
        if !permission.allows_write() {
            return Err(crate::Error::PermissionDenied(format!(
                "Cannot write to '{}'",
                path.display()
            )));
        }

        // Check for conflicts
        self.check_conflict(path)
            .await
            .map_err(|e| crate::Error::Conflict(e))?;

        // Get content from block
        let memory = ctx.memory();
        let content = memory
            .get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await?
            .ok_or_else(|| crate::Error::NotFound("Block not found".to_string()))?;

        // Write to disk
        let full_path = self.full_path(path);
        tokio::fs::write(&full_path, &content)
            .await
            .map_err(|e| crate::Error::Io(e.to_string()))?;

        // Update tracking
        let (mtime, size) = self
            .get_file_metadata(path)
            .await
            .map_err(|e| crate::Error::Io(e.to_string()))?;

        if let Some(mut info) = self.loaded_blocks.get_mut(&full_path) {
            info.disk_mtime = mtime;
            info.disk_size = size;
        }

        Ok(())
    }

    async fn delete(&self, path: &Path, ctx: Arc<dyn ToolContext>) -> crate::Result<()> {
        let full_path = self.full_path(path);

        // Check permission - delete usually requires escalation
        let permission = self.permission_for(path);
        if !permission.allows_delete() {
            return Err(crate::Error::PermissionDenied(format!(
                "Cannot delete '{}' - requires escalation",
                path.display()
            )));
        }

        // Delete file
        tokio::fs::remove_file(&full_path)
            .await
            .map_err(|e| crate::Error::Io(e.to_string()))?;

        // Remove from tracking
        self.loaded_blocks.remove(&full_path);

        // Delete block if it exists
        let label = self.make_label(path);
        let memory = ctx.memory();
        let _ = memory.delete_block(&ctx.agent_id().to_string(), &label).await;

        Ok(())
    }

    async fn start_watch(&self) -> Option<broadcast::Receiver<FileChange>> {
        // v1: No watching support
        None
    }

    async fn stop_watch(&self) -> crate::Result<()> {
        Ok(())
    }

    fn status(&self) -> BlockSourceStatus {
        let val = self.status.load(std::sync::atomic::Ordering::SeqCst);
        match val {
            0 => BlockSourceStatus::Idle,
            1 => BlockSourceStatus::Watching,
            _ => BlockSourceStatus::Idle,
        }
    }

    async fn reconcile(
        &self,
        paths: &[PathBuf],
        ctx: Arc<dyn ToolContext>,
    ) -> crate::Result<Vec<ReconcileResult>> {
        let mut results = Vec::new();

        for path in paths {
            let full_path = self.full_path(path);

            if let Some(info) = self.loaded_blocks.get(&full_path) {
                match self.get_file_metadata(path).await {
                    Ok((mtime, size)) => {
                        if mtime != info.disk_mtime || size != info.disk_size {
                            // File changed - for v1, just report it
                            results.push(ReconcileResult::NeedsResolution {
                                path: path.to_string_lossy().to_string(),
                                disk_changes: "File modified on disk".to_string(),
                                agent_changes: "Block may have local changes".to_string(),
                            });
                        } else {
                            results.push(ReconcileResult::NoChange {
                                path: path.to_string_lossy().to_string(),
                            });
                        }
                    }
                    Err(_) => {
                        // File may have been deleted
                        results.push(ReconcileResult::NeedsResolution {
                            path: path.to_string_lossy().to_string(),
                            disk_changes: "File not accessible".to_string(),
                            agent_changes: "Block exists".to_string(),
                        });
                    }
                }
            } else {
                results.push(ReconcileResult::NoChange {
                    path: path.to_string_lossy().to_string(),
                });
            }
        }

        Ok(results)
    }

    async fn history(
        &self,
        _block_ref: &BlockRef,
        _ctx: Arc<dyn ToolContext>,
    ) -> crate::Result<Vec<VersionInfo>> {
        // v1: No history support (would use Loro's version tracking)
        Ok(vec![])
    }

    async fn rollback(
        &self,
        _block_ref: &BlockRef,
        _version: &str,
        _ctx: Arc<dyn ToolContext>,
    ) -> crate::Result<()> {
        Err(crate::Error::NotImplemented(
            "Rollback not yet implemented".to_string(),
        ))
    }

    async fn diff(
        &self,
        block_ref: &BlockRef,
        _from: Option<&str>,
        _to: Option<&str>,
        ctx: Arc<dyn ToolContext>,
    ) -> crate::Result<String> {
        // Simple diff: show current block content vs disk
        let path_str = block_ref
            .label
            .split(':')
            .nth(2)
            .ok_or_else(|| crate::Error::InvalidInput("Invalid file label".to_string()))?;
        let path = Path::new(path_str);

        let memory = ctx.memory();
        let block_content = memory
            .get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await?
            .unwrap_or_default();

        let disk_content = tokio::fs::read_to_string(self.full_path(path))
            .await
            .unwrap_or_else(|_| "(file not readable)".to_string());

        if block_content == disk_content {
            Ok("No differences".to_string())
        } else {
            Ok(format!(
                "Block and disk differ.\n\n--- Disk ---\n{}\n\n--- Block ---\n{}",
                disk_content, block_content
            ))
        }
    }
}
```

**Step 4: Add to mod.rs exports**

```rust
// In crates/pattern_core/src/data_source/mod.rs
pub mod file_source;
pub use file_source::FileSource;
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core file_source_load -- --nocapture`
Expected: PASS

**Step 6: Add more tests**

```rust
#[tokio::test]
async fn test_file_source_create() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let source = FileSource::new("file", temp_dir.path().to_path_buf(), vec![]);

    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    let block_ref = source
        .create(
            Path::new("new_file.txt"),
            Some("New content"),
            ctx.clone(),
            AgentId::new("test-agent"),
        )
        .await
        .unwrap();

    // Verify file created on disk
    let disk_content = std::fs::read_to_string(temp_dir.path().join("new_file.txt")).unwrap();
    assert_eq!(disk_content, "New content");

    // Verify block created
    let block_content = memory
        .get_rendered_content("test-agent", &block_ref.label)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(block_content, "New content");
}

#[tokio::test]
async fn test_file_source_save() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("save_test.txt");
    std::fs::write(&test_file, "Original").unwrap();

    let source = FileSource::new("file", temp_dir.path().to_path_buf(), vec![]);
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    // Load file
    let block_ref = source
        .load(
            Path::new("save_test.txt"),
            ctx.clone(),
            AgentId::new("test-agent"),
        )
        .await
        .unwrap();

    // Modify block
    memory
        .update_block_text("test-agent", &block_ref.label, "Modified")
        .await
        .unwrap();

    // Save back to disk
    source.save(&block_ref, ctx).await.unwrap();

    // Verify disk updated
    let disk_content = std::fs::read_to_string(&test_file).unwrap();
    assert_eq!(disk_content, "Modified");
}

#[tokio::test]
async fn test_file_source_conflict_detection() {
    use tempfile::TempDir;
    use std::thread::sleep;
    use std::time::Duration;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("conflict_test.txt");
    std::fs::write(&test_file, "Original").unwrap();

    let source = FileSource::new("file", temp_dir.path().to_path_buf(), vec![]);
    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    // Load file
    let block_ref = source
        .load(
            Path::new("conflict_test.txt"),
            ctx.clone(),
            AgentId::new("test-agent"),
        )
        .await
        .unwrap();

    // Simulate external modification
    sleep(Duration::from_millis(100));  // Ensure different mtime
    std::fs::write(&test_file, "External change").unwrap();

    // Modify block
    memory
        .update_block_text("test-agent", &block_ref.label, "Agent change")
        .await
        .unwrap();

    // Try to save - should fail with conflict
    let result = source.save(&block_ref, ctx).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("modified externally"));
}
```

**Step 7: Run all FileSource tests**

Run: `cargo test -p pattern_core file_source -- --nocapture`
Expected: All PASS

**Step 8: Commit**

```bash
git add crates/pattern_core/src/data_source/file_source.rs
git add crates/pattern_core/src/data_source/mod.rs
git add crates/pattern_core/src/data_source/tests.rs
git commit -m "$(cat <<'EOF'
feat: implement FileSource as first DataBlock

Features:
- Load files into Loro-backed blocks
- Save blocks back to disk
- Create new files
- Conflict detection via mtime
- Glob-based permission rules
- Hash-based label format for uniqueness

v1 implementation with on-demand sync, no file watching yet.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: File Tool Implementation

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/file.rs`
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs`
- Modify: `crates/pattern_core/src/tool/builtin/tests.rs`

**Step 1: Write failing test**

```rust
#[tokio::test]
async fn test_file_tool_load() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("tool_test.txt");
    std::fs::write(&test_file, "Tool test content").unwrap();

    let source = Arc::new(FileSource::new(
        "file",
        temp_dir.path().to_path_buf(),
        vec![],
    ));

    let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

    let tool = FileTool::new(ctx.clone(), source);
    let result = tool
        .execute(
            FileInput {
                op: FileOp::Load,
                path: Some("tool_test.txt".to_string()),
                label: None,
                content: None,
                old: None,
                new: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

    assert!(result.success);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core file_tool_load -- --nocapture`
Expected: FAIL - FileTool not found

**Step 3: Implement FileTool**

```rust
// crates/pattern_core/src/tool/builtin/file.rs
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::data_source::file_source::FileSource;
use crate::data_source::DataBlock;
use crate::memory::MemoryStore;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};
use crate::AgentId;

use super::types::{FileInput, FileOp, ToolOutput};

/// File operations tool - registered by FileSource.
///
/// Operations:
/// - `load` - Load file from disk into block
/// - `save` - Save block content to disk
/// - `create` - Create new file
/// - `delete` - Delete file
/// - `append` - Append to file
/// - `replace` - Find/replace in file
#[derive(Clone)]
pub struct FileTool {
    ctx: Arc<dyn ToolContext>,
    source: Arc<FileSource>,
}

impl FileTool {
    pub fn new(ctx: Arc<dyn ToolContext>, source: Arc<FileSource>) -> Self {
        Self { ctx, source }
    }

    fn agent_id(&self) -> AgentId {
        AgentId::new(self.ctx.agent_id())
    }

    async fn handle_load(&self, path: Option<&str>) -> ToolOutput {
        let path = match path {
            Some(p) => p,
            None => return ToolOutput::error("load requires 'path' parameter"),
        };

        match self
            .source
            .load(Path::new(path), self.ctx.clone(), self.agent_id())
            .await
        {
            Ok(block_ref) => ToolOutput::success_with_data(
                format!("Loaded file '{}'", path),
                json!({
                    "label": block_ref.label,
                    "block_id": block_ref.block_id,
                }),
            ),
            Err(e) => ToolOutput::error(format!("Failed to load file: {}", e)),
        }
    }

    async fn handle_save(&self, path: Option<&str>, label: Option<&str>) -> ToolOutput {
        // Get block ref from path or label
        let block_ref = if let Some(lbl) = label {
            let memory = self.ctx.memory();
            match memory
                .get_block_metadata(self.ctx.agent_id(), lbl)
                .await
            {
                Ok(Some(meta)) => crate::data_source::BlockRef {
                    label: lbl.to_string(),
                    block_id: meta.id,
                    agent_id: self.ctx.agent_id().to_string(),
                },
                Ok(None) => return ToolOutput::error(format!("Block '{}' not found", lbl)),
                Err(e) => return ToolOutput::error(format!("Failed to get block: {}", e)),
            }
        } else if let Some(p) = path {
            // Construct label from path
            let label = self.source.make_label_public(Path::new(p));
            let memory = self.ctx.memory();
            match memory
                .get_block_metadata(self.ctx.agent_id(), &label)
                .await
            {
                Ok(Some(meta)) => crate::data_source::BlockRef {
                    label,
                    block_id: meta.id,
                    agent_id: self.ctx.agent_id().to_string(),
                },
                Ok(None) => {
                    return ToolOutput::error(format!(
                        "File '{}' not loaded. Use 'file load' first.",
                        p
                    ))
                }
                Err(e) => return ToolOutput::error(format!("Failed to get block: {}", e)),
            }
        } else {
            return ToolOutput::error("save requires 'path' or 'label' parameter");
        };

        match self.source.save(&block_ref, self.ctx.clone()).await {
            Ok(()) => ToolOutput::success(format!("Saved to disk")),
            Err(e) => ToolOutput::error(format!("Failed to save: {}", e)),
        }
    }

    async fn handle_create(&self, path: Option<&str>, content: Option<&str>) -> ToolOutput {
        let path = match path {
            Some(p) => p,
            None => return ToolOutput::error("create requires 'path' parameter"),
        };

        match self
            .source
            .create(Path::new(path), content, self.ctx.clone(), self.agent_id())
            .await
        {
            Ok(block_ref) => ToolOutput::success_with_data(
                format!("Created file '{}'", path),
                json!({
                    "label": block_ref.label,
                    "block_id": block_ref.block_id,
                }),
            ),
            Err(e) => ToolOutput::error(format!("Failed to create file: {}", e)),
        }
    }

    async fn handle_delete(&self, path: Option<&str>) -> ToolOutput {
        let path = match path {
            Some(p) => p,
            None => return ToolOutput::error("delete requires 'path' parameter"),
        };

        match self.source.delete(Path::new(path), self.ctx.clone()).await {
            Ok(()) => ToolOutput::success(format!("Deleted file '{}'", path)),
            Err(e) => ToolOutput::error(format!("Failed to delete: {}", e)),
        }
    }

    async fn handle_append(&self, path: Option<&str>, content: Option<&str>) -> ToolOutput {
        let path = match path {
            Some(p) => p,
            None => return ToolOutput::error("append requires 'path' parameter"),
        };
        let content = match content {
            Some(c) => c,
            None => return ToolOutput::error("append requires 'content' parameter"),
        };

        // Ensure file is loaded
        let label = self.source.make_label_public(Path::new(path));
        let memory = self.ctx.memory();

        if memory
            .get_block_metadata(self.ctx.agent_id(), &label)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            // Auto-load if not loaded
            if let Err(e) = self
                .source
                .load(Path::new(path), self.ctx.clone(), self.agent_id())
                .await
            {
                return ToolOutput::error(format!("Failed to load file: {}", e));
            }
        }

        // Append to block
        match memory
            .append_to_block(self.ctx.agent_id(), &label, content)
            .await
        {
            Ok(()) => ToolOutput::success(format!("Appended to '{}'", path)),
            Err(e) => ToolOutput::error(format!("Failed to append: {}", e)),
        }
    }

    async fn handle_replace(
        &self,
        path: Option<&str>,
        old: Option<&str>,
        new: Option<&str>,
    ) -> ToolOutput {
        let path = match path {
            Some(p) => p,
            None => return ToolOutput::error("replace requires 'path' parameter"),
        };
        let old = match old {
            Some(o) => o,
            None => return ToolOutput::error("replace requires 'old' parameter"),
        };
        let new = match new {
            Some(n) => n,
            None => return ToolOutput::error("replace requires 'new' parameter"),
        };

        // Ensure file is loaded
        let label = self.source.make_label_public(Path::new(path));
        let memory = self.ctx.memory();

        if memory
            .get_block_metadata(self.ctx.agent_id(), &label)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            // Auto-load if not loaded
            if let Err(e) = self
                .source
                .load(Path::new(path), self.ctx.clone(), self.agent_id())
                .await
            {
                return ToolOutput::error(format!("Failed to load file: {}", e));
            }
        }

        // Replace in block
        match memory
            .replace_in_block(self.ctx.agent_id(), &label, old, new)
            .await
        {
            Ok(()) => ToolOutput::success(format!("Replaced in '{}'", path)),
            Err(e) => ToolOutput::error(format!("Failed to replace: {}", e)),
        }
    }
}

#[async_trait]
impl AiTool for FileTool {
    type Input = FileInput;
    type Output = ToolOutput;

    fn name(&self) -> &'static str {
        "file"
    }

    fn description(&self) -> &'static str {
        "File operations: load files into memory, save changes to disk, create or delete files, append or replace content."
    }

    fn usage_rule(&self) -> &'static str {
        "Use for file I/O. Load files before editing. Save to persist changes to disk."
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(self.name(), ToolRuleType::ContinueLoop)]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["load", "save", "create", "delete", "append", "replace"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output, String> {
        let result = match input.op {
            FileOp::Load => self.handle_load(input.path.as_deref()).await,
            FileOp::Save => {
                self.handle_save(input.path.as_deref(), input.label.as_deref())
                    .await
            }
            FileOp::Create => {
                self.handle_create(input.path.as_deref(), input.content.as_deref())
                    .await
            }
            FileOp::Delete => self.handle_delete(input.path.as_deref()).await,
            FileOp::Append => {
                self.handle_append(input.path.as_deref(), input.content.as_deref())
                    .await
            }
            FileOp::Replace => {
                self.handle_replace(
                    input.path.as_deref(),
                    input.old.as_deref(),
                    input.new.as_deref(),
                )
                .await
            }
        };
        Ok(result)
    }
}
```

**Step 4: Add make_label_public to FileSource**

```rust
// In FileSource, make this public:
pub fn make_label_public(&self, path: &Path) -> String {
    self.make_label(path)
}
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core file_tool_load -- --nocapture`
Expected: PASS

**Step 6: Add more tests**

```rust
#[tokio::test]
async fn test_file_tool_create_and_append() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let source = Arc::new(FileSource::new(
        "file",
        temp_dir.path().to_path_buf(),
        vec![],
    ));

    let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let tool = FileTool::new(ctx.clone(), source);

    // Create
    let result = tool
        .execute(
            FileInput {
                op: FileOp::Create,
                path: Some("new.txt".to_string()),
                label: None,
                content: Some("Initial".to_string()),
                old: None,
                new: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Append
    let result = tool
        .execute(
            FileInput {
                op: FileOp::Append,
                path: Some("new.txt".to_string()),
                label: None,
                content: Some(" more".to_string()),
                old: None,
                new: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Save
    let result = tool
        .execute(
            FileInput {
                op: FileOp::Save,
                path: Some("new.txt".to_string()),
                label: None,
                content: None,
                old: None,
                new: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();
    assert!(result.success);

    // Verify disk
    let content = std::fs::read_to_string(temp_dir.path().join("new.txt")).unwrap();
    assert_eq!(content, "Initial more");
}
```

**Step 7: Run all file tool tests**

Run: `cargo test -p pattern_core file_tool -- --nocapture`
Expected: All PASS

**Step 8: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/file.rs
git add crates/pattern_core/src/tool/builtin/mod.rs
git add crates/pattern_core/src/tool/builtin/tests.rs
git add crates/pattern_core/src/data_source/file_source.rs
git commit -m "$(cat <<'EOF'
feat: implement file tool for FileSource operations

Operations: load, save, create, delete, append, replace
- Ergonomic file-focused interface
- Auto-loads files for edit operations
- Works with FileSource for disk sync

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: BuiltinTools Integration

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs`

**Step 1: Update BuiltinTools to include new tools**

The existing `BuiltinTools` struct should be extended to include the new tools alongside existing ones:

```rust
// In crates/pattern_core/src/tool/builtin/mod.rs, update BuiltinTools struct and impl:

pub struct BuiltinTools {
    // Existing tools
    pub recall_tool: Box<dyn DynamicTool>,
    pub context_tool: Box<dyn DynamicTool>,
    pub search_tool: Box<dyn DynamicTool>,
    pub send_message_tool: Box<dyn DynamicTool>,
    // ... other existing ...

    // New tools
    pub block_tool: Box<dyn DynamicTool>,
    pub block_edit_tool: Box<dyn DynamicTool>,
    pub source_tool: Box<dyn DynamicTool>,
}

impl BuiltinTools {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self {
            // ... existing tool initialization ...

            // New tools
            block_tool: Box::new(DynamicToolAdapter::new(BlockTool::new(ctx.clone()))),
            block_edit_tool: Box::new(DynamicToolAdapter::new(BlockEditTool::new(ctx.clone()))),
            source_tool: Box::new(DynamicToolAdapter::new(SourceTool::new(ctx))),
        }
    }

    pub fn register_all(&self, registry: &ToolRegistry) {
        // ... existing registrations ...

        // New tools
        registry.register_dynamic(dyn_clone::clone_box(&*self.block_tool));
        registry.register_dynamic(dyn_clone::clone_box(&*self.block_edit_tool));
        registry.register_dynamic(dyn_clone::clone_box(&*self.source_tool));
    }
}
```

**Step 2: Add test for new tool registration**

```rust
#[tokio::test]
async fn test_new_builtin_tools_registration() {
    let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

    let registry = ToolRegistry::new();
    let builtin = BuiltinTools::new(ctx);
    builtin.register_all(&registry);

    let tool_names = registry.list_tools();
    // New tools
    assert!(tool_names.iter().any(|n| n == "block"));
    assert!(tool_names.iter().any(|n| n == "block_edit"));
    assert!(tool_names.iter().any(|n| n == "source"));
    // Existing tools still present
    assert!(tool_names.iter().any(|n| n == "recall"));
    assert!(tool_names.iter().any(|n| n == "context"));
}
```

**Step 3: Run test**

Run: `cargo test -p pattern_core new_builtin_tools_registration -- --nocapture`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/mod.rs
git add crates/pattern_core/src/tool/builtin/tests.rs
git commit -m "$(cat <<'EOF'
feat: integrate new tools into BuiltinTools

Adds block, block_edit, source tools to standard builtin set.
File tool registered separately by FileSource.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Final Integration and Cargo Check

**Step 1: Run full cargo check**

Run: `cargo check -p pattern_core`
Expected: No errors

**Step 2: Run all tests**

Run: `cargo test -p pattern_core`
Expected: All tests pass

**Step 3: Run clippy**

Run: `cargo clippy -p pattern_core -- -D warnings`
Expected: No warnings

**Step 4: Final commit if any fixups needed**

```bash
git add -A
git commit -m "$(cat <<'EOF'
chore: fix clippy warnings and final cleanup

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

## Summary

**Tools implemented:**
1. `block` - lifecycle (load, pin, unpin, archive, info)
2. `block_edit` - content (append, replace, patch stub, set_field)
3. `recall` - archival entries (insert, search)
4. `source` - data source control (pause, resume, status, list)
5. `file` - FileSource operations (load, save, create, delete, append, replace)

**FileSource implemented:**
- DataBlock trait implementation
- Loro-backed blocks for files
- On-demand sync with conflict detection
- Glob-based permission rules

**Not yet implemented (future work):**
- `patch` operation in block_edit
- File watching (v2)
- Rollback/history using Loro versions
- Shell hook integration
