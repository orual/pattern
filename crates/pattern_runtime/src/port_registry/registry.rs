//! `PortRegistryImpl` — concrete `PortRegistry` impl for `pattern_runtime`.
//!
//! Backed by a `DashMap<PortId, Arc<dyn Port>>` for atomic CRUD plus a tokio
//! mpsc handle to the [`dispatcher`](super::dispatcher) actor. The
//! dispatcher does the actual async work for `Call` / `Subscribe` /
//! `Unsubscribe`; the registry only owns the storage and the dispatcher
//! channel.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use pattern_core::traits::{Port, PortRegistry};
use pattern_core::types::port::{PortError, PortId, PortMetadata};

use super::dispatcher;

/// Bound on the dispatcher Op channel. Generous because Ops are small and the
/// actor processes them quickly; if the channel ever fills the eval-worker
/// blocks on `blocking_send` — observable diagnostic, not a silent stall.
pub(crate) const DISPATCHER_CHANNEL_BOUND: usize = 256;

#[derive(Debug)]
pub struct PortRegistryImpl {
    /// Registered ports keyed by `PortId`. Shared via `Arc` with the
    /// dispatcher actor so `Op::Call` / `Op::Subscribe` can look up the port
    /// without round-tripping through the registry.
    ports: Arc<DashMap<PortId, Arc<dyn Port>>>,
    /// Handle to the dispatcher actor. Crate-public so `TidepoolRuntime`'s
    /// Drop impl can `try_send(Op::Shutdown)` directly without going through
    /// an accessor (Drop can't `await`, and the channel is the cleanest tear-
    /// down mechanism).
    pub(crate) dispatcher_tx: tokio::sync::mpsc::Sender<dispatcher::Op>,
}

impl PortRegistryImpl {
    /// Construct a registry and spawn the dispatcher actor on the supplied
    /// tokio runtime.
    ///
    /// `tokio_handle` is typically `TidepoolRuntime`'s stored handle (Phase 3
    /// Task 5 wired this onto the runtime). The dispatcher task lives until
    /// the registry is dropped (the channel sender is dropped, the actor's
    /// `recv()` returns `None`, the loop exits) or `Op::Shutdown` is sent.
    pub fn new(tokio_handle: &tokio::runtime::Handle) -> Self {
        let ports = Arc::new(DashMap::new());
        let (dispatcher_tx, dispatcher_rx) = tokio::sync::mpsc::channel(DISPATCHER_CHANNEL_BOUND);
        tokio_handle.spawn(dispatcher::run(dispatcher_rx, Arc::clone(&ports)));
        Self {
            ports,
            dispatcher_tx,
        }
    }

    /// Sync registration path for boot-time use from non-async callers.
    ///
    /// Functionally equivalent to [`PortRegistry::register`] but skips the
    /// `async fn` so it can be called from `TidepoolRuntime::new` (which is
    /// sync) and any other non-runtime context. Both the ports map and Arc
    /// refcount are sync DashMap operations; nothing needs awaiting.
    pub fn register_sync(&self, port: Arc<dyn Port>) -> Result<(), PortError> {
        let id = port.id().clone();
        match self.ports.entry(id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(PortError::AlreadyRegistered(id)),
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(port);
                Ok(())
            }
        }
    }

    /// Handle to the dispatcher Op channel. Used by `PortHandler` (Task 6)
    /// to enqueue `Call` / `Subscribe` / `Unsubscribe` ops.
    pub fn dispatcher(&self) -> &tokio::sync::mpsc::Sender<dispatcher::Op> {
        &self.dispatcher_tx
    }
}

