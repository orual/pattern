# Phase 4: Port trait + PortRegistry + PortHandler

**Goal:** Replace the Sources and Rpc handler stubs (and the `DataStream`/`SourceManager` traits in `pattern_core`) with a single unified `Port` trait + `PortRegistry` runtime coordinator + `PortHandler` SDK effect. A `Port` is the agent's call/subscribe interface to any external service. Plugin-registered ports (Plan 4 — v3-extensibility) and runtime-provided ports (Phase 5's `HttpPort`) consume this trait.

**Architecture:** `Port` trait lives in `pattern_core` (replaces `DataStream`); it has `id`, `metadata`, `subscribe`, `call`, `capabilities`, and `library` methods. `PortRegistry` lives in `pattern_runtime` (replaces `SourceManager`); it's runtime-global like ProcessManager — one per `TidepoolRuntime`, shared across sessions via `Arc`. `PortHandler<SessionContext>` dispatches `PortReq` to `cx.user().port_registry()`. The `library()` method returns optional Haskell helper source compiled into the agent's prelude when the port is in the agent's `CapabilitySet` — gives agents typed ergonomic access without manual JSON construction. Subscription events use the same between-turn async-reminder buffer Phase 2 introduces (`SessionContext::record_async_reminder`); Phase 4 adds a `MessageAttachment::PortEvent { port_id, payload, at }` top-level variant in `pattern_core/src/types/message.rs` next to Phase 2's `FileEdit` and Phase 3's `ShellOutput`. The dispatcher actor's per-subscription drain task converts each `PortEvent` into the attachment and enqueues it. Compose-time drain in agent_loop splices onto the next turn's first user message; Segment2Pass renders as a `<system-reminder>` block. Per-session subscription state (the `tokio::AbortHandle` so `Unsubscribe` can stop a stream) lives on the dispatcher actor.

**Tech Stack:** Rust async (tokio), `async_trait`, `futures::stream::BoxStream`, `serde_json::Value` (port payloads), `dashmap`, `smol_str` (PortId).

**Scope:** Phase 4 of 5. Independent of Phases 1-3 *except* for the between-turn async-reminder buffer Phase 2 introduces (`SessionContext::record_async_reminder`). Phase 4 adds a `MessageAttachment::PortEvent { port_id, payload, at }` top-level variant in `pattern_core/src/types/message.rs` next to Phase 2's `FileEdit` and Phase 3's `ShellOutput`, plus a render arm in `Segment2Pass`, plus the dispatcher actor's drain task that builds/enqueues the variant. Depends on **Plan 3 (v3-multi-agent) Phase 1** for `CapabilitySet` (agents see only ports their capability set permits). Plan 4 (v3-extensibility) **depends on this phase** — plugins register as Ports, so the trait must be stable here first.

**Codebase verified:** 2026-04-24. Evidence:
- `SourcesHandler` stub at `crates/pattern_runtime/src/sdk/handlers/sources.rs:1-72`. `RpcHandler` stub at `crates/pattern_runtime/src/sdk/handlers/rpc.rs:1-71`.
- `SourcesReq` at `crates/pattern_runtime/src/sdk/requests/sources.rs:1-14` (Stream, Subscribe, List). `RpcReq` at `crates/pattern_runtime/src/sdk/requests/rpc.rs:1-17` (Call, Recv).
- `DataStream` trait at `crates/pattern_core/src/traits/data_stream.rs:1-66` (subscribe + as_any).
- `SourceManager` trait at `crates/pattern_core/src/traits/source_manager.rs:1-78` (register + list_streams + get_stream_source).
- SdkBundle at `crates/pattern_runtime/src/sdk/bundle.rs:40-57` — currently 16 handlers including `SourcesHandler` (line 52) and `RpcHandler` (line 54). After Phase 4: 15 handlers (SourcesHandler + RpcHandler removed; PortHandler added in their place, ending at 15).
- Parity test entries at `crates/pattern_runtime/src/sdk/requests.rs:88-91, 255-274` for SourcesReq + RpcReq — both removed.
- Haskell SDK modules at `crates/pattern_runtime/haskell/Pattern/`: 17 `.hs` files including `Sources.hs` and `Rpc.hs`. After Phase 4: `Sources.hs` and `Rpc.hs` deleted; `Port.hs` added → 16 modules.
- Preamble at `crates/pattern_runtime/src/sdk/preamble.rs:6,11-13` — comment says "16 SDK effect module imports"; needs updating to 15 (or to "the SDK effect module imports" without a number, more durable).
- `canonical_effect_decls()` test at `crates/pattern_runtime/src/sdk/bundle.rs:88-107` asserts 16 entries — change to 15.

---

## Acceptance Criteria Coverage

### v3-sandbox-io.AC4: Port trait and registry
- **v3-sandbox-io.AC4.1 Success:** `Port` trait defined in `pattern_core` with `id()`, `metadata()`, `subscribe()`, `call()`, `capabilities()`, `library()`
- **v3-sandbox-io.AC4.2 Success:** `PortRegistry` resolves registered ports by `PortId`; `Port.List()` returns all registered ports with metadata
- **v3-sandbox-io.AC4.3 Success:** `Port.Call(id, method, payload)` dispatches to the correct port implementation; response returned to agent
- **v3-sandbox-io.AC4.4 Success:** `Port.Subscribe(id, config)` returns a subscription; events arrive as system reminders between turns
- **v3-sandbox-io.AC4.5 Success:** `Port.Unsubscribe(id)` stops event delivery; no further system reminders from that port
- **v3-sandbox-io.AC4.6 Success:** Port with `library()` returning Haskell source: source compiled into agent's prelude when port is in CapabilitySet
- **v3-sandbox-io.AC4.7 Failure:** `Port.Call` to a port not in agent's CapabilitySet: port's effect constructors absent from prelude (compile-time rejection)
- **v3-sandbox-io.AC4.8 Failure:** `Port.Call` to an unregistered `PortId` returns `PortError::NotFound`
- **v3-sandbox-io.AC4.9 Edge:** Port library excluded from prelude when port not in CapabilitySet; agent code referencing the library fails at compilation
- **v3-sandbox-io.AC4.10 Edge:** `DataStream` trait and `SourceManager` trait removed from `pattern_core`; `cargo check --workspace` passes without them

---

## Subcomponent layout

