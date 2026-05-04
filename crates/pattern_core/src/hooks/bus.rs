//! HookBus: glob-based event dispatcher.
//!
//! Subscribers register with a `HookFilter` (compiled glob) and receive
//! matching events. Blocking events await responses; notifications are
//! fire-and-forget.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use super::event::{HookEvent, HookResponse, HookSemantics};
use super::filter::HookFilter;

/// Unique identifier for a subscription.
pub type SubscriptionId = u64;

/// Capacity for subscriber channels.
const CHANNEL_CAPACITY: usize = 64;

/// A blocking delivery: event + reply channel.
#[derive(Debug)]
pub struct BlockingDelivery {
    /// The event being delivered.
    pub event: HookEvent,
    /// Reply channel. Subscriber sends a `HookResponse` to let the
    /// emitter know whether to continue, block, or modify.
    pub reply: oneshot::Sender<HookResponse>,
}

/// The hook event bus.
///
/// Subscribers register with a glob filter. Events are dispatched to all
/// matching subscribers in registration order. Notification events are
/// fire-and-forget; blocking events await responses with a timeout.
#[derive(Debug, Clone)]
pub struct HookBus {
    inner: Arc<RwLock<BusInner>>,
    blocking_timeout: Duration,
}

#[derive(Debug, Default)]
struct BusInner {
    next_id: SubscriptionId,
    subs: Vec<Subscription>,
}

#[derive(Debug)]
struct Subscription {
    id: SubscriptionId,
    filter: HookFilter,
    sender: SubscriberSender,
}

#[derive(Debug)]
enum SubscriberSender {
    Blocking {
        tx: mpsc::Sender<BlockingDelivery>,
    },
    Notification {
        tx: mpsc::Sender<HookEvent>,
    },
}

impl Default for HookBus {
    fn default() -> Self {
        Self::new()
    }
}

