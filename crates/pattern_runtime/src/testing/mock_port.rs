//! `MockPort` — a configurable `Port` implementation for integration tests.
//!
//! Used by `tests/port_handler.rs` (Task 9) to cover AC4.2–AC4.9 without
//! bringing in a real external service. Tests configure call responses,
//! push events into the subscription stream, and optionally provide a
//! Haskell library snippet.
//!
//! # Design
//!
//! - `call()` returns a configurable `serde_json::Value` (default `null`).
//!   Tests can swap the response mid-test via `set_call_response`.
//! - **Snapshot mode** (`MockPort::new`): `subscribe()` drains all
//!   currently-queued events from the channel into a `Vec`, then wraps
//!   them as a finite `stream::iter` boxed stream. Events pushed *after*
//!   `subscribe()` returns are NOT delivered — the stream is a snapshot.
//!   Use this for tests that don't need live delivery after subscribe.
//! - **Live mode** (`MockPort::new_live`): `subscribe()` wraps the raw
//!   `tokio::sync::mpsc::UnboundedReceiver` as a live
//!   `tokio_stream::wrappers::UnboundedReceiverStream`. Events pushed via
//!   `push_event_live()` after `subscribe()` returns ARE delivered
//!   in-order to the drain task. Use this for tests that must verify
//!   abort/unsubscribe behaviour (finite streams end naturally before
//!   `Unsubscribe` runs, making abort a no-op — live streams expose the
//!   real distinction).
//! - `library()` returns the `library_src` field — `None` by default;
//!   `Some(&'static str)` when set at construction time.

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use pattern_core::traits::port::Port;
use pattern_core::types::port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};

/// Subscribe mode for a `MockPort`.
///
/// - `Snapshot`: `subscribe()` drains all queued events into a finite
///   `stream::iter`. Events pushed after `subscribe()` returns are not
///   delivered in the current subscription.
/// - `Live`: `subscribe()` hands the raw `UnboundedReceiver` to a
///   `tokio_stream::wrappers::UnboundedReceiverStream`. Events pushed via
///   `push_event_live()` after `subscribe()` returns ARE delivered in
///   order. Use this when the test must distinguish "abort actually
///   stopped delivery" from "stream finished naturally".
#[derive(Debug)]
enum SubscribeMode {
    Snapshot {
        /// Drained by the first `subscribe()` call.
        rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PortEvent>>>,
    },
    Live {
        /// Handed to `UnboundedReceiverStream` by the first `subscribe()` call.
        rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PortEvent>>>,
    },
}

/// A configurable test double for the `Port` trait.
///
/// Construct via [`MockPort::new`] (snapshot mode) or [`MockPort::new_live`]
/// (live-streaming mode). Use builder methods to set the call response and
/// attach a library snippet.
#[derive(Debug)]
pub struct MockPort {
    id: PortId,
    metadata: PortMetadata,
    capabilities: PortCapabilities,
    /// Response returned by `call()`. Guarded by `Mutex` so tests can swap
    /// it between calls.
    call_response: Mutex<Result<serde_json::Value, String>>,
    /// Optional Haskell library source. `&'static str` because preamble
    /// splicing expects that lifetime (see `Port::library()` contract).
    library_src: Option<&'static str>,
    /// Events sender — used by both snapshot and live modes.
    event_tx: tokio::sync::mpsc::UnboundedSender<PortEvent>,
    /// Subscribe mode: controls how `subscribe()` wraps the receiver.
    mode: SubscribeMode,
}

