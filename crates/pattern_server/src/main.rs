//! Pattern daemon binary.
//!
//! Provides `start`, `stop`, and `status` subcommands for managing the
//! background daemon that owns the agent runtime and exposes it over QUIC.
//!
//! On `start`, the daemon:
//! 1. Checks whether an instance is already running (via state file + process check).
//! 2. Spawns the [`DaemonServer`] actor.
//! 3. Creates a QUIC endpoint with a self-signed certificate.
//! 4. Sets up a QUIC listener that forwards remote `PatternProtocol` messages
//!    into the actor's channel.
//! 5. Writes PID, bind address, and certificate to `~/.pattern/daemon/`.
//! 6. Blocks until SIGTERM or Ctrl-C, then cleans up state.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use clap::{Parser, Subcommand};
use tracing::info;

use irpc::rpc::RemoteService;
use pattern_server::protocol::PatternProtocol;
use pattern_server::server::DaemonServer;
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
        Command::Start { port } => cmd_start(port).await,
        Command::Stop => cmd_stop(),
        Command::Status => cmd_status(),
    }
}

async fn cmd_start(port: u16) -> miette::Result<()> {
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

    // Spawn the server actor.
    let handle = DaemonServer::spawn();

    // Create QUIC endpoint with a self-signed certificate.
    let bind_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into();
    let (endpoint, cert_der) = irpc::util::make_server_endpoint(bind_addr)
        .map_err(|e| miette::miette!("failed to create QUIC endpoint: {e}"))?;

    let local_addr = endpoint
        .local_addr()
        .map_err(|e| miette::miette!("failed to get local addr: {e}"))?;

    // Set up the QUIC listener that forwards remote messages into the actor.
    // `as_local()` extracts the `LocalSender<PatternProtocol>` from the client
    // so the remote handler can forward deserialised messages into the actor's
    // tokio::sync::mpsc channel.
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
