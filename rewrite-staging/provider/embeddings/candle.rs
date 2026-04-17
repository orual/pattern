// MOVING TO: pattern_provider/src/embeddings/candle.rs
// ORIGIN: crates/pattern_core/src/embeddings/candle.rs
// PHASE: future
// RESHAPE: Deferred until provider-side embedding use case confirmed
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Candle-based local embedding provider

use super::{Embedding, EmbeddingError, EmbeddingProvider, Result, validate_input};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "embed-candle")]
use {
    candle_core::{Device, Module, Tensor},
    candle_nn::VarBuilder,
    candle_transformers::models::bert::{BertModel, Config as BertConfig},
    candle_transformers::models::jina_bert::{BertModel as JinaBertModel, Config as JinaConfig},
    hf_hub::{Repo, RepoType},
    tokenizers::{PaddingParams, Tokenizer},
};

/// Candle-based embedding provider
pub struct CandleEmbedder {
    model: String,
    dimensions: usize,
    #[cfg(feature = "embed-candle")]
    model_type: ModelType,
    #[cfg(feature = "embed-candle")]
    tokenizer: Arc<Mutex<Tokenizer>>,
    #[cfg(feature = "embed-candle")]
    device: Device,
}

#[cfg(feature = "embed-candle")]
enum ModelType {
    Bert(Arc<Mutex<BertModel>>),
    JinaBert(Arc<Mutex<JinaBertModel>>),
}

impl CandleEmbedder {
    /// Create a new Candle embedder
    pub async fn new(model: &str, cache_dir: Option<String>) -> Result<Self> {
        #[cfg(not(feature = "embed-candle"))]
        {
            let _ = (model, cache_dir);
            return Err(EmbeddingError::GenerationFailed(
                "Candle feature not enabled".into(),
            ));
        }

        #[cfg(feature = "embed-candle")]
        {
            let (dimensions, is_jina) = match model {
                "BAAI/bge-small-en-v1.5" => (384, false),
                "BAAI/bge-base-en-v1.5" => (768, false),
                "BAAI/bge-large-en-v1.5" => (1024, false),
                "jinaai/jina-embeddings-v2-small-en" => (512, true),
                "jinaai/jina-embeddings-v2-base-en" => (768, true),
                _ => return Err(EmbeddingError::ModelNotFound(model.to_string())),
            };

            // Setup device (CPU or CUDA if available)
            let device = Device::cuda_if_available(0)
                .or_else(|_| Device::new_metal(0))
                .unwrap_or(Device::Cpu);

            // Download model files
            // Create API builder
            let mut api_builder = hf_hub::api::tokio::ApiBuilder::new();

            // Set custom cache directory if provided
            if let Some(cache_dir) = cache_dir {
                api_builder = api_builder.with_cache_dir(cache_dir.into());
            }

            let api = api_builder
                .build()
                .map_err(|e| EmbeddingError::GenerationFailed(e.into()))?;

            let repo = api.repo(Repo::new(model.to_string(), RepoType::Model));

            // Download model files
            let config_path = repo.get("config.json").await.map_err(|e| {
                EmbeddingError::ModelNotFound(format!("Failed to download config: {}", e))
            })?;

            let tokenizer_path = repo.get("tokenizer.json").await.map_err(|e| {
                EmbeddingError::ModelNotFound(format!("Failed to download tokenizer: {}", e))
            })?;

            let weights_path = repo.get("pytorch_model.bin").await.map_err(|e| {
                EmbeddingError::ModelNotFound(format!("Failed to download weights: {}", e))
            })?;

            // Load config
            let config_str = std::fs::read_to_string(&config_path)
                .map_err(|e| EmbeddingError::GenerationFailed(e.into()))?;

            // Load tokenizer
            let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
                .map_err(|e| EmbeddingError::GenerationFailed(e.into()))?;

            // Setup padding
            tokenizer.with_padding(Some(PaddingParams {
                strategy: tokenizers::PaddingStrategy::BatchLongest,
                ..Default::default()
            }));

            // Load model weights
            let vb = VarBuilder::from_pth(&weights_path, candle_core::DType::F32, &device)
                .map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to load weights: {}", e).into(),
                    )
                })?;

            let model_type = if is_jina {
                let config: JinaConfig = serde_json::from_str(&config_str)
                    .map_err(|e| EmbeddingError::GenerationFailed(e.into()))?;
                let jina_model = JinaBertModel::new(vb, &config).map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to load Jina model: {}", e).into(),
                    )
                })?;
                ModelType::JinaBert(Arc::new(Mutex::new(jina_model)))
            } else {
                let config: BertConfig = serde_json::from_str(&config_str)
                    .map_err(|e| EmbeddingError::GenerationFailed(e.into()))?;
                let bert_model = BertModel::load(vb, &config).map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to load BERT model: {}", e).into(),
                    )
                })?;
                ModelType::Bert(Arc::new(Mutex::new(bert_model)))
            };

            Ok(Self {
                model: model.to_string(),
                dimensions,
                model_type,
                tokenizer: Arc::new(Mutex::new(tokenizer)),
                device,
            })
        }
    }
}

impl std::fmt::Debug for CandleEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandleEmbedder")
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

