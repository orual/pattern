//! `pattern daemon {start,stop,status}` subcommand implementations.
//!
//! Manages the `pattern-server` daemon process. The daemon owns the agent
//! runtime and exposes it over IRPC (QUIC on localhost). State is persisted
//! to `~/.pattern/daemon/state.json` by the server process itself.
//!
//! The CLI is a thin manager layer: it discovers the server binary, spawns or
//! signals the process, and reads the state file for discovery. All state
//! ownership lives in `pattern_server`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;
use miette::{IntoDiagnostic, Result as MietteResult, miette};
use pattern_server::state::DaemonState;

/// Manage the Pattern daemon.
#[derive(clap::Args)]
pub struct DaemonCmd {
    #[command(subcommand)]
    pub sub: DaemonSub,
}

#[derive(Subcommand)]
pub enum DaemonSub {
    /// Start the daemon in the background.
    Start {
        /// Port for the QUIC listener (0 = OS-assigned).
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// Path to the project root (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Stop the running daemon.
    Stop,
    /// Show daemon status (running state, PID, listen address).
    Status,
}

pub fn cmd_daemon(cmd: DaemonCmd) -> MietteResult<()> {
    match cmd.sub {
        DaemonSub::Start { port, path } => cmd_start(port, path),
        DaemonSub::Stop => cmd_stop(),
        DaemonSub::Status => cmd_status(),
    }
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

fn cmd_start(port: u16, _path: Option<PathBuf>) -> MietteResult<()> {
    // Check for an already-running daemon.
    if let Ok(state) = DaemonState::load() {
        if state.is_process_alive() {
            println!(
                "daemon already running (pid {}, addr {})",
                state.pid, state.addr
            );
            return Ok(());
        }
        // Stale state file — the server will clean it on startup, but clean it
        // here too for clarity.
        DaemonState::clear().ok();
    }

    let server_bin = locate_server_binary()?;

    // Build the argument list for the server binary.
    let mut cmd = std::process::Command::new(&server_bin);
    cmd.arg("start");
    if port != 0 {
        cmd.arg("--port").arg(port.to_string());
    }

    // Detach: don't inherit stdin; inherit stdout/stderr so early errors are
    // visible. The server will eventually daemonize itself if needed, but for
    // now we spawn it as a background child and let the terminal session
    // determine its lifetime.
    cmd.stdin(std::process::Stdio::null());

    let child = cmd.spawn().into_diagnostic()?;
    let child_pid = child.id();

    // Don't wait on the child — let it run in the background.
    // Explicitly forget the child handle so the process isn't signalled on drop.
    std::mem::forget(child);

    println!("starting daemon (pid {child_pid})…");

    // Wait for the state file to appear (the server writes it after binding).
    match wait_for_state_file(Duration::from_secs(5)) {
        Ok(state) => {
            println!("daemon started");
            println!("  pid:  {}", state.pid);
            println!("  addr: {}", state.addr);
        }
        Err(_) => {
            println!("daemon process launched (pid {child_pid}) but state file not yet written.");
            println!("  run `pattern daemon status` to check when it is ready.");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// stop
// ---------------------------------------------------------------------------

fn cmd_stop() -> MietteResult<()> {
    let state =
        DaemonState::load().map_err(|_| miette!("daemon not running (no state file found)"))?;

    if !state.is_process_alive() {
        DaemonState::clear().ok();
        return Err(miette!("daemon not running (stale state file cleaned up)"));
    }

    // Send SIGTERM via the nix crate (safe typed wrapper).
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    signal::kill(Pid::from_raw(state.pid as i32), Signal::SIGTERM)
        .map_err(|e| miette!("failed to signal daemon (pid {}): {e}", state.pid))?;

    DaemonState::clear().ok();
    println!("daemon stopped (pid {})", state.pid);
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn cmd_status() -> MietteResult<()> {
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

// ---------------------------------------------------------------------------
// ensure_daemon_running
// ---------------------------------------------------------------------------

/// Ensure the daemon is running and return its listen address.
///
/// Used by TUI startup (Phase 2) for AC1.7: `pattern chat` auto-starts the
/// daemon if it is not already running, then connects.
///
/// # Errors
///
/// Returns an error if:
/// - The server binary cannot be found.
/// - The daemon fails to start within the timeout.
// Phase 2 (TUI startup) uses this function. Allow dead_code until then.
#[allow(dead_code)]
pub fn ensure_daemon_running() -> MietteResult<SocketAddr> {
    // Fast path: already running.
    if let Ok(state) = DaemonState::load() {
        if state.is_process_alive() {
            return Ok(state.addr);
        }
        // Stale state — clean up before starting a fresh daemon.
        DaemonState::clear().ok();
    }

    // Spawn the daemon server binary detached.
    let server_bin = locate_server_binary()?;
    let mut cmd = std::process::Command::new(&server_bin);
    cmd.arg("start");
    cmd.stdin(std::process::Stdio::null());

    let child = cmd.spawn().into_diagnostic()?;
    // Detach: don't wait on the child handle.
    std::mem::forget(child);

    // Wait for the state file (the server writes it once the QUIC endpoint is
    // bound). Use a generous timeout — the daemon may need a moment to bind.
    let state = wait_for_state_file(Duration::from_secs(10)).map_err(|_| {
        miette!("daemon failed to start within 10 seconds — check `pattern-server` logs")
    })?;

    Ok(state.addr)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Locate the `pattern-server` binary.
///
/// Search order:
/// 1. Same directory as the currently running `pattern` binary (covers the
///    `cargo build` → `./target/debug/` case and installed layouts where both
///    binaries live in the same `bin/` directory).
/// 2. `PATH` via `which`.
fn locate_server_binary() -> MietteResult<PathBuf> {
    // Try sibling binary first — most reliable for dev + installed layouts.
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let candidate = dir.join("pattern-server");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Fall back to PATH lookup.
    which::which("pattern-server")
        .map_err(|_| miette!("pattern-server binary not found — is it installed?"))
}

/// Poll for the daemon state file to appear, waiting up to `timeout`.
///
/// Returns the loaded [`DaemonState`] on success, or an error if the file did
/// not appear (or contained a dead PID) within the deadline.
fn wait_for_state_file(timeout: Duration) -> MietteResult<DaemonState> {
    let deadline = std::time::Instant::now() + timeout;
    let poll_interval = Duration::from_millis(100);

    while std::time::Instant::now() < deadline {
        if let Ok(state) = DaemonState::load() {
            if state.is_process_alive() {
                return Ok(state);
            }
        }
        std::thread::sleep(poll_interval);
    }

    Err(miette!("timed out waiting for daemon state file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that the `DaemonCmd` clap structure parses all three
    /// subcommands without panicking. This exercises the derive macros and
    /// confirms no argument definition conflicts.
    #[test]
    fn daemon_cmd_parses_start() {
        use clap::Parser;

        // Wrap DaemonCmd in a minimal Parser so we can call try_parse_from.
        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            sub: DaemonSub,
        }

        let w = Wrapper::try_parse_from(["pattern-daemon", "start"]).unwrap();
        assert!(matches!(
            w.sub,
            DaemonSub::Start {
                port: 0,
                path: None
            }
        ));
    }

    #[test]
    fn daemon_cmd_parses_start_with_port() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            sub: DaemonSub,
        }

        let w = Wrapper::try_parse_from(["pattern-daemon", "start", "--port", "9001"]).unwrap();
        assert!(matches!(
            w.sub,
            DaemonSub::Start {
                port: 9001,
                path: None
            }
        ));
    }

    #[test]
    fn daemon_cmd_parses_stop() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            sub: DaemonSub,
        }

        let w = Wrapper::try_parse_from(["pattern-daemon", "stop"]).unwrap();
        assert!(matches!(w.sub, DaemonSub::Stop));
    }

    #[test]
    fn daemon_cmd_parses_status() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            sub: DaemonSub,
        }

        let w = Wrapper::try_parse_from(["pattern-daemon", "status"]).unwrap();
        assert!(matches!(w.sub, DaemonSub::Status));
    }

    /// ensure_daemon_running returns an error when no daemon is present and
    /// the server binary cannot be found (test environment without the binary
    /// on PATH). We use PATTERN_STATE_DIR to guarantee an empty state dir.
    #[test]
    fn ensure_daemon_running_returns_error_without_binary() {
        let dir = tempfile::tempdir().unwrap();
        // Safety: nextest isolates each test in its own process.
        unsafe {
            std::env::set_var("PATTERN_STATE_DIR", dir.path().to_str().unwrap());
        }

        // Temporarily shadow PATH so which() cannot find pattern-server.
        let old_path = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", "");
        }

        let result = ensure_daemon_running();

        // Restore env.
        unsafe {
            std::env::set_var("PATH", old_path);
            std::env::remove_var("PATTERN_STATE_DIR");
        }

        assert!(result.is_err(), "expected error when binary not found");
    }
}
