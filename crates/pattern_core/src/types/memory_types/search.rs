//! Search-related types that appear in [`crate::traits::MemoryStore`]
//! signatures.

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
