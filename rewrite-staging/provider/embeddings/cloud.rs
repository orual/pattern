// MOVING TO: pattern_provider/src/embeddings/cloud.rs
// ORIGIN: crates/pattern_core/src/embeddings/cloud.rs
// PHASE: future
// RESHAPE: Deferred until provider-side embedding use case confirmed
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Cloud-based embedding providers (OpenAI, Cohere, etc.)

use super::{Embedding, EmbeddingError, EmbeddingProvider, Result, validate_input};
use async_trait::async_trait;
use http::HeaderMap;
use serde::Deserialize;
use tracing::warn;

/// OpenAI embedding provider
///
#[derive(Clone)]
pub struct OpenAIEmbedder {
    model: String,
    api_key: String,
    dimensions: Option<usize>,
}

impl std::fmt::Debug for OpenAIEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIEmbedder")
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl OpenAIEmbedder {
    /// Create a new OpenAI embedder
    pub fn new(model: String, api_key: String, dimensions: Option<usize>) -> Self {
        Self {
            model,
            api_key,
            dimensions,
        }
    }

    /// Get the actual dimensions for the model
    fn get_dimensions(&self) -> usize {
        self.dimensions.unwrap_or(match self.model.as_str() {
            "text-embedding-3-small" => 1536,
            "text-embedding-3-large" => 3072,
            "text-embedding-ada-002" => 1536,
            _ => 1536,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAIEmbedder {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }

        let embeddings = self.embed_batch(&[text.to_string()]).await?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::GenerationFailed("No embedding returned".into()))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        validate_input(texts)?;

        if texts.len() > self.max_batch_size() {
            return Err(EmbeddingError::BatchSizeTooLarge {
                size: texts.len(),
                max: self.max_batch_size(),
            });
        }

        let client = reqwest::Client::new();
        let mut request_body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        if let Some(dims) = self.dimensions {
            request_body["dimensions"] = serde_json::json!(dims);
        }

        // Retry with backoff and provider-aware headers
        let mut retries = 0u32;
        let max_retries = 6u32;
        let mut backoff_ms: u64 = 1000; // 1s initial
        let response_data: OpenAIEmbeddingResponse = loop {
            let resp_res = client
                .post("https://api.openai.com/v1/embeddings")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await;

            let resp = match resp_res {
                Ok(r) => r,
                Err(e) => {
                    if retries >= max_retries {
                        return Err(EmbeddingError::ApiError(format!("Request failed: {}", e)));
                    }
                    warn!(
                        "OpenAI embed request error (attempt {}/{}): {} — backing off {}ms",
                        retries + 1,
                        max_retries,
                        e,
                        backoff_ms
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    backoff_ms = (backoff_ms * 2).min(60_000) + (rand::random::<u64>() % 500);
                    retries += 1;
                    continue;
                }
            };

            if resp.status().is_success() {
                match resp.json::<OpenAIEmbeddingResponse>().await {
                    Ok(parsed) => break parsed,
                    Err(e) => {
                        if retries >= max_retries {
                            return Err(EmbeddingError::ApiError(format!(
                                "Failed to parse response: {}",
                                e
                            )));
                        }
                        warn!(
                            "OpenAI embed parse error (attempt {}/{}): {} — backing off {}ms",
                            retries + 1,
                            max_retries,
                            e,
                            backoff_ms
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(60_000) + (rand::random::<u64>() % 500);
                        retries += 1;
                        continue;
                    }
                }
            }

            let status = resp.status();
            let headers = resp.headers().clone();
            let err_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            // Rate-limit or transient errors: 429/529/503
            if [429, 503, 529].contains(&status.as_u16()) && retries < max_retries {
                let (wait_ms, src) = compute_wait_from_headers(&headers)
                    .map(|ms| (ms, "headers".to_string()))
                    .unwrap_or_else(|| (backoff_ms, "backoff".to_string()));
                warn!(
                    "OpenAI rate limit/status {} (attempt {}/{}), waiting {}ms before retry (source: {}) — {}",
                    status.as_u16(),
                    retries + 1,
                    max_retries,
                    wait_ms,
                    src,
                    err_text
                );
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                backoff_ms = (wait_ms * 2).min(60_000) + (rand::random::<u64>() % 500);
                retries += 1;
                continue;
            }

            // Non-retryable or out of retries
            return Err(EmbeddingError::ApiError(format!(
                "OpenAI API error ({}): {}",
                status, err_text
            )));
        };

        // Sort by index to ensure correct order
        let mut indexed_embeddings: Vec<_> = response_data.data.into_iter().collect();
        indexed_embeddings.sort_by_key(|item| item.index);

        let embeddings = indexed_embeddings
            .into_iter()
            .map(|item| Embedding::new(item.embedding, self.model.clone()))
            .collect();

        Ok(embeddings)
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.get_dimensions()
    }

    fn max_batch_size(&self) -> usize {
        2048 // OpenAI's batch limit
    }
}

/// Google Gemini embedding provider (Generative Language API)
///
/// Task type guidance:
/// - Use `RETRIEVAL_QUERY` when embedding search queries (default if none is set).
/// - Use `RETRIEVAL_DOCUMENT` when embedding documents/records for your index.
///   Pairing `RETRIEVAL_QUERY` (queries) with `RETRIEVAL_DOCUMENT` (documents) often yields better retrieval.
/// - Use `SEMANTIC_SIMILARITY` for generic similarity comparisons between texts.
#[derive(Clone)]
pub struct GeminiEmbedder {
    model: String,
    api_key: String,
    dimensions: Option<usize>,
    task_type: Option<GeminiEmbeddingTaskType>,
}

impl std::fmt::Debug for GeminiEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiEmbedder")
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("task_type", &self.task_type)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl GeminiEmbedder {
    pub fn new(model: String, api_key: String, dimensions: Option<usize>) -> Self {
        Self {
            model,
            api_key,
            dimensions,
            task_type: None,
        }
    }

    /// Configure the task type to optimize embedding quality for a use case.
    ///
    /// Recommended:
    /// - `RETRIEVAL_QUERY` for user queries (default if not provided).
    /// - `RETRIEVAL_DOCUMENT` for the corpus/documents being searched.
    /// - `SEMANTIC_SIMILARITY` for general-purpose similarity.
    pub fn with_task_type(mut self, task_type: Option<GeminiEmbeddingTaskType>) -> Self {
        self.task_type = task_type;
        self
    }

    fn get_dimensions(&self) -> usize {
        // Docs: gemini-embedding-001 defaults to 3072; recommend 768/1536/3072
        self.dimensions.unwrap_or(3072)
    }

    /// Build the correct endpoint URL and request body for Gemini embeddings.
    /// Emits a warning if the configured model doesn't look like an embedding model,
    /// suggesting `gemini-embedding-001`.
    fn build_request(&self, texts: &[String]) -> (String, serde_json::Value) {
        if !self.model.to_lowercase().contains("embedding") {
            tracing::warn!(
                "Gemini embedding model '{}' does not look like an embedding model. Consider 'gemini-embedding-001' for best results.",
                self.model
            );
        }

        let single = texts.len() == 1;
        let url = if single {
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:embedContent",
                self.model
            )
        } else {
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:batchEmbedContents",
                self.model
            )
        };

        let tt = self
            .task_type
            .unwrap_or(GeminiEmbeddingTaskType::RetrievalQuery);

        let body = if single {
            let mut obj = serde_json::json!({
                "model": format!("models/{}", self.model),
                "content": { "parts": [ { "text": &texts[0] } ] },
                "taskType": tt.as_str(),
            });
            if let Some(dims) = self.dimensions {
                obj["outputDimensionality"] = serde_json::json!(dims);
            }
            obj
        } else {
            let mut requests: Vec<serde_json::Value> = Vec::with_capacity(texts.len());
            for t in texts.iter() {
                let mut req = serde_json::json!({
                    "content": { "parts": [ { "text": t } ] },
                    "taskType": tt.as_str(),
                });
                if let Some(dims) = self.dimensions {
                    req["outputDimensionality"] = serde_json::json!(dims);
                }
                requests.push(req);
            }
            serde_json::json!({
                "model": format!("models/{}", self.model),
                "requests": requests,
            })
        };

        (url, body)
    }
}

#[async_trait]
impl EmbeddingProvider for GeminiEmbedder {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }
        let embeddings = self.embed_batch(&[text.to_string()]).await?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::GenerationFailed("No embedding returned".into()))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        validate_input(texts)?;

        if texts.len() > self.max_batch_size() {
            return Err(EmbeddingError::BatchSizeTooLarge {
                size: texts.len(),
                max: self.max_batch_size(),
            });
        }

        let client = reqwest::Client::new();
        let (url, body) = self.build_request(texts);

        let mut retries = 0u32;
        let max_retries = 6u32;
        let mut backoff_ms: u64 = 1000;

        let resp_json = loop {
            let resp_res = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("x-goog-api-key", &self.api_key)
                .json(&body)
                .send()
                .await;

            let resp = match resp_res {
                Ok(r) => r,
                Err(e) => {
                    if retries >= max_retries {
                        return Err(EmbeddingError::ApiError(format!("Request failed: {}", e)));
                    }
                    warn!(
                        "Gemini embed request error (attempt {}/{}): {} — backing off {}ms",
                        retries + 1,
                        max_retries,
                        e,
                        backoff_ms
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    backoff_ms = (backoff_ms * 2).min(60_000) + (rand::random::<u64>() % 500);
                    retries += 1;
                    continue;
                }
            };

            if resp.status().is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(v) => break v,
                    Err(e) => {
                        if retries >= max_retries {
                            return Err(EmbeddingError::ApiError(format!(
                                "Failed to parse response: {}",
                                e
                            )));
                        }
                        warn!(
                            "Gemini embed parse error (attempt {}/{}): {} — backing off {}ms",
                            retries + 1,
                            max_retries,
                            e,
                            backoff_ms
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(60_000) + (rand::random::<u64>() % 500);
                        retries += 1;
                        continue;
                    }
                }
            }

            let status = resp.status();
            let headers = resp.headers().clone();
            let err_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            if [429, 503, 529].contains(&status.as_u16()) && retries < max_retries {
                let (wait_ms, src) = compute_wait_from_headers(&headers)
                    .map(|ms| (ms, "headers".to_string()))
                    .unwrap_or_else(|| (backoff_ms, "backoff".to_string()));
                warn!(
                    "Gemini rate limit/status {} (attempt {}/{}), waiting {}ms before retry (source: {}) — {}",
                    status.as_u16(),
                    retries + 1,
                    max_retries,
                    wait_ms,
                    src,
                    err_text
                );
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                backoff_ms = (wait_ms * 2).min(60_000) + (rand::random::<u64>() % 500);
                retries += 1;
                continue;
            }

            return Err(EmbeddingError::ApiError(format!(
                "Gemini API error ({}): {}",
                status, err_text
            )));
        };

        // Extract embeddings from JSON shape (support both single and multi)
        let embeddings: Vec<Vec<f32>> =
            if let Some(arr) = resp_json.get("embeddings").and_then(|v| v.as_array()) {
                // { embeddings: [ { values: [...] }, ... ] }
                arr.iter()
                    .map(|item| {
                        item.get("values")
                            .and_then(|v| v.as_array())
                            .ok_or_else(|| EmbeddingError::ApiError("Missing values".into()))
                            .and_then(|arr| collect_f32(arr))
                    })
                    .collect::<std::result::Result<_, _>>()?
            } else if let Some(emb) = resp_json
                .get("embedding")
                .and_then(|v| v.get("values"))
                .and_then(|v| v.as_array())
            {
                vec![collect_f32(emb)?]
            } else {
                return Err(EmbeddingError::ApiError(
                    "Missing embeddings in response".into(),
                ));
            };

        Ok(embeddings
            .into_iter()
            .map(|vals| Embedding::new(vals, self.model.clone()))
            .collect())
    }

    fn model_id(&self) -> &str {
        &self.model
    }
    fn dimensions(&self) -> usize {
        self.get_dimensions()
    }
    fn max_batch_size(&self) -> usize {
        100
    }
}

