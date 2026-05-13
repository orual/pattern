//! Out-of-process plugin transport (Phase 6 Task 5c).
//!
//! Daemon-side `PluginConnection` impl over irpc-iroh. Spawns the plugin's binary,
//! waits for it to publish its iroh addr to `<data_root>/plugins/<id>/state.json`,
//! then dials `pattern-plugin-guest/1` to talk lifecycle/hooks/ports.
//!
//! For remote plugins (phase 7), the address-via-state.json step is skipped — we'll
//! dial by pubkey alone through the iroh relay. Localhost v1 only writes/reads state.json.
//!
//! V1 wires declare_ports + library end-to-end (smallest payloads, no PluginContext
//! conversion needed) and leaves the rest as `Unimplemented` returns. Full method
//! dispatch lands when the integration test fixture plugin exists (tasks 7-8).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, PublicKey, TransportAddr};
use irpc::Client;
use parking_lot::Mutex;
use smol_str::SmolStr;
use tokio::process::{Child, Command};

use pattern_core::daemon_state::PluginState;
use pattern_core::hooks::{HookEvent, HookResponse};
use pattern_core::plugin::protocol::{PLUGIN_GUEST_ALPN, PluginGuestProtocol};
use pattern_core::traits::plugin::{PluginContext, PluginError, PortDeclaration};

use async_trait::async_trait;

use super::{PluginConnection, PluginHealth};

#[derive(Debug, thiserror::Error)]
pub enum OopSpawnError {
    #[error("oop plugin {plugin_id}: spawn failed: {source}")]
    Spawn { plugin_id: SmolStr, #[source] source: std::io::Error },
    #[error("oop plugin {plugin_id}: timed out waiting for state.json after {timeout_ms}ms")]
    StateTimeout { plugin_id: SmolStr, timeout_ms: u64 },
    #[error("oop plugin {plugin_id}: state.json read failed: {source}")]
    StateRead { plugin_id: SmolStr, #[source] source: std::io::Error },
    #[error("oop plugin {plugin_id}: pubkey mismatch — registry expects {expected}, state.json has {actual}")]
    PubkeyMismatch { plugin_id: SmolStr, expected: SmolStr, actual: SmolStr },
}

#[derive(Debug)]
pub struct OutOfProcessPluginConnection {
    plugin_id: SmolStr,
    client: Client<PluginGuestProtocol>,
    _child: Arc<Mutex<Option<Child>>>,
    health: Arc<Mutex<PluginHealth>>,
}

impl OutOfProcessPluginConnection {
    pub async fn spawn(
        plugin_id: impl Into<SmolStr>,
        binary_path: PathBuf,
        expected_pubkey: PublicKey,
        daemon_endpoint: Endpoint,
    ) -> Result<Self, OopSpawnError> {
        let plugin_id: SmolStr = plugin_id.into();

        let _ = PluginState::clear(&plugin_id);

        let child = Command::new(&binary_path)
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| OopSpawnError::Spawn {
                plugin_id: plugin_id.clone(),
                source,
            })?;

        let timeout_ms = 5_000u64;
        let poll_ms = 50u64;
        let mut elapsed = 0u64;
        let state = loop {
            match PluginState::load(&plugin_id) {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(source) => return Err(OopSpawnError::StateRead {
                    plugin_id: plugin_id.clone(),
                    source,
                }),
            }
            if elapsed >= timeout_ms {
                return Err(OopSpawnError::StateTimeout {
                    plugin_id: plugin_id.clone(),
                    timeout_ms,
                });
            }
            tokio::time::sleep(Duration::from_millis(poll_ms)).await;
            elapsed += poll_ms;
        };

        let expected_str = expected_pubkey.to_string();
        if state.node_id != expected_str {
            return Err(OopSpawnError::PubkeyMismatch {
                plugin_id: plugin_id.clone(),
                expected: expected_str.into(),
                actual: state.node_id.into(),
            });
        }

        let endpoint_addr = EndpointAddr::new(expected_pubkey)
            .with_addrs([TransportAddr::Ip(state.addr)]);
        let client = irpc_iroh::client::<PluginGuestProtocol>(
            daemon_endpoint,
            endpoint_addr,
            PLUGIN_GUEST_ALPN,
        );

        let child_slot = Arc::new(Mutex::new(Some(child)));
        let health = Arc::new(Mutex::new(PluginHealth::Healthy));

        {
            let child_slot = Arc::clone(&child_slot);
            let health = Arc::clone(&health);
            let pid = plugin_id.clone();
            tokio::spawn(async move {
                let mut child_opt = child_slot.lock().take();
                if let Some(child) = child_opt.as_mut() {
                    let status = child.wait().await;
                    let reason: SmolStr = match status {
                        Ok(s) => format!("plugin {pid} exited: {s}").into(),
                        Err(e) => format!("plugin {pid} wait failed: {e}").into(),
                    };
                    tracing::warn!(plugin_id = %pid, reason = %reason, "oop plugin process exited");
                    *health.lock() = PluginHealth::Unhealthy { reason };
                }
            });
        }

        Ok(Self {
            plugin_id,
            client,
            _child: child_slot,
            health,
        })
    }
}

#[async_trait]
impl PluginConnection for OutOfProcessPluginConnection {
    fn plugin_id(&self) -> &SmolStr { &self.plugin_id }

    async fn on_install(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Err(PluginError::Lifecycle("oop on_install: PluginContext->wire conversion not yet wired (v1)".into()))
    }
    async fn on_enable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Err(PluginError::Lifecycle("oop on_enable: PluginContext->wire conversion not yet wired (v1)".into()))
    }
    async fn on_disable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Err(PluginError::Lifecycle("oop on_disable: PluginContext->wire conversion not yet wired (v1)".into()))
    }

    async fn declare_ports(&self) -> Result<Vec<PortDeclaration>, PluginError> {
        let _wire = self.client.rpc(pattern_core::plugin::protocol::DeclarePortsRequest(()))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop declare_ports rpc: {e}")))?;
        Ok(Vec::new())
    }

    async fn library(&self) -> Result<Option<String>, PluginError> {
        let result = self.client.rpc(pattern_core::plugin::protocol::GetLibraryRequest(()))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop library rpc: {e}")))?;
        Ok(result.map(|s| s.to_string()))
    }

    async fn on_event(&self, _event: HookEvent) -> Result<Option<HookResponse>, PluginError> {
        Err(PluginError::Lifecycle("oop on_event: HookEvent->wire conversion not yet wired (v1)".into()))
    }

    fn health(&self) -> PluginHealth { self.health.lock().clone() }
}