impl HookBus {
    /// Create a bus with default 5-second blocking timeout.
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(5))
    }

    /// Create a bus with a custom blocking timeout.
    pub fn with_timeout(blocking_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BusInner::default())),
            blocking_timeout,
        }
    }

    /// Subscribe to blocking events matching the filter.
    /// Returns the subscription ID and a receiver for blocking deliveries.
    pub fn subscribe_blocking(
        &self,
        filter: HookFilter,
    ) -> (SubscriptionId, mpsc::Receiver<BlockingDelivery>) {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut inner = self.inner.write();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.subs.push(Subscription {
            id,
            filter,
            sender: SubscriberSender::Blocking { tx },
        });
        (id, rx)
    }

    /// Subscribe to notification events matching the filter.
    pub fn subscribe_notifications(
        &self,
        filter: HookFilter,
    ) -> (SubscriptionId, mpsc::Receiver<HookEvent>) {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut inner = self.inner.write();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.subs.push(Subscription {
            id,
            filter,
            sender: SubscriberSender::Notification { tx },
        });
        (id, rx)
    }

    /// Remove a subscription by ID. Returns true if found.
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut inner = self.inner.write();
        let len_before = inner.subs.len();
        inner.subs.retain(|s| s.id != id);
        inner.subs.len() < len_before
    }

    /// Emit a notification event. Fire-and-forget to all matching subscribers.
    pub fn emit(&self, event: HookEvent) {
        let inner = self.inner.read();
        for sub in &inner.subs {
            if !sub.filter.matches(&event.tag) {
                continue;
            }
            if let SubscriberSender::Notification { tx } = &sub.sender {
                if tx.try_send(event.clone()).is_err() {
                    debug!(
                        sub_id = sub.id,
                        tag = %event.tag,
                        "notification subscriber drop (buffer full or closed)"
                    );
                }
            }
        }
    }

    /// Emit a blocking event. Awaits responses from all matching subscribers.
    ///
    /// Returns the first `Block` response if any subscriber blocks;
    /// the last `Modify` if any modifies; otherwise `Continue`.
    pub async fn emit_blocking(&self, event: HookEvent) -> HookResponse {
        // Snapshot the matching subscribers under a short read lock.
        let subs_snapshot: Vec<(SubscriptionId, mpsc::Sender<BlockingDelivery>)> = {
            let inner = self.inner.read();
            inner
                .subs
                .iter()
                .filter_map(|s| match &s.sender {
                    SubscriberSender::Blocking { tx } if s.filter.matches(&event.tag) => {
                        Some((s.id, tx.clone()))
                    }
                    _ => None,
                })
                .collect()
        };

        let mut response = HookResponse::Continue;

        for (sub_id, tx) in subs_snapshot {
            let (reply_tx, reply_rx) = oneshot::channel();
            let delivery = BlockingDelivery {
                event: event.clone(),
                reply: reply_tx,
            };

            // Send the delivery.
            if tx.send(delivery).await.is_err() {
                debug!(sub_id, tag = %event.tag, "blocking subscriber closed");
                continue;
            }

            // Await the response with timeout.
            match tokio::time::timeout(self.blocking_timeout, reply_rx).await {
                Ok(Ok(resp)) => match resp {
                    HookResponse::Block { .. } => return resp,
                    HookResponse::Modify(_) => response = resp,
                    HookResponse::Continue => {}
                },
                Ok(Err(_)) => {
                    warn!(sub_id, tag = %event.tag, "blocking subscriber dropped reply channel");
                }
                Err(_) => {
                    warn!(
                        sub_id,
                        tag = %event.tag,
                        timeout_ms = self.blocking_timeout.as_millis() as u64,
                        "blocking subscriber timed out"
                    );
                }
            }
        }

        response
    }

    /// Number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.inner.read().subs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::filter::HookFilter;

    #[tokio::test]
    async fn notification_delivered_to_matching_subscriber() {
        let bus = HookBus::new();
        let filter = HookFilter::new("test.*").unwrap();
        let (_id, mut rx) = bus.subscribe_notifications(filter);

        bus.emit(HookEvent::notification("test.hello", serde_json::json!({})));

        let event = rx.try_recv().expect("should receive event");
        assert_eq!(event.tag.as_str(), "test.hello");
    }

    #[tokio::test]
    async fn notification_not_delivered_to_non_matching() {
        let bus = HookBus::new();
        let filter = HookFilter::new("other.*").unwrap();
        let (_id, mut rx) = bus.subscribe_notifications(filter);

        bus.emit(HookEvent::notification("test.hello", serde_json::json!({})));

        assert!(rx.try_recv().is_err(), "should not receive non-matching event");
    }

    #[tokio::test]
    async fn blocking_event_receives_continue() {
        let bus = HookBus::new();
        let filter = HookFilter::new("gate.*").unwrap();
        let (_id, mut rx) = bus.subscribe_blocking(filter);

        // Spawn a subscriber that replies Continue.
        tokio::spawn(async move {
            if let Some(delivery) = rx.recv().await {
                let _ = delivery.reply.send(HookResponse::Continue);
            }
        });

        let resp = bus
            .emit_blocking(HookEvent::blocking("gate.test", serde_json::json!({})))
            .await;
        assert!(matches!(resp, HookResponse::Continue));
    }

    #[tokio::test]
    async fn blocking_event_block_stops_processing() {
        let bus = HookBus::new();
        let filter = HookFilter::new("gate.*").unwrap();
        let (_id, mut rx) = bus.subscribe_blocking(filter);

        tokio::spawn(async move {
            if let Some(delivery) = rx.recv().await {
                let _ = delivery.reply.send(HookResponse::Block {
                    reason: "denied".into(),
                });
            }
        });

        let resp = bus
            .emit_blocking(HookEvent::blocking("gate.test", serde_json::json!({})))
            .await;
        assert!(matches!(resp, HookResponse::Block { .. }));
    }

    #[tokio::test]
    async fn unsubscribe_removes_subscription() {
        let bus = HookBus::new();
        let filter = HookFilter::new("**").unwrap();
        let (id, _rx) = bus.subscribe_notifications(filter);
        assert_eq!(bus.subscription_count(), 1);
        assert!(bus.unsubscribe(id));
        assert_eq!(bus.subscription_count(), 0);
    }
}