- **A (tasks 1-3): `Port` trait + supporting types in `pattern_core`.** New trait, no implementations yet.
- **B (tasks 4-5): `PortRegistry` runtime coordinator + `TidepoolRuntime`/`SessionContext` wiring.**
- **C (tasks 6-7): `PortReq` enum + `PortHandler` + library prelude integration.**
- **D (tasks 8-9): Retire `DataStream`/`SourceManager`, delete Sources/Rpc stubs, update SdkBundle + canonical_effect_decls; AC4 test suite.**

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: `Port` trait + supporting types

**Files:**
- Create: `crates/pattern_core/src/traits/port.rs` — `Port` trait.
- Create: `crates/pattern_core/src/types/port.rs` — `PortId`, `PortMetadata`, `PortCapabilities`, `PortEvent`, `PortError`.
- Modify: `crates/pattern_core/src/traits.rs` (or `mod.rs` re-export site, investigator pointed at `crates/pattern_core/src/traits/`) — `pub mod port; pub use port::Port;`.
- Modify: `crates/pattern_core/src/types/mod.rs` — re-export `PortId`, `PortMetadata`, etc.

**Implementation:**

```rust
// pattern_core/src/types/port.rs
use std::collections::BTreeSet;
use serde::{Serialize, Deserialize};
use smol_str::SmolStr;

/// Stable identifier for a Port. Lowercase ascii + hyphens by convention
/// (`http`, `slack`, `weather-api`). Plugins choose; the registry rejects
/// duplicates loudly at registration time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortId(pub SmolStr);

impl PortId {
    pub fn new(s: impl Into<SmolStr>) -> Self { Self(s.into()) }
    pub fn as_str(&self) -> &str { self.0.as_str() }
}

impl std::fmt::Display for PortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMetadata {
    pub id: PortId,
    /// Human-readable description for the agent's `Port.List` view.
    pub description: String,
    /// Optional version hint for the port (used in diagnostics + logs).
    pub version: Option<String>,
    /// Method names this port responds to via `call()`. Informational —
    /// not enforced at trait level (a port can dispatch any method),
    /// but agents read this from `Port.List` to discover surface area.
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortCapabilities {
    /// True if the port supports `subscribe()` (event-stream usage).
    /// Agents that try to subscribe to a non-subscribable port get a
    /// clear error; this lets `Port.List` surface "callable only" ports.
    pub subscribable: bool,
    /// True if the port supports `call()`. Almost always true; a few
    /// pure-event-stream ports may set this false.
    pub callable: bool,
    /// True if the port's call() requires a prior `configure` call.
    /// `Port.List` shows this; agents needing to configure call
    /// `Port.Call(id, "configure", config)` first.
    pub requires_configuration: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortEvent {
    pub port_id: PortId,
    /// Opaque event payload. Interpretation is port-specific.
    pub payload: serde_json::Value,
    pub at: jiff::Timestamp,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PortError {
    #[error("port not found: {0}")]
    NotFound(PortId),
    #[error("port {port} does not support {method}")]
    UnsupportedMethod { port: PortId, method: String },
    #[error("port {port} requires configuration before {method} (call \"configure\" first)")]
    NotConfigured { port: PortId, method: String },
    #[error("port {0} is not subscribable")]
    NotSubscribable(PortId),
    #[error("port {0} call failed: {1}")]
    CallFailed(PortId, String),
    #[error("subscription failed for {0}: {1}")]
    SubscribeFailed(PortId, String),
    #[error("invalid payload for {port}.{method}: {message}")]
    BadPayload { port: PortId, method: String, message: String },
    #[error("capability denied: port {0} not in agent's CapabilitySet")]
    CapabilityDenied(PortId),
    #[error("port {0} is already registered")]
    AlreadyRegistered(PortId),
    #[error("port dispatcher actor closed (runtime shutting down?)")]
    DispatcherClosed,
}
```

```rust
// pattern_core/src/traits/port.rs
use std::any::Any;
use async_trait::async_trait;
use futures::stream::BoxStream;
use crate::types::port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};

/// The agent's unified call/subscribe interface to an external service.
///
/// One impl per concrete service (HttpPort for HTTP, SlackPort for Slack,
/// etc.). Runtime-provided ports register at startup via the runtime's
/// PortRegistry; plugin-registered ports register at plugin load time
/// (Plan 4 — v3-extensibility).
///
/// Configuration is convention-based: ports needing configuration are
/// reached via `call("configure", config)` before any other method. The
/// `requires_configuration` flag in `capabilities()` advertises this.
#[async_trait]
pub trait Port: Send + Sync {
    fn id(&self) -> &PortId;
    fn metadata(&self) -> PortMetadata;
    fn capabilities(&self) -> PortCapabilities;

    /// Subscribe to this port's event stream. Returns a boxed stream of
    /// `PortEvent`s the runtime drains and surfaces via system reminders.
    /// Implementations may close the stream at any time (server disconnect,
    /// rate-limit, etc.); the runtime cleans up gracefully.
    async fn subscribe(&self, config: serde_json::Value)
        -> Result<BoxStream<'static, PortEvent>, PortError>;

    /// One-shot call. Method name + JSON payload; returns JSON response.
    /// `method = "configure"` is the convention for setup; ports that need
    /// configuration enforce it internally and return PortError::NotConfigured
    /// from other methods until configure is called.
    async fn call(&self, method: &str, payload: serde_json::Value)
        -> Result<serde_json::Value, PortError>;

    /// Optional Haskell helper source compiled into the agent's prelude
    /// when the port is in the agent's CapabilitySet. Conventionally:
    /// typed wrappers around `Port.Call(id, method, payload)` so agents
    /// write `Http.get url` instead of constructing JSON by hand.
    ///
    /// Returns `&'static str` because port libraries are typically
    /// compile-time string literals (concat! / include_str!). Plugins
    /// can leak() if they need a runtime-built string.
    fn library(&self) -> Option<&'static str> { None }

    /// Downcast escape hatch — same pattern as the v2 DataStream trait.
    /// Lets specialized consumers reach the concrete port impl.
    fn as_any(&self) -> &dyn Any;
}
```

**Verifies:** AC4.1.

**Verification:**
- `cargo check -p pattern-core`.
- A doctest on `Port` showing a `Dummy` impl (mirrors v2's `DataStream` doctest).

**Commit:** `[pattern-core] Port trait + PortId/PortMetadata/PortCapabilities/PortEvent/PortError`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `PortRegistry` trait

**Files:**
- Create: `crates/pattern_core/src/traits/port_registry.rs` — trait definition (no impl).

**Note:** unlike Phase 3's `ProcessManager` (concrete, lives in `pattern_runtime`), `PortRegistry` is split into a trait (in `pattern_core`) + concrete impl (in `pattern_runtime`). This matches the existing `SourceManager` shape and keeps the boundary clean for Plan 4's plugin system, which references `&dyn PortRegistry` from plugin host code.

```rust
// pattern_core/src/traits/port_registry.rs
use std::sync::Arc;
use async_trait::async_trait;
use crate::traits::port::Port;
use crate::types::port::{PortError, PortId, PortMetadata};