#[async_trait]
impl PortRegistry for PortRegistryImpl {
    async fn register(&self, port: Arc<dyn Port>) -> Result<(), PortError> {
        let id = port.id().clone();
        match self.ports.entry(id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(PortError::AlreadyRegistered(id)),
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(port);
                Ok(())
            }
        }
    }

    async fn unregister(&self, id: &PortId) {
        self.ports.remove(id);
        // Cancel any active subscriptions for this port (across all sessions).
        // Best-effort send: if the dispatcher channel is closed (runtime
        // tearing down), the subscriptions get cleaned up by the actor's
        // exit path.
        let _ = self
            .dispatcher_tx
            .send(dispatcher::Op::CancelSubscriptionsFor(id.clone()))
            .await;
    }

    fn list(&self) -> Vec<PortMetadata> {
        self.ports.iter().map(|e| e.value().metadata()).collect()
    }

    fn get(&self, id: &PortId) -> Option<Arc<dyn Port>> {
        self.ports.get(id).map(|e| Arc::clone(e.value()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::port::{PortCapabilities, PortEvent, PortMetadata};
    use std::any::Any;

    /// Minimal `Port` impl for trait-shape tests. No real subscribe/call
    /// behaviour — the dispatcher integration tests in `tests/port_handler.rs`
    /// exercise those paths via `MockPort`.
    #[derive(Debug)]
    struct StubPort {
        id: PortId,
        metadata: PortMetadata,
    }

    impl StubPort {
        fn new(id: &str) -> Arc<Self> {
            let pid = PortId::new(id);
            Arc::new(Self {
                id: pid.clone(),
                metadata: PortMetadata::new(pid, "stub"),
            })
        }
    }

    #[async_trait]
    impl Port for StubPort {
        fn id(&self) -> &PortId {
            &self.id
        }
        fn metadata(&self) -> PortMetadata {
            self.metadata.clone()
        }
        fn capabilities(&self) -> PortCapabilities {
            PortCapabilities::default().with_callable(true)
        }
        async fn subscribe(
            &self,
            _config: serde_json::Value,
        ) -> Result<futures::stream::BoxStream<'static, PortEvent>, PortError> {
            Err(PortError::NotSubscribable(self.id.clone()))
        }
        async fn call(
            &self,
            _method: &str,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, PortError> {
            Ok(serde_json::Value::Null)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Build a registry on the current runtime. The dispatcher task is
    /// spawned but does nothing in these tests (no Subscribe ops sent).
    fn registry() -> PortRegistryImpl {
        PortRegistryImpl::new(&tokio::runtime::Handle::current())
    }

    #[tokio::test]
    async fn register_then_get_returns_port() {
        let r = registry();
        let p = StubPort::new("stub");
        r.register(p.clone() as Arc<dyn Port>).await.unwrap();
        let got = r
            .get(&PortId::new("stub"))
            .expect("registered port present");
        // Identity via Arc pointer comparison.
        assert!(Arc::ptr_eq(&got, &(p as Arc<dyn Port>)));
    }

    #[tokio::test]
    async fn register_duplicate_fails_with_already_registered() {
        let r = registry();
        let p1 = StubPort::new("dupe");
        let p2 = StubPort::new("dupe");
        r.register(p1 as Arc<dyn Port>).await.unwrap();
        let err = r
            .register(p2 as Arc<dyn Port>)
            .await
            .expect_err("second register must fail");
        assert!(
            matches!(err, PortError::AlreadyRegistered(ref id) if id.as_str() == "dupe"),
            "expected AlreadyRegistered, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn register_sync_rejects_duplicate() {
        let r = registry();
        let p1 = StubPort::new("sync-dupe");
        r.register_sync(p1 as Arc<dyn Port>).unwrap();
        let p2 = StubPort::new("sync-dupe");
        let err = r
            .register_sync(p2 as Arc<dyn Port>)
            .expect_err("duplicate must fail");
        assert!(matches!(err, PortError::AlreadyRegistered(_)));
    }

    #[tokio::test]
    async fn unregister_removes_entry() {
        let r = registry();
        let p = StubPort::new("removeme");
        r.register(p as Arc<dyn Port>).await.unwrap();
        r.unregister(&PortId::new("removeme")).await;
        assert!(r.get(&PortId::new("removeme")).is_none());
    }

    #[tokio::test]
    async fn list_returns_all_metadata() {
        let r = registry();
        for id in &["a", "b", "c"] {
            r.register(StubPort::new(id) as Arc<dyn Port>)
                .await
                .unwrap();
        }
        let list = r.list();
        assert_eq!(list.len(), 3);
        let mut ids: Vec<_> = list.iter().map(|m| m.id.as_str().to_string()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    /// Locked-in invariant for the `register` ↔ `register_sync` parity:
    /// both produce identical state. Tests use `register_sync` from a sync
    /// context (boot-time registration) while `register` is the async trait
    /// surface; a regression that diverges them silently would be subtle.
    #[tokio::test]
    async fn register_sync_and_register_produce_equivalent_state() {
        let r1 = registry();
        let r2 = registry();
        r1.register_sync(StubPort::new("p") as Arc<dyn Port>)
            .unwrap();
        r2.register(StubPort::new("p") as Arc<dyn Port>)
            .await
            .unwrap();
        assert!(r1.get(&PortId::new("p")).is_some());
        assert!(r2.get(&PortId::new("p")).is_some());
        // Both reject re-registration the same way.
        assert!(matches!(
            r1.register_sync(StubPort::new("p") as Arc<dyn Port>)
                .unwrap_err(),
            PortError::AlreadyRegistered(_)
        ));
        assert!(matches!(
            r2.register(StubPort::new("p") as Arc<dyn Port>)
                .await
                .unwrap_err(),
            PortError::AlreadyRegistered(_)
        ));
    }
}
