//! Database models.
//!
//! These structs map directly to database tables. Row structs gain
//! inherent `fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self>`
//! methods as queries are ported (Tasks 6-9).

mod agent;
mod event;
mod folder;
mod memory;
mod message;
mod migration;
mod source;
mod task;

pub use agent::{
    Agent, AgentAtprotoEndpoint, AgentStatus, ENDPOINT_TYPE_BLUESKY, ModelRoutingConfig,
    ModelRoutingRule, RoutingCondition,
};
pub use event::{Event, EventOccurrence, OccurrenceStatus};
pub use folder::{FilePassage, Folder, FolderAccess, FolderAttachment, FolderFile, FolderPathType};
pub use memory::{
    ArchivalEntry, MemoryBlock, MemoryBlockCheckpoint, MemoryBlockType, MemoryBlockUpdate,
    MemoryGate, MemoryOp, MemoryPermission, SharedBlockAttachment, UpdateSource, UpdateStats,
};
pub use message::{ArchiveSummary, BatchType, Message, MessageRole, MessageSummary, QueuedMessage};
pub use migration::{
    EntityImport, IssueSeverity, MigrationAudit, MigrationIssue, MigrationLog, MigrationStats,
};
pub use source::{AgentDataSource, DataSource, SourceType};
pub use task::{Task, UserTaskStatus};
