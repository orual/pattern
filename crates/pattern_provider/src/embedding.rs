// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Local embedding provider backed by llama.cpp via the `llama-cpp-4` crate.
//!
//! Implements [`EmbeddingProvider`] from `pattern_core` using a locally loaded
//! GGUF embedding model (e.g. EmbeddingGemma 300M) with Vulkan GPU acceleration.
//!
//! The model is loaded once at construction time and held in memory. Embedding
//! calls are serialized via a Mutex to avoid concurrent GPU access.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::LlamaModel;
use llama_cpp_4::model::AddBos;

use pattern_core::error::embedding::EmbeddingError;
use pattern_core::traits::EmbeddingProvider;
use pattern_core::types::embedding::{Embedding, EmbeddingResult};

/// Configuration for the local llama.cpp embedding provider.
#[derive(Debug, Clone)]
pub struct LlamaEmbeddingConfig {
    /// Path to the GGUF model file.
    pub model_path: PathBuf,
    /// Number of GPU layers to offload (0 = CPU only, 999 = all).
    pub n_gpu_layers: u32,
    /// Context size for the embedding model.
    pub context_size: u32,
    /// Number of threads for batch processing.
    pub n_threads: i32,
}

impl Default for LlamaEmbeddingConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::new(),
            n_gpu_layers: 999,
            context_size: 2048,
            n_threads: 4i32,
        }
    }
}

/// Local embedding provider using llama.cpp with Vulkan acceleration.
///
/// Holds a loaded model and backend in Arc. A Mutex serializes embedding
/// calls to avoid concurrent GPU access. Each embed call creates a
/// context (cheap — ~1ms vs ~500ms for model load), uses it, and drops it.
pub struct LlamaEmbeddingProvider {
    model: Arc<LlamaModel>,
    backend: Arc<LlamaBackend>,
    /// Serializes access to the GPU — only one embed at a time.
    gpu_lock: Arc<Mutex<()>>,
    config: LlamaEmbeddingConfig,
    dimensions: usize,
}

impl std::fmt::Debug for LlamaEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlamaEmbeddingProvider")
            .field("config", &self.config)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

impl LlamaEmbeddingProvider {
    /// Create a new provider by loading the model from disk.
    ///
    /// This is expensive (loads model into GPU memory) — call once at startup.
    pub fn new(config: LlamaEmbeddingConfig) -> Result<Self, EmbeddingError> {
        let mut backend = LlamaBackend::init()
            .map_err(|e| EmbeddingError::BackendInit(Box::new(e)))?;
        backend.void_logs();

        let model_params = LlamaModelParams::default()
            .with_n_gpu_layers(config.n_gpu_layers);

        let model = LlamaModel::load_from_file(&backend, &config.model_path, &model_params)
            .map_err(|e| EmbeddingError::ModelLoad {
                path: config.model_path.clone(),
                source: Box::new(e),
            })?;

        let dimensions = model.n_embd() as usize;

        tracing::info!(
            model_path = ?config.model_path,
            dimensions,
            n_gpu_layers = config.n_gpu_layers,
            "llama embedding provider initialized",
        );

        Ok(Self {
            model: Arc::new(model),
            backend: Arc::new(backend),
            gpu_lock: Arc::new(Mutex::new(())),
            config,
            dimensions,
        })
    }

