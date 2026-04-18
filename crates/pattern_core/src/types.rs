//! Core value types used across the pattern_core trait surface.
//!
//! This module is the public type surface for Pattern's core data structures.
//! All types are designed to cross crate boundaries and appear in trait
//! signatures; implementation-internal types live in their respective modules.

pub mod batch;
pub mod block;
pub mod block_ref;
pub mod embedding;
pub mod ids;
pub mod message;
pub mod origin;
pub mod provider;
pub mod search;
pub mod snapshot;
pub mod turn;

pub use batch::{BatchType, MessageBatch};
pub use block::{BlockCreate, BlockHandle, BlockWrite, BlockWriteKind};
pub use block_ref::BlockRef;
pub use ids::{
    AgentId, BatchId, ConstellationId, ConversationId, DiscordIdentityId, EventId, GroupId,
    MemoryId, MessageId, ModelId, OAuthTokenId, ProjectId, QueuedMessageId, RelationId, RequestId,
    SessionId, TaskId, ToolCallId, UserId, WakeupId, WorkspaceId, new_id,
};
pub use message::{Message, ResponseMeta};
pub use origin::{AgentAuthor, Author, Human, MessageOrigin, Partner, Sphere, SystemReason};
pub use search::SearchScope;
pub use snapshot::{PersonaConfig, PersonaSnapshot, SessionSnapshot};
pub use turn::{StepReply, StopReason, TurnCacheMetrics, TurnId, TurnInput, TurnOutput};
