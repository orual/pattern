// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Embedding-provider trait.
//!
//! An [`EmbeddingProvider`] turns text into dense embedding vectors. The
//! trait is extracted from the pre-v3 `pattern_core::embeddings::mod` file
//! (now staged to `rewrite-staging/provider/embeddings/`); concrete
//! backends (Candle, OpenAI, Cohere, Ollama, Gemini) remain staged pending
//! the Phase 4 provider-crate rebase, but the trait itself lives here so
//! `pattern_core` code — especially the memory cache's hybrid search —
//! can name it without an out-of-tree dep.

use async_trait::async_trait;

use crate::types::embedding::{Embedding, EmbeddingResult};

/// Trait for text-to-vector embedding providers.
///
/// Default method implementations for `max_batch_size`, `health_check`,
/// `embed_query`, and `model_name` are carried over unchanged from the
/// pre-v3 trait definition so existing consumers need not adapt.
///
/// # Example
///
/// ```no_run
/// use async_trait::async_trait;
/// use pattern_core::traits::EmbeddingProvider;
/// use pattern_core::types::embedding::{Embedding, EmbeddingResult};
///
/// #[derive(Debug)]
/// struct Dummy;
///
/// #[async_trait]
/// impl EmbeddingProvider for Dummy {
///     async fn embed(&self, _t: &str) -> EmbeddingResult<Embedding> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn embed_batch(&self, _t: &[String]) -> EmbeddingResult<Vec<Embedding>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn model_id(&self) -> &str { "dummy" }
///     fn dimensions(&self) -> usize { 0 }
/// }
/// ```
#[async_trait]
pub trait EmbeddingProvider: Send + Sync + std::fmt::Debug {
    /// Generate an embedding for a single text.
    async fn embed(&self, text: &str) -> EmbeddingResult<Embedding>;

    /// Generate embeddings for multiple texts.
    async fn embed_batch(&self, texts: &[String]) -> EmbeddingResult<Vec<Embedding>>;

    /// Get the model identifier.
    fn model_id(&self) -> &str;

    /// Get the embedding dimensions.
    fn dimensions(&self) -> usize;

    /// Get the maximum batch size supported. Default: 256.
    fn max_batch_size(&self) -> usize {
        256
    }

    /// Check if the provider is available/healthy. Default: always OK.
    async fn health_check(&self) -> EmbeddingResult<()> {
        Ok(())
    }

    /// Convenience method for embedding a single query (alias for `embed`).
    async fn embed_query(&self, query: &str) -> EmbeddingResult<Vec<f32>> {
        Ok(self.embed(query).await?.vector)
    }

    /// Get the model name (alias for `model_id`).
    fn model_name(&self) -> &str {
        self.model_id()
    }
}