/// Registry of `Port` implementations.
///
/// Implementations use interior mutability so the registry can be shared
/// by reference across many call sites without threading a mutable borrow
/// through every call. Matches the existing SourceManager pattern.
#[async_trait]
pub trait PortRegistry: Send + Sync {
    /// Register a port. Fails with PortError::CallFailed (specific variant
    /// could be added later) if the id is already registered — explicit
    /// duplicates surface as errors rather than silently overwriting.
    async fn register(&self, port: Arc<dyn Port>) -> Result<(), PortError>;

    /// Unregister by id. No-op if not registered.
    async fn unregister(&self, id: &PortId);

    /// List all registered port metadatas (for `Port.List`).
    fn list(&self) -> Vec<PortMetadata>;

    /// Fetch a port by id.
    fn get(&self, id: &PortId) -> Option<Arc<dyn Port>>;
}
```

**Verifies:** AC4.2 (trait shape).

**Verification:** `cargo check -p pattern-core`.

**Commit:** `[pattern-core] PortRegistry trait`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `PortRegistryImpl` — registry storage + dispatcher actor

**Files:**
- Create: `crates/pattern_runtime/src/port_registry/mod.rs` — module root.
- Create: `crates/pattern_runtime/src/port_registry/registry.rs` — `PortRegistryImpl` (storage + lifecycle).
- Create: `crates/pattern_runtime/src/port_registry/dispatcher.rs` — actor task + `Op` enum + dispatcher handle.

**Architecture:** the registry is a sync DashMap of registered ports (CRUD ops are infrequent and don't need an actor). The `dispatcher` is the actor task that drives async work for handler-side `Call` and `Subscribe` requests. Handler dispatches via crossbeam → actor task on tokio runtime → `port.call(...).await` → crossbeam reply.

**Op channel design** (handler ↔ actor):
- Handler → actor: `tokio::sync::mpsc::Sender<Op>` with a generous bound (256). Handler does `tx.blocking_send(op)` from sync code (works without runtime context — `Sender::blocking_send` is documented for non-async callers; the bounded channel is required because `UnboundedSender` doesn't expose `blocking_send`). If the channel ever fills, the eval worker blocks on send — an observable diagnostic, not a silent stall.
- Actor → handler reply: `crossbeam_channel::Sender<Result<...>>` embedded in each Op variant. Actor does `reply.send(...)` (sync, non-blocking on bounded(1)). Handler does `reply.recv_timeout(...)` (sync, with timeout).

```rust
// port_registry/registry.rs
use std::sync::Arc;
use async_trait::async_trait;
use dashmap::DashMap;
use pattern_core::traits::{Port, PortRegistry};
use pattern_core::types::port::{PortError, PortId, PortMetadata};

#[derive(Debug)]
pub struct PortRegistryImpl {
    ports: Arc<DashMap<PortId, Arc<dyn Port>>>,
    /// Handle to the dispatcher actor. Created at TidepoolRuntime::new
    /// (Task 4) using the supplied tokio Handle.
    /// Crate-public so `TidepoolRuntime`'s Drop impl can `try_send` an
    /// `Op::Shutdown` directly (Drop can't `await`, and going through an
    /// accessor would require returning a `&Sender` from `&self` which is
    /// fine but adds noise; field visibility is the simpler path).
    pub(crate) dispatcher_tx: tokio::sync::mpsc::Sender<crate::port_registry::dispatcher::Op>,
}

impl PortRegistryImpl {
    pub fn dispatcher(&self) -> &tokio::sync::mpsc::Sender<crate::port_registry::dispatcher::Op> {
        &self.dispatcher_tx
    }

    /// Sync registration path — for boot-time use from `TidepoolRuntime::new`
    /// (which is sync) and any other non-async caller. Functionally equivalent
    /// to `register()` but skips the async trait method to avoid the need for
    /// a runtime context. Both ports map and refcount are sync DashMap inserts;
    /// no work needs awaiting.
    pub fn register_sync(&self, port: Arc<dyn Port>) -> Result<(), PortError> {
        let id = port.id().clone();
        match self.ports.entry(id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(PortError::AlreadyRegistered(id)),
            dashmap::mapref::entry::Entry::Vacant(e) => { e.insert(port); Ok(()) }
        }
    }
}

#[async_trait]
impl PortRegistry for PortRegistryImpl {
    async fn register(&self, port: Arc<dyn Port>) -> Result<(), PortError> {
        let id = port.id().clone();
        // dashmap::entry is sync and gives us atomic check-and-insert.
        match self.ports.entry(id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(PortError::AlreadyRegistered(id)),
            dashmap::mapref::entry::Entry::Vacant(e) => { e.insert(port); Ok(()) }
        }
    }

    async fn unregister(&self, id: &PortId) {
        self.ports.remove(id);
        // Cancel any active subscriptions for this port. Dispatcher owns
        // the AbortHandles; send a CancelAllSubscriptionsFor(id) op.
        let _ = self.dispatcher_tx.send(
            crate::port_registry::dispatcher::Op::CancelSubscriptionsFor(id.clone())
        ).await;
    }

    fn list(&self) -> Vec<PortMetadata> {
        self.ports.iter().map(|e| e.value().metadata()).collect()
    }

    fn get(&self, id: &PortId) -> Option<Arc<dyn Port>> {
        self.ports.get(id).map(|e| Arc::clone(e.value()))
    }
}
```

```rust
// port_registry/dispatcher.rs
use std::collections::HashMap;
use std::sync::Arc;
use crossbeam_channel::Sender as XSender;
use dashmap::DashMap;
use futures::StreamExt;
use pattern_core::traits::Port;
use pattern_core::types::port::{PortError, PortEvent, PortId};
use crate::memory::MemoryStoreAdapter;

