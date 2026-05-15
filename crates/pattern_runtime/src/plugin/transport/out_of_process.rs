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
use pattern_core::traits::plugin::{PluginContext, PluginError};
use pattern_core::traits::plugin::wire::WirePortDeclaration;

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
    /// Plugin's resolved root directory (where its binary + state live). Wire
    /// conversions need this — PluginContext's `plugin_root` is host-side, but
    /// what the plugin sees as its root may differ (e.g. mount-relative paths).
    plugin_root: std::path::PathBuf,
    /// User config from the plugin's registry entry. Passed to the plugin
    /// at lifecycle events as WireJson.
    user_config: serde_json::Value,
    /// Effective capabilities for this plugin. For v1, this is the registry's
    /// `capability_overrides` if set, else the default permissive set. The
    /// plugin SDK enforces these guest-side.
    effective_capabilities: pattern_core::CapabilitySet,
}

impl OutOfProcessPluginConnection {
    pub async fn spawn(
        plugin_id: impl Into<SmolStr>,
        binary_path: PathBuf,
        expected_pubkey: PublicKey,
        daemon_endpoint: Endpoint,
        plugin_root: PathBuf,
        user_config: serde_json::Value,
        effective_capabilities: pattern_core::CapabilitySet,
    ) -> Result<Self, OopSpawnError> {
        let plugin_id: SmolStr = plugin_id.into();

        let _ = PluginState::clear(&plugin_id);

        // Capture plugin stderr to a per-plugin log file so plugin tracing
        // output isn't lost to the void. Daemon uses tracing-appender direct-to-
        // file and its own stderr typically goes nowhere when detached, so
        // inheriting daemon stdio would drop plugin output.
        let plugin_log_path = plugin_root.join("plugin.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&plugin_log_path)
            .map_err(|source| OopSpawnError::Spawn {
                plugin_id: plugin_id.clone(),
                source,
            })?;
        let stderr_file = log_file.try_clone().map_err(|source| OopSpawnError::Spawn {
            plugin_id: plugin_id.clone(),
            source,
        })?;

        let child = Command::new(&binary_path)
            .current_dir(&plugin_root)
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(stderr_file))
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
            plugin_root,
            user_config,
            effective_capabilities,
        })
    }

    /// Build a `WirePluginContext` from this connection's stashed registry info
    /// plus the incoming `PluginContext`. Host-only fields (`hook_bus`,
    /// `memory_store`, `scope`) are dropped — the plugin reaches those via
    /// `PluginHostProtocol` rather than directly.
    fn build_wire_context(
        &self,
        ctx: &PluginContext,
    ) -> pattern_core::traits::plugin::wire::WirePluginContext {
        pattern_core::traits::plugin::wire::WirePluginContext {
            plugin_id: ctx.plugin_id.clone(),
            plugin_root: self.plugin_root.clone(),
            mount_path: ctx.mount_path.clone(),
            user_config: pattern_core::traits::plugin::wire::WireJson::from_value(&self.user_config)
                .unwrap_or_else(|_| pattern_core::traits::plugin::wire::WireJson("null".to_string())),
            effective_capabilities: self.effective_capabilities.clone(),
        }
    }
}

