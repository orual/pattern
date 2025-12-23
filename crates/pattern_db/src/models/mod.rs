//! Database models.
//!
//! These structs map directly to database tables via sqlx.

mod agent;
mod coordination;
mod event;
mod folder;
mod memory;
mod message;
mod migration;
mod source;
mod task;

pub use agent::{
    Agent, AgentGroup, AgentStatus, GroupMember, GroupMemberRole, ModelRoutingConfig,
    ModelRoutingRule, PatternType, RoutingCondition,
};
pub use coordination::{
    ActivityEvent, ActivityEventType, AgentSummary, ConstellationSummary, CoordinationState,
    CoordinationTask, EventImportance, HandoffNote, NotableEvent, TaskPriority, TaskStatus,
};
pub use event::{Event, EventOccurrence, OccurrenceStatus};
pub use folder::{FilePassage, Folder, FolderAccess, FolderAttachment, FolderFile, FolderPathType};
pub use memory::{
    ArchivalEntry, MemoryBlock, MemoryBlockCheckpoint, MemoryBlockType, MemoryBlockUpdate,
    MemoryGate, MemoryOp, MemoryPermission, SharedBlockAttachment, UpdateSource, UpdateStats,
};
pub use message::{ArchiveSummary, Message, MessageRole, MessageSummary};
pub use migration::{
    EntityImport, IssueSeverity, MigrationAudit, MigrationIssue, MigrationLog, MigrationStats,
};
pub use source::{AgentDataSource, DataSource, SourceType};
pub use task::{Task, TaskSummary, UserTaskPriority, UserTaskStatus};
