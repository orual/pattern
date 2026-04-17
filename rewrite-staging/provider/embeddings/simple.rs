// MOVING TO: pattern_provider/src/embeddings/simple.rs
// ORIGIN: crates/pattern_core/src/embeddings/simple.rs
// PHASE: future
// RESHAPE: Deferred until provider-side embedding use case confirmed
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

use async_trait::async_trait;

use super::{Embedding, EmbeddingError, EmbeddingProvider, Result};

/// Simple embedding provider that returns fixed-size vectors
/// Useful for development and testing when real embeddings aren't needed
#[derive(Debug, Clone)]
pub struct SimpleEmbeddingProvider {
    model: String,
    dimensions: usize,
}

impl SimpleEmbeddingProvider {
    pub fn new() -> Self {
        Self {
            model: "simple-embedding".to_string(),
            dimensions: 384,
        }
    }

    pub fn with_dimensions(mut self, dimensions: usize) -> Self {
        self.dimensions = dimensions;
        self
    }

    pub fn with_model_name(mut self, name: String) -> Self {
        self.model = name;
        self
    }
}

impl Default for SimpleEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingProvider for SimpleEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }

        // Generate a deterministic but simple embedding
        // Use text length and char sum to create variation
        let text_len = text.len() as f32;
        let char_sum: u32 = text.chars().map(|c| c as u32).sum();
        let base_value = (char_sum as f32 / text_len) / 1000.0;

        // Create vector with some variation
        let mut vector = vec![0.0; self.dimensions];
        for (i, val) in vector.iter_mut().enumerate() {
            *val = base_value + (i as f32 * 0.001);
        }

        // Normalize
        let mut embedding = Embedding::new(vector, self.model.clone());
        embedding.normalize();

        Ok(embedding)
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        super::validate_input(texts)?;

        let mut embeddings = Vec::with_capacity(texts.len());
        for text in texts {
            embeddings.push(self.embed(text).await?);
        }

        Ok(embeddings)
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
