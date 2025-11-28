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
pub mod models;
pub mod queries;
pub mod vector;

pub use connection::ConstellationDb;
pub use error::{DbError, DbResult};

// Re-export vector module types
pub use vector::{
    ContentType, DEFAULT_EMBEDDING_DIMENSIONS, EmbeddingStats, VectorSearchResult, init_sqlite_vec,
    verify_sqlite_vec,
};

// Re-export key model types for convenience
pub use models::{
    // Coordination models
    ActivityEvent,
    ActivityEventType,
    // Agent models
    Agent,
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
    EventImportance,
    GroupMember,
    GroupMemberRole,
    HandoffNote,
    MemoryBlock,
    MemoryBlockCheckpoint,
    MemoryBlockType,
    MemoryGate,
    MemoryOp,
    MemoryPermission,
    Message,
    MessageRole,
    MessageSummary,
    NotableEvent,
    PatternType,
    SharedBlockAttachment,
    TaskPriority,
    TaskStatus,
};
