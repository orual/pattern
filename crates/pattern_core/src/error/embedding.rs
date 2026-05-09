//! Embedding errors.
//!
//! This file defines errors that occur when generating or comparing
//! embedding vectors. Surfaced through [`super::core::CoreError::Embedding`].
//!
//! Extracted from the pre-v3 staging module; only the variants that do not
//! depend on staged concrete-provider types are kept here. The pre-v3
//! `GenerationFailed`, `ModelNotFound`, and `ApiError` variants are
//! provider-specific and will re-emerge in Phase 4 alongside the concrete
//! backends.

use miette::Diagnostic;
use thiserror::Error;

/// Errors from embedding generation and vector comparison.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum EmbeddingError {
    /// Two embeddings had mismatched dimensions (usually from different
    /// models being mixed at the same comparison site).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::EmbeddingError;
    ///
    /// let err = EmbeddingError::DimensionMismatch { expected: 768, actual: 512 };
    /// assert!(err.to_string().contains("768"));
    /// ```
    #[error("invalid dimensions: expected {expected}, got {actual}")]
    #[diagnostic(
        code(pattern_core::embedding::dimension_mismatch),
        help("all embeddings must use the same model to ensure consistent dimensions")
    )]
    DimensionMismatch {
        /// Expected vector length.
        expected: usize,
        /// Actual vector length observed.
        actual: usize,
    },

    /// A batch-embed call exceeded the provider's supported batch size.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::EmbeddingError;
    ///
    /// let err = EmbeddingError::BatchSizeTooLarge { size: 1024, max: 256 };
    /// assert!(err.to_string().contains("1024"));
    /// ```
    #[error("batch size too large: {size} (max: {max})")]
    #[diagnostic(
        code(pattern_core::embedding::batch_too_large),
        help("split the batch and retry; the provider caps batches at {max}")
    )]
    BatchSizeTooLarge {
        /// Requested batch size.
        size: usize,
        /// Maximum batch size supported.
        max: usize,
    },

    /// The caller supplied an empty input batch.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::EmbeddingError;
    ///
    /// let err = EmbeddingError::EmptyInput;
    /// assert!(err.to_string().contains("empty"));
    /// ```
    #[error("empty input provided")]
    #[diagnostic(
        code(pattern_core::embedding::empty_input),
        help("provide at least one non-empty text to embed")
    )]
    EmptyInput,

    /// The embedding model could not be loaded.
    #[error("model load failed: {path}")]
    #[diagnostic(
        code(pattern_core::embedding::model_load),
        help("check that the model file exists and is a valid GGUF")
    )]
    ModelLoad {
        path: std::path::PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Backend initialization failed (Vulkan, CUDA, etc.).
    #[error("backend init failed")]
    #[diagnostic(
        code(pattern_core::embedding::backend_init),
        help("check GPU drivers and backend availability")
    )]
    BackendInit(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Tokenization of the input text failed.
    #[error("tokenization failed")]
    #[diagnostic(
        code(pattern_core::embedding::tokenization),
        help("input may contain characters unsupported by the model's tokenizer")
    )]
    Tokenization(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The model's decode/inference step failed.
    #[error("inference failed")]
    #[diagnostic(
        code(pattern_core::embedding::inference),
        help("input may exceed context window, or GPU ran out of memory")
    )]
    Inference(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The input text was empty or otherwise unsuitable.
    #[error("invalid input: empty after tokenization")]
    #[diagnostic(
        code(pattern_core::embedding::empty_input_after_tokenize),
        help("provide non-empty text within the model's context window")
    )]
    EmptyAfterTokenize,

    /// A blocking task (spawn_blocking) was cancelled or panicked.
    #[error("async task failed")]
    #[diagnostic(
        code(pattern_core::embedding::task_failed),
        help("the embedding computation was cancelled or panicked")
    )]
    TaskFailed(#[source] tokio::task::JoinError),
}
