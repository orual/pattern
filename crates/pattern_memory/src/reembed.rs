// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Async re-embed queue bridging sync subscriber workers to async embedding
//! providers.
//!
//! The re-embed queue is a tokio task that consumes [`ReembedRequest`]s from an
//! unbounded channel. Each request triggers an async embedding computation via
//! [`EmbeddingProvider::embed_query`] and persists the result to the vector
//! index in the constellation database.
//!
//! The `UnboundedSender` is callable from sync contexts (OS threads), making it
//! the bridge between the sync subscriber workers and the async embedding world.

use std::sync::Arc;

use pattern_core::traits::EmbeddingProvider;
use pattern_db::ConstellationDb;
use tokio::sync::mpsc;

use crate::subscriber::event::ReembedRequest;

/// A running re-embed queue. Dropping this struct does NOT cancel the
/// background task — use the returned [`tokio::task::JoinHandle`] or
/// drop the sender side to signal shutdown.
pub struct ReembedQueue {
    _handle: tokio::task::JoinHandle<()>,
}

impl ReembedQueue {
    /// Spawn the re-embed queue as a tokio task.
    ///
    /// Returns the queue (which holds the task handle) and a sender that
    /// subscriber workers use to submit re-embed requests.
    ///
    /// If `provider` is `None`, the queue drains requests without computing
    /// embeddings (useful for tests and configurations without vector search).
    pub fn spawn(
        provider: Option<Arc<dyn EmbeddingProvider>>,
        db: Arc<ConstellationDb>,
    ) -> (Self, mpsc::UnboundedSender<ReembedRequest>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<ReembedRequest>();

        let handle = tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let Some(ref provider) = provider else {
                    // No embedding provider — silently drain.
                    continue;
                };

                let content = match String::from_utf8(req.canonical_bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            block_id = %req.block_id,
                            "re-embed request had non-UTF-8 content: {e}"
                        );
                        continue;
                    }
                };

                // Compute embedding via async provider.
                let embedding = match provider.embed_query(&content).await {
                    Ok(emb) => emb,
                    Err(e) => {
                        metrics::counter!("memory.reembed.failed").increment(1);
                        tracing::warn!(
                            block_id = %req.block_id,
                            error = %e,
                            "embedding computation failed"
                        );
                        continue;
                    }
                };

                // Persist to vector index via spawn_blocking (rusqlite is sync).
                let db = db.clone();
                let block_id = req.block_id.clone();
                let req_content_type = req.content_type;
                let content_hash_hex = blake3::Hash::from(req.content_hash).to_hex().to_string();
                let store_result = tokio::task::spawn_blocking(move || {
                    let conn = db.get()?;
                    pattern_db::vector::update_embedding(
                        &conn,
                        req_content_type,
                        &block_id,
                        &embedding,
                        None,
                        Some(&content_hash_hex),
                    )
                })
                .await;

                match store_result {
                    Ok(Ok(rowid)) => {
                        tracing::debug!(block_id = %req.block_id, "{:?} reembeded: {rowid}", req.content_type);
                        metrics::counter!("memory.reembed.success").increment(1);
                    }
                    Ok(Err(e)) => {
                        metrics::counter!("memory.reembed.store_failed").increment(1);
                        tracing::warn!(
                            block_id = %req.block_id,
                            error = %e,
                            "embedding store failed"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            block_id = %req.block_id,
                            error = %e,
                            "spawn_blocking for embedding store panicked"
                        );
                    }
                }
            }
        });

        (Self { _handle: handle }, tx)
    }
}
