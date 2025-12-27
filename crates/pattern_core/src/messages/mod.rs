//! Message storage and coordination.
//!
//! This module provides the MessageStore wrapper for agent-scoped message operations,
//! along with re-exports of relevant types.

mod store;

#[cfg(test)]
mod tests;

pub use store::MessageStore;

// Re-export domain Message type from crate::message
pub use crate::message::Message;

// Re-export other message types from pattern_db
pub use pattern_db::models::{ArchiveSummary, MessageSummary};

// Re-export coordination types from pattern_db
pub use pattern_db::models::{
    ActivityEvent, ActivityEventType, AgentSummary, ConstellationSummary, CoordinationState,
    CoordinationTask, EventImportance, HandoffNote, NotableEvent, TaskPriority, TaskStatus,
};
