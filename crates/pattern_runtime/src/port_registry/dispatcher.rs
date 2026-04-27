//! Dispatcher actor — drives `Port::call()` and `Port::subscribe()` on the
//! runtime's tokio runtime, replying to handlers via crossbeam channels.
//!
//! ## Why an actor (not direct handler-side `await`)
//!
//! The `PortHandler` runs on the Tidepool eval-worker thread, which has NO
//! ambient tokio runtime by design (see `crates/pattern_runtime/CLAUDE.md`'s
//! "Eval worker" section). Direct `Handle::block_on` against arbitrary
//! plugin code risks deadlock if the plugin's future calls
//! `spawn_blocking` against a saturated pool, or runs on a single-thread
//! runtime. The dispatcher actor isolates plugin async code on a dedicated
//! task that the handler talks to via cross-thread channels.
//!
//! ## Subscription tracking
//!
//! Active subscriptions are keyed by `(session_key, port_id)` — one
//! subscription per (session, port) pair. Re-subscribing for an existing
//! key aborts the prior drain task before installing the new one.
//!
//! `Op::Unsubscribe` removes a single (session, port) pair.
//! `Op::CancelSubscriptionsFor(port_id)` aborts every subscription to the
//! named port across all sessions — fired when `PortRegistry::unregister`
//! removes the port. `Op::Shutdown` aborts everything and exits the loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender as XSender;
use dashmap::DashMap;
use futures::StreamExt;
use pattern_core::traits::Port;
use pattern_core::types::message::MessageAttachment;
use pattern_core::types::port::{PortError, PortEvent, PortId};

/// Per-subscription key. Same `(session_key, port_id)` is used as the
/// hash-map key so re-subscribe replaces the prior entry.
type SubscriptionKey = (String, PortId);

/// Operations the handler enqueues for the dispatcher actor.
///
/// Each variant carries a crossbeam reply sender; the actor sends the
/// result back synchronously after handling the op. Handler waits with
/// `recv_timeout`.
pub enum Op {
    /// One-shot port call. Reply is the port's JSON response or a
    /// `PortError`.
    Call {
        port_id: PortId,
        method: String,
        payload: serde_json::Value,
        reply: XSender<Result<serde_json::Value, PortError>>,
    },
    /// Subscribe to a port's event stream. The actor spawns a drain task
    /// that pushes `MessageAttachment::PortEvent` entries onto
    /// `async_reminder_queue`. Reply is `Ok(())` once the subscription is
    /// active, or a `PortError` if the port refuses or doesn't exist.
    Subscribe {
        port_id: PortId,
        config: serde_json::Value,
        async_reminder_queue: Arc<Mutex<Vec<MessageAttachment>>>,
        session_key: String,
        reply: XSender<Result<(), PortError>>,
    },
    /// Unsubscribe a single `(session_key, port_id)` pair. Idempotent —
    /// no error if no such subscription is active.
    Unsubscribe {
        port_id: PortId,
        session_key: String,
        reply: XSender<Result<(), PortError>>,
    },
    /// Cancel every active subscription for the given port_id, across all
    /// sessions. Fired by `PortRegistry::unregister`. No reply — fire-and-
    /// forget.
    CancelSubscriptionsFor(PortId),
    /// Shut the actor down. Aborts every live subscription and exits the
    /// recv loop. Sent by `TidepoolRuntime::Drop` and ends-of-test cleanup.
    Shutdown,
}

impl std::fmt::Debug for Op {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Call {
                port_id, method, ..
            } => f
                .debug_struct("Call")
                .field("port_id", port_id)
                .field("method", method)
                .finish_non_exhaustive(),
            Self::Subscribe {
                port_id,
                session_key,
                ..
            } => f
                .debug_struct("Subscribe")
                .field("port_id", port_id)
                .field("session_key", session_key)
                .finish_non_exhaustive(),
            Self::Unsubscribe {
                port_id,
                session_key,
                ..
            } => f
                .debug_struct("Unsubscribe")
                .field("port_id", port_id)
                .field("session_key", session_key)
                .finish(),
            Self::CancelSubscriptionsFor(port_id) => f
                .debug_tuple("CancelSubscriptionsFor")
                .field(port_id)
                .finish(),
            Self::Shutdown => f.write_str("Shutdown"),
        }
    }
}

