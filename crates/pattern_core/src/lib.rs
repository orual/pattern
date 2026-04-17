//! Pattern Core — agent framework and memory system for Pattern v3.
//!
//! This crate provides the foundational value types, error hierarchy, and
//! memory abstraction that power Pattern's multi-agent cognitive support
//! system. Higher-level concerns (agent loop, provider integration, context
//! composition) will live in dedicated crates once Phase 3 is complete.
//!
//! # Quick start
//!
//! ```
//! use pattern_core::{AgentId, UserId, TurnId, new_id};
//! use smol_str::SmolStr;
//!
//! let agent: AgentId = SmolStr::new("orual-companion");
//! let _user: UserId = new_id();
//! let turn: TurnId = new_id();
//! assert_eq!(turn.len(), 32);
//! ```

pub mod base_instructions;
pub mod config;
pub mod error;
#[cfg(feature = "export")]
pub mod export;
pub mod memory;
pub mod memory_acl;
pub mod permission;
pub mod traits;
pub mod types;
pub mod utils;

#[cfg(test)]
pub mod test_helpers;

// Macros are automatically available at crate root due to #[macro_export].

pub use base_instructions::DEFAULT_BASE_INSTRUCTIONS;
pub use error::{ConfigError, CoreError, MemoryError, ProviderError, Result, RuntimeError};

// ── Type re-exports ──────────────────────────────────────────────────────────
// Explicit re-exports (no wildcard) so the public surface is greppable.

// IDs and identity — all are `SmolStr` aliases; `new_id()` mints fresh UUIDs.
pub use types::ids::{
    AgentId, BatchId, ConstellationId, ConversationId, DiscordIdentityId, EventId, GroupId,
    MemoryId, MessageId, ModelId, OAuthTokenId, ProjectId, QueuedMessageId, RelationId, RequestId,
    SessionId, TaskId, ToolCallId, UserId, WakeupId, WorkspaceId, new_id,
};

// Message / batch
pub use types::batch::{BatchType, MessageBatch};
pub use types::block_ref::BlockRef;
pub use types::message::{Message, ResponseMeta};

// Block value types
pub use types::block::{Block, BlockHandle, BlockWrite};

// Turn types
pub use types::turn::{TurnCacheMetrics, TurnId, TurnInput, TurnOutput};

// Snapshot types (Phase 3 checkpoint stubs)
pub use types::snapshot::{PersonaSnapshot, SessionSnapshot};