/// Operations the handler enqueues for the dispatcher actor.
pub enum Op {
    Call {
        port_id: PortId,
        method: String,
        payload: serde_json::Value,
        reply: XSender<Result<serde_json::Value, PortError>>,
    },
    Subscribe {
        port_id: PortId,
        config: serde_json::Value,
        /// Handle to the session's between-turn async-reminder buffer
        /// (Phase 2 introduced). Drain task pushes PortEvent attachments
        /// here; compose-time drain on the next turn surfaces them.
        async_reminder_queue: Arc<Mutex<Vec<MessageAttachment>>>,
        /// Per-session subscription key — typically the session id, used so
        /// dispatcher can cancel all subscriptions for a session on shutdown.
        session_key: String,
        reply: XSender<Result<(), PortError>>,
    },
    Unsubscribe {
        port_id: PortId,
        session_key: String,
        reply: XSender<Result<(), PortError>>,
    },
    CancelSubscriptionsFor(PortId),
    Shutdown,
}

/// Active subscription: per-session, per-port. Holding the AbortHandle is
/// what lets us stop the drain task on Unsubscribe / Shutdown.
type SubscriptionKey = (String /* session_key */, PortId);

pub async fn run(
    mut rx: tokio::sync::mpsc::Receiver<Op>,
    ports: Arc<DashMap<PortId, Arc<dyn Port>>>,
) {
    let mut subscriptions: HashMap<SubscriptionKey, tokio::task::AbortHandle> = HashMap::new();

    while let Some(op) = rx.recv().await {
        match op {
            Op::Call { port_id, method, payload, reply } => {
                let port = match ports.get(&port_id) {
                    Some(p) => Arc::clone(p.value()),
                    None => { let _ = reply.send(Err(PortError::NotFound(port_id))); continue; }
                };
                // Plugin code runs here. NOT block_on — we're already on the
                // runtime. If the plugin's call() future hangs, this actor
                // task hangs with it, but only this one task — handler is
                // protected by its own recv_timeout.
                let result = port.call(&method, payload).await;
                let _ = reply.send(result);
            }
            Op::Subscribe { port_id, config, async_reminder_queue, session_key, reply } => {
                let port = match ports.get(&port_id) {
                    Some(p) => Arc::clone(p.value()),
                    None => { let _ = reply.send(Err(PortError::NotFound(port_id))); continue; }
                };
                let stream_result = port.subscribe(config).await;
                let stream = match stream_result {
                    Ok(s) => s,
                    Err(e) => { let _ = reply.send(Err(e)); continue; }
                };
                let key = (session_key.clone(), port_id.clone());
                if let Some(prev) = subscriptions.remove(&key) { prev.abort(); }
                let port_id_for_task = port_id.clone();
                let task = tokio::spawn(drain_subscription(
                    port_id_for_task, stream, async_reminder_queue,
                ));
                subscriptions.insert(key, task.abort_handle());
                let _ = reply.send(Ok(()));
            }
            Op::Unsubscribe { port_id, session_key, reply } => {
                let key = (session_key, port_id);
                if let Some(handle) = subscriptions.remove(&key) {
                    handle.abort();
                }
                let _ = reply.send(Ok(()));
            }
            Op::CancelSubscriptionsFor(port_id) => {
                let to_remove: Vec<_> = subscriptions.keys()
                    .filter(|(_, p)| p == &port_id).cloned().collect();
                for key in to_remove {
                    if let Some(h) = subscriptions.remove(&key) { h.abort(); }
                }
            }
            Op::Shutdown => {
                for (_, h) in subscriptions.drain() { h.abort(); }
                break;
            }
        }
    }
}

