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
use std::path::PathBuf;
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

        /// Project path for memory mount. Defaults to current directory.
        /// Ignored in echo mode.
        #[arg(long)]
        path: Option<PathBuf>,

        /// Path to a persona KDL file. Required unless running in echo mode.
        #[arg(long)]
        persona: Option<PathBuf>,
    },
    /// Stop a running daemon.
    Stop,
    /// Show daemon status.
    Status,
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("pattern_server=info")
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Start {
            port,
            echo,
            path,
            persona,
        } => cmd_start(port, echo, path, persona).await,
        Command::Stop => cmd_stop(),
        Command::Status => cmd_status(),
    }
}

async fn cmd_start(
    port: u16,
    echo: bool,
    project_path: Option<PathBuf>,
    persona_path: Option<PathBuf>,
) -> miette::Result<()> {
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
    let handle = if echo {
        info!("starting daemon in echo mode");
        DaemonServer::spawn()
    } else {
        // Resolve project path.
        let project_path = project_path
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| {
                miette::miette!(
                    "could not determine project path; pass --path or run from a project directory"
                )
            })?;

        // Load persona.
        let persona_path = persona_path
            .ok_or_else(|| miette::miette!("--persona is required when not in echo mode"))?;
        let persona = pattern_runtime::persona_loader::load_persona(&persona_path)?;
        info!(
            persona = %persona.name,
            agent_id = %persona.agent_id,
            "loaded persona"
        );

        // Mount memory store.
        info!(path = %project_path.display(), "attaching to mount");
        let mounted = pattern_memory::mount::attach(&project_path).map_err(|e| {
            miette::miette!("failed to attach mount at {}: {e}", project_path.display())
        })?;

        // Build provider (Anthropic auth chain + gateway).
        let chain: Arc<dyn pattern_provider::auth::CredentialChain> =
            Arc::new(pattern_provider::auth::AnthropicAuthChain::api_key_only());
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

        let config = SessionConfig {
            sdk,
            memory_store: mounted.cache.clone(),
            provider,
            db: mounted.db.clone(),
            persona,
            mount_path: Some(mounted.mount_path.clone()),
        };

        info!("starting daemon with real session infrastructure");
        let handle = DaemonServer::spawn_with_config(config);

        // Leak the MountedStore to keep it alive for the daemon's lifetime.
        // The watcher and backup scheduler live inside it and must not be dropped.
        std::mem::forget(mounted);

        handle
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
