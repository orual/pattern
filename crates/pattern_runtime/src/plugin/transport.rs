//! Plugin transport abstraction (Phase 6 Task 4).
//!
//! [`PluginConnection`] decouples the runtime from how a plugin is reached.
//! The in-process variant ([`InProcessPluginConnection`]) wraps a
//! `PluginExtension` trait object — zero serialization, direct vtable
//! dispatch. The out-of-process variant (Task 5) lives in
//! `transport/out_of_process.rs` and goes through IRPC over QUIC.
//!
//! Runtime code calls into `Box<dyn PluginConnection>` uniformly. CC + MCP
//! adapters wrap their concrete `PluginExtension` impls with
//! `InProcessPluginConnection`; native out-of-process plugins are wrapped
//! with `OutOfProcessPluginConnection` at install time.

use std::sync::Arc;

use async_trait::async_trait;
use smol_str::SmolStr;

use pattern_core::hooks::event::{HookEvent, HookResponse};
use pattern_core::traits::plugin::{PluginContext, PluginError, PluginExtension};
use pattern_core::traits::plugin::wire::WirePortDeclaration;
use pattern_core::traits::port::Port;

/// Health snapshot for a plugin connection.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PluginHealth {
    /// Connection is alive and processing requests.
    Healthy,
    /// Connection is degraded or down.
    Unhealthy { reason: SmolStr },
}

/// Transport-agnostic plugin interface.
///
/// Runtime code calls into `Arc<dyn PluginConnection>` uniformly. Two
/// implementations exist:
/// - [`InProcessPluginConnection`] for plugins compiled into the daemon
///   (CC adapter, MCP adapter, future native plugins linked at build-time).
/// - `OutOfProcessPluginConnection` (Task 5) for plugins running as their
///   own process, reached via IRPC over QUIC.
#[async_trait]
pub trait PluginConnection: Send + Sync + std::fmt::Debug {
    /// Plugin id (for logging / routing).
    fn plugin_id(&self) -> &SmolStr;

    /// Lifecycle: install. Called once when added to the registry.
    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Lifecycle: enable. Called when bound to a session/runtime context.
    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Lifecycle: disable. Called when detached or session ends.
    async fn on_disable(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Declared ports / tools.
    /// Wire-friendly port declarations (id + metadata + capabilities + library).
    /// Daemon side uses these to register `Port` impls in the `PortRegistry`.
    /// In-process: derived from `port_impls()`. OOP: comes over the wire.
    async fn declare_ports(&self) -> Result<Vec<WirePortDeclaration>, PluginError>;

    /// In-process Port impls. For OOP plugins, returns `None` — the daemon
    /// builds wire-backed proxies from `declare_ports()` instead.
    fn port_impls(&self) -> Option<Vec<Arc<dyn Port>>> { None }

    /// Forward an agent's `Port.call` to this plugin's port impl. In-process:
    /// looks up the port in `port_impls()` by id, calls directly. OOP: sends
    /// wire `PortCall` to the plugin process.
    async fn port_call(
        &self,
        port_id: &pattern_core::types::port::PortId,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, pattern_core::types::port::PortError> {
        let _ = (method, payload);
        Err(pattern_core::types::port::PortError::CallFailed(
            port_id.clone(),
            "port_call: default trait impl — connection must override".into(),
        ))
    }

    /// Forward an agent's `Port.subscribe` to this plugin's port impl. Returns
    /// a stream of `PortEvent`s. In-process: delegates to in-proc `Port::subscribe`.
    /// OOP: opens wire stream, converts items.
    async fn port_subscribe(
        &self,
        port_id: &pattern_core::types::port::PortId,
        config: serde_json::Value,
    ) -> Result<
        futures::stream::BoxStream<'static, pattern_core::types::port::PortEvent>,
        pattern_core::types::port::PortError,
    > {
        let _ = config;
        Err(pattern_core::types::port::PortError::SubscribeFailed(
            port_id.clone(),
            "port_subscribe: default trait impl — connection must override".into(),
        ))
    }

    /// Forward an agent's `Port.unsubscribe` to this plugin's port impl.
    /// Symmetric pair with [`port_subscribe`]. Default impl is a no-op,
    /// matching the [`Port::unsubscribe`] trait default.
    async fn port_unsubscribe(
        &self,
        port_id: &pattern_core::types::port::PortId,
    ) -> Result<(), pattern_core::types::port::PortError> {
        let _ = port_id;
        Ok(())
    }

    /// Optional Haskell prelude library shipped by the plugin.
    async fn library(&self) -> Result<Option<String>, PluginError>;

    /// Hook event dispatch. Returns `Some(HookResponse)` for blocking events.
    async fn on_event(&self, event: HookEvent) -> Result<Option<HookResponse>, PluginError>;

    /// Connection health snapshot. Out-of-process variants surface reconnect
    /// state here; in-process is always `Healthy`.
    fn health(&self) -> PluginHealth {
        PluginHealth::Healthy
    }
}

// ── In-process transport ─────────────────────────────────────────────────────

/// In-process plugin connection: direct trait dispatch into a wrapped
/// [`PluginExtension`]. Zero serialization overhead (vtable call only).
#[derive(Debug)]
pub struct InProcessPluginConnection {
    extension: Arc<dyn PluginExtension>,
    plugin_id: SmolStr,
}

impl InProcessPluginConnection {
    pub fn new(extension: Arc<dyn PluginExtension>, plugin_id: impl Into<SmolStr>) -> Self {
        Self {
            extension,
            plugin_id: plugin_id.into(),
        }
    }
}

#[async_trait]
impl PluginConnection for InProcessPluginConnection {
    fn plugin_id(&self) -> &SmolStr {
        &self.plugin_id
    }

    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        self.extension.on_install(ctx).await
    }

    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        self.extension.on_enable(ctx).await
    }

