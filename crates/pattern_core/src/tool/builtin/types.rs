// crates/pattern_core/src/tool/builtin/types.rs
//! Shared input/output types for the v2 tool taxonomy.
//!
//! These types support the new tool system (`block`, `block_edit`, `recall`, `source`, `file`)
//! which will eventually replace the legacy `context` and `recall` tools.

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
    /// Set viewport for Text blocks (controls which lines are visible)
    Viewport,
    /// Share block with another agent by name
    Share,
    /// Remove sharing from another agent by name
    Unshare,
}

/// Input for the `block` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockInput {
    /// Operation to perform
    pub op: BlockOp,
    /// Block label
    pub label: String,
    /// Optional source ID for load operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Starting line for viewport operation (1-indexed, default: 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// Number of lines to display for viewport operation (default: show all)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_lines: Option<usize>,
    /// Target agent name for share/unshare operations
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent: Option<String>,
    /// Permission level for share operation (default: Append)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<crate::memory::MemoryPermission>,
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
    /// Replace a range of lines with new content
    EditRange,
}

/// Mode for the replace operation
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceMode {
    /// Replace first occurrence (default)
    #[default]
    First,
    /// Replace all occurrences
    All,
    /// Replace nth occurrence - parse "N: pattern" from 'old' field
    Nth,
    /// Treat 'old' as a regex pattern
    Regex,
}

/// Input for the `block_edit` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlockEditInput {
    /// Operation to perform
    pub op: BlockEditOp,
    /// Block label
    pub label: String,
    /// Content for append operation, or "START-END: content" for edit_range
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Old text for replace operation. For nth mode: "N: pattern"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// New text for replace operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    /// Field name for set_field operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Value for set_field operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Patch content for patch operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// Mode for replace operation (default: first)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ReplaceMode>,
}

/// Operations for the `recall` tool (archival entries)
///
/// Note: This is part of the v2 tool taxonomy. The legacy `RecallInput` in `recall.rs`
/// uses `ArchivalMemoryOperationType` which has different operations (Insert, Append, Read, Delete).
/// This new version is simpler: just Insert and Search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecallOp {
    /// Create new archival entry
    Insert,
    /// Search archival entries
    Search,
}

/// Input for the `recall` tool
///
/// This is the new recall input type that replaces the legacy version.
/// Uses simple Insert/Search operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecallInput {
    /// Operation to perform
    pub op: RecallOp,
    /// Content for insert operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Metadata for insert operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Query for search operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Limit for search results
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// List files in source (supports glob patterns)
    List,
    /// Check sync status of loaded files
    Status,
    /// Show unified diff between memory and disk
    Diff,
    /// Reload file from disk, discarding memory changes
    Reload,
}

/// Input for the `file` tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileInput {
    /// Operation to perform
    pub op: FileOp,
    /// File path (relative to source base, or absolute for path-based routing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Block label (alternative to path for save)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Content for create/append operations
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Old text for replace operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// New text for replace operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    /// Glob pattern for list operation (e.g., "**/*.rs")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Explicit source ID (optional - if not provided, inferred from path or label)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Standard output for tool operations
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolOutput {
    /// Whether operation succeeded
    pub success: bool,
    /// Human-readable message
    pub message: String,
    /// Optional structured data
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