async fn drain_subscription(
    port_id: PortId,
    mut stream: futures::stream::BoxStream<'static, PortEvent>,
    queue: Arc<Mutex<Vec<MessageAttachment>>>,
) {
    while let Some(event) = stream.next().await {
        let attachment = MessageAttachment::PortEvent {
            port_id: port_id.to_string(),
            payload: event.payload,
            at: event.at,
        };
        queue.lock().unwrap().push(attachment);
    }
    // Stream end (server disconnect, etc.) is silent — agent learns from
    // the absence of further events. Future enhancement: emit a sentinel
    // PortEvent at stream-end if reviewer wants it.
}
```

**I13 fix (resolved):** the dispatcher actor processes ops serially (single recv loop, no `tokio::select` over multiple channels), so an Unsubscribe op queued after this Subscribe waits for the spawn-and-insert sequence to complete. The original I13 concern about a race window between spawn and insert (in a hypothetical multi-threaded receiver) does not apply to this actor design. Spawn-then-insert order is correct because the AbortHandle comes from the JoinHandle returned by `tokio::spawn`.

**I12 fix:** `register_duplicate` now returns `PortError::AlreadyRegistered(id)` (the new variant added in Task 1), not the misused `CallFailed` variant.

**Verifies:** AC4.2 (registry CRUD), mechanism for AC4.3 / AC4.4 / AC4.5 (dispatcher).

**Verification:**
- `cargo check -p pattern-runtime`.
- Unit tests:
    - `register_then_get_returns_port` — register a `MockPort`; `get(id)` returns it.
    - `register_duplicate_fails_with_already_registered` — second register returns `PortError::AlreadyRegistered`.
    - `unregister_removes_entry` — `get(id)` after unregister returns None.
    - `list_returns_all_metadata` — three ports → list len 3.
    - Dispatcher tests live in Task 9.

**Commit:** `[pattern-runtime] PortRegistryImpl + dispatcher actor (sync handler boundary, async port impls)`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

---

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->

<!-- START_TASK_4 -->
### Task 4: Wire `PortRegistryImpl` + dispatcher actor into `TidepoolRuntime` + `SessionContext`

**Files:**
- Modify: `crates/pattern_runtime/src/runtime.rs:32` — add `port_registry: Arc<PortRegistryImpl>` field. (Concrete type, not `Arc<dyn PortRegistry>`, so callers can reach `.dispatcher()` for handler-side dispatch.)
- Modify: `crates/pattern_runtime/src/runtime.rs` — add `impl Drop for TidepoolRuntime` that sends `Op::Shutdown` to `self.port_registry.dispatcher_tx` via best-effort `try_send` (Drop can't `await`). Without this the dispatcher actor task leaks every time a runtime is constructed and dropped — common in test fixtures (I-NEW-4 fix).
- Modify: `crates/pattern_runtime/src/session.rs:40-121` — add `port_registry: Arc<PortRegistryImpl>` field on SessionContext + `port_registry()` accessor.

**Note on Phase 3 dependency:** `TidepoolRuntime::new` already takes a `tokio::runtime::Handle` (Phase 3 Task 5). PortRegistry uses it to spawn the dispatcher actor task at construction.

**Implementation:**

Add `PortRegistryImpl::new(tokio_handle: &tokio::runtime::Handle) -> Self` (M-NEW-3 — match the `ProcessManager::new` constructor convention) that handles ports map + dispatcher channel + actor spawn internally:

```rust
// port_registry/registry.rs
impl PortRegistryImpl {
    pub fn new(tokio_handle: &tokio::runtime::Handle) -> Self {
        let ports = Arc::new(DashMap::new());
        let (dispatcher_tx, dispatcher_rx) = tokio::sync::mpsc::channel(256);
        tokio_handle.spawn(crate::port_registry::dispatcher::run(
            dispatcher_rx, Arc::clone(&ports),
        ));
        Self { ports, dispatcher_tx }
    }
}
```

Then in `TidepoolRuntime::new`:
```rust
let port_registry = Arc::new(PortRegistryImpl::new(&tokio_handle));
```

Flows into `SessionContext` at session-open time (cloned `Arc`).

```rust
// session.rs
pub fn port_registry(&self) -> &Arc<PortRegistryImpl> { &self.port_registry }
```

`TidepoolRuntime` exposes `pub fn port_registry(&self) -> &Arc<PortRegistryImpl>` so callers (Phase 5's HttpPort registration, Plan 4's plugin loader) can register at startup.

**Why explicit `Arc<PortRegistryImpl>` (not `Arc<dyn PortRegistry>`)?** Handler dispatch needs `.dispatcher()` access, which is not part of the trait (the trait stays plugin-facing). External callers that only want the trait API can do `Arc<dyn PortRegistry>` via coercion: `let trait_obj: Arc<dyn PortRegistry> = registry.clone();`.

**Shutdown:** `TidepoolRuntime`'s Drop impl sends `Op::Shutdown` to the dispatcher; the actor loop breaks on `Op::Shutdown`, aborts all live subscriptions, and exits. Drop is sync, so use `try_send` (best-effort — if the runtime that owns the dispatcher is already torn down, or the channel is full, the send fails silently and the actor task is leaked at process exit, which is acceptable).

```rust
impl Drop for TidepoolRuntime {
    fn drop(&mut self) {
        // try_send is non-blocking; safe to call from Drop. Failure means
        // either the dispatcher's tokio runtime is already gone (acceptable,
        // task is leaked but process is exiting anyway) or the Op channel
        // is at its 256-bound (extremely unlikely at shutdown — log only).
        if let Err(e) = self.port_registry.dispatcher_tx.try_send(
            crate::port_registry::dispatcher::Op::Shutdown
        ) {
            tracing::debug!(error = %e, "TidepoolRuntime drop: dispatcher shutdown send skipped");
        }
    }
}
```

**Verifies:** Mechanism — handler reaches registry via `cx.user().port_registry()`, then dispatcher via `.dispatcher()`.

**Verification:**
- `cargo check -p pattern-runtime`.
- Existing `session_lifecycle.rs` tests still pass.

**Commit:** `[pattern-runtime] PortRegistryImpl + dispatcher actor on TidepoolRuntime + SessionContext`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `MessageAttachment::PortEvent` variant + Segment2Pass render arm

**Files:**
- Modify: `crates/pattern_core/src/types/message.rs` — add `MessageAttachment::PortEvent { port_id: String, payload: serde_json::Value, at: jiff::Timestamp }` variant alongside Phase 2's `FileEdit` and Phase 3's `ShellOutput`.
- Modify: `crates/pattern_provider/src/compose/passes/segment_2.rs` — add a render arm for `MessageAttachment::PortEvent` next to the others.

**Note on what moved.** Subscription lifecycle (AbortHandles, drain task spawning) lives inside the dispatcher actor (Task 3). SessionContext doesn't need subscription-specific fields — only the shared `async_reminder_queue` Phase 2 introduced. The actor's drain task pushes `MessageAttachment::PortEvent` entries into that queue; compose-time drain in agent_loop splices them onto the next turn's first user message.

**Variant + render:**

```rust
// pattern_core/src/types/message.rs (addition)
pub enum MessageAttachment {
    BatchOpeningSnapshot { /* existing */ },
    FileEdit { /* Phase 2 */ },
    ShellOutput { /* Phase 3 */ },
    /// One subscription event delivered by a Port. The dispatcher actor's
    /// drain task (Phase 4 Task 3) builds these from the BoxStream<PortEvent>
    /// returned by the Port impl's subscribe() and enqueues them via
    /// SessionContext::record_async_reminder.
    PortEvent {
        port_id: String,
        payload: serde_json::Value,
        at: jiff::Timestamp,
    },
}
```

```rust
// pattern_provider/src/compose/passes/segment_2.rs (addition)
match attachment {
    // … existing arms …
    MessageAttachment::PortEvent { port_id, payload, at } => {
        let body = format!(
            "<system-reminder>\n\
             Port event from {port_id} @ {at}:\n\
             ```json\n{}\n```\n\
             </system-reminder>",
            serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string()),
        );
        push_user_block(message, body);
    }
}
```

**Verifies:** AC4.4 (variant + render — actually consumed by the dispatcher's drain task in Task 3).

**Verification:**
- `cargo check --workspace`.
- Unit tests on the Segment2Pass arm: snapshot-test the body for known inputs via `insta`.
- Lifecycle tests live in Task 9 (full subscribe → event → next-turn-attachment integration via the dispatcher).

**Commit:** `[pattern-core] [pattern-provider] PortEvent attachment variant + Segment2Pass render arm`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

---

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: `PortReq` enum + `PortHandler` impl

**Files:**
- Create: `crates/pattern_runtime/src/sdk/requests/port.rs`.
- Create: `crates/pattern_runtime/src/sdk/handlers/port.rs`.
- Modify: `crates/pattern_runtime/src/sdk/requests.rs` — `pub mod port; pub use port::PortReq;` + add to parity table.
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs:40-57` — replace `SourcesHandler` and `RpcHandler` with `PortHandler`.
- Modify: `crates/pattern_runtime/haskell/Pattern/Port.hs` — new file, GADT for Port effect.

**`PortReq` enum:**

```rust
#[derive(Debug, FromCore)]
pub enum PortReq {
    #[core(module = "Pattern.Port", name = "List")]
    List,
    #[core(module = "Pattern.Port", name = "Call")]
    Call(String, String, String), // (port_id, method, payload_json)
    #[core(module = "Pattern.Port", name = "Subscribe")]
    Subscribe(String, String),    // (port_id, config_json)
    #[core(module = "Pattern.Port", name = "Unsubscribe")]
    Unsubscribe(String),          // port_id
}
```

