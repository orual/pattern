//! Database models.
//!
//! These structs map directly to database tables via sqlx.

mod agent;
mod coordination;
mod memory;
mod message;

pub use agent::{Agent, AgentGroup, AgentStatus, GroupMember, GroupMemberRole, PatternType};
pub use coordination::{
    ActivityEvent, ActivityEventType, AgentSummary, ConstellationSummary, CoordinationState,
    CoordinationTask, EventImportance, HandoffNote, NotableEvent, TaskPriority, TaskStatus,
};
pub use memory::{
    ArchivalEntry, MemoryBlock, MemoryBlockCheckpoint, MemoryBlockType, MemoryGate, MemoryOp,
    MemoryPermission, SharedBlockAttachment,
};
pub use message::{ArchiveSummary, Message, MessageRole, MessageSummary};