impl MockPort {
    /// Construct a new `MockPort` in snapshot mode.
    ///
    /// `subscribe()` drains all currently-queued events into a finite
    /// `stream::iter`. Events pushed after `subscribe()` returns are not
    /// delivered in the current subscription. Defaults: callable and
    /// subscribable, call returns `null`, no library.
    pub fn new(id: &str) -> Arc<Self> {
        let port_id = PortId::new(id);
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            id: port_id.clone(),
            metadata: PortMetadata::new(port_id, "MockPort for testing"),
            capabilities: PortCapabilities::default()
                .with_callable(true)
                .with_subscribable(true),
            call_response: Mutex::new(Ok(serde_json::Value::Null)),
            library_src: None,
            event_tx,
            mode: SubscribeMode::Snapshot {
                rx: Mutex::new(Some(event_rx)),
            },
        })
    }

    /// Construct a `MockPort` in live-streaming mode.
    ///
    /// `subscribe()` wraps the channel receiver as a
    /// `tokio_stream::wrappers::UnboundedReceiverStream`. Events pushed via
    /// `push_event_live()` after `subscribe()` returns ARE delivered
    /// in order to the active drain task. Use this when tests must verify
    /// that `Unsubscribe` actually aborts the drain task rather than just
    /// waiting for the stream to finish naturally (finite streams end
    /// before unsubscribe runs — live streams keep the drain task alive).
    pub fn new_live(id: &str) -> Arc<Self> {
        let port_id = PortId::new(id);
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            id: port_id.clone(),
            metadata: PortMetadata::new(port_id, "MockPort (live) for testing"),
            capabilities: PortCapabilities::default()
                .with_callable(true)
                .with_subscribable(true),
            call_response: Mutex::new(Ok(serde_json::Value::Null)),
            library_src: None,
            event_tx,
            mode: SubscribeMode::Live {
                rx: Mutex::new(Some(event_rx)),
            },
        })
    }

    /// Construct a `MockPort` with a custom description (snapshot mode).
    pub fn new_with_desc(id: &str, description: &str) -> Arc<Self> {
        let port_id = PortId::new(id);
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            id: port_id.clone(),
            metadata: PortMetadata::new(port_id, description),
            capabilities: PortCapabilities::default()
                .with_callable(true)
                .with_subscribable(true),
            call_response: Mutex::new(Ok(serde_json::Value::Null)),
            library_src: None,
            event_tx,
            mode: SubscribeMode::Snapshot {
                rx: Mutex::new(Some(event_rx)),
            },
        })
    }

    /// Construct a `MockPort` with a static Haskell library snippet (AC4.6/4.9).
    pub fn new_with_library(id: &str, library_src: &'static str) -> Arc<Self> {
        let port_id = PortId::new(id);
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            id: port_id.clone(),
            metadata: PortMetadata::new(port_id, "MockPort with library"),
            capabilities: PortCapabilities::default()
                .with_callable(true)
                .with_subscribable(true),
            call_response: Mutex::new(Ok(serde_json::Value::Null)),
            library_src: Some(library_src),
            event_tx,
            mode: SubscribeMode::Snapshot {
                rx: Mutex::new(Some(event_rx)),
            },
        })
    }

    /// Set the value that `call()` will return on the next call.
    ///
    /// Pass `Ok(value)` for a successful response or `Err(msg)` to
    /// simulate a `PortError::CallFailed`.
    pub fn set_call_response(&self, response: Result<serde_json::Value, String>) {
        *self
            .call_response
            .lock()
            .expect("call_response mutex poisoned") = response;
    }

    /// Push an event into the subscription channel.
    ///
    /// In **snapshot mode**: the event will be delivered to the next
    /// subscriber (events pushed before `subscribe()` returns are
    /// collected into the finite snapshot).
    ///
    /// In **live mode**: the event is delivered to the active
    /// `UnboundedReceiverStream` immediately. Prefer `push_event_live` in live
    /// mode for clarity; this method works equivalently.
    pub fn push_event(&self, event: PortEvent) {
        // If the receiver is gone (subscribe already consumed it and the
        // stream was dropped), the send silently fails — fine for tests.
        let _ = self.event_tx.send(event);
    }

    /// Push an event on a live-mode port after `subscribe()` has returned.
    ///
    /// This is the primary entry point for AC4.5-style tests that verify
    /// `Unsubscribe` aborts delivery: push an event, assert it arrives,
    /// unsubscribe, push another event, assert it does NOT arrive.
    ///
    /// Calling this on a snapshot-mode port works but is misleading —
    /// events pushed after `subscribe()` on a snapshot port won't be
    /// delivered (the snapshot was already taken). Use `push_event` for
    /// snapshot-mode ports.
    pub fn push_event_live(&self, event: PortEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Drain all queued events into a Vec (for testing the queue state
    /// without subscribing). Only usable before `subscribe()` is called
    /// (snapshot mode only; live mode has no meaningful pre-subscribe drain).
    pub fn drain_queued_events(&self) -> Vec<PortEvent> {
        match &self.mode {
            SubscribeMode::Snapshot { rx } | SubscribeMode::Live { rx } => {
                let mut guard = rx.lock().expect("event_rx mutex poisoned");
                if let Some(rx) = guard.as_mut() {
                    let mut events = Vec::new();
                    // Non-blocking drain: collect whatever is already in the channel.
                    while let Ok(e) = rx.try_recv() {
                        events.push(e);
                    }
                    events
                } else {
                    Vec::new()
                }
            }
        }
    }
}

#[async_trait]
impl Port for MockPort {
    fn id(&self) -> &PortId {
        &self.id
    }

    fn metadata(&self) -> PortMetadata {
        self.metadata.clone()
    }

    fn capabilities(&self) -> PortCapabilities {
        self.capabilities.clone()
    }

    /// Returns a stream of events.
    ///
    /// **Snapshot mode**: drains all currently-queued events into a
    /// finite `stream::iter`. The stream ends when those events are
    /// exhausted. Events pushed after this call returns are NOT delivered.
    ///
    /// **Live mode**: hands the `UnboundedReceiver` to a
    /// `tokio_stream::wrappers::UnboundedReceiverStream`. The stream remains open
    /// until the `MockPort` is dropped or `Unsubscribe` aborts the drain
    /// task. Events pushed via `push_event_live()` after this call
    /// returns ARE delivered in order.
    async fn subscribe(
        &self,
        _config: serde_json::Value,
    ) -> Result<futures::stream::BoxStream<'static, PortEvent>, PortError> {
        match &self.mode {
            SubscribeMode::Snapshot { rx } => {
                let mut rx_guard = rx.lock().expect("event_rx mutex poisoned");
                let events: Vec<PortEvent> = if let Some(rx) = rx_guard.as_mut() {
                    // Drain all currently-pending events. Since this is async
                    // context (the actor task awaits subscribe), we use
                    // `try_recv` in a loop rather than blocking.
                    let mut collected = Vec::new();
                    while let Ok(e) = rx.try_recv() {
                        collected.push(e);
                    }
                    collected
                } else {
                    Vec::new()
                };
                Ok(futures::stream::iter(events).boxed())
            }
            SubscribeMode::Live { rx } => {
                let mut rx_guard = rx.lock().expect("event_rx mutex poisoned");
                // Take the receiver out — only one subscription at a time.
                // If subscribe() is called twice on a live port, the second
                // call gets an empty stream (None → stream::empty). Tests
                // that re-subscribe on the same live port should create a
                // new MockPort::new_live().
                let receiver = rx_guard.take();
                match receiver {
                    Some(r) => {
                        use tokio_stream::wrappers::UnboundedReceiverStream;
                        Ok(UnboundedReceiverStream::new(r).boxed())
                    }
                    None => Ok(futures::stream::empty().boxed()),
                }
            }
        }
    }

    /// Returns the configured call response.
    async fn call(
        &self,
        _method: &str,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, PortError> {
        let guard = self
            .call_response
            .lock()
            .expect("call_response mutex poisoned");
        match &*guard {
            Ok(val) => Ok(val.clone()),
            Err(msg) => Err(PortError::CallFailed(self.id.clone(), msg.clone())),
        }
    }

    /// Returns the optional Haskell library source (AC4.6/4.9).
    fn library(&self) -> Option<smol_str::SmolStr> {
        self.library_src.map(smol_str::SmolStr::new_static)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_port_call_returns_configured_response() {
        let port = MockPort::new("test");
        port.set_call_response(Ok(serde_json::json!({"ok": true})));
        let result = port.call("ping", serde_json::Value::Null).await.unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn mock_port_call_failure_returns_call_failed() {
        let port = MockPort::new("test");
        port.set_call_response(Err("boom".into()));
        let err = port
            .call("ping", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PortError::CallFailed(_, _)),
            "expected CallFailed, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn mock_port_subscribe_delivers_queued_events() {
        let port = MockPort::new("evt");
        let now = jiff::Timestamp::now();
        port.push_event(PortEvent::new(
            PortId::new("evt"),
            serde_json::json!(1),
            now,
        ));
        port.push_event(PortEvent::new(
            PortId::new("evt"),
            serde_json::json!(2),
            now,
        ));

        let mut stream = port
            .subscribe(serde_json::Value::Null)
            .await
            .expect("subscribe ok");
        let e1 = stream.next().await.expect("first event");
        let e2 = stream.next().await.expect("second event");
        assert!(
            stream.next().await.is_none(),
            "stream must end after queue drains"
        );

        assert_eq!(e1.payload, serde_json::json!(1));
        assert_eq!(e2.payload, serde_json::json!(2));
    }

    #[test]
    fn mock_port_library_returns_source() {
        let port = MockPort::new_with_library("mock", "module Mock where mockFn = pure ()\n");
        assert_eq!(
            port.library(),
            Some(smol_str::SmolStr::new_static(
                "module Mock where mockFn = pure ()\n"
            ))
        );
    }

    #[test]
    fn mock_port_no_library_returns_none() {
        let port = MockPort::new("nolibrary");
        assert!(port.library().is_none());
    }
}
