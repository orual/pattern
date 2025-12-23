//! Pattern Database Layer
//!
//! SQLite-based storage backend for Pattern constellations.
//!
//! # Architecture
//!
//! - **One database per constellation** - Physical isolation, no cross-constellation leaks
//! - **Loro CRDT for memory blocks** - Versioned, mergeable documents
//! - **sqlite-vec for vectors** - Semantic search over memories
//! - **FTS5 for text search** - Full-text search over messages and memories
//!
//! # Usage
//!
//! ```rust,ignore
//! use pattern_db::ConstellationDb;
//!
//! let db = ConstellationDb::open("path/to/constellation.db").await?;
//! ```

pub mod connection;
pub mod error;
pub mod fts;
pub mod models;
pub mod queries;
pub mod search;
pub mod vector;

pub use connection::ConstellationDb;
pub use error::{DbError, DbResult};

// Re-export vector module types
pub use vector::{
    ContentType, DEFAULT_EMBEDDING_DIMENSIONS, EmbeddingStats, VectorSearchResult, init_sqlite_vec,
    verify_sqlite_vec,
};

// Re-export FTS module types
pub use fts::{FtsContentType, FtsMatch, FtsSearchResult, FtsStats};

// Re-export hybrid search types
pub use search::{
    ContentFilter, FusionMethod, HybridSearchBuilder, ScoreBreakdown, SearchContentType,
    SearchMode, SearchResult,
};

// Re-export key model types for convenience
pub use models::{
    // Coordination models
    ActivityEvent,
    ActivityEventType,
    // Agent models
    Agent,
    // Source models
    AgentDataSource,
    AgentGroup,
    AgentStatus,
    AgentSummary,
    // Memory models
    ArchivalEntry,
    // Message models
    ArchiveSummary,
    ConstellationSummary,
    CoordinationState,
    CoordinationTask,
    DataSource,
    // Migration models
    EntityImport,
    // Event models
    Event,
    EventImportance,
    EventOccurrence,
    // Folder models
    FilePassage,
    Folder,
    FolderAccess,
    FolderAttachment,
    FolderFile,
    FolderPathType,
    GroupMember,
    GroupMemberRole,
    HandoffNote,
    IssueSeverity,
    MemoryBlock,
    MemoryBlockCheckpoint,
    MemoryBlockType,
    MemoryGate,
    MemoryOp,
    MemoryPermission,
    Message,
    MessageRole,
    MessageSummary,
    MigrationAudit,
    MigrationIssue,
    MigrationLog,
    MigrationStats,
    ModelRoutingConfig,
    ModelRoutingRule,
    NotableEvent,
    OccurrenceStatus,
    PatternType,
    RoutingCondition,
    SharedBlockAttachment,
    SourceType,
    // Task models (ADHD)
    Task,
    TaskPriority,
    TaskStatus,
    TaskSummary,
    UserTaskPriority,
    UserTaskStatus,
};