#[cfg(feature = "embed-candle")]
#[async_trait]
impl EmbeddingProvider for CandleEmbedder {
    async fn embed(&self, text: &str) -> Result<Embedding> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }

        #[cfg(not(feature = "embed-candle"))]
        {
            let _ = text;
            return Err(EmbeddingError::GenerationFailed(
                "Candle feature not enabled".into(),
            ));
        }

        #[cfg(feature = "embed-candle")]
        {
            let embeddings = self.embed_batch(&[text.to_string()]).await?;
            embeddings
                .into_iter()
                .next()
                .ok_or_else(|| EmbeddingError::GenerationFailed("No embedding returned".into()))
        }
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        validate_input(texts)?;

        #[cfg(not(feature = "embed-candle"))]
        {
            let _ = texts;
            return Err(EmbeddingError::GenerationFailed(
                "Candle feature not enabled".into(),
            ));
        }

        #[cfg(feature = "embed-candle")]
        {
            // Tokenize all texts
            let tokenizer = self.tokenizer.lock().await;
            let encodings = tokenizer.encode_batch(texts.to_vec(), true).map_err(|e| {
                EmbeddingError::GenerationFailed(format!("Tokenization failed: {}", e).into())
            })?;

            let mut all_embeddings = Vec::with_capacity(texts.len());

            // Process in batches to avoid OOM on large inputs
            const BATCH_SIZE: usize = 32;
            for batch_encodings in encodings.chunks(BATCH_SIZE) {
                // Convert to tensors
                let input_ids: Vec<Vec<u32>> = batch_encodings
                    .iter()
                    .map(|enc| enc.get_ids().to_vec())
                    .collect();

                let max_len = input_ids.iter().map(|ids| ids.len()).max().unwrap_or(0);

                // Pad sequences
                let padded_ids: Vec<u32> = input_ids
                    .iter()
                    .flat_map(|ids| {
                        let mut padded = ids.clone();
                        padded.resize(max_len, 0); // 0 is typically [PAD] token
                        padded
                    })
                    .collect();

                let input_tensor =
                    Tensor::from_vec(padded_ids, &[batch_encodings.len(), max_len], &self.device)
                        .map_err(|e| {
                        EmbeddingError::GenerationFailed(
                            format!("Failed to create input tensor: {}", e).into(),
                        )
                    })?;

                // Create attention mask
                let attention_mask: Vec<f32> = input_ids
                    .iter()
                    .flat_map(|ids| {
                        let mut mask = vec![1.0; ids.len()];
                        mask.resize(max_len, 0.0);
                        mask
                    })
                    .collect();

                let mask_tensor = Tensor::from_vec(
                    attention_mask,
                    &[batch_encodings.len(), max_len],
                    &self.device,
                )
                .map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to create attention mask: {}", e).into(),
                    )
                })?;

                // Forward pass
                let embeddings = match &self.model_type {
                    ModelType::Bert(bert_model) => {
                        let model = bert_model.lock().await;
                        model
                            .forward(&input_tensor, &mask_tensor, None)
                            .map_err(|e| {
                                EmbeddingError::GenerationFailed(
                                    format!("BERT forward pass failed: {}", e).into(),
                                )
                            })?
                    }
                    ModelType::JinaBert(jina_model) => {
                        let model = jina_model.lock().await;
                        model.forward(&input_tensor).map_err(|e| {
                            EmbeddingError::GenerationFailed(
                                format!("Jina forward pass failed: {}", e).into(),
                            )
                        })?
                    }
                };

                // Mean pooling over sequence dimension
                let embeddings_sum = embeddings.sum(1).map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to sum embeddings: {}", e).into(),
                    )
                })?;

                let mask_sum = mask_tensor.sum(1).map_err(|e| {
                    EmbeddingError::GenerationFailed(format!("Failed to sum mask: {}", e).into())
                })?;

                // Unsqueeze mask_sum to match embeddings dimensions [batch, 1]
                let mask_sum = mask_sum.unsqueeze(1).map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to unsqueeze mask sum: {}", e).into(),
                    )
                })?;

                let pooled = embeddings_sum.broadcast_div(&mask_sum).map_err(|e| {
                    EmbeddingError::GenerationFailed(
                        format!("Failed to pool embeddings: {}", e).into(),
                    )
                })?;

                // Convert to Vec<f32>
                let pooled_vec: Vec<f32> = pooled
                    .flatten_all()
                    .map_err(|e| {
                        EmbeddingError::GenerationFailed(
                            format!("Failed to flatten tensor: {}", e).into(),
                        )
                    })?
                    .to_vec1()
                    .map_err(|e| {
                        EmbeddingError::GenerationFailed(
                            format!("Failed to convert tensor to vec: {}", e).into(),
                        )
                    })?;

                // Split back into individual embeddings
                for i in 0..batch_encodings.len() {
                    let start = i * self.dimensions;
                    let end = start + self.dimensions;
                    let embedding_vec = pooled_vec[start..end].to_vec();
                    all_embeddings.push(Embedding::new(embedding_vec, self.model.clone()));
                }
            }

            Ok(all_embeddings)
        }
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
