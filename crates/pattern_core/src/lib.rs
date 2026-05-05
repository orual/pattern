// Pre-existing style lints in legacy `error/core.rs`, `memory/document.rs`
// are suppressed crate-wide because they predate the v3 rewrite and are
// orthogonal to Phase 2's scope.
#![allow(clippy::type_complexity)] // CoreError::provider_http_parts return types; factoring deferred.
#![allow(clippy::result_large_err)] // CoreError is a deliberately rich diagnostic enum; boxing regresses ergonomics.
#![allow(clippy::doc_lazy_continuation)] // Rustdoc list-indent lint on pre-existing comments in memory/document.rs; deferred.

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
//! use pattern_core::{AgentId, UserId, TurnId, new_id, new_snowflake_id};
//! use smol_str::SmolStr;
//!
//! let _agent: AgentId = SmolStr::new("orual-companion");
//! // UserId: non-ordered — UUID is fine.
//! let _user: UserId = new_id();
//! // TurnId: lex-sortable — use snowflake (convention: any ID that
//! // orders turns/batches/messages must be a snowflake, not a UUID).
//! let turn: TurnId = new_snowflake_id();
//! assert!(!turn.is_empty());
//! ```

pub mod base_instructions;
pub mod capability;
#[cfg(feature = "mcp-client")]
#[cfg(feature = "mcp-client")]
pub mod mcp;
pub mod hooks;
pub mod plugin;
pub mod constellation;
pub mod error;
pub mod fronting;
pub mod memory;
// `memory_acl` module removed: MemoryOp, MemoryGate, and check() are
// canonical in types::memory_types::core_types (as methods on MemoryGate).
pub mod paths;
pub mod permission;
pub mod spawn;
pub mod traits;
pub mod types;
pub mod utils;

#[cfg(test)]
pub mod test_helpers;

// ── Common re-exports ────────────────────────────────────────────────────────

pub use base_instructions::DEFAULT_BASE_INSTRUCTIONS;
pub use paths::{PatternRoots, RootsError};
pub use capability::{
    CapabilityError, CapabilityFlag, CapabilityParseError, CapabilitySet, EffectCategory,
    EffectClass, PolicyAction, PolicyContext, PolicyMatcher, PolicyRule, PolicySet, Precedence,
    RuntimeClassCheck,
};

/// Reserved memory-block label for the agent's persona content.
/// Segment 1 reads this block to inject persona into the system prompt.
pub const PERSONA_LABEL: &str = "persona";
pub use error::{
    ConfigError, CoreError, EmbeddingError, MemoryError, MemoryResult, ProviderError, Result,
    RuntimeError,
};

// ── Trait re-exports ─────────────────────────────────────────────────────────
// Explicit (no wildcard) so the public surface is greppable.

pub use traits::{
    AgentRuntime, EmbeddingProvider, Endpoint, EndpointRegistry, MemoryStore, ProviderClient,
    Session,
};

// ── Type re-exports ──────────────────────────────────────────────────────────

// IDs and identity — all `SmolStr` aliases. `new_id()` mints fresh UUIDs
// for non-ordered IDs; `new_snowflake_id()` mints lex-sortable snowflake
// IDs for anything that must order by creation time (TurnId, BatchId,
// message `position`).
pub use types::ids::{
    AgentId, BatchId, ConstellationId, ConversationId, DiscordIdentityId, EventId, GroupId,
    MemoryId, MessageId, ModelId, OAuthTokenId, PersonaId, ProjectId, QueuedMessageId, RelationId,
    RequestId, SessionId, TaskId, ToolCallId, UserId, WakeupId, WorkspaceId, new_id,
    new_snowflake_id,
};

// Message / batch
pub use types::batch::{BatchType, MessageBatch};
pub use types::block_ref::BlockRef;
pub use types::message::{Message, ResponseMeta};

// Block value types
pub use types::block::{BlockCreate, BlockHandle, BlockWrite, BlockWriteKind};

// Origin / provenance
pub use types::origin::{AgentAuthor, Author, Human, MessageOrigin, Partner, Sphere, SystemReason};

// Turn types
pub use types::turn::{StepReply, StopReason, TurnCacheMetrics, TurnId, TurnInput, TurnOutput};

// Snapshot / persona types (Phase 3 checkpoint stubs)
pub use types::snapshot::{PersonaSnapshot, SessionSnapshot};

// Embedding value types
pub use types::embedding::{Embedding, EmbeddingResult};

// Spawn-config types — multi-agent session spawning primitives.
pub use spawn::{
    EphemeralConfig, ForkConfig, ForkIsolation, PersonaConfig, RelationshipKind, SiblingConfig,
    SiblingPersona,
};

// Provider request / response types + genai re-exports for callers that
// want `use pattern_core::*` without also depending on genai directly.
pub use types::provider::{
    CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, CompletionRequest,
    ProviderCredential, ReasoningEffort, StreamEnd, SystemBlock, TokenCount, Tool, ToolCall,
    ToolResponse, Usage,
};

// ── Constellation + fronting types ───────────────────────────────────────────

pub use constellation::{
    ConstellationRegistry, EdgeDirection, PersonaGroup, PersonaRecord, PersonaStatus,
    RegistryError, RegistryScope, RelationshipEdge, RelationshipSpec,
};
// `EmptyConstellationRegistry` is test-only: no production path uses it after
// Phase 6. External test crates needing a stub should use
// `pattern_runtime::testing::InMemoryConstellationRegistry`.
#[cfg(test)]
pub use constellation::EmptyConstellationRegistry;

pub use fronting::{
    FrontingLoadError, FrontingResolver, FrontingSet, MessagePattern, ResolveOutcome, RoutingRule,
    RoutingTable, parse_direct_address,
};
