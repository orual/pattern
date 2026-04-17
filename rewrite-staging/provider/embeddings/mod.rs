// MOVING TO: pattern_provider/src/embeddings/mod.rs
// ORIGIN: crates/pattern_core/src/embeddings/mod.rs
// PHASE: future
// RESHAPE: Deferred until provider-side embedding use case confirmed
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Embedding providers for Pattern
//!
//! This module provides traits and implementations for generating
//! embeddings from text content, supporting both local and cloud providers.

use async_trait::async_trait;
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

#[cfg(feature = "embed-candle")]
pub mod candle;
#[cfg(feature = "embed-cloud")]
pub mod cloud;
#[cfg(feature = "embed-ollama")]
pub mod ollama;

pub mod simple;
pub use simple::SimpleEmbeddingProvider;

/// Embedding provider error type
#[derive(Error, Debug, Diagnostic)]
pub enum EmbeddingError {
    #[error("Embedding generation failed")]
    #[diagnostic(help("Check your embedding model configuration and input text"))]
    GenerationFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("Model not found: {0}")]
    #[diagnostic(help("Ensure the model is downloaded or accessible"))]
    ModelNotFound(String),

    #[error("Invalid dimensions: expected {expected}, got {actual}")]
    #[diagnostic(help("All embeddings must use the same model to ensure consistent dimensions"))]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("API error: {0}")]
    #[diagnostic(help("Check your API key and network connection"))]
    ApiError(String),

    #[error("Batch size too large: {size} (max: {max})")]
    BatchSizeTooLarge { size: usize, max: usize },

    #[error("Empty input provided")]
    #[diagnostic(help("Provide at least one non-empty text to embed"))]
    EmptyInput,
}

pub type Result<T> = std::result::Result<T, EmbeddingError>;

/// An embedding vector with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    /// The embedding vector
    pub vector: Vec<f32>,
    /// Model used to generate this embedding
    pub model: String,
    /// Dimensions of the vector
    pub dimensions: usize,
    /// Optional metadata about the embedding
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl Embedding {
    /// Create a new embedding
    pub fn new(vector: Vec<f32>, model: String) -> Self {
        let dimensions = vector.len();
        Self {
            vector,
            model,
            dimensions,
            metadata: None,
        }
    }

    /// Calculate cosine similarity with another embedding
    pub fn cosine_similarity(&self, other: &Embedding) -> Result<f32> {
        if self.dimensions != other.dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.dimensions,
                actual: other.dimensions,
            });
        }

        let dot_product: f32 = self
            .vector
            .iter()
            .zip(&other.vector)
            .map(|(a, b)| a * b)
            .sum();

        let norm_a: f32 = self.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = other.vector.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return Ok(0.0);
        }

        Ok(dot_product / (norm_a * norm_b))
    }

    /// Normalize the embedding vector to unit length
    pub fn normalize(&mut self) {
        let norm: f32 = self.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut self.vector {
                *val /= norm;
            }
        }
    }
}

/// Trait for embedding providers
#[async_trait]
pub trait EmbeddingProvider: Send + Sync + std::fmt::Debug {
    /// Generate an embedding for a single text
    async fn embed(&self, text: &str) -> Result<Embedding>;

    /// Generate embeddings for multiple texts
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>>;

    /// Get the model identifier
    fn model_id(&self) -> &str;

    /// Get the embedding dimensions
    fn dimensions(&self) -> usize;

    /// Get the maximum batch size supported
    fn max_batch_size(&self) -> usize {
        256 // Default batch size
    }

    /// Check if the provider is available/healthy
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// Convenience method for embedding a single query (alias for embed)
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        Ok(self.embed(query).await?.vector)
    }

    /// Get the model name (alias for model_id)
    fn model_name(&self) -> &str {
        self.model_id()
    }
}

/// Configuration for embedding providers
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum EmbeddingConfig {
    #[cfg(feature = "embed-candle")]
    Candle {
        model: String,
        #[serde(default)]
        cache_dir: Option<String>,
    },
    #[cfg(feature = "embed-cloud")]
    OpenAI {
        model: String,
        api_key: String,
        #[serde(default)]
        dimensions: Option<usize>,
    },
    #[cfg(feature = "embed-cloud")]
    Cohere {
        model: String,
        api_key: String,
        #[serde(default)]
        input_type: Option<String>,
    },
    /// Google Gemini embeddings provider.
    ///
    /// - Recommended model: `gemini-embedding-001`.
    /// - Dimensions: Defaults to 3072. Google recommends 768, 1536, or 3072 depending on storage/latency needs.
    /// - Task type (optional): If set, optimizes embeddings for a specific use case.
    ///   - Use `RETRIEVAL_QUERY` for search queries (default if omitted).
    ///   - Use `RETRIEVAL_DOCUMENT` for documents/items you will retrieve.
    ///   - Use `SEMANTIC_SIMILARITY` for general similarity comparisons.
    ///   - Other supported values: `CLASSIFICATION`, `CLUSTERING`, `CODE_RETRIEVAL_QUERY`, `QUESTION_ANSWERING`, `FACT_VERIFICATION`.
    #[cfg(feature = "embed-cloud")]
    Gemini {
        /// Gemini embedding model ID, e.g. `gemini-embedding-001`.
        model: String,
        /// API key for the Gemini API.
        api_key: String,
        /// Output dimensionality (truncation size). Defaults to 3072.
        #[serde(default)]
        dimensions: Option<usize>,
        /// Optional task type that tunes embedding behavior.
        /// Recommended: `RETRIEVAL_QUERY` (queries) or `RETRIEVAL_DOCUMENT` (documents).
        #[serde(default)]
        task_type: Option<String>,
    },
    #[cfg(feature = "embed-ollama")]
    Ollama { model: String, url: String },
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        #[cfg(feature = "embed-candle")]
        {
            EmbeddingConfig::Candle {
                model: "BAAI/bge-small-en-v1.5".to_string(),
                cache_dir: None,
            }
        }
        #[cfg(all(not(feature = "embed-candle"), feature = "embed-cloud"))]
        {
            EmbeddingConfig::OpenAI {
                model: "text-embedding-3-small".to_string(),
                api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
                dimensions: Some(1536),
            }
        }
        #[cfg(all(
            not(feature = "embed-candle"),
            not(feature = "embed-cloud"),
            feature = "embed-ollama"
        ))]
        {
            EmbeddingConfig::Ollama {
                model: "mxbai-embed-large".to_string(),
                url: "http://localhost:11434".to_string(),
            }
        }
        #[cfg(not(any(
            feature = "embed-candle",
            feature = "embed-cloud",
            feature = "embed-ollama"
        )))]
        {
            panic!("No embedding provider features enabled")
        }
    }
}

