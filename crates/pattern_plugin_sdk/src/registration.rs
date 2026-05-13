//! Plugin SDK entry point: `register_plugin`.
//!
//! Phase 6 Task 5e: plugin process startup. The plugin author calls this from their
//! `main()` after constructing their `PluginExtension`. It handles:
//!
//! 1. Reading the daemon-injected env vars (PATTERN_PLUGIN_ID, PATTERN_DAEMON_PUBKEY,
//!    PATTERN_DAEMON_ADDR).
//! 2. Loading or generating the plugin's iroh keypair via `PluginKeyStore`.
//! 3. Opening an iroh::Endpoint with the plugin's keypair.
//! 4. Registering the `pattern-plugin-guest/1` ALPN accept on the endpoint's Router so
//!    the daemon can dial back with lifecycle/hook/port calls (v1: stub handler that
//!    returns Unimplemented; real dispatch into the PluginExtension impl lands when a
//!    fixture plugin exists to exercise it — parallel shape to the 5b-accept stub).
//! 5. Dialing the daemon's `pattern-plugin-host/1` ALPN to obtain a HostApi client
//!    for plugin→runtime calls (memory ops, HostSendMessage, etc.).
//! 6. Blocking until process shutdown (ctrl-c) or endpoint closure.

use std::net::SocketAddr;
use std::sync::Arc;

use iroh::{Endpoint, EndpointAddr, PublicKey, SecretKey, TransportAddr, endpoint::presets};
use iroh::protocol::Router;
use irpc::Client;
use irpc::rpc::RemoteService;
use irpc_iroh::IrohProtocol;
use smol_str::SmolStr;
use tokio::sync::mpsc;

use pattern_core::daemon_state::{DaemonState, PluginState};
use pattern_core::plugin::auth::{KeyStoreError, PluginKeyStore};
use pattern_core::plugin::protocol::{
    PLUGIN_GUEST_ALPN, PLUGIN_HOST_ALPN, PluginGuestMessage, PluginGuestProtocol,
    PluginHostProtocol,
};
use pattern_core::plugin::PluginId;
use pattern_core::traits::plugin::wire::*;
use pattern_core::traits::plugin::{PluginContext, PluginExtension};

