//! Entity trait for SurrealDB compatibility
//!
//! This module re-exports the entity system from the db module.

// Re-export the entity system
pub use crate::db::entity::{
    AgentMemoryRelation, BaseEvent, BaseTask, BaseTaskPriority, BaseTaskStatus, DbEntity,
    EntityError, Result as EntityResult,
};
