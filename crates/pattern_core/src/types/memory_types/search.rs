// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Search-related types that appear in [`crate::traits::MemoryStore`]
//! signatures.


/// Search mode configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
/// `Scope(Scope)` searches a single ownership boundary (e.g. one
/// project's blocks or one persona's blocks). `Constellation` searches
/// across every scope visible to the caller.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemorySearchScope {
    /// Search a single scope's blocks.
    Scope(super::Scope),
    /// Search all scopes in the constellation.
    Constellation,
}

/// Address of a search hit. Distinguishes block vs archival vs message hits
/// since the underlying row-id type differs, and gives callers what they need
/// to read the hit's content via the normal MemoryStore paths.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum SearchHit {
    /// Hit on a memory block. Addressable via (scope, label).
    Block {
        scope: super::Scope,
        label: smol_str::SmolStr,
    },
    /// Hit on an archival entry. Addressable via entry id.
    Archival { entry_id: String },
    /// Hit on a stored message.
    Message { message_id: String },
}

/// Search result from memory operations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemorySearchResult {
    /// Addressable target of this hit.
    pub hit: SearchHit,
    /// Content type (kept for compatibility with consumers that switch on it;
    /// redundant with `hit`'s variant).
    pub content_type: SearchContentType,
    /// The actual content text (snippet or full body, impl-defined).
    pub content: Option<String>,
    /// Relevance score (0-1, higher is better).
    pub score: f64,
}

impl MemorySearchResult {
    /// String identifier for display / correlation. For block hits this is the
    /// block label; for archival / message hits it's the entry / message id.
    /// Callers that need typed addressing should match on `self.hit` directly.
    pub fn display_id(&self) -> &str {
        match &self.hit {
            SearchHit::Block { label, .. } => label.as_str(),
            SearchHit::Archival { entry_id } => entry_id.as_str(),
            SearchHit::Message { message_id } => message_id.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_search_scope_agent_variant() {
        use crate::types::memory_types::Scope as Sc;
        let s = MemorySearchScope::Scope(Sc::global("agent-1"));
        assert_eq!(s, MemorySearchScope::Scope(Sc::global("agent-1")));
        assert_ne!(s, MemorySearchScope::Constellation);
    }

    #[test]
    fn memory_search_scope_constellation_variant() {
        let scope = MemorySearchScope::Constellation;
        assert_eq!(scope, MemorySearchScope::Constellation);
    }
}
