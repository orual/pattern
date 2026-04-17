//! Embedding vector value type.
//!
//! An [`Embedding`] is a dense floating-point vector with provenance
//! metadata (model name, dimensions, optional tags). It is produced by the
//! [`crate::traits::EmbeddingProvider`] trait and consumed anywhere a
//! similarity comparison or vector search is needed.

use serde::{Deserialize, Serialize};

use crate::error::embedding::EmbeddingError;

/// Result alias for embedding operations.
pub type EmbeddingResult<T> = std::result::Result<T, EmbeddingError>;

/// A dense embedding vector with provenance metadata.
///
/// Extracted verbatim from the pre-v3 `pattern_core::embeddings::Embedding`;
/// the containing module has since been staged to `rewrite-staging/`. The
/// shape (and its cosine-similarity / normalize helpers) is preserved
/// unchanged so downstream code rebased on v3 continues to compile against
/// the new import path.
///
/// # Examples
///
/// ```
/// use pattern_core::types::embedding::Embedding;
///
/// let a = Embedding::new(vec![1.0, 0.0], "m".into());
/// let b = Embedding::new(vec![1.0, 0.0], "m".into());
/// let sim = a.cosine_similarity(&b).unwrap();
/// assert!((sim - 1.0).abs() < 1e-6);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// Model used to generate this embedding.
    pub model: String,
    /// Dimensions of the vector.
    pub dimensions: usize,
    /// Optional metadata about the embedding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl Embedding {
    /// Create a new embedding.
    pub fn new(vector: Vec<f32>, model: String) -> Self {
        let dimensions = vector.len();
        Self {
            vector,
            model,
            dimensions,
            metadata: None,
        }
    }

    /// Calculate cosine similarity with another embedding.
    ///
    /// Returns [`EmbeddingError::DimensionMismatch`] if the two embeddings
    /// have different dimensions.
    pub fn cosine_similarity(&self, other: &Embedding) -> EmbeddingResult<f32> {
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

    /// Normalize the embedding vector to unit length.
    pub fn normalize(&mut self) {
        let norm: f32 = self.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut self.vector {
                *val /= norm;
            }
        }
    }
}
