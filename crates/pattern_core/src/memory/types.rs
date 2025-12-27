//! Types for the v2 memory system

use chrono::{DateTime, Utc};
use loro::VersionVector;
use serde::{Deserialize, Serialize};

use crate::memory::StructuredDocument;

/// A cached memory block with its LoroDoc
#[derive(Debug)]
pub struct CachedBlock {
    /// Block metadata from DB
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub description: String,
    pub block_type: BlockType,
    pub char_limit: i64,
    pub permission: pattern_db::models::MemoryPermission,

    /// The structured document wrapper (LoroDoc is internally Arc'd and thread-safe)
    pub doc: StructuredDocument,

    /// Last sequence number we've seen from DB
    pub last_seq: i64,

    /// Frontier at last persist (for delta export)
    pub last_persisted_frontier: Option<VersionVector>,

    /// Whether we have unpersisted changes
    pub dirty: bool,

    /// When this was last accessed (for eviction)
    pub last_accessed: DateTime<Utc>,
}

/// Block types matching pattern_db
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Core,
    Working,
    Archival,
    Log,
}

impl From<pattern_db::models::MemoryBlockType> for BlockType {
    fn from(t: pattern_db::models::MemoryBlockType) -> Self {
        match t {
            pattern_db::models::MemoryBlockType::Core => BlockType::Core,
            pattern_db::models::MemoryBlockType::Working => BlockType::Working,
            pattern_db::models::MemoryBlockType::Archival => BlockType::Archival,
            pattern_db::models::MemoryBlockType::Log => BlockType::Log,
        }
    }
}

impl From<BlockType> for pattern_db::models::MemoryBlockType {
    fn from(t: BlockType) -> Self {
        match t {
            BlockType::Core => pattern_db::models::MemoryBlockType::Core,
            BlockType::Working => pattern_db::models::MemoryBlockType::Working,
            BlockType::Archival => pattern_db::models::MemoryBlockType::Archival,
            BlockType::Log => pattern_db::models::MemoryBlockType::Log,
        }
    }
}

/// Error type for memory operations
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Block not found: {agent_id}/{label}")]
    NotFound { agent_id: String, label: String },

    #[error("Block is read-only: {0}")]
    ReadOnly(String),

    #[error(
        "Permission denied for block '{block_label}': required {required:?}, actual {actual:?}"
    )]
    PermissionDenied {
        block_label: String,
        required: pattern_db::models::MemoryPermission,
        actual: pattern_db::models::MemoryPermission,
    },

    #[error("Database error: {0}")]
    Database(#[from] pattern_db::DbError),

    #[error("Loro error: {0}")]
    Loro(String),

    #[error("Document error: {0}")]
    Document(#[from] crate::memory::DocumentError),

    #[error("Memory operation failed: {0}")]
    Other(String),
}

pub type MemoryResult<T> = Result<T, MemoryError>;

/// Source of a memory change (for audit trails)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeSource {
    /// Change made by an agent
    Agent(String),
    /// Change made by a human/partner
    Human(String),
    /// Change made by system (e.g., compression, migration)
    System,
    /// Change from external integration (e.g., Discord, Bluesky)
    Integration(String),
}

/// Search mode configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Only use FTS5 keyword search
    Fts,
    /// Only use vector similarity search
    Vector,
    /// Combine both using fusion
    Hybrid,
    /// Automatically choose based on embedder availability
    Auto,
}

impl SearchMode {
    /// Returns true if this mode requires an embedding provider
    pub fn needs_embedding(&self) -> bool {
        matches!(self, Self::Vector | Self::Hybrid)
    }
}

/// Content types for search
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchContentType {
    Blocks,
    Archival,
    Messages,
}

impl SearchContentType {
    /// Convert to pattern_db SearchContentType
    pub fn to_db_content_type(self) -> pattern_db::search::SearchContentType {
        match self {
            Self::Blocks => pattern_db::search::SearchContentType::MemoryBlock,
            Self::Archival => pattern_db::search::SearchContentType::ArchivalEntry,
            Self::Messages => pattern_db::search::SearchContentType::Message,
        }
    }
}

/// Search options for memory operations
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Search mode (FTS, Vector, Hybrid, Auto)
    pub mode: SearchMode,
    /// Content types to search
    pub content_types: Vec<SearchContentType>,
    /// Maximum number of results
    pub limit: usize,
}

impl SearchOptions {
    /// Create new search options with defaults
    pub fn new() -> Self {
        Self {
            mode: SearchMode::Fts,
            content_types: vec![
                SearchContentType::Blocks,
                SearchContentType::Archival,
                SearchContentType::Messages,
            ],
            limit: 10,
        }
    }

    /// Set the search mode
    pub fn mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set content types to search
    pub fn content_types(mut self, types: Vec<SearchContentType>) -> Self {
        self.content_types = types;
        self
    }

    /// Set the result limit
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Search only blocks
    pub fn blocks_only(mut self) -> Self {
        self.content_types = vec![SearchContentType::Blocks];
        self
    }

    /// Search only archival
    pub fn archival_only(mut self) -> Self {
        self.content_types = vec![SearchContentType::Archival];
        self
    }

    /// Search only messages
    pub fn messages_only(mut self) -> Self {
        self.content_types = vec![SearchContentType::Messages];
        self
    }
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Search result from memory operations
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    /// Content ID
    pub id: String,
    /// Content type
    pub content_type: SearchContentType,
    /// The actual content text
    pub content: Option<String>,
    /// Relevance score (0-1, higher is better)
    pub score: f64,
}

impl MemorySearchResult {
    /// Convert from pattern_db SearchResult
    pub fn from_db_result(result: pattern_db::search::SearchResult) -> Self {
        let content_type = match result.content_type {
            pattern_db::search::SearchContentType::Message => SearchContentType::Messages,
            pattern_db::search::SearchContentType::MemoryBlock => SearchContentType::Blocks,
            pattern_db::search::SearchContentType::ArchivalEntry => SearchContentType::Archival,
        };

        Self {
            id: result.id,
            content_type,
            content: result.content,
            score: result.score,
        }
    }
}