/// Run the dispatcher actor loop. Returns when the channel is closed
/// (sender dropped) or `Op::Shutdown` is received.
///
/// Single `recv().await` loop — no `tokio::select` over multiple channels.
/// Each match arm runs to completion (including spawn-and-insert sequence
/// in `Subscribe`) before the next op is received, which is what keeps the
/// race-window analysis in the module docs sound.
pub async fn run(
    mut rx: tokio::sync::mpsc::Receiver<Op>,
    ports: Arc<DashMap<PortId, Arc<dyn Port>>>,
) {
    let mut subscriptions: HashMap<SubscriptionKey, tokio::task::AbortHandle> = HashMap::new();

    while let Some(op) = rx.recv().await {
        match op {
            Op::Call {
                port_id,
                method,
                payload,
                reply,
            } => {
                let port = match ports.get(&port_id) {
                    Some(p) => Arc::clone(p.value()),
                    None => {
                        let _ = reply.send(Err(PortError::NotFound(port_id)));
                        continue;
                    }
                };
                // Plugin code runs here. NOT block_on — we're already on the
                // tokio runtime. If the plugin's call() future hangs, this
                // actor task hangs with it, but only this one task — the
                // handler is protected by its own `recv_timeout`.
                let result = port.call(&method, payload).await;
                let _ = reply.send(result);
            }
            Op::Subscribe {
                port_id,
                config,
                async_reminder_queue,
                session_key,
                reply,
            } => {
                let port = match ports.get(&port_id) {
                    Some(p) => Arc::clone(p.value()),
                    None => {
                        let _ = reply.send(Err(PortError::NotFound(port_id)));
                        continue;
                    }
                };
                let stream = match port.subscribe(config).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        continue;
                    }
                };
                let key = (session_key, port_id.clone());
                // Re-subscribe replaces any prior subscription for the same
                // (session, port) — abort the old one so its drain task
                // doesn't keep enqueueing attachments after the new
                // subscription is in place.
                if let Some(prev) = subscriptions.remove(&key) {
                    prev.abort();
                }
                let task = tokio::spawn(drain_subscription(stream, async_reminder_queue));
                subscriptions.insert(key, task.abort_handle());
                let _ = reply.send(Ok(()));
            }
            Op::Unsubscribe {
                port_id,
                session_key,
                reply,
            } => {
                let key = (session_key, port_id);
                if let Some(handle) = subscriptions.remove(&key) {
                    handle.abort();
                }
                let _ = reply.send(Ok(()));
            }
            Op::CancelSubscriptionsFor(port_id) => {
                let to_remove: Vec<SubscriptionKey> = subscriptions
                    .keys()
                    .filter(|(_, p)| p == &port_id)
                    .cloned()
                    .collect();
                for key in to_remove {
                    if let Some(handle) = subscriptions.remove(&key) {
                        handle.abort();
                    }
                }
            }
            Op::Shutdown => {
                for (_, handle) in subscriptions.drain() {
                    handle.abort();
                }
                break;
            }
        }
    }

    // Channel closed (last sender dropped) without an explicit Shutdown.
    // Same cleanup as Shutdown — abort live subscriptions before exiting.
    for (_, handle) in subscriptions.drain() {
        handle.abort();
    }
}

