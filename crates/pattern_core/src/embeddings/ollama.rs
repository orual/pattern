//! Ollama-based embedding provider

use super::{Embedding, EmbeddingError, EmbeddingProvider, Result, validate_input};
use async_trait::async_trait;

/// Ollama embedding provider
#[derive(Debug)]
pub struct OllamaEmbedder {
    model: String,
    url: String,
    client: reqwest::Client,
}

impl OllamaEmbedder {
    /// Create a new Ollama embedder
    pub fn new(model: String, url: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| EmbeddingError::GenerationFailed(Box::new(e)))?;

        Ok(Self { model, url, client })
    }

    /// Get the dimensions for common Ollama embedding models
    fn get_dimensions(&self) -> usize {
        match self.model.as_str() {
            "mxbai-embed-large" => 1024,
            "nomic-embed-text" => 768,
            "all-minilm" => 384,
            "llama2" => 4096, // When used for embeddings
            _ => 768,         // Default assumption
        }
    }

    /// Check if Ollama is running
    async fn health_check_impl(&self) -> Result<()> {
        let health_url = format!("{}/api/tags", self.url);

        let response = self
            .client
            .get(&health_url)
            .send()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Ollama not reachable: {}", e)))?;

        if !response.status().is_success() {
            return Err(EmbeddingError::ApiError(format!(
                "Ollama health check failed with status: {}",
                response.status()
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbedder {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }

        let request_body = serde_json::json!({
            "model": self.model,
            "prompt": text,
        });

        let response = self
            .client
            .post(format!("{}/api/embeddings", self.url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmbeddingError::ApiError(format!(
                "Ollama API error: {}",
                error_text
            )));
        }

        let response_data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Failed to parse response: {}", e)))?;

        let embedding = response_data
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(|| EmbeddingError::ApiError("Missing embedding field".to_string()))?
            .iter()
            .filter_map(|v| v.as_f64())
            .map(|v| v as f32)
            .collect();

        Ok(Embedding::new(embedding, self.model.clone()))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        validate_input(texts)?;

        // Ollama doesn't have native batch support, so we process sequentially
        // In production, consider parallel requests with rate limiting
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
        self.get_dimensions()
    }

    fn max_batch_size(&self) -> usize {
        // Ollama doesn't have a batch API, so we set a reasonable limit
        // for sequential processing
        50
    }

    async fn health_check(&self) -> Result<()> {
        self.health_check_impl().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_dimensions() {
        let embedder = OllamaEmbedder::new(
            "mxbai-embed-large".to_string(),
            "http://localhost:11434".to_string(),
        )
        .unwrap();
        assert_eq!(embedder.dimensions(), 1024);

        let embedder_nomic = OllamaEmbedder::new(
            "nomic-embed-text".to_string(),
            "http://localhost:11434".to_string(),
        )
        .unwrap();
        assert_eq!(embedder_nomic.dimensions(), 768);
    }

    #[tokio::test]
    #[ignore = "requires running ollama server with all-minilm model"]
    async fn test_ollama_embed() {
        let embedder = OllamaEmbedder::new(
            "all-minilm".to_string(),
            "http://localhost:11434".to_string(),
        )
        .unwrap();

        let result = embedder.embed("test text").await.unwrap();
        assert_eq!(result.dimensions, 384);
        assert_eq!(result.vector.len(), 384);
        assert_eq!(result.model, "all-minilm");
    }

    #[tokio::test]
    async fn test_ollama_empty_input() {
        // This test doesn't need a server - it fails before making the request
        let embedder = OllamaEmbedder::new(
            "all-minilm".to_string(),
            "http://localhost:11434".to_string(),
        )
        .unwrap();

        let result = embedder.embed("").await;
        assert!(matches!(result, Err(EmbeddingError::EmptyInput)));
    }
}