fn collect_f32(arr: &[serde_json::Value]) -> Result<Vec<f32>> {
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        match v.as_f64() {
            Some(n) => out.push(n as f32),
            None => {
                return Err(EmbeddingError::ApiError(
                    "Non-numeric embedding value".into(),
                ));
            }
        }
    }
    Ok(out)
}

// Shared helper: compute wait duration from classic headers
fn compute_wait_from_headers(headers: &HeaderMap) -> Option<u64> {
    // Retry-After seconds or HTTP-date
    if let Some(raw) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        let s = raw.trim();
        if let Ok(secs) = s.parse::<u64>() {
            return Some(secs * 1000 + (rand::random::<u64>() % 500));
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
            let now = chrono::Utc::now();
            let ms = dt
                .with_timezone(&chrono::Utc)
                .signed_duration_since(now)
                .num_milliseconds();
            if ms > 0 {
                return Some(ms as u64 + (rand::random::<u64>() % 500));
            }
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            let now = chrono::Utc::now();
            let ms = dt
                .with_timezone(&chrono::Utc)
                .signed_duration_since(now)
                .num_milliseconds();
            if ms > 0 {
                return Some(ms as u64 + (rand::random::<u64>() % 500));
            }
        }
    }

    // Provider-specific resets (OpenAI/Groq-like)
    let keys = [
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
        "x-ratelimit-reset-input-tokens",
        "x-ratelimit-reset-output-tokens",
        "x-ratelimit-reset-images-requests",
        "x-ratelimit-reset",
        "ratelimit-reset",
    ];
    for k in keys.iter() {
        if let Some(raw) = headers.get(*k).and_then(|v| v.to_str().ok()) {
            let s = raw.trim();
            if let Some(stripped) = s.strip_suffix("ms") {
                if let Ok(v) = stripped.trim().parse::<u64>() {
                    return Some(v + (rand::random::<u64>() % 500));
                }
            }
            if let Some(stripped) = s.strip_suffix('s') {
                if let Ok(v) = stripped.trim().parse::<u64>() {
                    return Some(v * 1000 + (rand::random::<u64>() % 500));
                }
            }
            if let Some(stripped) = s.strip_suffix('m') {
                if let Ok(v) = stripped.trim().parse::<u64>() {
                    return Some(v * 60_000 + (rand::random::<u64>() % 500));
                }
            }
            if let Some(stripped) = s.strip_suffix('h') {
                if let Ok(v) = stripped.trim().parse::<u64>() {
                    return Some(v * 3_600_000 + (rand::random::<u64>() % 500));
                }
            }
            if let Ok(secs) = s.parse::<u64>() {
                return Some(secs * 1000 + (rand::random::<u64>() % 500));
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                let now = chrono::Utc::now();
                let ms = dt
                    .with_timezone(&chrono::Utc)
                    .signed_duration_since(now)
                    .num_milliseconds();
                if ms > 0 {
                    return Some(ms as u64 + (rand::random::<u64>() % 500));
                }
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
                let now = chrono::Utc::now();
                let ms = dt
                    .with_timezone(&chrono::Utc)
                    .signed_duration_since(now)
                    .num_milliseconds();
                if ms > 0 {
                    return Some(ms as u64 + (rand::random::<u64>() % 500));
                }
            }
        }
    }
    None
}