**`PortHandler` impl:**

```rust
#[derive(Default, Clone)]
pub struct PortHandler;

impl DescribeEffect for PortHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Port",
            description: "External-service ports (List/Call/Subscribe/Unsubscribe)",
            constructors: &[
                "List        :: Port [PortInfo]",
                "Call        :: PortId -> Method -> Payload -> Port Payload",
                "Subscribe   :: PortId -> ConfigJson -> Port ()",
                "Unsubscribe :: PortId -> Port ()",
            ],
            type_defs: &[
                "type PortId = Text",
                "type Method = Text",
                "type Payload = Text  -- JSON",
                "type ConfigJson = Text  -- JSON",
                "type PortInfo = Text  -- JSON: {id, description, version, methods, capabilities}",
            ],
            helpers: &[
                "listPorts :: Member Port effs => Eff effs [Text]\nlistPorts = send List",
                "call :: Member Port effs => PortId -> Method -> Payload -> Eff effs Payload\ncall pid m p = send (Call pid m p)",
                "subscribe :: Member Port effs => PortId -> ConfigJson -> Eff effs ()\nsubscribe pid c = send (Subscribe pid c)",
                "unsubscribe :: Member Port effs => PortId -> Eff effs ()\nunsubscribe pid = send (Unsubscribe pid)",
            ],
        }
    }
}

```rust
// SAFETY / DESIGN NOTE: PortHandler runs on the Tidepool eval worker —
// a dedicated OS thread with NO ambient tokio runtime. This handler does
// NOT call `block_on` against arbitrary plugin code. Instead it sends an
// `Op` to the dispatcher actor task (running on the runtime's tokio
// runtime via the Handle supplied at TidepoolRuntime::new) and waits on
// a crossbeam reply channel with `recv_timeout`. The actor handles all
// `await`s including those into plugin code; if a plugin's call() hangs,
// only the actor task hangs, not the eval worker.
impl EffectHandler<SessionContext> for PortHandler {
    type Request = PortReq;

