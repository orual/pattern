//! Pending batch management for grouping posts by thread.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use jacquard::common::types::string::AtUri;

use super::firehose::FirehosePost;

/// Pending batch of posts being collected
#[derive(Debug, Default)]
pub(super) struct PendingBatch {
    /// Posts grouped by thread root URI
    posts_by_thread: DashMap<AtUri<'static>, Vec<FirehosePost>>,
    /// When each batch started collecting
    batch_timers: DashMap<AtUri<'static>, Instant>,
    /// URIs we've already sent notifications for
    processed_uris: DashMap<AtUri<'static>, Instant>,
}

impl PendingBatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a post to the appropriate thread batch
    pub fn add_post(&self, post: FirehosePost) {
        let thread_root = post.thread_root();

        self.batch_timers
            .entry(thread_root.clone())
            .or_insert_with(Instant::now);

        self.posts_by_thread
            .entry(thread_root)
            .or_default()
            .push(post);
    }

    /// Get expired batches (past the batch window)
    pub fn get_expired_batches(&self, batch_window: Duration) -> Vec<AtUri<'static>> {
        let now = Instant::now();
        self.batch_timers
            .iter()
            .filter_map(|entry| {
                if now.duration_since(*entry.value()) >= batch_window {
                    Some(entry.key().clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Flush a batch, returning its posts
    pub fn flush_batch(&self, thread_root: &AtUri<'static>) -> Option<Vec<FirehosePost>> {
        self.batch_timers.remove(thread_root);
        self.posts_by_thread.remove(thread_root).map(|(_, v)| v)
    }

    /// Mark a URI as processed
    pub fn mark_processed(&self, uri: &AtUri<'static>) {
        self.processed_uris.insert(uri.clone(), Instant::now());
    }

    /// Clean up old processed entries
    pub fn cleanup_old_processed(&self, older_than: Duration) {
        self.processed_uris.retain(|_, t| t.elapsed() < older_than);
    }
}