/// Gemini embedding task types (per Google docs)
#[derive(Debug, Clone, Copy)]
pub enum GeminiEmbeddingTaskType {
    SemanticSimilarity,
    Classification,
    Clustering,
    RetrievalDocument,
    RetrievalQuery,
    CodeRetrievalQuery,
    QuestionAnswering,
    FactVerification,
}

impl GeminiEmbeddingTaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SemanticSimilarity => "SEMANTIC_SIMILARITY",
            Self::Classification => "CLASSIFICATION",
            Self::Clustering => "CLUSTERING",
            Self::RetrievalDocument => "RETRIEVAL_DOCUMENT",
            Self::RetrievalQuery => "RETRIEVAL_QUERY",
            Self::CodeRetrievalQuery => "CODE_RETRIEVAL_QUERY",
            Self::QuestionAnswering => "QUESTION_ANSWERING",
            Self::FactVerification => "FACT_VERIFICATION",
        }
    }

    pub fn parse<S: AsRef<str>>(s: S) -> Option<Self> {
        match s.as_ref().to_ascii_uppercase().as_str() {
            "SEMANTIC_SIMILARITY" => Some(Self::SemanticSimilarity),
            "CLASSIFICATION" => Some(Self::Classification),
            "CLUSTERING" => Some(Self::Clustering),
            "RETRIEVAL_DOCUMENT" => Some(Self::RetrievalDocument),
            "RETRIEVAL_QUERY" => Some(Self::RetrievalQuery),
            "CODE_RETRIEVAL_QUERY" => Some(Self::CodeRetrievalQuery),
            "QUESTION_ANSWERING" => Some(Self::QuestionAnswering),
            "FACT_VERIFICATION" => Some(Self::FactVerification),
            _ => None,
        }
    }
}

