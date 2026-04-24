//! Search-related types that appear in [`crate::traits::MemoryStore`]
//! signatures.

use crate::types::ids::AgentId;

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

/// Scope for [`crate::traits::MemoryStore::search`].
///
/// Replaces the pre-Phase-3 separate `search` (agent-scoped) and
/// `search_all` (constellation-scoped) methods. This is the
/// **storage-layer** scope — a simpler type than the handler-level
/// [`crate::types::SearchScope`] which includes `CurrentAgent` and
/// `Agents` variants resolved by the scope resolver before reaching
/// the store.
///
/// Phase 8's `MemoryScope` layers additional routing (persona + project)
/// on top of this.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemorySearchScope {
    /// Search only this agent's data.
    Agent(AgentId),
    /// Search all agents in the constellation.
    Constellation,
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_search_scope_agent_variant() {
        let scope = MemorySearchScope::Agent("agent-1".into());
        assert_eq!(scope, MemorySearchScope::Agent("agent-1".into()));
        assert_ne!(scope, MemorySearchScope::Constellation);
    }

    #[test]
    fn memory_search_scope_constellation_variant() {
        let scope = MemorySearchScope::Constellation;
        assert_eq!(scope, MemorySearchScope::Constellation);
    }
}
