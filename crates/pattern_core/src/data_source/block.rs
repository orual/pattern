//! DataBlock permission and file change types.
//!
//! Types for path-based access control, file change detection,
//! version history, and conflict resolution for DataBlock sources.

use std::{
    any::Any,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use globset::Glob;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::Result;
use crate::id::AgentId;
use crate::memory::MemoryPermission;
use crate::runtime::ToolContext;
use crate::tool::rules::ToolRule;

use super::{BlockEdit, BlockRef, BlockSchemaSpec, EditFeedback};

/// Permission rule for path-based access control
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub fn matches(&self, path: impl AsRef<Path>) -> bool {
        match Glob::new(&self.pattern) {
            Ok(glob) => glob.compile_matcher().is_match(path),
            Err(_) => false, // Invalid pattern doesn't match
        }
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
    pub path: PathBuf,
    pub change_type: FileChangeType,
    /// Block ID if we have a loaded block for this path
    pub block_id: Option<String>,
    /// When the change was detected
    pub timestamp: Option<DateTime<Utc>>,
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

/// Status of a block source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockSourceStatus {
    /// Source is idle (not watching)
    Idle,
    /// Source is actively watching for changes
    Watching,
}

/// Document-oriented data source with Loro-backed versioning.
///
/// Presents files and persistent documents as memory blocks with gated edits,
/// version history, and rollback capabilities. Agent works with these like
/// documents, pulling content when needed.
///
/// # Sync Model
///
/// ```text
/// Agent tools <-> Loro <-> Disk <-> Editor (ACP)
///                  ^
///             Shell side effects
/// ```
///
/// - **Loro as working state**: Agent's view with full version history
/// - **Disk as canonical**: External changes win via reconcile
/// - **Permission-gated writes**: Glob patterns determine access levels
///
/// # Interior Mutability
///
/// Like DataStream, implementers should use interior mutability (Mutex, RwLock)
/// for state management since all methods take `&self`.
///
/// # Example
///
/// ```ignore
/// impl DataBlock for FileSource {
///     async fn load(&self, path: &str, ctx: Arc<dyn ToolContext>, owner: AgentId)
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

    /// Block schema this source creates (for documentation/validation)
    fn block_schema(&self) -> BlockSchemaSpec;

    /// Permission rules (glob patterns -> permission levels)
    fn permission_rules(&self) -> &[PermissionRule];

    /// Tools required when working with this source
    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]
    }

    /// Check if path matches this source's scope (default: uses permission_rules)
    fn matches(&self, path: &Path) -> bool {
        self.permission_rules().iter().any(|r| r.matches(path))
    }

    /// Get permission for a specific path
    fn permission_for(&self, path: &Path) -> MemoryPermission;

    // === Load/Save Operations ===

    /// Load file content into memory store as a block
    async fn load(
        &self,
        path: &Path,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Create a new file with optional initial content
    async fn create(
        &self,
        path: &Path,
        initial_content: Option<&str>,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Save block back to disk (permission-gated)
    async fn save(&self, block_ref: &BlockRef, ctx: Arc<dyn ToolContext>) -> Result<()>;

    /// Delete file (usually requires escalation)
    async fn delete(&self, path: &Path, ctx: Arc<dyn ToolContext>) -> Result<()>;

    // === Watch/Reconcile ===

    /// Start watching for external changes (optional)
    async fn start_watch(&self) -> Option<broadcast::Receiver<FileChange>>;

    /// Stop watching for changes
    async fn stop_watch(&self) -> Result<()>;

    /// Current status of the block source
    fn status(&self) -> BlockSourceStatus;

    /// Reconcile disk state with Loro overlay after external changes
    async fn reconcile(
        &self,
        paths: &[PathBuf],
        ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<ReconcileResult>>;

    // === History Operations ===

    /// Get version history for a loaded block
    async fn history(
        &self,
        block_ref: &BlockRef,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<VersionInfo>>;

    /// Rollback to a previous version
    async fn rollback(
        &self,
        block_ref: &BlockRef,
        version: &str,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<()>;

    /// Diff between versions or current vs disk
    async fn diff(
        &self,
        block_ref: &BlockRef,
        from: Option<&str>,
        to: Option<&str>,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<String>;

    // === Event Handlers ===

    /// Handle a file change event from the watch task.
    ///
    /// Called by the monitoring task when external file changes are detected.
    /// The source can trigger reconciliation, notify agents, or take other actions.
    ///
    /// Default implementation does nothing.
    async fn handle_file_change(
        &self,
        _change: &FileChange,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<()> {
        Ok(())
    }

    /// Handle a block edit for blocks this source manages.
    ///
    /// Called when an agent edits a memory block that this source registered
    /// interest in via `register_edit_subscriber`. The source can approve,
    /// reject, or mark the edit as pending (e.g., for permission checks).
    ///
    /// Default implementation approves all edits.
    async fn handle_block_edit(
        &self,
        _edit: &BlockEdit,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<EditFeedback> {
        Ok(EditFeedback::Applied { message: None })
    }

    // === Downcasting Support ===

    /// Returns self as `&dyn Any` for downcasting to concrete types.
    ///
    /// This enables tools tightly coupled to specific source types to access
    /// source-specific methods not exposed through the DataBlock trait.
    ///
    /// # Example
    /// ```ignore
    /// if let Some(file_source) = source.as_any().downcast_ref::<FileSource>() {
    ///     file_source.list_files(pattern).await?;
    /// }
    /// ```
    fn as_any(&self) -> &dyn Any;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to test glob matching via PermissionRule
    fn glob_match(pattern: &str, path: &str) -> bool {
        PermissionRule::new(pattern, MemoryPermission::ReadOnly).matches(path)
    }

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
        // Note: globset treats ** differently - it matches zero or more path components
        // So src/**/*.rs matches src/foo.rs (** matches zero components)
        assert!(glob_match("src/**/*.rs", "src/foo.rs"));
        assert!(glob_match("src/**/*.rs", "src/bar/baz.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/c/d.rs"));
        assert!(!glob_match("src/**/*.rs", "test/foo.rs"));
    }

    #[test]
    fn test_glob_match_all() {
        assert!(glob_match("**", "anything/at/all.txt"));
    }

    #[test]
    fn test_permission_rule_equality() {
        let rule1 = PermissionRule::new("*.rs", MemoryPermission::ReadOnly);
        let rule2 = PermissionRule::new("*.rs", MemoryPermission::ReadOnly);
        let rule3 = PermissionRule::new("*.rs", MemoryPermission::ReadWrite);

        assert_eq!(rule1, rule2);
        assert_ne!(rule1, rule3);
    }

    #[test]
    fn test_permission_rule_invalid_pattern() {
        // Invalid glob pattern (unclosed bracket) should return false for any path
        let rule = PermissionRule::new("[invalid", MemoryPermission::ReadOnly);
        assert!(!rule.matches("any/path"));
        assert!(!rule.matches("src/main.rs"));
        assert!(!rule.matches("[invalid")); // Even matching the literal pattern fails
    }
}