// (Gemini request building tests merged into the tests module below)

/// Cohere embedding provider
pub struct CohereEmbedder {
    model: String,
    api_key: String,
    input_type: Option<String>,
}

impl std::fmt::Debug for CohereEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CohereEmbedder")
            .field("model", &self.model)
            .field("input_type", &self.input_type)
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

impl CohereEmbedder {
    /// Create a new Cohere embedder
    pub fn new(model: String, api_key: String, input_type: Option<String>) -> Self {
        Self {
            model,
            api_key,
            input_type,
        }
    }

    /// Get the dimensions for the model
    fn get_dimensions(&self) -> usize {
        match self.model.as_str() {
            "embed-english-v3.0" => 1024,
            "embed-multilingual-v3.0" => 1024,
            "embed-english-light-v3.0" => 384,
            "embed-multilingual-light-v3.0" => 384,
            _ => 1024,
        }
    }
}

#[async_trait]
impl EmbeddingProvider for CohereEmbedder {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }

        let embeddings = self.embed_batch(&[text.to_string()]).await?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::GenerationFailed("No embedding returned".into()))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        validate_input(texts)?;

        if texts.len() > self.max_batch_size() {
            return Err(EmbeddingError::BatchSizeTooLarge {
                size: texts.len(),
                max: self.max_batch_size(),
            });
        }

        let client = reqwest::Client::new();
        let mut request_body = serde_json::json!({
            "model": self.model,
            "texts": texts,
        });

        if let Some(ref input_type) = self.input_type {
            request_body["input_type"] = serde_json::json!(input_type);
        }

        let response = client
            .post("https://api.cohere.ai/v1/embed")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
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
                "Cohere API error: {}",
                error_text
            )));
        }

        let response_data: CohereEmbeddingResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Failed to parse response: {}", e)))?;

        let embeddings = response_data
            .embeddings
            .into_iter()
            .map(|embedding| Embedding::new(embedding, self.model.clone()))
            .collect();

        Ok(embeddings)
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.get_dimensions()
    }

    fn max_batch_size(&self) -> usize {
        96 // Cohere's batch limit
    }
}

