//! Pattern Database Layer
//!
//! SQLite-based storage backend for Pattern constellations.
//!
//! # Architecture
//!
//! - **Two databases per constellation** - `memory.db` + `messages.db`,
//!   physically isolated. Messages are attached as the `msg` schema.
//! - **Loro CRDT for memory blocks** - Versioned, mergeable documents
//! - **sqlite-vec for vectors** - Semantic search over memories
//! - **FTS5 for text search** - Full-text search over messages and memories
//!
//! # Usage
//!
//! ```rust,ignore
//! use pattern_db::ConstellationDb;
//!
//! let db = ConstellationDb::open("path/to/memory.db", "path/to/messages.db")?;
//! ```

pub mod connection;
pub mod error;
pub mod fts;
pub mod json_wrapper;
pub mod migrations;
pub mod models;
pub mod queries;
pub mod search;
pub mod sql_types;
pub mod vector;

pub use connection::ConstellationDb;
pub use error::{DbError, DbResult};
pub use json_wrapper::Json;

// Re-export the unified database statistics type.
pub use queries::stats::DbStats;

// Re-export vector module types.
pub use vector::{ContentType, DEFAULT_EMBEDDING_DIMENSIONS, EmbeddingStats, VectorSearchResult};

// Re-export FTS module types.
pub use fts::{FtsContentType, FtsMatch, FtsSearchResult, FtsStats};

// Re-export hybrid search types.
pub use search::{
    ContentFilter, FusionMethod, HybridSearchBuilder, ScoreBreakdown, SearchContentType,
    SearchMode, SearchResult,
};

// Re-export key model types for convenience.
pub use models::{
    ActivityEvent, ActivityEventType, Agent, AgentAtprotoEndpoint, AgentDataSource, AgentGroup,
    AgentStatus, AgentSummary, ArchivalEntry, ArchiveSummary, ConstellationSummary,
    CoordinationState, CoordinationTask, DataSource, ENDPOINT_TYPE_BLUESKY, EntityImport, Event,
    EventImportance, EventOccurrence, FilePassage, Folder, FolderAccess, FolderAttachment,
    FolderFile, FolderPathType, GroupMember, GroupMemberRole, HandoffNote, IssueSeverity,
    MemoryBlock, MemoryBlockCheckpoint, MemoryBlockType, MemoryGate, MemoryOp, MemoryPermission,
    Message, MessageRole, MessageSummary, MigrationAudit, MigrationIssue, MigrationLog,
    MigrationStats, ModelRoutingConfig, ModelRoutingRule, NotableEvent, OccurrenceStatus,
    PatternType, RoutingCondition, SharedBlockAttachment, SourceType, Task, TaskPriority,
    TaskStatus, TaskSummary, UserTaskPriority, UserTaskStatus,
};
