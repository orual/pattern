//! Hybrid search combining FTS5 and vector similarity.
//!
//! This module provides unified search across text and semantic dimensions,
//! with configurable fusion strategies for combining results.
//!
//! # Search Strategies
//!
//! - **FTS-only**: Fast keyword search, good for exact matches
//! - **Vector-only**: Semantic search, good for conceptual similarity
//! - **Hybrid**: Combines both, best for most use cases
//!
//! # Fusion Methods
//!
//! When combining FTS and vector results, we support:
//! - **RRF (Reciprocal Rank Fusion)**: Rank-based, parameter-free (default)
//! - **Linear combination**: Weighted average of normalized scores

use crate::error::DbResult;
use crate::fts::{self, FtsMatch};
use crate::vector::{self, ContentType, VectorSearchResult};

/// Unified search result combining FTS and vector scores.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Content ID.
    pub id: String,
    /// Content type.
    pub content_type: SearchContentType,
    /// The actual content text (if available).
    pub content: Option<String>,
    /// Combined relevance score (higher is better, normalized 0-1).
    pub score: f64,
    /// Individual scores for debugging/tuning.
    pub scores: ScoreBreakdown,
}

/// Breakdown of how the final score was computed.
#[derive(Debug, Clone, Default)]
pub struct ScoreBreakdown {
    /// FTS BM25 rank (lower is better, typically negative).
    pub fts_rank: Option<f64>,
    /// Vector distance (lower is better, 0-2 for cosine).
    pub vector_distance: Option<f32>,
    /// Normalized FTS score (0-1, higher is better).
    pub fts_normalized: Option<f64>,
    /// Normalized vector score (0-1, higher is better).
    pub vector_normalized: Option<f64>,
    /// Position in FTS results (1-indexed).
    pub fts_position: Option<usize>,
    /// Position in vector results (1-indexed).
    pub vector_position: Option<usize>,
}

/// Content types for search (mirrors both FTS and vector types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchContentType {
    Message,
    MemoryBlock,
    ArchivalEntry,
}

impl SearchContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::MemoryBlock => "memory_block",
            Self::ArchivalEntry => "archival_entry",
        }
    }

    pub fn to_vector_content_type(self) -> ContentType {
        match self {
            Self::Message => ContentType::Message,
            Self::MemoryBlock => ContentType::MemoryBlock,
            Self::ArchivalEntry => ContentType::ArchivalEntry,
        }
    }
}

/// Search mode configuration.
#[derive(Debug, Clone, Copy, Default)]
pub enum SearchMode {
    /// Only use FTS5 keyword search.
    FtsOnly,
    /// Only use vector similarity search.
    VectorOnly,
    /// Combine both using fusion.
    Hybrid,
    /// Automatically choose based on what's provided (default).
    /// - Both text + embedding -> Hybrid
    /// - Only text -> FtsOnly
    /// - Only embedding -> VectorOnly
    #[default]
    Auto,
}

/// Fusion method for combining FTS and vector results.
#[derive(Debug, Clone, Copy)]
pub enum FusionMethod {
    /// Reciprocal Rank Fusion - combines based on rank positions.
    /// Score = sum(1 / (k + rank)) across both result sets.
    /// Default k=60 works well empirically.
    Rrf { k: u32 },
    /// Linear combination of normalized scores.
    /// Score = fts_weight * fts_score + vector_weight * vector_score.
    Linear { fts_weight: f64, vector_weight: f64 },
}

impl Default for FusionMethod {
    fn default() -> Self {
        Self::Rrf { k: 60 }
    }
}

/// Content filter for search scope.
#[derive(Debug, Clone, Default)]
pub struct ContentFilter {
    /// Filter to specific content type.
    pub content_type: Option<SearchContentType>,
    /// Filter to specific agent (for messages/memory blocks).
    pub agent_id: Option<String>,
}

impl ContentFilter {
    pub fn messages(agent_id: Option<&str>) -> Self {
        Self {
            content_type: Some(SearchContentType::Message),
            agent_id: agent_id.map(String::from),
        }
    }

    pub fn memory_blocks(agent_id: Option<&str>) -> Self {
        Self {
            content_type: Some(SearchContentType::MemoryBlock),
            agent_id: agent_id.map(String::from),
        }
    }

