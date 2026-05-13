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
//! 5. Dialing the daemon's `pattern-plugin-host/1` ALPN to obtain a PluginHost client
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
use pattern_core::traits::plugin::PluginExtension;

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
pub async fn register_plugin<P>(plugin_id: PluginId, _plugin: P) -> Result<PluginHandle, RegisterError>
where
    P: PluginExtension + Send + Sync + 'static,
{

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

    let guest_client = spawn_guest_stub();
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

// ─── Guest handler stub (v1) ─────────────────────────────────────────────────

/// Spawn the guest-side stub actor. Returns a Client<PluginGuestProtocol> whose
/// `as_local()` is passed to `PluginGuestProtocol::remote_handler` for Router accept.
fn spawn_guest_stub() -> Client<PluginGuestProtocol> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run_guest(rx));
    Client::local(tx)
}

async fn run_guest(mut rx: mpsc::Receiver<PluginGuestMessage>) {
    while let Some(msg) = rx.recv().await {
        handle_guest(msg).await;
    }
}

fn pe(m: &str) -> WirePluginError {
    WirePluginError::Unimplemented { method: m.into() }
}

async fn handle_guest(msg: PluginGuestMessage) {
    use irpc::WithChannels;
    use PluginGuestMessage::*;
    match msg {
        OnInstall(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("OnInstall"))).await; }
        OnEnable(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("OnEnable"))).await; }
        OnDisable(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("OnDisable"))).await; }
        DeclarePorts(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Vec::new()).await; }
        GetLibrary(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(None).await; }
        OnHookEvent(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(()).await; let _ = pe; }
        OnHookEventBlocking(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(WireHookResponse::Continue).await; }
        PortCall(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(WirePortError::MethodNotFound { port_id: req_method_unreachable(), method: "<stub>".into() })).await; }
        PortSubscribe(req) => { let WithChannels { tx, .. } = req; drop(tx); }
    }
}

fn req_method_unreachable() -> pattern_core::types::port::PortId {
    // Stub returns an unused error variant; consumer never sees this path under
    // production routing (real dispatch comes when fixture plugin lands).
    pattern_core::types::port::PortId::new("stub")
}