/// Errors register_plugin can raise before the run loop starts.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("register_plugin: failed to load daemon state: {source}")]
    DaemonState { #[source] source: std::io::Error },
    #[error("register_plugin: invalid daemon state field {field}: {message}")]
    InvalidDaemonState { field: &'static str, message: SmolStr },
    #[error("register_plugin: keystore: {0}")]
    KeyStore(#[from] KeyStoreError),
    #[error("register_plugin: iroh endpoint bind failed: {message}")]
    EndpointBind { message: SmolStr },
    #[error("register_plugin: failed to publish plugin state.json: {source}")]
    PluginStateWrite { #[source] source: std::io::Error },
    #[error("register_plugin: io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Plugin registration handle. Returned by `register_plugin` after setup completes.
/// Holds the host client for plugin→daemon calls and the Router so its drop is observable.
pub struct PluginHandle {
    /// Client for calling into the daemon's pattern-plugin-host/1 protocol.
    pub host: Client<PluginHostProtocol>,
    /// Daemon-spawned router accepting the guest protocol from the daemon side.
    _router: Router,
    /// Plugin's iroh endpoint (kept alive for the connection lifecycle).
    _endpoint: Endpoint,
}

/// Entry point for an out-of-process plugin. Sets up auth + transport + lifecycle wiring
/// against the daemon, then returns a handle the plugin's main loop holds for the duration
/// of the process. Dropping the handle tears down the router + endpoint.
///
/// V1 stub: the guest-side ALPN accept routes incoming PluginGuestProtocol messages to a
/// no-op actor returning Unimplemented. Real dispatch into the `plugin` impl lands when a
/// fixture plugin exists to exercise it.
pub async fn register_plugin<P>(plugin_id: PluginId, plugin: P) -> Result<PluginHandle, RegisterError>
where
    P: PluginExtension + Send + Sync + 'static,
{
    // `--pattern-plugin-init` mode: triggered by `pattern plugin install` after
    // copying the binary into the cache. Generates the plugin keypair (or loads
    // existing) via PluginKeyStore, prints `{plugin_id, pubkey, sdk_version}` JSON
    // to stdout, exits zero. Doesn't enter the bind-and-serve loop.
    if std::env::args().any(|a| a == "--pattern-plugin-init") {
        let sk = PluginKeyStore::load_or_generate(&plugin_id)?;
        let pubkey = sk.public().to_string();
        let info = serde_json::json!({
            "plugin_id": plugin_id.as_str(),
            "pubkey": pubkey,
            "sdk_version": env!("CARGO_PKG_VERSION"),
        });
        println!("{}", serde_json::to_string(&info).expect("plugin-init info json"));
        std::process::exit(0);
    }

    let plugin: std::sync::Arc<dyn PluginExtension> = std::sync::Arc::new(plugin);

    let state = DaemonState::load().map_err(|source| RegisterError::DaemonState { source })?;
    let daemon_pubkey: PublicKey = state.node_id.parse().map_err(|e: <PublicKey as std::str::FromStr>::Err| RegisterError::InvalidDaemonState {
        field: "node_id",
        message: e.to_string().into(),
    })?;
    let daemon_addr: SocketAddr = state.addr;

    let plugin_sk: SecretKey = PluginKeyStore::load_or_generate(&plugin_id)?;

    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(plugin_sk)
        .bind()
        .await
        .map_err(|e| RegisterError::EndpointBind { message: e.to_string().into() })?;

    // Publish our addr + node_id so the daemon can dial back.
    let bound_addr = endpoint
        .bound_sockets()
        .into_iter()
        .next()
        .ok_or_else(|| RegisterError::EndpointBind { message: "no bound socket".into() })?;
    let plugin_state = PluginState {
        pid: std::process::id(),
        addr: bound_addr,
        node_id: endpoint.id().to_string(),
    };
    plugin_state.save(plugin_id.as_str()).map_err(|source| RegisterError::PluginStateWrite { source })?;

    let guest_client = spawn_guest(std::sync::Arc::clone(&plugin));
    let guest_local = guest_client
        .as_local()
        .expect("freshly-spawned guest client must be local");
    let guest_handler = PluginGuestProtocol::remote_handler(guest_local);

    let router = Router::builder(endpoint.clone())
        .accept(PLUGIN_GUEST_ALPN, IrohProtocol::new(guest_handler))
        .spawn();

    let daemon_endpoint_addr =
        EndpointAddr::new(daemon_pubkey).with_addrs([TransportAddr::Ip(daemon_addr)]);
    let host = irpc_iroh::client::<PluginHostProtocol>(
        endpoint.clone(),
        daemon_endpoint_addr,
        PLUGIN_HOST_ALPN,
    );

    tracing::info!(
        plugin_id = %plugin_id,
        daemon = %daemon_pubkey,
        "plugin registered with daemon"
    );

    Ok(PluginHandle {
        host,
        _router: router,
        _endpoint: endpoint,
    })
}

// ─── Guest handler dispatcher ───────────────────────────────────────────────

/// Spawn the guest-side dispatcher actor wired to the plugin's PluginExtension impl.
fn spawn_guest(plugin: std::sync::Arc<dyn PluginExtension>) -> Client<PluginGuestProtocol> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run_guest(rx, plugin));
    Client::local(tx)
}

async fn run_guest(
    mut rx: mpsc::Receiver<PluginGuestMessage>,
    plugin: std::sync::Arc<dyn PluginExtension>,
) {
    while let Some(msg) = rx.recv().await {
        let plugin = std::sync::Arc::clone(&plugin);
        tokio::spawn(async move { handle_guest(msg, plugin).await });
    }
}

/// Build a plugin-side `PluginContext` from a `WirePluginContext`. Local hook bus +
/// no memory store (memory ops route via `HostApi` client) + no scope.
fn ctx_from_wire(wire: pattern_core::traits::plugin::wire::WirePluginContext) -> PluginContext {
    PluginContext {
        plugin_id: wire.plugin_id,
        hook_bus: std::sync::Arc::new(pattern_core::hooks::HookBus::new()),
        plugin_root: wire.plugin_root,
        memory_store: None,
        scope: None,
    }
}

async fn handle_guest(
    msg: PluginGuestMessage,
    plugin: std::sync::Arc<dyn PluginExtension>,
) {
    use irpc::WithChannels;
    use PluginGuestMessage::*;
    use pattern_core::traits::plugin::wire::{WireHookResponse, WireJson};
    match msg {
        OnInstall(req) => {
            let WithChannels { tx, inner, .. } = req;
            let pattern_core::plugin::protocol::OnInstallRequest(wire_ctx) = inner;
            let ctx = ctx_from_wire(wire_ctx);
            let resp = plugin.on_install(&ctx).await
                .map_err(|e| WirePluginError::Unimplemented { method: format!("OnInstall failed: {e}").into() });
            let _ = tx.send(resp).await;
        }
        OnEnable(req) => {
            let WithChannels { tx, inner, .. } = req;
            let pattern_core::plugin::protocol::OnEnableRequest(wire_ctx) = inner;
            let ctx = ctx_from_wire(wire_ctx);
            let resp = plugin.on_enable(&ctx).await
                .map_err(|e| WirePluginError::Unimplemented { method: format!("OnEnable failed: {e}").into() });
            let _ = tx.send(resp).await;
        }
        OnDisable(req) => {
            let WithChannels { tx, inner, .. } = req;
            let pattern_core::plugin::protocol::OnDisableRequest(wire_ctx) = inner;
            let ctx = ctx_from_wire(wire_ctx);
            let resp = plugin.on_disable(&ctx).await
                .map_err(|e| WirePluginError::Unimplemented { method: format!("OnDisable failed: {e}").into() });
            let _ = tx.send(resp).await;
        }
        DeclarePorts(req) => {
            let WithChannels { tx, .. } = req;
            let decls: Vec<pattern_core::traits::plugin::wire::WirePortDeclaration> =
                plugin.ports().into_iter().map(|p| {
                    pattern_core::traits::plugin::wire::WirePortDeclaration {
                        id: p.id().clone(),
                        metadata: p.metadata(),
                        capabilities: p.capabilities(),
                        library: p.library(),
                    }
                }).collect();
            let _ = tx.send(decls).await;
        }
        GetLibrary(req) => {
            let WithChannels { tx, .. } = req;
            let lib = plugin.library().map(smol_str::SmolStr::from);
            let _ = tx.send(lib).await;
        }
        OnHookEvent(req) => {
            let WithChannels { tx, inner, .. } = req;
            let pattern_core::plugin::protocol::OnHookEventRequest(event) = inner;
            let _ = plugin.on_event(&event);
            let _ = tx.send(()).await;
        }
        OnHookEventBlocking(req) => {
            let WithChannels { tx, inner, .. } = req;
            let pattern_core::plugin::protocol::OnHookEventBlockingRequest(event) = inner;
            let resp = match plugin.on_event(&event) {
                None => WireHookResponse::Continue,
                Some(pattern_core::hooks::event::HookResponse::Continue) => WireHookResponse::Continue,
                Some(pattern_core::hooks::event::HookResponse::Block { reason }) => WireHookResponse::Block { reason },
                Some(pattern_core::hooks::event::HookResponse::Modify(v)) => {
                    WireHookResponse::Modify(WireJson::from_value(&v).unwrap_or(WireJson("null".into())))
                }
                _ => WireHookResponse::Continue,
            };
            let _ = tx.send(resp).await;
        }
        PortCall(req) => {
            let WithChannels { tx, inner, .. } = req;
            let payload_val = inner.payload.parse().unwrap_or(serde_json::Value::Null);
            let resp = match plugin.ports().iter().find(|p| p.id() == &inner.port_id) {
                None => Err(WirePortError::NotFound { port_id: inner.port_id.clone() }),
                Some(port) => match port.call(&inner.method, payload_val).await {
                    Ok(v) => match WireJson::from_value(&v) {
                        Ok(wj) => Ok(wj),
                        Err(e) => Err(WirePortError::InvalidPayload { reason: format!("encode response: {e}").into() }),
                    },
                    Err(e) => Err(WirePortError::CallFailed { port_id: inner.port_id, message: e.to_string().into() }),
                },
            };
            let _ = tx.send(resp).await;
        }
        PortSubscribe(req) => {
            let WithChannels { tx, inner, .. } = req;
            let config_val = inner.config.parse().unwrap_or(serde_json::Value::Null);
            let port_id = inner.port_id.clone();
            let ports = plugin.ports();
            let port = ports.iter().find(|p| p.id() == &port_id).cloned();
            tokio::spawn(async move {
                use pattern_core::traits::plugin::wire::{WirePortEvent, WirePortStreamItem};
                let Some(port) = port else {
                    let _ = tx.send(WirePortStreamItem::Done { reason: "port not found".into() }).await;
                    return;
                };
                let stream = match port.subscribe(config_val).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(WirePortStreamItem::Done { reason: format!("subscribe failed: {e}").into() }).await;
                        return;
                    }
                };
                use futures::StreamExt;
                let mut stream = stream;
                while let Some(ev) = stream.next().await {
                    let wire_ev = WirePortEvent {
                        port_id: ev.port_id,
                        payload: WireJson::from_value(&ev.payload).unwrap_or(WireJson("null".into())),
                        at: ev.at,
                    };
                    if tx.send(WirePortStreamItem::Event(wire_ev)).await.is_err() {
                        break;
                    }
                }
                let _ = tx.send(WirePortStreamItem::Done { reason: "stream ended".into() }).await;
            });
        }
    }
}
