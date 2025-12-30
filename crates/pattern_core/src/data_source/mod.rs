//! # Data Sources - Event and Document Sources
//!
//! This module provides the data source architecture for Pattern, enabling agents
//! to consume external data through two complementary trait families.
//!
//! ## Overview
//!
//! Data sources bridge the gap between external systems and agent memory. They
//! create and manage memory blocks that agents can read and (with permission)
//! modify. The architecture follows these key design principles:
//!
//! - **No generics on traits**: Type safety enforced at source boundary
//! - **Unified access model**: Sources receive `Arc<dyn ToolContext>` - same access as tools
//! - **Channel-based delivery**: Notifications sent via tokio broadcast channels
//! - **Block references**: `BlockRef` points to blocks in the memory store
//! - **Loro-backed versioning**: DataBlock sources get full version history
//!
//! ## DataStream - Event-Driven Sources
//!
//! For sources that produce real-time notifications and/or maintain state blocks:
//!
//! - **Examples**: Bluesky firehose, Discord events, LSP diagnostics, sensors
//! - **Lifecycle**: `start()` spawns processing, returns `broadcast::Receiver<Notification>`
//! - **State management**: Via interior mutability (Mutex, RwLock)
//! - **Block types**: Pinned (always in context) or ephemeral (batch-scoped)
//!
//! ```ignore
//! impl DataStream for BlueskySource {
//!     async fn start(&self, ctx: Arc<dyn ToolContext>, owner: AgentId)
//!         -> Result<broadcast::Receiver<Notification>>
//!     {
//!         // Create pinned config block via memory
//!         let memory = ctx.memory();
//!         let config_id = memory.create_block(&owner, "bluesky_config", ...).await?;
//!
//!         // Spawn event processor that sends Notifications
//!         let (tx, rx) = broadcast::channel(256);
//!         // ... spawn task that calls tx.send(notification) ...
//!         Ok(rx)
//!     }
//! }
//! ```
//!
//! ## DataBlock - Document-Oriented Sources
//!
//! For persistent documents with versioning and permission-gated edits:
//!
//! - **Examples**: Files, configs, structured documents, databases
//! - **Versioning**: Loro CRDT-backed with full history and rollback
//! - **Permissions**: Glob-based rules determine read/write/escalation
//! - **Sync model**: Disk is canonical; reconcile after external changes
//!
//! ```text
//! Agent tools <-> Loro <-> Disk <-> Editor (ACP)
//!                   ^
//!              Shell side effects
//! ```
//!
//! ```ignore
//! impl DataBlock for FileSource {
//!     async fn load(&self, path: &Path, ctx: Arc<dyn ToolContext>, owner: AgentId)
//!         -> Result<BlockRef>
//!     {
//!         let content = tokio::fs::read_to_string(path).await?;
//!         let memory = ctx.memory();
//!         let block_id = memory.create_block(&owner, ...).await?;
//!         memory.update_block_text(&owner, &label, &content).await?;
//!         Ok(BlockRef::new(label, block_id).owned_by(owner))
//!     }
//! }
//! ```
//!
//! ## Key Types
//!
//! ### Core References
//!
//! - [`BlockRef`]: Reference to a block in the memory store (label + block_id + owner)
//! - [`Notification`]: Message plus block references delivered via broadcast channel
//! - [`StreamCursor`]: Opaque cursor for pull-based pagination
//!
//! ### Schema and Status
//!
//! - [`BlockSchemaSpec`]: Declares block schemas a source creates (pinned vs ephemeral)
//! - [`StreamStatus`]: Running, Stopped, or Paused state for stream sources
//! - [`BlockSourceStatus`]: Idle or Watching state for block sources
//!
//! ### Block Source Types
//!
//! - [`PermissionRule`]: Glob pattern to permission level mapping
//! - [`FileChange`]: External file modification event
//! - [`VersionInfo`]: Version history entry with timestamp
//! - [`ReconcileResult`]: Outcome of disk/Loro reconciliation
//!
//! ## Source Management
//!
//! [`SourceManager`] is the trait for source lifecycle and operations, implemented
//! by `RuntimeContext`. Tools and sources access it via `ToolContext::sources()`.
//!
//! Key operations:
//! - **Stream lifecycle**: `pause_stream`, `resume_stream`, `subscribe_to_stream`
//! - **Block operations**: `load_block`, `save_block`, `reconcile_blocks`
//! - **Edit routing**: `handle_block_edit` routes edits to interested sources
//!
//! ## Helper Utilities
//!
//! This module provides fluent builders for source implementations:
//!
//! - [`BlockBuilder`]: Create blocks with proper metadata in one call chain
//! - [`NotificationBuilder`]: Build notifications with message and block refs
//! - [`EphemeralBlockCache`]: Get-or-create cache for ephemeral blocks by external ID
//!
//! ```ignore
//! // Creating a block
//! let block_ref = BlockBuilder::new(memory, owner, "user_profile")
//!     .description("User profile information")
//!     .schema(BlockSchema::Text)
//!     .pinned()
//!     .content("Initial content")
//!     .build()
//!     .await?;
//!
//! // Building a notification
//! let notification = NotificationBuilder::new()
//!     .text("New message from @alice")
//!     .block(user_block_ref)
//!     .block(context_block_ref)
//!     .build();
//! ```

mod block;
mod file_source;
mod helpers;
mod manager;
mod registry;
mod stream;
mod types;

#[cfg(test)]
mod tests;

pub use block::{
    BlockSourceStatus, ConflictResolution, DataBlock, FileChange, FileChangeType, PermissionRule,
    ReconcileResult, RestoreStats, VersionInfo,
};
pub use file_source::{
    FileInfo, FileSource, FileSyncStatus, ParsedFileLabel, is_file_label, parse_file_label,
};
pub use helpers::{BlockBuilder, EphemeralBlockCache, NotificationBuilder};
pub use manager::{BlockEdit, BlockSourceInfo, EditFeedback, SourceManager, StreamSourceInfo};
pub use registry::{
    CustomBlockSourceFactory, CustomStreamSourceFactory, available_custom_block_types,
    available_custom_stream_types, create_custom_block, create_custom_stream,
};
pub use stream::{DataStream, StreamStatus};
pub use types::*;
