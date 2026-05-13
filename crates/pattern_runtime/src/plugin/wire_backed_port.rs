//! `WireBackedPort` — daemon-side `Port` impl that forwards calls + subscribes
//! to an out-of-process plugin over its `PluginConnection`. Built from a
//! `WirePortDeclaration` at session-open time; stashes metadata/capabilities
//! /library locally so synchronous accessors don't have to cross the wire.

use std::any::Any;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use futures::stream::BoxStream;
use smol_str::SmolStr;

use pattern_core::traits::plugin::wire::WirePortDeclaration;
use pattern_core::traits::port::Port;
use pattern_core::types::port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};

use super::transport::PluginConnection;

/// Daemon-side `Port` that forwards operations across a plugin connection.
///
/// Holds a `Weak<dyn PluginConnection>` so it doesn't keep the plugin alive
/// independent of the registry's primary handle. Cached fields
/// (`id`/`metadata`/`capabilities`/`library`) are populated from the
/// declaration at construction time, so `Port`'s sync accessors don't have
/// to roundtrip.
#[derive(Debug)]
pub struct WireBackedPort {
    id: PortId,
    metadata: PortMetadata,
    capabilities: PortCapabilities,
    library: Option<SmolStr>,
    connection: Weak<dyn PluginConnection>,
}

impl WireBackedPort {
    pub fn new(declaration: WirePortDeclaration, connection: Weak<dyn PluginConnection>) -> Self {
        Self {
            id: declaration.id,
            metadata: declaration.metadata,
            capabilities: declaration.capabilities,
            library: declaration.library,
            connection,
        }
    }

    fn upgrade(&self) -> Result<Arc<dyn PluginConnection>, PortError> {
        self.connection.upgrade().ok_or_else(|| {
            PortError::CallFailed(
                self.id.clone(),
                "plugin connection dropped - plugin process likely terminated".into(),
            )
        })
    }
}

#[async_trait]
impl Port for WireBackedPort {
    fn id(&self) -> &PortId {
        &self.id
    }

    fn metadata(&self) -> PortMetadata {
        self.metadata.clone()
    }

    fn capabilities(&self) -> PortCapabilities {
        self.capabilities.clone()
    }

    fn library(&self) -> Option<SmolStr> {
        self.library.clone()
    }

    async fn call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, PortError> {
        let conn = self.upgrade()?;
        conn.port_call(&self.id, method, payload).await
    }

    async fn subscribe(
        &self,
        config: serde_json::Value,
    ) -> Result<BoxStream<'static, PortEvent>, PortError> {
        let conn = self.upgrade()?;
        conn.port_subscribe(&self.id, config).await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
