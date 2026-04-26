//! Port identifier and supporting types.
//!
//! A `Port` is the agent's unified call/subscribe interface to an external
//! service. This module holds the pure data types — `PortId`, `PortMetadata`,
//! `PortCapabilities`, `PortEvent`, and `PortError` — that cross crate
//! boundaries in trait signatures. Execution machinery lives in
//! `pattern_runtime`.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Stable identifier for a port. Lowercase ASCII + hyphens by convention
/// (`http`, `slack`, `weather-api`). Plugins choose their own ID; the
/// registry rejects duplicates loudly at registration time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PortId(pub SmolStr);

impl PortId {
    /// Construct a `PortId` from any `SmolStr`-compatible value.
    pub fn new(s: impl Into<SmolStr>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for PortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl AsRef<str> for PortId {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl std::ops::Deref for PortId {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        self.0.as_str()
    }
}

impl<S: Into<SmolStr>> From<S> for PortId {
    fn from(s: S) -> Self {
        Self::new(s)
    }
}

/// Human-readable metadata for a registered port, returned by
/// `Port.List` so agents can discover what ports are available and what
/// operations they support.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PortMetadata {
    /// The port's stable identifier.
    pub id: PortId,
    /// Human-readable description for the agent's `Port.List` view.
    pub description: String,
    /// Optional version hint used in diagnostics and logs.
    pub version: Option<String>,
    /// Method names this port responds to via `call()`. Informational —
    /// not enforced at the trait level (a port may dispatch any method
    /// string), but agents read this from `Port.List` to discover the
    /// available surface area.
    pub methods: Vec<String>,
}

impl PortMetadata {
    /// Construct metadata with the required fields; `version` defaults to
    /// `None` and `methods` defaults to empty.
    pub fn new(id: impl Into<PortId>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            version: None,
            methods: Vec::new(),
        }
    }

    /// Builder: attach a version string.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Builder: attach the list of supported method names.
    #[must_use]
    pub fn with_methods(mut self, methods: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.methods = methods.into_iter().map(Into::into).collect();
        self
    }
}

/// Declares the runtime capabilities of a port.
///
/// Surfaces in `Port.List` so agents can understand what a port supports
/// before attempting to subscribe or call it. All fields default to
/// `false`; a port explicitly opts into each capability.
///
/// Construct via [`PortCapabilities::default`] (all false) or the
/// builder methods ([`PortCapabilities::with_callable`], etc.):
///
/// ```
/// use pattern_core::types::port::PortCapabilities;
///
/// let caps = PortCapabilities::default()
///     .with_callable(true)
///     .with_subscribable(true);
/// assert!(caps.callable);
/// assert!(caps.subscribable);
/// assert!(!caps.requires_configuration);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PortCapabilities {
    /// True if the port supports `subscribe()` (event-stream usage).
    /// Agents that try to subscribe to a non-subscribable port receive a
    /// clear `PortError::NotSubscribable` error; this flag lets `Port.List`
    /// surface "callable only" ports before the attempt.
    pub subscribable: bool,
    /// True if the port supports `call()`. Almost always true; a few
    /// pure-event-stream ports may set this `false`.
    pub callable: bool,
    /// True if the port's `call()` requires a prior `"configure"` call.
    /// `Port.List` surfaces this; agents that need to configure first call
    /// `Port.Call(id, "configure", config)` before any other method. Ports
    /// that require configuration enforce it internally and return
    /// `PortError::NotConfigured` from other methods until configuration
    /// is complete.
    pub requires_configuration: bool,
}

impl PortCapabilities {
    /// Builder: set whether the port supports `call()`.
    #[must_use]
    pub fn with_callable(mut self, callable: bool) -> Self {
        self.callable = callable;
        self
    }

    /// Builder: set whether the port supports `subscribe()`.
    #[must_use]
    pub fn with_subscribable(mut self, subscribable: bool) -> Self {
        self.subscribable = subscribable;
        self
    }

    /// Builder: set whether the port requires prior configuration.
    #[must_use]
    pub fn with_requires_configuration(mut self, requires_configuration: bool) -> Self {
        self.requires_configuration = requires_configuration;
        self
    }
}

/// A single event emitted by a subscribed port.
///
/// The dispatcher actor's drain task builds these from the
/// `BoxStream<PortEvent>` returned by `Port::subscribe`, then enqueues
/// them into the session's between-turn async-reminder buffer. The
/// compose-time drain splices them as `MessageAttachment::PortEvent`
/// entries onto the next turn's first user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PortEvent {
    /// The port that produced this event.
    pub port_id: PortId,
    /// Opaque event payload. Interpretation is port-specific; the agent
    /// reads it via the port's Haskell library wrappers (if any) or
    /// directly as JSON.
    pub payload: serde_json::Value,
    /// Wall-clock time at which the event was produced.
    pub at: jiff::Timestamp,
}

impl PortEvent {
    /// Construct a `PortEvent`. Required because the struct is
    /// `#[non_exhaustive]` — external callers can't use struct-literal
    /// syntax, and Port impls are by definition external to `pattern_core`.
    pub fn new(
        port_id: impl Into<PortId>,
        payload: serde_json::Value,
        at: jiff::Timestamp,
    ) -> Self {
        Self {
            port_id: port_id.into(),
            payload,
            at,
        }
    }
}

/// Errors arising from port operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PortError {
    /// No port with the given id is registered.
    #[error("port not found: {0}")]
    NotFound(PortId),

    /// The port does not implement the requested method.
    #[error("port {port} does not support method {method:?}")]
    UnsupportedMethod { port: PortId, method: String },

    /// The port requires configuration before `method` can be called.
    #[error("port {port} requires configuration before {method:?} (call \"configure\" first)")]
    NotConfigured { port: PortId, method: String },

    /// The port does not support subscriptions.
    #[error("port {0} is not subscribable")]
    NotSubscribable(PortId),

    /// The port's `call()` returned an error.
    #[error("port {0} call failed: {1}")]
    CallFailed(PortId, String),

    /// The port's `subscribe()` failed to establish the stream.
    #[error("subscription failed for {0}: {1}")]
    SubscribeFailed(PortId, String),

    /// The payload supplied to a port method could not be interpreted.
    #[error("invalid payload for {port}.{method}: {message}")]
    BadPayload {
        port: PortId,
        method: String,
        message: String,
    },

    /// The agent's `CapabilitySet` does not include this port.
    #[error("capability denied: port {0} not in agent's CapabilitySet")]
    CapabilityDenied(PortId),

    /// A port with this id is already registered. Returned by `PortRegistry::register`
    /// when a duplicate registration is attempted (I12 fix — explicit error
    /// rather than silent overwrite).
    #[error("port {0} is already registered")]
    AlreadyRegistered(PortId),

    /// The dispatcher actor's channel is closed, indicating the runtime is
    /// shutting down. Handlers should propagate this as a session-level
    /// error rather than retrying.
    #[error("port dispatcher actor closed (runtime shutting down?)")]
    DispatcherClosed,
}