    /// Embed a single text synchronously (call from blocking context).
    fn embed_sync(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let tokens = self.model
            .str_to_token(text, AddBos::Always)
            .map_err(|e| EmbeddingError::Tokenization(Box::new(e)))?;

        if tokens.is_empty() {
            return Err(EmbeddingError::EmptyAfterTokenize);
        }

        // Chunk tokens into context-sized windows with 10% overlap.
        let max_tokens = self.config.context_size as usize;
        let overlap = max_tokens / 10;
        let stride = max_tokens - overlap;

        let chunks: Vec<&[llama_cpp_4::token::LlamaToken]> = if tokens.len() <= max_tokens {
            vec![&tokens]
        } else {
            let mut c = Vec::new();
            let mut start = 0;
            while start < tokens.len() {
                let end = (start + max_tokens).min(tokens.len());
                c.push(&tokens[start..end]);
                if end == tokens.len() { break; }
                start += stride;
            }
            tracing::debug!(n_chunks = c.len(), total_tokens = tokens.len(), "splitting content for embedding");
            c
        };

        // Serialize GPU access
        let _guard = self.gpu_lock.lock().map_err(|e| {
            EmbeddingError::Inference(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("gpu mutex poisoned: {e}"),
            )))
        })?;

        let n_threads: i32 = std::thread::available_parallelism()
            .map(|p| p.get() as i32)
            .unwrap_or(self.config.n_threads);

        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());

        for chunk in &chunks {
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(std::num::NonZeroU32::new(self.config.context_size))
                .with_n_batch(self.config.context_size)
                .with_n_ubatch(self.config.context_size)
                .with_n_threads(n_threads)
                .with_n_threads_batch(n_threads)
                .with_embeddings(true);

            let mut ctx = self.model
                .new_context(&self.backend, ctx_params)
                .map_err(|e| {
                    tracing::error!(step = "context_create", error = %e, "embedding context creation failed");
                    EmbeddingError::Inference(Box::new(e))
                })?;

            let mut batch = LlamaBatch::new(self.config.context_size as usize, 1);
            let seq_id = 0;
            let last_idx = chunk.len() - 1;

            for (i, &token) in chunk.iter().enumerate() {
                let output = i == last_idx;
                batch.add(token, i as i32, &[seq_id], output)
                    .map_err(|e| {
                        tracing::error!(step = "batch_add", token_idx = i, error = %e, "batch add failed");
                        EmbeddingError::Inference(Box::new(e))
                    })?;
            }

            ctx.decode(&mut batch)
                .map_err(|e| {
                    tracing::error!(step = "decode", n_tokens = chunk.len(), error = %e, "decode failed");
                    EmbeddingError::Inference(Box::new(e))
                })?;

            let embedding = ctx.embeddings_seq_ith(seq_id)
                .map_err(|e| {
                    tracing::error!(step = "extract_embedding", seq_id, error = %e, "embedding extraction failed");
                    EmbeddingError::Inference(Box::new(e))
                })?;

            // L2 normalize each chunk embedding
            let mut vec = embedding.to_vec();
            let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for val in &mut vec {
                    *val /= norm;
                }
            }

            all_embeddings.push(vec);
        }

        // Average chunk embeddings and re-normalize
        let dim = self.dimensions;
        let mut avg = vec![0.0f32; dim];
        for emb in &all_embeddings {
            for (i, &v) in emb.iter().enumerate() {
                avg[i] += v;
            }
        }
        let n = all_embeddings.len() as f32;
        for val in &mut avg {
            *val /= n;
        }
        let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut avg {
                *val /= norm;
            }
        }

        Ok(avg)
    }
}

#[async_trait]
impl EmbeddingProvider for LlamaEmbeddingProvider {
    async fn embed(&self, text: &str) -> EmbeddingResult<Embedding> {
        let text = text.to_string();
        let model = Arc::clone(&self.model);
        let backend = Arc::clone(&self.backend);
        let gpu_lock = Arc::clone(&self.gpu_lock);
        let config = self.config.clone();
        let dimensions = self.dimensions;

        let vec = tokio::task::spawn_blocking(move || {
            // Reconstruct a view with the cloned Arcs
            let provider = LlamaEmbeddingProvider {
                model,
                backend,
                gpu_lock,
                config,
                dimensions,
            };
            provider.embed_sync(&text)
        })
        .await
        .map_err(EmbeddingError::TaskFailed)??
        ;

        Ok(Embedding::new(vec, self.model_id().to_string()))
    }

    async fn embed_batch(&self, texts: &[String]) -> EmbeddingResult<Vec<Embedding>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn model_id(&self) -> &str {
        "embeddinggemma-300m-qat-q8_0"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
