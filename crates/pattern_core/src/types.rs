//! Core value types used across the pattern_core trait surface.
//!
//! This module is the public type surface for Pattern's core data structures.
//! All types are designed to cross crate boundaries and appear in trait
//! signatures; implementation-internal types live in their respective modules.

pub mod batch;
pub mod block;
pub mod block_ref;
pub mod caller;
pub mod ids;
pub mod message;
pub mod snapshot;
pub mod turn;

pub use batch::{BatchType, MessageBatch};
pub use block::{Block, BlockHandle, BlockWrite};
pub use block_ref::BlockRef;
pub use caller::Caller;
pub use ids::{
    AgentId, BatchId, ConstellationId, ConversationId, Did, DiscordIdentityId, EventId, GroupId,
    IdError, IdType, MemoryId, MessageId, ModelId, OAuthTokenId, ProjectId, QueuedMessageId,
    RelationId, RequestId, SessionId, TaskId, ToolCallId, UserId, WakeupId, WorkspaceId,
};
pub use message::{Message, ResponseMeta};
pub use snapshot::{PersonaSnapshot, SessionSnapshot};
pub use turn::{TurnId, TurnInput, TurnOutput};
