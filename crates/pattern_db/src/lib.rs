// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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

// Re-export rusqlite so downstream crates that already depend on pattern_db
// can use its types (Connection, params, etc.) without an additional direct dep.
pub use rusqlite;

pub use connection::ConstellationDb;
pub use error::{DbError, DbResult};
pub use json_wrapper::Json;

// Re-export the unified database statistics type.
pub use queries::stats::DbStats;

// Re-export the rusqlite-backed ConstellationRegistry implementation.
pub use queries::constellation::ConstellationRegistryDb;

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
    Agent, AgentAtprotoEndpoint, AgentDataSource, AgentStatus, ArchivalEntry, ArchiveSummary,
    DataSource, ENDPOINT_TYPE_BLUESKY, EntityImport, Event, EventOccurrence, FilePassage, Folder,
    FolderAccess, FolderAttachment, FolderFile, FolderPathType, IssueSeverity, MemoryBlock,
    MemoryBlockCheckpoint, MemoryBlockType, MemoryGate, MemoryOp, MemoryPermission, Message,
    MessageRole, MessageSummary, MigrationAudit, MigrationIssue, MigrationLog, MigrationStats,
    ModelRoutingConfig, ModelRoutingRule, OccurrenceStatus, RoutingCondition,
    SharedBlockAttachment, SourceType, Task, UserTaskStatus,
};
