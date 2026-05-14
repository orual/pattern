//! Pattern daemon binary.
//!
//! Provides `start`, `stop`, and `status` subcommands for managing the
//! background daemon that owns the agent runtime and exposes it over QUIC.
//!
//! On `start`, the daemon:
//! 1. Checks whether an instance is already running (via state file + process check).
//! 2. Optionally mounts memory and builds provider infrastructure (unless `--echo`).
//! 3. Spawns the [`DaemonServer`] actor (echo mode or real session mode).
//! 4. Creates a QUIC endpoint with a self-signed certificate.
//! 5. Sets up a QUIC listener that forwards remote `PatternProtocol` messages
//!    into the actor's channel.
//! 6. Writes PID, bind address, and certificate to `~/.pattern/daemon/`.
//! 7. Blocks until SIGTERM or Ctrl-C, then cleans up state.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use clap::{Parser, Subcommand};
use tracing::info;

use irpc::rpc::RemoteService;
use pattern_server::protocol::PatternProtocol;
use pattern_server::server::{DaemonServer, SessionConfig};
use pattern_server::state::DaemonState;

#[derive(Parser)]
#[command(name = "pattern-server", about = "Pattern daemon process")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon.
    Start {
        /// Port to listen on (0 = OS-assigned).
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// Run in echo mode (no LLM, echoes messages back). Used for testing.
        #[arg(long)]
        echo: bool,
    },
    /// Stop a running daemon.
    Stop,
    /// Show daemon status.
    Status,
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,pattern_server=info,pattern_runtime=info,pattern_provider=info, pattern_db=info,pattern_memory=info,loro_internal=warn,loro=warn".into());
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Start { port, echo } => cmd_start(port, echo).await,
        Command::Stop => cmd_stop(),
        Command::Status => cmd_status(),
    }
}