    pub fn archival(agent_id: Option<&str>) -> Self {
        Self {
            content_type: Some(SearchContentType::ArchivalEntry),
            agent_id: agent_id.map(String::from),
        }
    }

    pub fn all() -> Self {
        Self::default()
    }
}

/// Builder for hybrid search queries.
pub struct HybridSearchBuilder<'a> {
    conn: &'a rusqlite::Connection,
    text_query: Option<String>,
    embedding: Option<&'a [f32]>,
    filter: ContentFilter,
    limit: i64,
    mode: SearchMode,
    fusion: FusionMethod,
    /// Minimum FTS score threshold (normalized, 0-1).
    min_fts_score: Option<f64>,
    /// Maximum vector distance threshold.
    max_vector_distance: Option<f32>,
}

impl<'a> HybridSearchBuilder<'a> {
    /// Create a new search builder.
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            conn,
            text_query: None,
            embedding: None,
            filter: ContentFilter::default(),
            limit: 10,
            mode: SearchMode::default(),
            fusion: FusionMethod::default(),
            min_fts_score: None,
            max_vector_distance: None,
        }
    }

    /// Set the text query for FTS search.
    pub fn text(mut self, query: impl Into<String>) -> Self {
        self.text_query = Some(query.into());
        self
    }

    /// Set the embedding vector for similarity search.
    pub fn embedding(mut self, embedding: &'a [f32]) -> Self {
        self.embedding = Some(embedding);
        self
    }

    /// Set the content filter.
    pub fn filter(mut self, filter: ContentFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Set the maximum number of results.
    pub fn limit(mut self, limit: i64) -> Self {
        self.limit = limit;
        self
    }

    /// Set the search mode.
    pub fn mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the fusion method for hybrid search.
    pub fn fusion(mut self, fusion: FusionMethod) -> Self {
        self.fusion = fusion;
        self
    }

    /// Set minimum FTS score threshold (0-1, higher is better).
    pub fn min_fts_score(mut self, threshold: f64) -> Self {
        self.min_fts_score = Some(threshold);
        self
    }

    /// Set maximum vector distance threshold.
    pub fn max_vector_distance(mut self, threshold: f32) -> Self {
        self.max_vector_distance = Some(threshold);
        self
    }

    /// Execute the search.
    #[allow(non_snake_case)]
    pub fn execute(self) -> DbResult<Vec<SearchResult>> {
        // Resolve Auto mode based on what's provided.
        let effective_mode = match self.mode {
            SearchMode::Auto => match (&self.text_query, &self.embedding) {
                (Some(_), Some(_)) => SearchMode::Hybrid,
                (Some(_), None) => SearchMode::FtsOnly,
                (None, Some(_)) => SearchMode::VectorOnly,
                (None, None) => {
                    return Err(crate::error::DbError::invalid_data(
                        "Search requires at least a text query or embedding",
                    ));
                }
            },
            other => other,
        };

        match effective_mode {
            SearchMode::FtsOnly => self.execute_fts_only(),
            SearchMode::VectorOnly => self.execute_vector_only(),
            SearchMode::Hybrid => self.execute_hybrid(),
            SearchMode::Auto => unreachable!(),
        }
    }

    fn execute_fts_only(self) -> DbResult<Vec<SearchResult>> {
        let query = self.text_query.as_deref().ok_or_else(|| {
            crate::error::DbError::invalid_data("FTS search requires a text query")
        })?;

        let fts_results = self.run_fts_search(query)?;

        // Normalize and convert.
        let max_rank = fts_results
            .iter()
            .map(|(_, m)| m.rank.abs())
            .fold(0.0f64, f64::max);

        Ok(fts_results
            .into_iter()
            .enumerate()
            .filter_map(|(pos, (content_type, m))| {
                let normalized = if max_rank > 0.0 {
                    1.0 - (m.rank.abs() / max_rank)
                } else {
                    1.0
                };

                // Apply threshold.
                if let Some(min_score) = self.min_fts_score
                    && normalized < min_score
                {
                    return None;
                }

                Some(SearchResult {
                    id: m.id,
                    content_type,
                    content: Some(m.content),
                    score: normalized,
                    scores: ScoreBreakdown {
                        fts_rank: Some(m.rank),
                        fts_normalized: Some(normalized),
                        fts_position: Some(pos + 1),
                        ..Default::default()
                    },
                })
            })
            .take(self.limit as usize)
            .collect())
    }

    fn execute_vector_only(self) -> DbResult<Vec<SearchResult>> {
        let embedding = self.embedding.as_ref().ok_or_else(|| {
            crate::error::DbError::invalid_data("Vector search requires an embedding")
        })?;

        let vector_results = self.run_vector_search(embedding)?;

        // Normalize distances (assuming cosine distance 0-2).
        let max_dist = vector_results
            .iter()
            .map(|r| r.distance)
            .fold(0.0f32, f32::max)
            .max(0.001);

        Ok(vector_results
            .into_iter()
            .enumerate()
            .filter_map(|(pos, r)| {
                // Apply threshold.
                if let Some(max_dist_thresh) = self.max_vector_distance
                    && r.distance > max_dist_thresh
                {
                    return None;
                }

                let normalized = 1.0 - (r.distance / max_dist) as f64;
                let content_type = match r.content_type {
                    ContentType::Message => SearchContentType::Message,
                    ContentType::MemoryBlock => SearchContentType::MemoryBlock,
                    ContentType::ArchivalEntry => SearchContentType::ArchivalEntry,
                    ContentType::FilePassage => return None,
                };

                Some(SearchResult {
                    id: r.content_id,
                    content_type,
                    content: None,
                    score: normalized,
                    scores: ScoreBreakdown {
                        vector_distance: Some(r.distance),
                        vector_normalized: Some(normalized),
                        vector_position: Some(pos + 1),
                        ..Default::default()
                    },
                })
            })
            .take(self.limit as usize)
            .collect())
    }

    #[allow(non_snake_case)]
    fn execute_hybrid(self) -> DbResult<Vec<SearchResult>> {
        // Run both searches sequentially (sync context).
        let (fts_results, vector_results) = match (&self.text_query, &self.embedding) {
            (Some(query), Some(embedding)) => {
                let fts = self.run_fts_search(query)?;
                let vec = self.run_vector_search(embedding)?;
                (Some(fts), Some(vec))
            }
            (Some(query), None) => {
                let fts = self.run_fts_search(query)?;
                (Some(fts), None)
            }
            (None, Some(embedding)) => {
                let vec = self.run_vector_search(embedding)?;
                (None, Some(vec))
            }
            (None, None) => {
                return Err(crate::error::DbError::invalid_data(
                    "Hybrid search requires at least a text query or embedding",
                ));
            }
        };

        // Fuse results.
        let results = match self.fusion {
            FusionMethod::Rrf { k } => self.fuse_rrf(fts_results, vector_results, k),
            FusionMethod::Linear {
                fts_weight,
                vector_weight,
            } => self.fuse_linear(fts_results, vector_results, fts_weight, vector_weight),
        };

        Ok(results.into_iter().take(self.limit as usize).collect())
    }

    /// Run FTS search across configured content types.
    #[allow(non_snake_case)]
    fn run_fts_search(&self, query: &str) -> DbResult<Vec<(SearchContentType, FtsMatch)>> {
        let agent_id = self.filter.agent_id.as_deref();
        let msgs_id = self.filter.agent_id.as_deref().map(|a| {
            a.rsplit_once(':')
                .and_then(|(_, a)| Some(a))
                .unwrap_or(agent_id.expect("if we're on this path, filter.agent_id better be Some"))
        });
        // Fetch more than limit to allow for fusion.
        let fetch_limit = self.limit * 2;

        let mut results = Vec::new();

        match self.filter.content_type {
            Some(SearchContentType::Message) => {
                let msgs = fts::search_messages(self.conn, query, msgs_id, fetch_limit)?;
                results.extend(msgs.into_iter().map(|m| (SearchContentType::Message, m)));
            }
            Some(SearchContentType::MemoryBlock) => {
                let blocks = fts::search_memory_blocks(self.conn, query, agent_id, fetch_limit)?;
                results.extend(
                    blocks
                        .into_iter()
                        .map(|m| (SearchContentType::MemoryBlock, m)),
                );
            }
            Some(SearchContentType::ArchivalEntry) => {
                let entries = fts::search_archival(self.conn, query, agent_id, fetch_limit)?;
                results.extend(
                    entries
                        .into_iter()
                        .map(|m| (SearchContentType::ArchivalEntry, m)),
                );
            }
            None => {
                // Search all types.
                let msgs = fts::search_messages(self.conn, query, msgs_id, fetch_limit)?;
                let blocks = fts::search_memory_blocks(self.conn, query, agent_id, fetch_limit)?;
                let entries = fts::search_archival(self.conn, query, agent_id, fetch_limit)?;

                results.extend(msgs.into_iter().map(|m| (SearchContentType::Message, m)));
                results.extend(
                    blocks
                        .into_iter()
                        .map(|m| (SearchContentType::MemoryBlock, m)),
                );
                results.extend(
                    entries
                        .into_iter()
                        .map(|m| (SearchContentType::ArchivalEntry, m)),
                );
            }
        }

        Ok(results)
    }

    /// Run vector search across configured content types.
    fn run_vector_search(&self, embedding: &[f32]) -> DbResult<Vec<VectorSearchResult>> {
        let content_type_filter = self
            .filter
            .content_type
            .map(|ct| ct.to_vector_content_type());
        // Fetch more than limit to allow for fusion.
        let fetch_limit = self.limit * 2;

        vector::knn_search(self.conn, embedding, fetch_limit, content_type_filter)
    }

    /// Reciprocal Rank Fusion - combines results based on rank position.
    /// Fetch the source-table content for a vector-only hit. Vector
    /// search returns content_id but not content; without this, fusion
    /// produces SearchResults with `content: None` for any vector hit
    /// not also matched by FTS (the conceptual-query case). Dispatch by
    /// content_type to messages / memory_blocks / archival_entries.
    fn fetch_content(&self, content_type: ContentType, id: &str) -> Option<String> {
        match content_type {
            ContentType::Message => crate::queries::get_message(self.conn, id)
                .ok()
                .flatten()
                .and_then(|m| m.content_preview),
            ContentType::MemoryBlock => crate::queries::get_block(self.conn, id)
                .ok()
                .flatten()
                .and_then(|b| b.content_preview),
            ContentType::ArchivalEntry => crate::queries::get_archival_entry(self.conn, id)
                .ok()
                .flatten()
                .map(|e| e.content),
            ContentType::FilePassage => None,
        }
    }

    fn fuse_rrf(
        &self,
        fts_results: Option<Vec<(SearchContentType, FtsMatch)>>,
        vector_results: Option<Vec<VectorSearchResult>>,
        k: u32,
    ) -> Vec<SearchResult> {
        use std::collections::HashMap;

        let k = k as f64;
        let mut scores: HashMap<String, SearchResult> = HashMap::new();

        // Process FTS results.
        if let Some(fts) = fts_results {
            for (pos, (content_type, m)) in fts.into_iter().enumerate() {
                let rrf_score = 1.0 / (k + (pos + 1) as f64);
                let entry = scores.entry(m.id.clone()).or_insert_with(|| SearchResult {
                    id: m.id.clone(),
                    content_type,
                    content: Some(m.content.clone()),
                    score: 0.0,
                    scores: ScoreBreakdown::default(),
                });
                entry.score += rrf_score;
                entry.scores.fts_rank = Some(m.rank);
                entry.scores.fts_position = Some(pos + 1);
                entry.content = Some(m.content);
            }
        }

        // Process vector results.
        if let Some(vec) = vector_results {
            for (pos, r) in vec.into_iter().enumerate() {
                let content_type = match r.content_type {
                    ContentType::Message => SearchContentType::Message,
                    ContentType::MemoryBlock => SearchContentType::MemoryBlock,
                    ContentType::ArchivalEntry => SearchContentType::ArchivalEntry,
                    ContentType::FilePassage => continue,
                };

                let rrf_score = 1.0 / (k + (pos + 1) as f64);
                let fetched_ct = r.content_type;
                let id_for_fetch = r.content_id.clone();
                let entry = scores
                    .entry(r.content_id.clone())
                    .or_insert_with(|| SearchResult {
                        id: r.content_id.clone(),
                        content_type,
                        content: None,
                        score: 0.0,
                        scores: ScoreBreakdown::default(),
                    });
                entry.score += rrf_score;
                entry.scores.vector_distance = Some(r.distance);
                entry.scores.vector_position = Some(pos + 1);
                if entry.content.is_none() {
                    entry.content = self.fetch_content(fetched_ct, &id_for_fetch);
                }
            }
        }

        // Sort by combined score (higher is better).
        let mut results: Vec<_> = scores.into_values().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results
    }

    /// Linear combination of normalized scores.
    fn fuse_linear(
        &self,
        fts_results: Option<Vec<(SearchContentType, FtsMatch)>>,
        vector_results: Option<Vec<VectorSearchResult>>,
        fts_weight: f64,
        vector_weight: f64,
    ) -> Vec<SearchResult> {
        use std::collections::HashMap;

        let mut scores: HashMap<String, SearchResult> = HashMap::new();

        // Process and normalize FTS results.
        if let Some(fts) = fts_results {
            let max_rank = fts
                .iter()
                .map(|(_, m)| m.rank.abs())
                .fold(0.0f64, f64::max)
                .max(0.001);

            for (pos, (content_type, m)) in fts.into_iter().enumerate() {
                let normalized = 1.0 - (m.rank.abs() / max_rank);
                let weighted = normalized * fts_weight;

                let entry = scores.entry(m.id.clone()).or_insert_with(|| SearchResult {
                    id: m.id.clone(),
                    content_type,
                    content: Some(m.content.clone()),
                    score: 0.0,
                    scores: ScoreBreakdown::default(),
                });
                entry.score += weighted;
                entry.scores.fts_rank = Some(m.rank);
                entry.scores.fts_normalized = Some(normalized);
                entry.scores.fts_position = Some(pos + 1);
                entry.content = Some(m.content);
            }
        }

        // Process and normalize vector results.
        if let Some(vec) = vector_results {
            let max_dist = vec
                .iter()
                .map(|r| r.distance)
                .fold(0.0f32, f32::max)
                .max(0.001);

            for (pos, r) in vec.into_iter().enumerate() {
                let content_type = match r.content_type {
                    ContentType::Message => SearchContentType::Message,
                    ContentType::MemoryBlock => SearchContentType::MemoryBlock,
                    ContentType::ArchivalEntry => SearchContentType::ArchivalEntry,
                    ContentType::FilePassage => continue,
                };

                let normalized = 1.0 - (r.distance / max_dist) as f64;
                let weighted = normalized * vector_weight;

                let fetched_ct = r.content_type;
                let id_for_fetch = r.content_id.clone();
                let entry = scores
                    .entry(r.content_id.clone())
                    .or_insert_with(|| SearchResult {
                        id: r.content_id.clone(),
                        content_type,
                        content: None,
                        score: 0.0,
                        scores: ScoreBreakdown::default(),
                    });
                entry.score += weighted;
                entry.scores.vector_distance = Some(r.distance);
                entry.scores.vector_normalized = Some(normalized);
                entry.scores.vector_position = Some(pos + 1);
                if entry.content.is_none() {
                    entry.content = self.fetch_content(fetched_ct, &id_for_fetch);
                }
            }
        }

        // Sort by combined score.
        let mut results: Vec<_> = scores.into_values().collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results
    }
}