/// Create an embedding provider from configuration
pub async fn create_provider(config: EmbeddingConfig) -> Result<Arc<dyn EmbeddingProvider>> {
    match config {
        #[cfg(feature = "embed-candle")]
        EmbeddingConfig::Candle { model, cache_dir } => Ok(Arc::new(
            candle::CandleEmbedder::new(&model, cache_dir).await?,
        )),
        #[cfg(feature = "embed-cloud")]
        EmbeddingConfig::OpenAI {
            model,
            api_key,
            dimensions,
        } => Ok(Arc::new(cloud::OpenAIEmbedder::new(
            model, api_key, dimensions,
        ))),
        #[cfg(feature = "embed-cloud")]
        EmbeddingConfig::Cohere {
            model,
            api_key,
            input_type,
        } => Ok(Arc::new(cloud::CohereEmbedder::new(
            model, api_key, input_type,
        ))),
        #[cfg(feature = "embed-cloud")]
        EmbeddingConfig::Gemini {
            model,
            api_key,
            dimensions,
            task_type,
        } => {
            let task = task_type
                .as_ref()
                .and_then(|s| cloud::GeminiEmbeddingTaskType::parse(s));
            Ok(Arc::new(
                cloud::GeminiEmbedder::new(model, api_key, dimensions).with_task_type(task),
            ))
        }
        #[cfg(feature = "embed-ollama")]
        EmbeddingConfig::Ollama { model, url } => {
            Ok(Arc::new(ollama::OllamaEmbedder::new(model, url)?))
        }
        #[allow(unreachable_patterns)]
        _ => Err(EmbeddingError::ModelNotFound(
            "No matching provider available".to_string(),
        )),
    }
}

/// Helper to validate text input
pub fn validate_input(texts: &[String]) -> Result<()> {
    if texts.is_empty() {
        return Err(EmbeddingError::EmptyInput);
    }

    if texts.iter().all(|t| t.trim().is_empty()) {
        return Err(EmbeddingError::EmptyInput);
    }

    Ok(())
}

/// Mock embedding provider for testing
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct MockEmbeddingProvider {
    pub model: String,
    pub dimensions: usize,
}

#[cfg(test)]
impl Default for MockEmbeddingProvider {
    fn default() -> Self {
        Self {
            model: "mock-model".to_string(),
            dimensions: 384,
        }
    }
}

#[cfg(test)]
#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }

        // Return a mock embedding with the configured dimensions
        Ok(Embedding::new(
            vec![0.1; self.dimensions],
            self.model.clone(),
        ))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        validate_input(texts)?;

        // Return mock embeddings for each text
        Ok(texts
            .iter()
            .map(|_| Embedding::new(vec![0.1; self.dimensions], self.model.clone()))
            .collect())
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_cosine_similarity() {
        let emb1 = Embedding::new(vec![1.0, 0.0, 0.0], "test".to_string());
        let emb2 = Embedding::new(vec![1.0, 0.0, 0.0], "test".to_string());
        let emb3 = Embedding::new(vec![0.0, 1.0, 0.0], "test".to_string());

        assert_eq!(emb1.cosine_similarity(&emb2).unwrap(), 1.0);
        assert_eq!(emb1.cosine_similarity(&emb3).unwrap(), 0.0);
    }

    #[test]
    fn test_embedding_normalize() {
        let mut emb = Embedding::new(vec![3.0, 4.0], "test".to_string());
        emb.normalize();

        let norm: f32 = emb.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_dimension_mismatch() {
        let emb1 = Embedding::new(vec![1.0, 0.0], "test".to_string());
        let emb2 = Embedding::new(vec![1.0, 0.0, 0.0], "test".to_string());

        assert!(matches!(
            emb1.cosine_similarity(&emb2),
            Err(EmbeddingError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_validate_input() {
        assert!(validate_input(&[]).is_err());
        assert!(validate_input(&["".to_string()]).is_err());
        assert!(validate_input(&["  ".to_string()]).is_err());
        assert!(validate_input(&["hello".to_string()]).is_ok());
    }
}
