//! SurrealDB Compatibility Layer for Pattern
//!
//! This crate contains deprecated SurrealDB-based code preserved for:
//! - Migration from SurrealDB to SQLite
//! - CAR file export/import functionality
//! - Reference during Phase E integration
//!
//! **Do not add new code here. This crate is in maintenance-only mode.**

pub mod agent_entity;
pub mod atproto_identity;
pub mod config;
pub mod db;
pub mod entity;
pub mod error;
pub mod export;
pub mod groups;
pub mod id;
pub mod memory;
pub mod message;
pub mod users;
pub mod utils;

// Re-export key types at crate root
pub use agent_entity::AgentRecord;
pub use atproto_identity::{
    AtprotoAuthCredentials, AtprotoAuthMethod, AtprotoAuthState, AtprotoIdentity, AtprotoProfile,
    HickoryDnsTxtResolver, PatternHttpClient, resolve_handle_to_pds,
};
pub use config::{ToolRuleConfig, ToolRuleTypeConfig};
pub use db::{DatabaseBackend, DatabaseConfig, DatabaseError, Query, Result as DbResult};
pub use entity::{AgentMemoryRelation, BaseEvent, BaseTask, DbEntity};
pub use error::{CoreError, EmbeddingError};
pub use export::{
    AgentExport, DEFAULT_CHUNK_SIZE, DEFAULT_MEMORY_CHUNK_SIZE, EXPORT_VERSION, ExportManifest,
    ExportStats, ExportType, MAX_BLOCK_BYTES, MemoryChunk,
};
pub use groups::{
    AgentGroup, AgentType, CompressionStrategy, Constellation, ConstellationMembership,
    CoordinationPattern, DelegationRules, DelegationStrategy, FallbackBehavior, GroupMemberRole,
    GroupMembership, GroupState, PipelineStage, SleeptimeTrigger, SnowflakePosition,
    StageFailureAction, TieBreaker, TriggerCondition, TriggerPriority, VotingRules,
    get_next_message_position, get_next_message_position_string, get_next_message_position_sync,
    get_position_generator,
};
pub use id::{
    AgentId, ConstellationId, Did, GroupId, IdError, IdType, MemoryId, MessageId, RelationId,
    UserId,
};
pub use memory::{Memory, MemoryBlock, MemoryPermission, MemoryType};
pub use message::{
    AgentMessageRelation, BatchType, CacheControl, ChatRole, ContentBlock, ContentPart,
    ImageSource, Message, MessageContent, MessageMetadata, MessageOptions, MessageRelationType,
    Response, ResponseMetadata, ToolCall, ToolResponse,
};
pub use users::User;
