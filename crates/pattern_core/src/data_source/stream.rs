//! DataStream trait for event-driven data sources.
//!
//! Sources that produce events over time (Bluesky firehose, Discord events,
//! LSP diagnostics, etc.) implement this trait.

use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::error::Result;
use crate::id::AgentId;
use crate::runtime::ToolContext;
use crate::tool::rules::ToolRule;

use super::{BlockEdit, BlockSchemaSpec, EditFeedback, Notification, StreamCursor};

/// Status of a data stream source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStatus {
    /// Source is stopped (not started or has been stopped)
    Stopped,
    /// Source is actively running and emitting events
    Running,
    /// Source is paused (may continue internal processing but not emitting)
    Paused,
}

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
///     async fn start(&self, ctx: Arc<dyn ToolContext>, owner: AgentId)
///         -> Result<broadcast::Receiver<Notification>>
///     {
///         // Create pinned config block via memory
///         let memory = ctx.memory();
///         let config_id = memory.create_block(&owner, "bluesky_config", ...).await?;
///
///         // Spawn event processor that sends Notifications
///         let (tx, rx) = broadcast::channel(256);
///         // ... spawn task ...
///         Ok(rx)
///     }
/// }
/// ```
#[async_trait]
pub trait DataStream: Send + Sync + Debug {
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
    /// Implementers should use interior mutability (e.g., Mutex, RwLock) for state.
    async fn start(
        &self,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>>;

    /// Stop the source and cleanup resources.
    /// Implementers should use interior mutability for state management.
    async fn stop(&self) -> Result<()>;

    // === Control ===

    /// Pause notification emission (source may continue processing internally).
    /// Implementers should use interior mutability for state management.
    fn pause(&self);

    /// Resume notification emission.
    /// Implementers should use interior mutability for state management.
    fn resume(&self);

    /// Current status of the stream source
    fn status(&self) -> StreamStatus;

    // === Optional Pull Support ===

    /// Whether this source supports on-demand pull (for backfill/history)
    fn supports_pull(&self) -> bool {
        false
    }

    /// Pull notifications on demand
    async fn pull(
        &self,
        _limit: usize,
        _cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>> {
        Ok(vec![])
    }

    // === Block Edit Handling ===

    /// Handle a block edit for blocks this source manages.
    ///
    /// Called when an agent edits a memory block that this source registered
    /// interest in via `register_edit_subscriber`. The source can approve,
    /// reject, or mark the edit as pending.
    ///
    /// Default implementation approves all edits.
    async fn handle_block_edit(
        &self,
        _edit: &BlockEdit,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<EditFeedback> {
        Ok(EditFeedback::Applied { message: None })
    }
}