/// Request/response types for API calls

#[derive(Debug, Deserialize)]
struct OpenAIEmbeddingResponse {
    data: Vec<OpenAIEmbeddingData>,
    #[allow(dead_code)]
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIEmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIUsage {
    prompt_tokens: usize,
    total_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct CohereEmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(texts: &[&str], dims: Option<usize>) -> (GeminiEmbedder, Vec<String>) {
        let emb = GeminiEmbedder::new(
            "gemini-embedding-001".to_string(),
            "test_key".to_string(),
            dims,
        )
        .with_task_type(Some(GeminiEmbeddingTaskType::RetrievalQuery));
        let batch = texts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        (emb, batch)
    }

    #[test]
    fn build_request_single_has_content_and_camelcase() {
        let (emb, batch) = mk(&["hello"], Some(1536));
        let (url, body) = emb.build_request(&batch);
        assert!(url.ends_with(":embedContent"));
        assert_eq!(
            body["model"],
            serde_json::json!("models/gemini-embedding-001")
        );
        assert!(body.get("content").is_some());
        assert!(body.get("contents").is_none());
        assert_eq!(body["taskType"], serde_json::json!("RETRIEVAL_QUERY"));
        assert_eq!(body["outputDimensionality"], serde_json::json!(1536));
        assert_eq!(
            body["content"]["parts"][0]["text"],
            serde_json::json!("hello")
        );
    }

    #[test]
    fn build_request_batch_has_requests_array() {
        let (emb, batch) = mk(&["a", "b"], None);
        let (url, body) = emb.build_request(&batch);
        assert!(url.ends_with(":batchEmbedContents"));
        assert!(body.get("requests").is_some());
        let reqs = body["requests"].as_array().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[0]["content"]["parts"][0]["text"],
            serde_json::json!("a")
        );
        assert_eq!(reqs[0]["taskType"], serde_json::json!("RETRIEVAL_QUERY"));
        assert!(reqs[0].get("outputDimensionality").is_none());
    }