    fn handle(&mut self, req: PortReq, cx: &EffectContext<'_, SessionContext>)
        -> Result<Value, EffectError>
    {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        let registry = cx.user().port_registry().clone();
        let cap = cx.user().capability_set().clone();
        let session_key = cx.user().session_id().to_string();
        let dispatcher = registry.dispatcher().clone();

        // Tunable bounds. List/Unsubscribe are fast (no plugin code);
        // Call/Subscribe wait on plugin code so they get a longer cap.
        const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
        const FAST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        match req {
            PortReq::List => {
                let metadatas = registry.list();
                let visible: Vec<_> = metadatas.into_iter()
                    .filter(|m| cap.has_port(&m.id))
                    .map(|m| serde_json::to_string(&m).unwrap_or_default())
                    .collect();
                cx.respond(visible)
            }
            PortReq::Call(port_id, method, payload_json) => {
                let port_id = PortId::new(port_id);
                if !cap.has_port(&port_id) {
                    return Err(EffectError::Handler(
                        PortError::CapabilityDenied(port_id).to_string()
                    ));
                }
                let payload: serde_json::Value = serde_json::from_str(&payload_json)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Port.Call: invalid payload JSON: {e}")))?;

                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                // tokio::sync::mpsc::Sender::blocking_send works from any
                // thread, including non-runtime threads like the eval worker.
                dispatcher.blocking_send(crate::port_registry::dispatcher::Op::Call {
                    port_id, method, payload, reply: reply_tx,
                }).map_err(|_| EffectError::Handler(PortError::DispatcherClosed.to_string()))?;

                let result = reply_rx.recv_timeout(CALL_TIMEOUT)
                    .map_err(|_| EffectError::Handler("Pattern.Port.Call: dispatcher reply timeout".to_string()))?;
                let response = result.map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.respond(serde_json::to_string(&response).unwrap_or_default())
            }
            PortReq::Subscribe(port_id, config_json) => {
                let port_id = PortId::new(port_id);
                if !cap.has_port(&port_id) {
                    return Err(EffectError::Handler(
                        PortError::CapabilityDenied(port_id).to_string()
                    ));
                }
                let config: serde_json::Value = serde_json::from_str(&config_json)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Port.Subscribe: invalid config JSON: {e}")))?;

                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                dispatcher.blocking_send(crate::port_registry::dispatcher::Op::Subscribe {
                    port_id, config,
                    async_reminder_queue: Arc::clone(cx.user().async_reminder_queue()),
                    session_key,
                    reply: reply_tx,
                }).map_err(|_| EffectError::Handler(PortError::DispatcherClosed.to_string()))?;

                let result = reply_rx.recv_timeout(CALL_TIMEOUT)
                    .map_err(|_| EffectError::Handler("Pattern.Port.Subscribe: dispatcher reply timeout".to_string()))?;
                result.map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.respond(())
            }
            PortReq::Unsubscribe(port_id) => {
                let port_id = PortId::new(port_id);
                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                dispatcher.blocking_send(crate::port_registry::dispatcher::Op::Unsubscribe {
                    port_id, session_key, reply: reply_tx,
                }).map_err(|_| EffectError::Handler(PortError::DispatcherClosed.to_string()))?;
                let _ = reply_rx.recv_timeout(FAST_TIMEOUT);
                cx.respond(())
            }
        }
    }
}
```

**Capability gating (I14 explicit prereq):** `cap.has_port(&port_id)` requires Plan 3's `CapabilitySet` to expose **per-port granularity** (not just per-effect-category). Phase 4 execution should verify this is the case as the first step — `grep -rn 'has_port\|fn has_port' crates/pattern_core/src` after Plan 3 lands. If only `has_port_effect()` exists, surface as a scope question before proceeding (per implementation guidance: do not stub or skate). AC4.7 / AC4.9 require this granularity.

**Haskell `Pattern/Port.hs`:**

```haskell
{-# LANGUAGE GADTs, KindSignatures, DataKinds #-}
module Pattern.Port where
import Data.Text (Text)
import qualified Data.Text as T
import Pattern.Eff (Eff, Member, send)

type PortId = Text
type Method = Text
type Payload = Text   -- JSON
type ConfigJson = Text  -- JSON
type PortInfo = Text  -- JSON

data Port :: * -> * where
    List :: Port [PortInfo]
    Call :: PortId -> Method -> Payload -> Port Payload
    Subscribe :: PortId -> ConfigJson -> Port ()
    Unsubscribe :: PortId -> Port ()
```

**Verifies:** AC4.2 (List), AC4.3 (Call dispatch), AC4.4 (Subscribe), AC4.5 (Unsubscribe), AC4.7 (capability deny), AC4.8 (NotFound).

**Verification:**
- `cargo check -p pattern-runtime`.
- Unit test for the parity table (auto-passes if the new variant is added correctly per existing convention).

**Commit:** `[pattern-runtime] PortReq + PortHandler dispatch`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Library prelude integration

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs:31` — `build()` gains a `port_libraries: &[(PortId, &str)]` parameter; for each pair, the library source is appended to the preamble after the SDK module imports.
- Modify: `crates/pattern_runtime/src/sdk/code_tool.rs` (and any other preamble caller) — pass the list of `(port_id, library_src)` pairs filtered by `cap.has_port(port_id)` from the runtime's port registry.

**Implementation:**

The preamble currently emits a fixed sequence: pragmas → module header → standard imports → SDK module imports → `type M` alias → API-doc comment. Library source from each enabled port slots in *after* the SDK imports, *before* `type M` (so library code can reference the SDK effect types).

```rust
pub fn build(decls: &[EffectDecl], port_libraries: &[(PortId, &str)]) -> String {
    let mut out = String::with_capacity(8192 + port_libraries.iter().map(|(_, s)| s.len()).sum::<usize>());
    // … existing pragmas + imports …

    // Port libraries — one block per enabled port. Comment header per
    // library so the LLM can identify which port a helper belongs to.
    for (port_id, library) in port_libraries {
        out.push_str(&format!("\n-- Port library: {port_id}\n"));
        out.push_str(library);
        out.push('\n');
    }

    // … existing type M alias + API-doc comment block …
    out
}
```

The caller (in `code_tool.rs`) builds the list:

```rust
let port_libraries: Vec<(PortId, &str)> = runtime.port_registry().list().iter()
    .filter(|m| cap.has_port(&m.id))
    .filter_map(|m| {
        let port = runtime.port_registry().get(&m.id)?;
        port.library().map(|src| (m.id.clone(), src))
    })
    .collect();
let preamble = preamble::build(canonical_effect_decls(), &port_libraries);
```

**`HttpPort` library example** (provided in Phase 5; here as illustration of expected shape):

```haskell
-- Port library: http
module Pattern.Http where
import Pattern.Port (call)
import Data.Text (Text)
import qualified Data.Aeson as A

httpGet :: Text -> Eff effs Text
httpGet url = call "http" "get" (A.encode (A.object ["url" A..= url]))

httpPost :: Text -> Text -> Eff effs Text
httpPost url body = call "http" "post" (A.encode (A.object ["url" A..= url, "body" A..= body]))
```

(Library uses `Pattern.Port.call`, which is in scope via the SDK imports above.)

**Verifies:** AC4.6, AC4.9.

**Verification:**
- `cargo check -p pattern-runtime`.
- Unit test in `preamble.rs`:
    - `library_appended_when_provided` — `build(decls, &[(id, "module Foo where ...")])` contains the library block.
    - `no_library_block_when_empty` — `build(decls, &[])` matches the no-library output (regression guard).
    - `multiple_libraries_each_get_header` — two ports → two `-- Port library:` headers.

**Commit:** `[pattern-runtime] preamble — splice port library source per CapabilitySet`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

<!-- START_SUBCOMPONENT_D (tasks 8-9) -->

<!-- START_TASK_8 -->
### Task 8: Retire `DataStream`/`SourceManager`; delete Sources/Rpc stubs

**Files:**
- Delete: `crates/pattern_core/src/traits/data_stream.rs`.
- Delete: `crates/pattern_core/src/traits/source_manager.rs`.
- Modify: `crates/pattern_core/src/traits.rs:20-27` — remove `pub mod data_stream;` and `pub mod source_manager;` plus the `pub use data_stream::{DataStream, StreamEvent};` and `pub use source_manager::{SourceManager, SourceName};` re-exports. (Investigator confirmed exact line numbers; verify at execution time.)
- Modify: `crates/pattern_core/src/lib.rs:67-68` — remove the crate-root `pub use traits::{DataStream, SourceManager}` re-exports.
- (CLAUDE.md updates moved to Phase 5 Task 3 to consolidate documentation edits — M19. Phase 4 Task 8 only deletes code.)
- Delete: `crates/pattern_runtime/src/sdk/handlers/sources.rs`.
- Delete: `crates/pattern_runtime/src/sdk/handlers/rpc.rs`.
- Delete: `crates/pattern_runtime/src/sdk/requests/sources.rs`.
- Delete: `crates/pattern_runtime/src/sdk/requests/rpc.rs`.
- Delete: `crates/pattern_runtime/haskell/Pattern/Sources.hs`.
- Delete: `crates/pattern_runtime/haskell/Pattern/Rpc.hs`.
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs:40-57` — remove `SourcesHandler` (line 52) and `RpcHandler` (line 54) from the HList; add `PortHandler` (in their slot or appended at the end — choose the lower-churn position).
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs:88-107` — `canonical_effect_decls()` test: change `assert_eq!(decls.len(), 16)` to `15`.
- Modify: `crates/pattern_runtime/src/sdk/requests.rs:34, 38, 88-91, 255-274` — remove SourcesReq + RpcReq pub use, parity entries, and asserts.
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs:6,11-13` — drop the "16" count or change to "15"; better: drop the literal count and say "the SDK effect modules" so future row changes don't require source edits.
- Modify: any Haskell code-tool preamble references — same drop.
- Test fixture cleanup (I-NEW-5 — enumerated explicitly per round-2 review):
    - Modify: `crates/pattern_runtime/tests/stub_effects.rs:30-31` — remove `SourcesHandler` and `RpcHandler` imports.
    - Modify: `crates/pattern_runtime/tests/stub_effects.rs:138, 164` — delete the `sources_stub_*` and `rpc_stub_*` test functions (their stubs are gone; tests would fail to compile).
    - Modify: `crates/pattern_runtime/tests/fixtures/cross_module_collision.hs` — adjust effect-row tuple to remove `Sources` and `Rpc` and add `Port` (effect-row position-shift impact: handlers after Sources shift up by 2, then `Port` lands wherever the SdkBundle HList places it; verify positions against the updated bundle).
    - Modify: `crates/pattern_runtime/tests/fixtures/file_stub_full_bundle.hs` — same effect-row adjustment.
    - Delete: `crates/pattern_runtime/tests/fixtures/sources_stub.hs`.
    - Delete: `crates/pattern_runtime/tests/fixtures/rpc_stub.hs`.

**Verification beyond `cargo check`:**

Pre-task: `grep -rn "DataStream\|SourceManager\|SourcesHandler\|RpcHandler\|SourcesReq\|RpcReq\|Pattern\.Sources\|Pattern\.Rpc" crates/ docs/ haskell/ 2>&1`. The post-task version of the same grep should return only references inside docs/notes/historical files — actual source must be free of references.

Per implementation guidance: "Excise-don't-stub. If code X references deleted code Y and X is also being rewritten, delete both in the same pass." This task is the excise pass.

Per implementation guidance: removed types do not get re-exported, do not get `_unused` rename, do not get `// removed` comments. They're gone.

**Verifies:** AC4.10.

**Verification:**
- `cargo check --workspace`.
- `cargo nextest run --workspace` — 646 existing + new tests pass.
- `grep -rn "DataStream\|SourceManager\|SourcesHandler\|RpcHandler" crates/` returns no matches.

**Commit:** `[pattern-core] [pattern-runtime] retire DataStream/SourceManager + delete Sources/Rpc stubs`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: AC4 test suite

**Files:**
- Create: `crates/pattern_runtime/tests/port_handler.rs` — full-handler integration tests.
- Add `MockPort` in a `pattern_runtime/src/testing/mock_port.rs` for test reuse.

**`MockPort`:**

```rust
pub struct MockPort {
    id: PortId,
    metadata: PortMetadata,
    capabilities: PortCapabilities,
    /// Configurable call response.
    call_response: Mutex<serde_json::Value>,
    /// Optional Haskell library source.
    library_src: Option<&'static str>,
    /// Channel for tests to push events into the subscription stream.
    event_tx: tokio::sync::mpsc::UnboundedSender<PortEvent>,
    event_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PortEvent>>>,
}
// Implements Port; subscribe converts the receiver into a BoxStream.
```

**Tests:**

| AC | Test name | Mechanism |
|----|-----------|-----------|
| 4.1 | Covered by Task 1's doctest. | — |
| 4.2 | `port_list_returns_registered_metadatas` | Register 3 MockPorts; `Port.List` returns 3 entries. |
| 4.3 | `port_call_dispatches_to_registered_port` | MockPort with call_response = `{"ok": true}`; `Port.Call("mock", "ping", "{}")` returns the response. |
| 4.4 | `port_subscribe_delivers_events_via_attachments` | Subscribe; push 3 events via MockPort's tx; await scheduler tick + one turn boundary; assert the next turn's first user message has 3 `MessageAttachment::PortEvent { port_id, payload, .. }` entries matching the pushed events. |
| 4.5 | `port_unsubscribe_stops_event_delivery` | Subscribe + push 1 event + drain. Unsubscribe + push another event + tick + drain — second event NOT present (AbortHandle stopped the task). |
| 4.6 | `port_library_appended_to_preamble_when_capable` | MockPort with `library_src = Some("module Mock where mockFn = ...")`; build preamble with capability granted; assert preamble contains `mockFn`. |
| 4.7 | `port_call_capability_denied_blocks_dispatch` | Capability set without the port; `Port.Call("mock", ...)` returns `PortError::CapabilityDenied`. |
| 4.8 | `port_call_unknown_port_returns_not_found` | `Port.Call("does-not-exist", ...)` → `PortError::NotFound`. |
| 4.9 | `port_library_excluded_when_not_capable` | MockPort with library; capability set excludes the port; preamble does NOT contain the library. |
| 4.10 | Compile gate. | `cargo check --workspace` after Task 8 deletions. |

**Verifies:** AC4.2, AC4.3, AC4.4, AC4.5, AC4.6, AC4.7, AC4.8, AC4.9.

**Verification:**
- `cargo nextest run -p pattern-runtime --test port_handler`.
- All workspace tests pass.

**Commit:** `[pattern-runtime] AC4 tests for Port trait + PortHandler + library integration`
<!-- END_TASK_9 -->

<!-- END_SUBCOMPONENT_D -->

---

## Open questions for human review (foreground at end of plan-write)

**Q1 [resolved 2026-04-24 → explicit prereq check]:** Per-port granularity (`cap.has_port(&port_id)`) is required for AC4.7/4.9. Phase 4 Task 6 now contains an explicit verification step that runs *before* execution begins: confirm Plan 3 exposes per-port granularity (not just `has_port_effect()`). If Plan 3 lands narrower, surface as a scope question, do not stub.

**Q2: SubscriptionId vs PortId-keyed subscriptions.** The dispatcher tracks one active subscription per `(session_key, port_id)` — re-subscribing replaces the prior. Alternative: assign a SubscriptionId per call so an agent can have multiple parallel subscriptions to the same port. Defaulted to one-per-port (simpler, matches typical use); flag if reviewer wants the SubscriptionId model.

**Q3 [resolved 2026-04-24]:** Originally proposed `Handle::block_on` from the handler. Updated: handler dispatches via crossbeam to the dispatcher actor task on the runtime's tokio runtime, and waits for reply via `recv_timeout`. **No `block_on` against arbitrary plugin code.** Plugin's `port.call().await` runs on the actor task; if it hangs, only that task hangs (handler is protected by `CALL_TIMEOUT = 60s`). Eval worker stays sync.

**Q4: `library()` returning `&'static str` vs `Cow<'static, str>` or `Arc<str>`.** Plan defaults to `&'static str` because typical port libraries are `include_str!` or `concat!` literals. Plugins building libraries at runtime would need `Box::leak`. Same trade-off discussion as Phase 1's bridge-extension `&'static str`; defaulted to the same answer. Flag if reviewer wants `Cow` or `Arc<str>` for plugin flexibility.

**Q5: Naming.** Trait `Port`, registry `PortRegistry`, types `PortId`/`PortMetadata`/`PortEvent`/`PortError`/`PortCapabilities`. All inherited from the design glossary. `Port` is generic enough that name clashes are possible (already overloaded with TCP/UDP/etc.). Alternatives: `Service`, `Connector`, `Endpoint`. `Endpoint` collides with `pattern_core::traits::endpoint::Endpoint` (message routing endpoint — different concept). `Service` is even more overloaded. Defaulted to `Port` per design plan; flag if reviewer wants a rename.
