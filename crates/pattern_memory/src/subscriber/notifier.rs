//! `BlockChangeNotifier` — callback fan-out when a block's content
//! changes.
//!
//! Each [`MemoryCache`](crate::cache::MemoryCache) holds one notifier,
//! shared with every subscriber worker spawned for the cache's blocks
//! via the worker's
//! [`WorkerConfig`](crate::subscriber::worker::WorkerConfig). After a
//! worker successfully renders a commit-event batch (i.e. real data
//! changed, not an echo), it calls [`BlockChangeNotifier::fire`] with
//! the block's id; any callbacks subscribed to that id are invoked
//! synchronously on the worker thread.
//!
//! The intended downstream consumer is `pattern_runtime::wake`'s
//! `BlockChanged` and `TaskDependencyResolved` evaluators (Phase 4
//! T8/T9), which subscribe a callback that pushes a
//! `MailboxInput::Wake { reason: ... }` onto a session's mailbox.
//! The notifier is kept transport-agnostic (just `Fn(&BlockRef)`) so
//! future consumers (e.g. an observability log subscriber) can hook
//! the same fan-out without coupling to the wake machinery.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use pattern_core::types::block_ref::BlockRef;

/// Callback fired when a block's content changes.
///
/// Invoked synchronously on the subscriber worker thread, so
/// callbacks should be cheap (a channel send is the canonical
/// shape). Heavy work belongs on a tokio task that owns the
/// channel's receiving end.
pub type BlockChangeCallback = Arc<dyn Fn(&BlockRef) + Send + Sync>;

#[derive(Default)]
struct NotifierInner {
    /// Registered callbacks keyed by block id (the canonical
    /// "this block" identifier — same string the worker sees as
    /// `block_id`). Each entry pairs a monotonic subscription id
    /// (for unsubscribe) with the callback.
    callbacks: DashMap<String, Vec<(u64, BlockChangeCallback)>>,
    next_id: AtomicU64,
}

/// Fan-out registry for block-change callbacks.
///
/// Cheap to clone — internally an `Arc<NotifierInner>`. The
/// subscriber worker holds a clone for `fire`; consumers (wake
/// evaluators) hold a clone for `subscribe`.
#[derive(Clone, Default)]
pub struct BlockChangeNotifier {
    inner: Arc<NotifierInner>,
}

impl BlockChangeNotifier {
    /// Construct a fresh notifier with no subscribers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to changes on `block_id`. Returns a guard that
    /// unsubscribes on drop. Multiple subscribers on the same
    /// block are allowed and fire in registration order.
    pub fn subscribe(&self, block_id: &str, callback: BlockChangeCallback) -> Subscription {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .callbacks
            .entry(block_id.to_string())
            .or_default()
            .push((id, callback));
        Subscription {
            inner: self.inner.clone(),
            block_id: block_id.to_string(),
            id,
        }
    }

    /// Fire callbacks registered for `block_id`. Called by the
    /// subscriber worker after a successful render. Callbacks fire
    /// synchronously; tolerate cheap work only.
    pub fn fire(&self, block_id: &str, block_ref: &BlockRef) {
        if let Some(callbacks) = self.inner.callbacks.get(block_id) {
            for (_, cb) in callbacks.iter() {
                cb(block_ref);
            }
        }
    }

    /// Number of subscribers currently registered for `block_id`.
    /// For tests + observability.
    pub fn subscriber_count(&self, block_id: &str) -> usize {
        self.inner
            .callbacks
            .get(block_id)
            .map(|cbs| cbs.len())
            .unwrap_or(0)
    }
}

impl std::fmt::Debug for BlockChangeNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockChangeNotifier")
            .field("blocks_with_subscribers", &self.inner.callbacks.len())
            .finish_non_exhaustive()
    }
}

/// RAII subscription guard. Dropping unsubscribes the callback.
pub struct Subscription {
    inner: Arc<NotifierInner>,
    block_id: String,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(mut entries) = self.inner.callbacks.get_mut(&self.block_id) {
            entries.retain(|(id, _)| *id != self.id);
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("block_id", &self.block_id)
            .field("id", &self.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn br(label: &str) -> BlockRef {
        BlockRef::new(label, "test-block-id")
    }

    #[test]
    fn fire_invokes_subscribed_callbacks_in_order() {
        let notifier = BlockChangeNotifier::new();
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let _g1 = notifier.subscribe("block-1", {
            let log = log.clone();
            Arc::new(move |bref| log.lock().unwrap().push(format!("a:{}", bref.label)))
        });
        let _g2 = notifier.subscribe("block-1", {
            let log = log.clone();
            Arc::new(move |bref| log.lock().unwrap().push(format!("b:{}", bref.label)))
        });

        notifier.fire("block-1", &br("notes"));
        let entries = log.lock().unwrap().clone();
        assert_eq!(entries, vec!["a:notes", "b:notes"]);
    }

    #[test]
    fn fire_skips_unsubscribed_blocks() {
        let notifier = BlockChangeNotifier::new();
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let _g = notifier.subscribe("block-1", {
            let log = log.clone();
            Arc::new(move |bref| log.lock().unwrap().push(bref.label.clone()))
        });

        notifier.fire("block-2", &br("other"));
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn drop_subscription_removes_callback() {
        let notifier = BlockChangeNotifier::new();
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let g = notifier.subscribe("block-1", {
            let log = log.clone();
            Arc::new(move |bref| log.lock().unwrap().push(bref.label.clone()))
        });
        assert_eq!(notifier.subscriber_count("block-1"), 1);

        drop(g);
        assert_eq!(notifier.subscriber_count("block-1"), 0);
        notifier.fire("block-1", &br("notes"));
        assert!(log.lock().unwrap().is_empty());
    }
}
