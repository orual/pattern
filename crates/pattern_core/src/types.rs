//! Core value types used across the pattern_core trait surface.
//!
//! This module is the public type surface for Pattern's core data structures.
//! All types are designed to cross crate boundaries and appear in trait
//! signatures; implementation-internal types live in their respective modules.

#[cfg(feature = "provider")]
pub mod batch;
pub mod block;
pub mod block_ref;
pub mod compression;
pub mod embedding;
pub mod ids;
pub mod memory_types;
#[cfg(feature = "provider")]
pub mod message;
pub mod origin;
pub mod port;
#[cfg(feature = "provider")]
pub mod provider;
pub mod search;
#[cfg(feature = "provider")]
pub mod snapshot;
mod sql_types;
#[cfg(feature = "provider")]
pub mod turn;

#[cfg(feature = "provider")]
pub use batch::{BatchType, MessageBatch};
pub use block::{BlockCreate, BlockHandle, BlockWrite, BlockWriteKind};
pub use block_ref::BlockRef;
pub use compression::CompressionStrategy;
pub use ids::{
    AgentId, BatchId, ConstellationId, ConversationId, DiscordIdentityId, EventId, GroupId,
    MemoryId, MessageId, ModelId, OAuthTokenId, ProjectId, QueuedMessageId, RelationId, RequestId,
    SessionId, TaskId, ToolCallId, UserId, WakeupId, WorkspaceId, new_id, new_snowflake_id,
};
#[cfg(feature = "provider")]
pub use message::{Message, ResponseMeta};
pub use origin::{AgentAuthor, Author, Human, MessageOrigin, Partner, Sphere, SystemReason};
pub use port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};
pub use search::SearchScope;
#[cfg(feature = "provider")]
pub use snapshot::{PersonaSnapshot, SessionSnapshot};
#[cfg(feature = "provider")]
pub use turn::{StepReply, StopReason, TurnCacheMetrics, TurnId, TurnInput, TurnOutput};