#[async_trait]
impl PluginConnection for OutOfProcessPluginConnection {
    fn plugin_id(&self) -> &SmolStr { &self.plugin_id }

    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let wire_ctx = self.build_wire_context(ctx);
        self.client.rpc(pattern_core::plugin::protocol::OnInstallRequest(wire_ctx))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop on_install rpc: {e}")))?
            .map_err(|e| PluginError::HostCallback(format!("oop on_install plugin: {e:?}")))?;
        Ok(())
    }
    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let wire_ctx = self.build_wire_context(ctx);
        self.client.rpc(pattern_core::plugin::protocol::OnEnableRequest(wire_ctx))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop on_enable rpc: {e}")))?
            .map_err(|e| PluginError::HostCallback(format!("oop on_enable plugin: {e:?}")))?;
        Ok(())
    }
    async fn on_disable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let wire_ctx = self.build_wire_context(ctx);
        self.client.rpc(pattern_core::plugin::protocol::OnDisableRequest(wire_ctx))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop on_disable rpc: {e}")))?
            .map_err(|e| PluginError::HostCallback(format!("oop on_disable plugin: {e:?}")))?;
        Ok(())
    }

    async fn declare_ports(&self) -> Result<Vec<WirePortDeclaration>, PluginError> {
        let wire = self.client.rpc(pattern_core::plugin::protocol::DeclarePortsRequest(()))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop declare_ports rpc: {e}")))?;
        Ok(wire)
    }

    async fn library(&self) -> Result<Option<String>, PluginError> {
        let result = self.client.rpc(pattern_core::plugin::protocol::GetLibraryRequest(()))
            .await
            .map_err(|e| PluginError::HostCallback(format!("oop library rpc: {e}")))?;
        Ok(result.map(|s| s.to_string()))
    }

    async fn on_event(&self, event: HookEvent) -> Result<Option<HookResponse>, PluginError> {
        // HookEvent is already wire-safe (postcard-compatible by construction).
        // Route to OnHookEvent (notification) or OnHookEventBlocking based on semantics.
        use pattern_core::hooks::event::HookSemantics;
        match event.semantics {
            HookSemantics::Notification => {
                self.client.rpc(pattern_core::plugin::protocol::OnHookEventRequest(event))
                    .await
                    .map_err(|e| PluginError::HostCallback(format!("oop on_event(notify) rpc: {e}")))?;
                Ok(None)
            }
            HookSemantics::Blocking => {
                let wire_resp = self.client.rpc(pattern_core::plugin::protocol::OnHookEventBlockingRequest(event))
                    .await
                    .map_err(|e| PluginError::HostCallback(format!("oop on_event(blocking) rpc: {e}")))?;
                // Convert WireHookResponse → HookResponse. Only Modify needs decoding.
                use pattern_core::traits::plugin::wire::WireHookResponse;
                let resp = match wire_resp {
                    WireHookResponse::Continue => HookResponse::Continue,
                    WireHookResponse::Block { reason } => HookResponse::Block { reason },
                    WireHookResponse::Modify(wire_json) => {
                        let value = wire_json.parse().map_err(|e| {
                            PluginError::HostCallback(format!("oop on_event: decode Modify payload: {e}"))
                        })?;
                        HookResponse::Modify(value)
                    }
                    _ => return Err(PluginError::HostCallback(
                        "oop on_event: unknown WireHookResponse variant".into(),
                    )),
                };
                Ok(Some(resp))
            }
            // HookSemantics is non_exhaustive — future variants default to
            // fire-and-forget notification semantics rather than panicking.
            _ => {
                self.client.rpc(pattern_core::plugin::protocol::OnHookEventRequest(event))
                    .await
                    .map_err(|e| PluginError::HostCallback(format!("oop on_event(unknown semantics): {e}")))?;
                Ok(None)
            }
        }
    }

    fn health(&self) -> PluginHealth { self.health.lock().clone() }

    async fn terminate(&self) {
        // Take the Child handle out of the slot; sending it kill_on_drop is
        // not enough during runtime teardown. Send SIGTERM explicitly so the
        // plugin gets a chance to flush its serenity gateway + drop its iroh
        // endpoint cleanly; then wait briefly. If still alive after the grace
        // period, kill_on_drop on the dropped Child will SIGKILL.
        let child_opt = self._child.lock().take();
        let Some(mut child) = child_opt else { return };
        let pid = child.id();
        tracing::info!(plugin_id = %self.plugin_id, ?pid, "sending SIGTERM to OOP plugin");
        #[cfg(unix)]
        if let Some(pid) = pid {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        // Wait up to 2s for graceful exit. If still running, drop Child →
        // kill_on_drop fires SIGKILL.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            child.wait(),
        ).await;
    }

    async fn port_call(
        &self,
        port_id: &pattern_core::types::port::PortId,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, pattern_core::types::port::PortError> {
        use pattern_core::traits::plugin::wire::{WireJson, WirePortCallRequest};
        let req = WirePortCallRequest {
            port_id: port_id.clone(),
            method: method.into(),
            payload: WireJson::from_value(&payload).map_err(|e| {
                pattern_core::types::port::PortError::BadPayload {
                    port: port_id.clone(),
                    method: method.to_string(),
                    message: format!("encode payload: {e}"),
                }
            })?,
        };
        let resp = self.client.rpc(req).await.map_err(|e| {
            pattern_core::types::port::PortError::CallFailed(
                port_id.clone(),
                format!("oop port_call rpc: {e}"),
            )
        })?;
        match resp {
            Ok(wire_json) => wire_json.parse().map_err(|e| {
                pattern_core::types::port::PortError::CallFailed(
                    port_id.clone(),
                    format!("decode response: {e}"),
                )
            }),
            Err(wire_err) => Err(wire_port_error_to_port_error(port_id, method, wire_err)),
        }
    }

    async fn port_subscribe(
        &self,
        port_id: &pattern_core::types::port::PortId,
        config: serde_json::Value,
    ) -> Result<
        futures::stream::BoxStream<'static, pattern_core::types::port::PortEvent>,
        pattern_core::types::port::PortError,
    > {
        use pattern_core::traits::plugin::wire::{WireJson, WirePortStreamItem, WirePortSubscribeRequest};
        let req = WirePortSubscribeRequest {
            port_id: port_id.clone(),
            config: WireJson::from_value(&config).map_err(|e| {
                pattern_core::types::port::PortError::BadPayload {
                    port: port_id.clone(),
                    method: "subscribe".to_string(),
                    message: format!("encode config: {e}"),
                }
            })?,
        };
        let rx = self.client.server_streaming(req, 64).await.map_err(|e| {
            pattern_core::types::port::PortError::SubscribeFailed(
                port_id.clone(),
                format!("oop port_subscribe open: {e}"),
            )
        })?;
        // Convert irpc mpsc → BoxStream<PortEvent> via futures::stream::unfold.
        // Drops on Done variant or on rx closure.
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(Some(WirePortStreamItem::Event(ev))) => {
                        if let Ok(payload) = ev.payload.parse() {
                            return Some((
                                pattern_core::types::port::PortEvent::new(
                                    ev.port_id, payload, ev.at,
                                ),
                                rx,
                            ));
                        }
                        // decode failed — skip + continue loop
                    }
                    Ok(Some(WirePortStreamItem::Done { .. })) | Ok(None) | Err(_) => return None,
                    _ => continue,
                }
            }
        });
        Ok(Box::pin(stream))
    }
}

fn wire_port_error_to_port_error(
    port_id: &pattern_core::types::port::PortId,
    method: &str,
    err: pattern_core::traits::plugin::wire::WirePortError,
) -> pattern_core::types::port::PortError {
    use pattern_core::traits::plugin::wire::WirePortError;
    use pattern_core::types::port::PortError;
    match err {
        WirePortError::NotFound { port_id } => PortError::NotFound(port_id),
        WirePortError::NotSubscribable { port_id } => PortError::NotSubscribable(port_id),
        WirePortError::MethodNotFound { port_id, method: m } => PortError::UnsupportedMethod {
            port: port_id,
            method: m.to_string(),
        },
        WirePortError::InvalidPayload { reason } => PortError::BadPayload {
            port: port_id.clone(),
            method: method.to_string(),
            message: reason.to_string(),
        },
        WirePortError::CallFailed { port_id, message } => {
            PortError::CallFailed(port_id, message.to_string())
        }
        WirePortError::RateLimited { retry_after_secs } => PortError::CallFailed(
            port_id.clone(),
            format!("rate limited; retry after {retry_after_secs}s"),
        ),
        _ => PortError::CallFailed(port_id.clone(), "unknown wire port error".into()),
    }
}