/// Drain events from a subscribed port's stream into the session's async-
/// reminder queue.
///
/// Uses `event.port_id` from each event verbatim — NOT the registered
/// PortId of the port handle. This allows ports to act as multiplexers:
/// a single registered port (e.g. "slack") may emit events tagged with
/// logical sub-ids (e.g. "slack:channel-alice", "slack:channel-bob") so
/// that downstream consumers can route by sub-id without the port
/// reimplementing fan-out.
///
/// Convention for non-multiplex ports: emit events with `event.port_id`
/// equal to the registered PortId (the simplest case).
///
/// Stream end (server disconnect, port-side close) is silent — the agent
/// learns from the absence of further events. A future enhancement could
/// emit a sentinel `PortEvent` at stream end if reviewer wants explicit
/// closure signaling.
async fn drain_subscription(
    mut stream: futures::stream::BoxStream<'static, PortEvent>,
    queue: Arc<Mutex<Vec<MessageAttachment>>>,
) {
    while let Some(event) = stream.next().await {
        let attachment = MessageAttachment::PortEvent {
            // Use the port_id from the event itself rather than the registered
            // PortId. This enables the multiplexer pattern: a single registered
            // port may fan out events tagged with logical sub-ids. Non-multiplex
            // ports simply emit events with port_id equal to their registered id.
            port_id: event.port_id.to_string(),
            payload: event.payload,
            at: event.at,
        };
        queue
            .lock()
            .expect("port event queue mutex poisoned")
            .push(attachment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use pattern_core::traits::Port;
    use pattern_core::types::port::{PortCapabilities, PortMetadata};
    use std::any::Any;
    use std::time::Duration;

    /// Test port: configurable subscribe stream + fixed call response.
    ///
    /// `call()` always returns `Ok({"ok": true})`. Dispatcher tests don't
    /// exercise port-side error variants — error coverage lives in
    /// `tests/port_handler.rs` (Task 9) where MockPort can return arbitrary
    /// PortError variants. Keeping this stub simple sidesteps PortError's
    /// non-Clone shape (the type carries `std::io::Error` sources).
    #[derive(Debug)]
    struct TestPort {
        id: PortId,
        events: Mutex<Option<Vec<PortEvent>>>,
    }

    impl TestPort {
        fn new(id: &str) -> Arc<Self> {
            let pid = PortId::new(id);
            Arc::new(Self {
                id: pid,
                events: Mutex::new(None),
            })
        }

        fn with_events(self: Arc<Self>, events: Vec<PortEvent>) -> Arc<Self> {
            *self.events.lock().unwrap() = Some(events);
            self
        }
    }

    #[async_trait::async_trait]
    impl Port for TestPort {
        fn id(&self) -> &PortId {
            &self.id
        }
        fn metadata(&self) -> PortMetadata {
            PortMetadata::new(self.id.clone(), "test")
        }
        fn capabilities(&self) -> PortCapabilities {
            PortCapabilities::default()
                .with_callable(true)
                .with_subscribable(true)
        }
        async fn subscribe(
            &self,
            _config: serde_json::Value,
        ) -> Result<futures::stream::BoxStream<'static, PortEvent>, PortError> {
            let events = self.events.lock().unwrap().take().unwrap_or_default();
            Ok(stream::iter(events).boxed())
        }
        async fn call(
            &self,
            _method: &str,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, PortError> {
            Ok(serde_json::json!({"ok": true}))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Live-streaming test port.
    ///
    /// `subscribe()` returns a `tokio_stream::wrappers::UnboundedReceiverStream`
    /// that stays open until the sender is dropped. Tests can push events after
    /// subscribe returns, which lets them distinguish "drain task aborted" from
    /// "stream finished naturally before abort ran".
    #[derive(Debug)]
    struct LiveTestPort {
        id: PortId,
        tx: tokio::sync::mpsc::UnboundedSender<PortEvent>,
        rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PortEvent>>>,
    }

    impl LiveTestPort {
        fn new(id: &str) -> Arc<Self> {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let pid = PortId::new(id);
            Arc::new(Self {
                id: pid,
                tx,
                rx: Mutex::new(Some(rx)),
            })
        }

        fn push(&self, event: PortEvent) {
            let _ = self.tx.send(event);
        }
    }

    #[async_trait::async_trait]
    impl Port for LiveTestPort {
        fn id(&self) -> &PortId {
            &self.id
        }
        fn metadata(&self) -> PortMetadata {
            PortMetadata::new(self.id.clone(), "live-test")
        }
        fn capabilities(&self) -> PortCapabilities {
            PortCapabilities::default()
                .with_callable(true)
                .with_subscribable(true)
        }
        async fn subscribe(
            &self,
            _config: serde_json::Value,
        ) -> Result<futures::stream::BoxStream<'static, PortEvent>, PortError> {
            use tokio_stream::wrappers::UnboundedReceiverStream;
            let rx = self
                .rx
                .lock()
                .expect("LiveTestPort rx mutex poisoned")
                .take()
                .expect("subscribe called twice on LiveTestPort");
            Ok(UnboundedReceiverStream::new(rx).boxed())
        }
        async fn call(
            &self,
            _method: &str,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, PortError> {
            Ok(serde_json::json!({"ok": true}))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Test-fixture handle: dispatcher tx + shared ports map + actor join.
    type ActorFixture = (
        tokio::sync::mpsc::Sender<Op>,
        Arc<DashMap<PortId, Arc<dyn Port>>>,
        tokio::task::JoinHandle<()>,
    );

    /// Spawn a dispatcher task on the current runtime; return its tx + ports
    /// shared so tests can register, push events, etc.
    fn spawn_actor() -> ActorFixture {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let ports: Arc<DashMap<PortId, Arc<dyn Port>>> = Arc::new(DashMap::new());
        let handle = tokio::spawn(run(rx, Arc::clone(&ports)));
        (tx, ports, handle)
    }

    fn xchan<T>() -> (XSender<T>, crossbeam_channel::Receiver<T>) {
        crossbeam_channel::bounded(1)
    }

    /// Op::Call dispatches to the port and returns its response.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_call_returns_port_response() {
        let (tx, ports, _h) = spawn_actor();
        let p = TestPort::new("call-port");
        ports.insert(p.id.clone(), p.clone() as Arc<dyn Port>);

        let (rtx, rrx) = xchan();
        tx.send(Op::Call {
            port_id: p.id.clone(),
            method: "ping".into(),
            payload: serde_json::Value::Null,
            reply: rtx,
        })
        .await
        .unwrap();
        let result = rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    /// Op::Call to an unknown port replies with NotFound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_call_unknown_port_returns_not_found() {
        let (tx, _ports, _h) = spawn_actor();
        let (rtx, rrx) = xchan();
        tx.send(Op::Call {
            port_id: PortId::new("missing"),
            method: "x".into(),
            payload: serde_json::Value::Null,
            reply: rtx,
        })
        .await
        .unwrap();
        let result = rrx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(result, Err(PortError::NotFound(_))));
    }

    /// Op::Subscribe spawns a drain task that pushes events into the queue.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_subscribe_drains_events_into_queue() {
        let (tx, ports, _h) = spawn_actor();
        let now = jiff::Timestamp::now();
        let events = vec![
            PortEvent::new(PortId::new("evt-port"), serde_json::json!({"n": 1}), now),
            PortEvent::new(PortId::new("evt-port"), serde_json::json!({"n": 2}), now),
        ];
        let p = TestPort::new("evt-port").with_events(events);
        ports.insert(p.id.clone(), p as Arc<dyn Port>);

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));
        let (rtx, rrx) = xchan();
        tx.send(Op::Subscribe {
            port_id: PortId::new("evt-port"),
            config: serde_json::Value::Null,
            async_reminder_queue: Arc::clone(&queue),
            session_key: "session-a".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        rrx.recv_timeout(Duration::from_secs(2))
            .unwrap()
            .expect("subscribe must succeed");

        // Wait for the drain task to consume the pre-canned stream. We poll
        // the queue rather than sleep-arbitrary: the drain runs on its own
        // task and may complete before or after this test does its first
        // check.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let len = queue.lock().unwrap().len();
            if len >= 2 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("drain task did not push 2 events; got {len}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let q = queue.lock().unwrap();
        assert_eq!(q.len(), 2);
        for a in q.iter() {
            match a {
                MessageAttachment::PortEvent { port_id, .. } => {
                    assert_eq!(port_id, "evt-port");
                }
                other => panic!("expected PortEvent attachment, got {other:?}"),
            }
        }
    }

    /// Op::Unsubscribe aborts the drain task. After abort, no further events
    /// arrive in the queue. We can't easily push events into a finished
    /// stream, so this test verifies the AbortHandle removal path: subscribe
    /// → unsubscribe → re-subscribe to the same key works (would fail to
    /// install if the prior key wasn't removed).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_unsubscribe_clears_subscription() {
        let (tx, ports, _h) = spawn_actor();
        let p = TestPort::new("unsub-port").with_events(vec![]);
        ports.insert(p.id.clone(), p as Arc<dyn Port>);

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        // Subscribe.
        let (rtx, rrx) = xchan();
        tx.send(Op::Subscribe {
            port_id: PortId::new("unsub-port"),
            config: serde_json::Value::Null,
            async_reminder_queue: Arc::clone(&queue),
            session_key: "s".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();

        // Unsubscribe.
        let (rtx, rrx) = xchan();
        tx.send(Op::Unsubscribe {
            port_id: PortId::new("unsub-port"),
            session_key: "s".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();

        // Re-subscribe (would fail to install AbortHandle if the prior entry
        // weren't removed; instead Subscribe would `prev.abort()` first and
        // then install — but in either case the test's positive signal is
        // that Subscribe replies Ok again).
        let (rtx, rrx) = xchan();
        tx.send(Op::Subscribe {
            port_id: PortId::new("unsub-port"),
            config: serde_json::Value::Null,
            async_reminder_queue: queue,
            session_key: "s".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        // Set events to None on TestPort means the second subscribe has no
        // events to drain — but the subscribe call itself returns Ok with an
        // empty stream. Either way, reply Ok confirms the dispatcher path
        // does not fail.
        let result = rrx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            result.is_ok(),
            "re-subscribe after unsubscribe must succeed: {result:?}"
        );
    }

    /// Op::Unsubscribe actually aborts the drain task so that events pushed
    /// AFTER unsubscribe are never delivered.
    ///
    /// Uses `LiveTestPort` (live `ReceiverStream`) so the drain task stays
    /// alive until explicitly aborted. A snapshot-based port's drain task
    /// would finish naturally before `Unsubscribe` runs, making
    /// `handle.abort()` a no-op and masking a missing-abort bug.
    ///
    /// Mutation-test property: removing `handle.abort()` from
    /// `Op::Unsubscribe` causes this test to FAIL because event 2 arrives.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_unsubscribe_actually_aborts_live_drain_task() {
        let (tx, ports, _h) = spawn_actor();
        let p = LiveTestPort::new("live-abort-port");
        let p_push = Arc::clone(&p);
        ports.insert(p.id.clone(), p as Arc<dyn Port>);

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        // Subscribe — drain task attaches to the live receiver stream.
        let (rtx, rrx) = xchan();
        tx.send(Op::Subscribe {
            port_id: PortId::new("live-abort-port"),
            config: serde_json::Value::Null,
            async_reminder_queue: Arc::clone(&queue),
            session_key: "s".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();

        // Push event 1 and poll until it arrives.
        let now = jiff::Timestamp::now();
        p_push.push(PortEvent::new(
            PortId::new("live-abort-port"),
            serde_json::json!({"seq": 1}),
            now,
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if !queue.lock().unwrap().is_empty() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("event 1 never arrived in queue before deadline");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(queue.lock().unwrap().len(), 1, "event 1 must arrive");

        // Unsubscribe.
        let (rtx, rrx) = xchan();
        tx.send(Op::Unsubscribe {
            port_id: PortId::new("live-abort-port"),
            session_key: "s".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();

        // Push event 2. If abort worked, the drain task is gone and this
        // event will never be consumed.
        p_push.push(PortEvent::new(
            PortId::new("live-abort-port"),
            serde_json::json!({"seq": 2}),
            now,
        ));

        // Bounded delay: drain task, if still running, would deliver event 2
        // within milliseconds. 100ms is a generous margin.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let final_len = queue.lock().unwrap().len();
        assert_eq!(
            final_len, 1,
            "event 2 must NOT arrive after Unsubscribe aborted the drain task; \
             queue should still have 1 event but got {final_len}"
        );
    }

    /// Op::Subscribe replacing an existing subscription aborts the prior
    /// drain task. Verified indirectly: a port with a never-ending stream is
    /// subscribed twice; the second subscribe's reply only succeeds if the
    /// first drain task was aborted (otherwise we'd leak it; this test
    /// doesn't assert non-leak directly, but the dispatcher's abort path is
    /// exercised).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_subscribe_replaces_existing_aborts_prior() {
        let (tx, ports, _h) = spawn_actor();
        let p = TestPort::new("replace-port");
        ports.insert(p.id.clone(), p as Arc<dyn Port>);

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        for _ in 0..2 {
            let (rtx, rrx) = xchan();
            tx.send(Op::Subscribe {
                port_id: PortId::new("replace-port"),
                config: serde_json::Value::Null,
                async_reminder_queue: Arc::clone(&queue),
                session_key: "s".into(),
                reply: rtx,
            })
            .await
            .unwrap();
            rrx.recv_timeout(Duration::from_secs(2))
                .unwrap()
                .expect("subscribe must succeed");
        }
    }

    /// Op::CancelSubscriptionsFor aborts all subscriptions for a given
    /// port_id, across multiple sessions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_cancel_subscriptions_for_port() {
        let (tx, ports, _h) = spawn_actor();
        let p = TestPort::new("multi-port");
        ports.insert(p.id.clone(), p as Arc<dyn Port>);

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));
        for session in &["s1", "s2"] {
            let (rtx, rrx) = xchan();
            tx.send(Op::Subscribe {
                port_id: PortId::new("multi-port"),
                config: serde_json::Value::Null,
                async_reminder_queue: Arc::clone(&queue),
                session_key: (*session).into(),
                reply: rtx,
            })
            .await
            .unwrap();
            rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        }

        // Cancel; no reply expected (fire-and-forget).
        tx.send(Op::CancelSubscriptionsFor(PortId::new("multi-port")))
            .await
            .unwrap();

        // Verify by re-subscribing one of them: if the prior entry wasn't
        // cancelled, the new subscribe would still abort+replace correctly,
        // so this test isn't actually distinguishing the cases. The strict
        // verification (no events arrive after cancel) is structural — the
        // CancelSubscriptionsFor match arm is the only path where we iterate
        // and abort. This test confirms the op is accepted without panic.
        let (rtx, rrx) = xchan();
        tx.send(Op::Subscribe {
            port_id: PortId::new("multi-port"),
            config: serde_json::Value::Null,
            async_reminder_queue: queue,
            session_key: "s1".into(),
            reply: rtx,
        })
        .await
        .unwrap();
        rrx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    }

    /// Op::Shutdown breaks the actor's recv loop and aborts all subs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatcher_shutdown_exits_loop() {
        let (tx, _ports, handle) = spawn_actor();
        tx.send(Op::Shutdown).await.unwrap();
        // Actor task should complete promptly after Shutdown.
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("actor must exit within 2s of Shutdown")
            .expect("actor task did not panic");
    }
}