/// Convenience function to create a hybrid search builder.
pub fn search(conn: &rusqlite::Connection) -> HybridSearchBuilder<'_> {
    HybridSearchBuilder::new(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_type_conversion() {
        assert_eq!(
            SearchContentType::Message.to_vector_content_type(),
            ContentType::Message
        );
        assert_eq!(
            SearchContentType::MemoryBlock.to_vector_content_type(),
            ContentType::MemoryBlock
        );
        assert_eq!(
            SearchContentType::ArchivalEntry.to_vector_content_type(),
            ContentType::ArchivalEntry
        );
    }

    #[test]
    fn test_content_filter_builders() {
        let filter = ContentFilter::messages(Some("agent_1"));
        assert_eq!(filter.content_type, Some(SearchContentType::Message));
        assert_eq!(filter.agent_id, Some("agent_1".to_string()));

        let filter = ContentFilter::memory_blocks(None);
        assert_eq!(filter.content_type, Some(SearchContentType::MemoryBlock));
        assert_eq!(filter.agent_id, None);

        let filter = ContentFilter::all();
        assert_eq!(filter.content_type, None);
        assert_eq!(filter.agent_id, None);
    }

    #[test]
    fn test_fusion_method_default() {
        let fusion = FusionMethod::default();
        assert!(matches!(fusion, FusionMethod::Rrf { k: 60 }));
    }

    #[test]
    fn test_search_mode_default() {
        let mode = SearchMode::default();
        assert!(matches!(mode, SearchMode::Auto));
    }
}
