//! SourceManager trait - the interface for source operations exposed to tools and sources.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::DataStream;
use crate::id::AgentId;
use crate::{DataBlock, error::Result};

use super::{
    BlockRef, BlockSchemaSpec, BlockSourceStatus, Notification, PermissionRule, ReconcileResult,
    StreamCursor, StreamStatus, VersionInfo,
};

/// Info about a registered stream source
#[derive(Debug, Clone)]
pub struct StreamSourceInfo {
    pub source_id: String,
    pub name: String,
    pub block_schemas: Vec<BlockSchemaSpec>,
    pub status: StreamStatus,
    pub supports_pull: bool,
}

/// Info about a registered block source
#[derive(Debug, Clone)]
pub struct BlockSourceInfo {
    pub source_id: String,
    pub name: String,
    pub block_schema: BlockSchemaSpec,
    pub permission_rules: Vec<PermissionRule>,
    pub status: BlockSourceStatus,
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
pub trait SourceManager: Send + Sync + std::fmt::Debug {
    // === Stream Source Operations ===

    /// List registered stream sources
    fn list_streams(&self) -> Vec<String>;

    /// Get stream source info
    fn get_stream_info(&self, source_id: &str) -> Option<StreamSourceInfo>;

    /// Pause a stream source (stops notifications, source may continue internally)
    async fn pause_stream(&self, source_id: &str) -> Result<()>;

    /// Resume a stream source
    async fn resume_stream(&self, source_id: &str) -> Result<()>;

    /// Subscribe agent to a stream source
    async fn subscribe_to_stream(
        &self,
        agent_id: &AgentId,
        source_id: &str,
    ) -> Result<broadcast::Receiver<Notification>>;

    /// Unsubscribe agent from a stream source
    async fn unsubscribe_from_stream(&self, agent_id: &AgentId, source_id: &str) -> Result<()>;

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
    async fn load_block(&self, source_id: &str, path: &Path, owner: AgentId) -> Result<BlockRef>;

    /// Get a block source by its source_id
    fn get_block_source(&self, source_id: &str) -> Option<Arc<dyn DataBlock>>;

    /// Find a block source that matches the given path.
    ///
    /// Iterates through registered block sources and returns the first one
    /// whose `matches(path)` returns true. This enables path-based routing
    /// where tools can find the appropriate source without knowing its ID.
    fn find_block_source_for_path(&self, path: &Path) -> Option<Arc<dyn DataBlock>>;

    /// Get a stream source by its source_id
    fn get_stream_source(&self, source_id: &str) -> Option<Arc<dyn DataStream>>;

    /// Create a new file/document
    async fn create_block(
        &self,
        source_id: &str,
        path: &Path,
        content: Option<&str>,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Save block back to external storage
    async fn save_block(&self, source_id: &str, block_ref: &BlockRef) -> Result<()>;

    /// Delete a file/document through a block source
    async fn delete_block(&self, source_id: &str, path: &Path) -> Result<()>;

    /// Reconcile after external changes
    async fn reconcile_blocks(
        &self,
        source_id: &str,
        paths: &[PathBuf],
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
