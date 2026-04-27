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
use std::sync::Arc;

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
        .unwrap_or_else(|_| "pattern_server=info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();

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
        };

        info!("starting daemon");
        DaemonServer::spawn_with_config(config)
    };

    // Create QUIC endpoint with a self-signed certificate.
    let bind_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into();
    let (endpoint, cert_der) = irpc::util::make_server_endpoint(bind_addr)
        .map_err(|e| miette::miette!("failed to create QUIC endpoint: {e}"))?;

    let local_addr = endpoint
        .local_addr()
        .map_err(|e| miette::miette!("failed to get local addr: {e}"))?;

    // Set up the QUIC listener that forwards remote messages into the actor.
    let local = handle
        .client
        .as_local()
        .expect("freshly-spawned server client must be local");
    let handler = PatternProtocol::remote_handler(local);
    let _listener = tokio::spawn(irpc::rpc::listen(endpoint, handler));

    // Write state so that `stop` and `status` can find us.
    let state = DaemonState {
        pid: std::process::id(),
        addr: local_addr,
    };
    state
        .save(&cert_der)
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