    #[test]
    fn test_openai_dimensions() {
        let embedder = OpenAIEmbedder::new(
            "text-embedding-3-small".to_string(),
            "test-key".to_string(),
            None,
        );
        assert_eq!(embedder.dimensions(), 1536);

        let embedder_custom = OpenAIEmbedder::new(
            "text-embedding-3-small".to_string(),
            "test-key".to_string(),
            Some(768),
        );
        assert_eq!(embedder_custom.dimensions(), 768);
    }

    #[test]
    fn test_cohere_dimensions() {
        let embedder = CohereEmbedder::new(
            "embed-english-v3.0".to_string(),
            "test-key".to_string(),
            None,
        );
        assert_eq!(embedder.dimensions(), 1024);

        let embedder_light = CohereEmbedder::new(
            "embed-english-light-v3.0".to_string(),
            "test-key".to_string(),
            None,
        );
        assert_eq!(embedder_light.dimensions(), 384);
    }

    #[tokio::test]
    async fn test_openai_embed() {
        let embedder = OpenAIEmbedder::new(
            "text-embedding-3-small".to_string(),
            "test-key".to_string(),
            None,
        );
        // Don't actually call the API in tests
        assert_eq!(embedder.model_id(), "text-embedding-3-small");
        assert_eq!(embedder.dimensions(), 1536);
    }

    #[tokio::test]
    async fn test_cohere_embed() {
        let embedder = CohereEmbedder::new(
            "embed-english-v3.0".to_string(),
            "test-key".to_string(),
            None,
        );
        // Don't actually call the API in tests
        assert_eq!(embedder.model_id(), "embed-english-v3.0");
        assert_eq!(embedder.dimensions(), 1024);
    }
}
