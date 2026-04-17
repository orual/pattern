//! # pattern_core
//!
//! Traits and types that every Pattern v3 component implements or consumes.
//!
//! This crate contains no execution machinery — the runtime lives in
//! `pattern_runtime`, LLM integration in `pattern_provider`. Memory storage
//! (loro CRDT + sqlite) will be re-absorbed here once `pattern_runtime`
//! lands; the concrete `MemoryCache` / `SharedBlockManager` implementations
//! are staged to `rewrite-staging/runtime_subsystems/memory_v2/` for the
//! duration of Phase 2 because they depend on plumbing
//! (`ConstellationDatabases`) that temporarily lives outside this crate.
//!
//! See `docs/design-plans/2026-04-16-v3-foundation.md` for the layering
//! rationale.
//!
//! # Quick start
//!
//! ```
//! use pattern_core::{AgentId, UserId, TurnId, new_id};
//! use smol_str::SmolStr;
//!
//! let _agent: AgentId = SmolStr::new("orual-companion");
//! let _user: UserId = new_id();
//! let turn: TurnId = new_id();
//! assert_eq!(turn.len(), 32);
//! ```

pub mod base_instructions;
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

// ── Common re-exports ────────────────────────────────────────────────────────

pub use base_instructions::DEFAULT_BASE_INSTRUCTIONS;
pub use error::{
    ConfigError, CoreError, EmbeddingError, MemoryError, ProviderError, Result, RuntimeError,
};

// ── Trait re-exports ─────────────────────────────────────────────────────────
// Explicit (no wildcard) so the public surface is greppable.

pub use traits::{
    AgentRuntime, DataStream, EmbeddingProvider, Endpoint, EndpointRegistry, MemoryStore,
    ProviderClient, Session, SourceManager,
};

// ── Type re-exports ──────────────────────────────────────────────────────────

// IDs and identity — all `SmolStr` aliases; `new_id()` mints fresh UUIDs.
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

// Origin / provenance
pub use types::origin::{AgentAuthor, Author, Human, MessageOrigin, Partner, Sphere};

// Turn types
pub use types::turn::{TurnCacheMetrics, TurnId, TurnInput, TurnOutput};

// Snapshot / persona types (Phase 3 checkpoint stubs)
pub use types::snapshot::{PersonaConfig, PersonaSnapshot, SessionSnapshot};

// Embedding value types
pub use types::embedding::{Embedding, EmbeddingResult};

// Provider request / response types
pub use types::provider::{CompletionChunk, CompletionRequest, CompletionResponse, TokenCount};
