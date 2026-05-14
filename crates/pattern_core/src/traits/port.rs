//! Port trait: the agent's unified call/subscribe interface to an external service.
//!
//! One implementation per concrete service (an `HttpPort` for HTTP, a
//! `SlackPort` for Slack, etc.). Runtime-provided ports register at startup
//! via the runtime's `PortRegistry`; plugin-registered ports register at
//! plugin load time (Plan 4 — v3-extensibility).
//!
//! Configuration is convention-based: ports that need initialisation expose
//! a `"configure"` method. Callers invoke
//! `Port::call("configure", config_json)` before any other method. The
//! `requires_configuration` flag in [`PortCapabilities`] advertises this
//! requirement so agents learn about it from `Port.List`.
//!
//! # Downcasting via `as_any`
//!
//! Tools that need typed access to a specific port implementation downcast via
//! [`Port::as_any`]. The `PortRegistry` returns trait objects; the consumer
//! downcasts to the concrete type at the point of use.

use std::any::Any;

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::types::port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};

/// The agent's unified call/subscribe interface to an external service.
///
/// Each concrete service implements this trait. Runtime-provided ports
/// register at startup; plugin-registered ports (Plan 4 — v3-extensibility)
/// register at plugin load time. The registry returns `Arc<dyn Port>` trait
/// objects.
///
/// # Example
///
/// ```no_run
/// use std::any::Any;
/// use async_trait::async_trait;
/// use futures::stream::BoxStream;
/// use pattern_core::traits::port::Port;
/// use pattern_core::types::port::{
///     PortCapabilities, PortError, PortEvent, PortId, PortMetadata,
/// };
///
/// #[derive(Debug)]
/// struct Dummy;
///
/// #[async_trait]
/// impl Port for Dummy {
///     fn id(&self) -> &PortId {
///         unimplemented!("dummy: satisfaction-only example")
///     }
///
///     fn metadata(&self) -> PortMetadata {
///         PortMetadata::new(PortId::new("dummy"), "A dummy port for illustration")
///     }
///
///     fn capabilities(&self) -> PortCapabilities {
///         // PortCapabilities is #[non_exhaustive]; use the builder methods.
///         PortCapabilities::default().with_callable(true)
///     }
///
///     async fn subscribe(
///         &self,
///         _config: serde_json::Value,
///     ) -> Result<BoxStream<'static, PortEvent>, PortError> {
///         Err(PortError::NotSubscribable(PortId::new("dummy")))
///     }
///
///     async fn call(
///         &self,
///         _method: &str,
///         _payload: serde_json::Value,
///     ) -> Result<serde_json::Value, PortError> {
///         Ok(serde_json::json!({"ok": true}))
///     }
///
///     fn as_any(&self) -> &dyn Any {
///         self
///     }
/// }
/// ```
#[async_trait]
pub trait Port: Send + Sync + std::fmt::Debug {
    /// The port's stable identifier.
    fn id(&self) -> &PortId;

    /// Human-readable metadata (description, version, method list). Called
    /// by `Port.List` to enumerate available ports for the agent.
    fn metadata(&self) -> PortMetadata;

    /// Runtime capability flags: `subscribable`, `callable`,
    /// `requires_configuration`. Informational — the port enforces its own
    /// invariants internally; this surface lets `Port.List` describe the
    /// port to agents before they attempt operations.
    fn capabilities(&self) -> PortCapabilities;

    /// Subscribe to this port's event stream.
    ///
    /// Returns a boxed stream of [`PortEvent`]s. The runtime drains the
    /// stream via a dispatcher actor task and surfaces events as
    /// `MessageAttachment::PortEvent` entries on the next agent turn.
    ///
    /// Implementations may close the stream at any time (server disconnect,
    /// rate-limit, etc.). The runtime handles stream-end gracefully: the
    /// subscription is silently cleaned up and no further events arrive.
    async fn subscribe(
        &self,
        config: serde_json::Value,
    ) -> Result<BoxStream<'static, PortEvent>, PortError>;

    /// Disable a previously-installed subscription.
    ///
    /// Symmetric pair with [`subscribe`]. Ports that maintain server-side
    /// state (active forwarders, registered listeners, etc) tear it down
    /// here. Ports whose subscriptions are purely stream-lifetime can keep
    /// the default no-op impl — dropping the `BoxStream` is sufficient for
    /// those.
    ///
    /// The Haskell SDK exposes `Port.unsubscribe`; this trait method is the
    /// runtime-side surface it dispatches into.
    async fn unsubscribe(&self) -> Result<(), PortError> {
        Ok(())
    }

    /// One-shot call to a named method.
    ///
    /// `method` is a plain string; `payload` is a JSON value. Returns a JSON
    /// response on success. The `"configure"` method name is the conventional
    /// setup entrypoint for ports that set `requires_configuration = true`.
    async fn call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, PortError>;

    /// Optional Haskell helper source compiled into the agent's prelude
    /// when the port is in the agent's `CapabilitySet`.
    ///
    /// Conventionally: typed wrappers around `Port.Call(id, method, payload)`
    /// so agents write ergonomic Haskell helpers (e.g., `Http.get url`)
    /// rather than constructing JSON by hand.
    ///
    /// Returns `SmolStr` so compile-time literals stay zero-alloc via
    /// `SmolStr::new_static(...)`, runtime-built strings allocate normally,
    /// and the wire format (Phase 6 `WirePortDeclaration`) is a simple
    /// string carried across the plugin-IRPC boundary.
    ///
    /// Returns `None` when the port provides no Haskell helpers.
    fn library(&self) -> Option<smol_str::SmolStr> {
        None
    }

    /// Downcast escape hatch.
    ///
    /// Lets specialized consumers reach the concrete port implementation.
    /// Use `as_any().downcast_ref::<ConcretePort>()` at the call site.
    fn as_any(&self) -> &dyn Any;
}