async fn cmd_start(port: u16, echo: bool) -> miette::Result<()> {
    // Check if already running.
    if let Ok(state) = DaemonState::load() {
        if state.is_process_alive() {
            return Err(miette::miette!(
                "daemon already running (pid {}, addr {})",
                state.pid,
                state.addr
            ));
        }
        // Stale state file — clean it up before starting fresh.
        DaemonState::clear().ok();
    }

    // Daemon-shared plugin route table. Threaded into SessionConfig so
    // per-mount session-open populates entries for OOP plugins from the
    // registry. Also wired into the iroh Router's host-ALPN accept handler
    // (SessionRoutingProtocolHandler) so accept-time pubkey lookup hits the
    // same table.
    let plugin_routes = Arc::new(pattern_core::plugin::auth::PluginRouteTable::new());

    // Bind iroh endpoint FIRST so SessionConfig can hold it for native-plugin
    // OOP spawn at session-open. Phase 6 Task 5 — replaces noq-cert-pinning
    // with iroh node-identity-pinning. Load secret_key from prior state if
    // present (stable node_id across restarts), else generate fresh.
    let bind_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into();
    let secret_key = match DaemonState::load()
        .ok()
        .and_then(|s| s.load_secret_bytes().ok())
    {
        Some(bytes) if bytes.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            iroh::SecretKey::from_bytes(&arr)
        }
        _ => iroh::SecretKey::generate(),
    };
    let node_id = secret_key.public();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0DisableRelay)
        .secret_key(secret_key.clone())
        .bind_addr(bind_addr)
        .map_err(|e| miette::miette!("failed to set bind addr: {e}"))?
        .bind()
        .await
        .map_err(|e| miette::miette!("failed to bind iroh endpoint: {e}"))?;
    let local_addr = endpoint
        .bound_sockets()
        .into_iter()
        .next()
        .ok_or_else(|| miette::miette!("iroh endpoint has no bound socket"))?;

    // Spawn the server actor — echo mode or real session mode.
    // Projects are mounted on demand via InitSession from the TUI client.
    let handle = if echo {
        info!("starting daemon in echo mode");
        DaemonServer::spawn()
    } else {
        // Build provider with the full auth chain: stored OAuth (keyring/JSON
        // fallback) → API key env var → session pickup (~/.claude/.credentials.json).
        // This mirrors pattern-test-cli's `build_chain` — the daemon should try
        // every credential source the user might have configured.
        let chain: Arc<dyn pattern_provider::auth::CredentialChain> = {
            use pattern_provider::auth::{PkceTier, SessionPickupTier};
            use pattern_provider::creds_store::{
                CredsStore, CredsStoreResolver, JsonFallbackStore, KeyringStore,
            };

            let session_pickup = SessionPickupTier::default();
            let pkce = Arc::new(PkceTier::anthropic());
            let primary: Arc<dyn CredsStore> = Arc::new(KeyringStore::new());
            let fallback: Arc<dyn CredsStore> = Arc::new(
                JsonFallbackStore::new()
                    .map_err(|e| miette::miette!("failed to init creds fallback store: {e}"))?,
            );
            let creds_store: Arc<dyn CredsStore> =
                Arc::new(CredsStoreResolver::new(primary, fallback));

            Arc::new(pattern_provider::auth::AnthropicAuthChain::with_oauth(
                session_pickup,
                pkce,
                creds_store,
            ))
        };
        let limiter =
            Arc::new(pattern_provider::ratelimit::ProviderRateLimiter::anthropic_default());
        let shaper_cfg = pattern_provider::shaper::ShaperConfig::default();
        let shaper = Arc::new(
            pattern_provider::shaper::HonestPatternShaper::new(shaper_cfg)
                .map_err(|e| miette::miette!("failed to create shaper: {e}"))?,
        );
        let counter = Arc::new(pattern_provider::token_count::TokenCounter::anthropic(
            limiter.clone(),
        ));
        let gateway = pattern_provider::gateway::PatternGatewayClient::builder()
            .with_provider("anthropic", chain, shaper, limiter)
            .with_token_counter("anthropic", counter)
            .build()
            .map_err(|e| miette::miette!("failed to build gateway: {e}"))?;
        let provider: Arc<dyn pattern_core::ProviderClient> = Arc::new(gateway);

        // Resolve SDK location.
        let sdk = pattern_runtime::sdk::SdkLocation::default();

        // Use `with_runtime_ports` (NOT `new`) so the daemon's registry
        // ships HttpPort and any other runtime-provided ports. Building
        // the registry via `new` directly leaves the daemon with no
        // HTTP capability and breaks `Port.call("http", ...)` at
        // dispatch time — surfaced by the v3-sandbox-io final review.
        let port_registry = std::sync::Arc::new(
            pattern_runtime::port_registry::PortRegistryImpl::with_runtime_ports(
                &tokio::runtime::Handle::current(),
            ),
        );
        let config = SessionConfig {
            sdk,
            provider,
            port_registry,
            plugin_routes: Some(Arc::clone(&plugin_routes)),
            daemon_endpoint: Some(endpoint.clone()),
        };

        info!("starting daemon");
        DaemonServer::spawn_with_config(config)
    };

    let local = handle
        .client
        .as_local()
        .expect("freshly-spawned server client must be local");
    let handler = PatternProtocol::remote_handler(local);

    // Plugin-host accept (Phase 6 Task 5b). v1 stub handler — returns
    // Unimplemented for all 17 PluginHostProtocol variants until 5c+ wires
    // real dispatch into the runtime plugin registry.
    use pattern_core::plugin::auth::SessionRoutingProtocolHandler;
    use pattern_core::plugin::protocol::{PLUGIN_HOST_ALPN, PluginHostProtocol};
    use std::sync::Arc;

    let host_client = pattern_runtime::plugin::host_handler::spawn();
    let host_local = host_client
        .as_local()
        .expect("freshly-spawned host client must be local");
    let host_handler = PluginHostProtocol::remote_handler(host_local);

    // Session-routing handler wraps host handler with the daemon-shared
    // route table (built earlier + passed into SessionConfig so sessions populate
    // it at open).
    let gated_host = SessionRoutingProtocolHandler::new(
        Arc::clone(&plugin_routes),
        irpc_iroh::IrohProtocol::new(host_handler),
    );

    // Multi-ALPN router. pattern/1 carries the TUI/client protocol;
    // pattern-plugin-host/1 carries Plugin→Runtime callbacks + memory ops,
    // session-gated by PluginRouteTable lookup.
    // Future: pattern-plugin-guest/1 (Runtime→Plugin) lives client-side in
    // OutOfProcessPluginConnection; pattern-plugin-memory-sync/1 gets its own
    // accept when MemorySyncProtocol handler ships.
    let _router = iroh::protocol::Router::builder(endpoint)
        .accept(b"pattern/1", irpc_iroh::IrohProtocol::new(handler))
        .accept(PLUGIN_HOST_ALPN, gated_host)
        .spawn();

    // plugin_routes is now threaded through SessionConfig → SessionRegistries
    // → SessionContext, so per-mount session-open populates from PluginRegistry
    // and Drop on SessionContext clears entries. The Arc here + Arc in SessionConfig
    // both point at the same dashmap-backed table.

    let state = DaemonState {
        pid: std::process::id(),
        addr: local_addr,
        node_id: node_id.to_string(),
    };
    state
        .save(&secret_key.to_bytes())
        .map_err(|e| miette::miette!("failed to write state: {e}"))?;

    info!("daemon listening on {}", local_addr);
    info!("state written to {}", DaemonState::state_path().display());

    // Block until Ctrl-C (or SIGTERM via the OS — tokio only catches Ctrl-C
    // portably; SIGTERM handling is done by the calling process or init system).
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| miette::miette!("failed to wait for ctrl-c: {e}"))?;

    info!("shutting down");
    DaemonState::clear().ok();

    Ok(())
}

fn cmd_stop() -> miette::Result<()> {
    let state =
        DaemonState::load().map_err(|_| miette::miette!("daemon not running (no state file)"))?;

    if !state.is_process_alive() {
        DaemonState::clear().ok();
        return Err(miette::miette!(
            "daemon not running (stale state file cleaned up)"
        ));
    }

    // Send SIGTERM via the nix crate — a safe, typed wrapper around kill(2).
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    signal::kill(Pid::from_raw(state.pid as i32), Signal::SIGTERM)
        .map_err(|e| miette::miette!("failed to send SIGTERM to pid {}: {e}", state.pid))?;

    DaemonState::clear().ok();
    println!("daemon stopped (pid {})", state.pid);
    Ok(())
}

fn cmd_status() -> miette::Result<()> {
    let state = match DaemonState::load() {
        Ok(s) => s,
        Err(_) => {
            println!("daemon not running");
            return Ok(());
        }
    };

    if !state.is_process_alive() {
        DaemonState::clear().ok();
        println!("daemon not running (stale state file cleaned up)");
        return Ok(());
    }

    println!("daemon running");
    println!("  pid:  {}", state.pid);
    println!("  addr: {}", state.addr);
    Ok(())
}