    async fn on_disable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        self.extension.on_disable(ctx).await
    }

    async fn declare_ports(&self) -> Result<Vec<WirePortDeclaration>, PluginError> {
        // Derive WirePortDeclaration from each in-process Port impl's metadata.
        Ok(self.extension.ports().into_iter().map(|p| WirePortDeclaration {
            id: p.id().clone(),
            metadata: p.metadata(),
            capabilities: p.capabilities(),
            library: p.library(),
        }).collect())
    }

    fn port_impls(&self) -> Option<Vec<Arc<dyn Port>>> {
        Some(self.extension.ports())
    }

    async fn port_call(
        &self,
        port_id: &pattern_core::types::port::PortId,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, pattern_core::types::port::PortError> {
        let ports = self.extension.ports();
        let port = ports.iter().find(|p| p.id() == port_id)
            .ok_or_else(|| pattern_core::types::port::PortError::NotFound(port_id.clone()))?;
        port.call(method, payload).await
    }

    async fn port_subscribe(
        &self,
        port_id: &pattern_core::types::port::PortId,
        config: serde_json::Value,
    ) -> Result<
        futures::stream::BoxStream<'static, pattern_core::types::port::PortEvent>,
        pattern_core::types::port::PortError,
    > {
        let ports = self.extension.ports();
        let port = ports.iter().find(|p| p.id() == port_id)
            .ok_or_else(|| pattern_core::types::port::PortError::NotFound(port_id.clone()))?;
        port.subscribe(config).await
    }

    async fn port_unsubscribe(
        &self,
        port_id: &pattern_core::types::port::PortId,
    ) -> Result<(), pattern_core::types::port::PortError> {
        let ports = self.extension.ports();
        let port = ports.iter().find(|p| p.id() == port_id)
            .ok_or_else(|| pattern_core::types::port::PortError::NotFound(port_id.clone()))?;
        port.unsubscribe().await
    }

    async fn library(&self) -> Result<Option<String>, PluginError> {
        Ok(self.extension.library().map(String::from))
    }

    async fn on_event(&self, event: HookEvent) -> Result<Option<HookResponse>, PluginError> {
        Ok(self.extension.on_event(&event))
    }

    fn health(&self) -> PluginHealth {
        PluginHealth::Healthy
    }
}

pub mod out_of_process;
pub use out_of_process::{OopSpawnError, OutOfProcessPluginConnection};
