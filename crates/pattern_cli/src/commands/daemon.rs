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
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use miette::{IntoDiagnostic, Result as MietteResult, miette};
use pattern_server::state::DaemonState;

// ---------------------------------------------------------------------------
// Default persona
// ---------------------------------------------------------------------------

/// Bundled default persona KDL, written to `~/.pattern/personas/@pattern-default/persona.kdl`
/// on first run if no persona is found.
const DEFAULT_PERSONA_KDL: &str = r#"name "pattern-default"
agent-id "pattern-default"

system-prompt "You are Pattern, an ADHD support assistant providing external executive function. Be helpful, concise, and proactive."

model provider="anthropic" model-id="claude-sonnet-4-6" {
    temperature 0.7
    max-tokens 4096
}

context {
    compress-check-message-floor 50
    compress-token-threshold 150000
    mid-batch "filter_self_edits"
    compression type="recursive_summarization" {
        chunk-size 20
        summarization-model "claude-haiku-4-5"
    }
}

budgets {
    wall-ms 30000
    cpu-ms 10000
}

memory {
    persona content="I am Pattern, an ADHD support assistant. I provide external executive function through structured support, gentle reminders, and adaptive task management." {
        memory-type "core"
        permission "read_only"
        pinned true
    }
    scratchpad content="Working notes for the current session." {
        memory-type "working"
        permission "read_write"
    }
}
"#;

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

        /// Run in echo mode (no LLM, echoes messages back). Used for testing.
        #[arg(long)]
        echo: bool,

        /// Path to a persona KDL file. Required unless running in echo mode.
        #[arg(long)]
        persona: Option<PathBuf>,
    },
    /// Stop the running daemon.
    Stop,
    /// Show daemon status (running state, PID, listen address).
    Status,
}

pub fn cmd_daemon(cmd: DaemonCmd) -> MietteResult<()> {
    match cmd.sub {
        DaemonSub::Start {
            port,
            path,
            echo,
            persona,
        } => cmd_start(port, path, echo, persona),
        DaemonSub::Stop => cmd_stop(),
        DaemonSub::Status => cmd_status(),
    }
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

fn cmd_start(
    port: u16,
    path: Option<PathBuf>,
    echo: bool,
    persona: Option<PathBuf>,
) -> MietteResult<()> {
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
    if echo {
        cmd.arg("--echo");
    }
    if let Some(persona_path) = &persona {
        cmd.arg("--persona").arg(persona_path);
    }
    if let Some(project_path) = &path {
        cmd.arg("--path").arg(project_path);
    }

    // Detach fully: no stdin, stdout/stderr to log file so daemon output
    // doesn't corrupt the TUI or clutter the terminal.
    let log_path = DaemonState::state_dir().join("daemon.log");
    std::fs::create_dir_all(DaemonState::state_dir()).into_diagnostic()?;
    let log_file = std::fs::File::create(&log_path).into_diagnostic()?;
    let log_err = log_file.try_clone().into_diagnostic()?;
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::from(log_file));
    cmd.stderr(std::process::Stdio::from(log_err));

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
// Persona resolution
// ---------------------------------------------------------------------------

/// Ensure that a default persona exists on disk for the given project.
///
/// Delegates to [`resolve_default_persona`], which writes the bundled default
/// to `~/.pattern/personas/@pattern-default/persona.kdl` if no persona is
/// found. Called by the TUI before sending `InitSession` so the daemon can
/// discover at least one persona.
pub fn ensure_default_persona(project_path: &Path) -> MietteResult<()> {
    let _ = resolve_default_persona(project_path)?;
    Ok(())
}

/// Resolve the default persona KDL path for daemon auto-start.
///
/// Resolution strategy:
/// 1. Try to find a project mount via `find_mount(project_path)`.
/// 2. If found, parse `.pattern.kdl` to get the `default` persona binding.
/// 3. Use `discover_personas` to map the handle to a file path.
/// 4. If no persona is found on disk, write the bundled default to
///    `~/.pattern/personas/@pattern-default/persona.kdl`.
/// 5. If no mount is found (no project), resolve from global `~/.pattern/personas/`.
///
/// # Errors
///
/// Returns an error if the global paths cannot be resolved (no home directory).
/// Returns (persona KDL path, persona agent_id).
fn resolve_default_persona(project_path: &Path) -> MietteResult<(PathBuf, String)> {
    use pattern_memory::PatternPaths;
    use pattern_memory::config::load_mount_config;
    use pattern_memory::mount::find_mount;
    use pattern_memory::persona::discover_personas;

    let paths = PatternPaths::default_paths()
        .map_err(|e| miette!("could not resolve pattern home directory: {e}"))?;

    // Try to find a project mount and extract the default persona handle.
    let (persona_handle, mount_path) = match find_mount(project_path) {
        Ok(mount) => {
            let config_path = mount.join(".pattern.kdl");
            match load_mount_config(&config_path) {
                Ok(config) => {
                    // Find the "default" slot in the personas section.
                    let handle = config
                        .personas
                        .entries
                        .iter()
                        .find(|b| b.slot == "default")
                        .map(|b| b.persona.clone());
                    (handle, Some(mount))
                }
                Err(_) => {
                    // Config unreadable — fall back to default handle.
                    (None, Some(mount))
                }
            }
        }
        Err(_) => (None, None),
    };

    let persona_handle = persona_handle.unwrap_or_else(|| "@pattern-default".to_string());

    // Normalize: strip leading '@' for the discovery map key.
    let normalized = persona_handle.trim_start_matches('@');

    // Discover available personas from global + project scopes.
    let personas = discover_personas(&paths, mount_path.as_deref())
        .map_err(|e| miette!("persona discovery failed: {e}"))?;

    if let Some(path) = personas.get(normalized) {
        return Ok((path.clone(), normalized.to_string()));
    }

    // Persona not found on disk — write the bundled default.
    let persona_dir = paths.base().join("personas").join("@pattern-default");
    std::fs::create_dir_all(&persona_dir)
        .into_diagnostic()
        .map_err(|e| miette!("failed to create default persona directory: {e}"))?;

    let persona_path = persona_dir.join("persona.kdl");
    std::fs::write(&persona_path, DEFAULT_PERSONA_KDL)
        .into_diagnostic()
        .map_err(|e| miette!("failed to write default persona: {e}"))?;

    Ok((persona_path, "pattern-default".to_string()))
}

// ---------------------------------------------------------------------------
// ensure_daemon_running
// ---------------------------------------------------------------------------

/// Ensure the daemon is running and return its listen address.
///
/// The daemon starts project-agnostic. The TUI sends an `InitSession` RPC
/// after connecting to tell the daemon which project it is working in.
///
/// # Errors
///
/// Returns an error if:
/// - The server binary cannot be found.
/// - The daemon fails to start within the timeout.
///
/// Returns the listen address.
pub fn ensure_daemon_running() -> MietteResult<SocketAddr> {
    // Fast path: already running.
    if let Ok(state) = DaemonState::load() {
        if state.is_process_alive() {
            return Ok(state.addr);
        }
        // Stale state — clean up before starting a fresh daemon.
        DaemonState::clear().ok();
    }

    let server_bin = locate_server_binary()?;
    let mut cmd = std::process::Command::new(&server_bin);
    cmd.arg("start");
    // The daemon starts bare — no --path or --persona needed.
    // Projects are mounted on demand via InitSession from the TUI.

    // Redirect all IO to log file — daemon must not write to the TUI terminal.
    let log_path = DaemonState::state_dir().join("daemon.log");
    std::fs::create_dir_all(DaemonState::state_dir()).into_diagnostic()?;
    let log_file = std::fs::File::create(&log_path).into_diagnostic()?;
    let log_err = log_file.try_clone().into_diagnostic()?;
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::from(log_file));
    cmd.stderr(std::process::Stdio::from(log_err));

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
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(dir) = current_exe.parent()
    {
        let candidate = dir.join("pattern-server");
        if candidate.exists() {
            return Ok(candidate);
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
        if let Ok(state) = DaemonState::load()
            && state.is_process_alive()
        {
            return Ok(state);
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
                path: None,
                echo: false,
                persona: None,
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
                path: None,
                echo: false,
                persona: None,
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

    // -----------------------------------------------------------------------
    // resolve_default_persona tests
    // -----------------------------------------------------------------------

    /// When no mount exists and no global persona is present, the bundled
    /// default is written to `~/.pattern/personas/@pattern-default/persona.kdl`.
    #[test]
    fn resolve_writes_bundled_default_when_no_persona_exists() {
        let home = tempfile::tempdir().unwrap();
        // Safety: nextest isolates each test in its own process.
        unsafe {
            std::env::set_var("PATTERN_HOME", home.path().to_str().unwrap());
        }

        // Use a random temp dir with no mount as the project path.
        let project = tempfile::tempdir().unwrap();
        let (persona_path, agent_id) = resolve_default_persona(project.path()).unwrap();

        let expected = home.path().join("personas/@pattern-default/persona.kdl");
        assert_eq!(persona_path, expected);
        assert_eq!(agent_id, "pattern-default");
        assert!(persona_path.is_file(), "persona.kdl should exist on disk");

        let content = std::fs::read_to_string(&persona_path).unwrap();
        assert!(
            content.contains("pattern-default"),
            "written content should contain persona name"
        );
        assert!(
            content.contains("ADHD support assistant"),
            "written content should contain system prompt"
        );

        unsafe {
            std::env::remove_var("PATTERN_HOME");
        }
    }

    /// When a global persona already exists at the expected path,
    /// `resolve_default_persona` returns that path without overwriting.
    #[test]
    fn resolve_finds_existing_global_persona() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("PATTERN_HOME", home.path().to_str().unwrap());
        }

        // Pre-create a persona with custom content.
        let persona_dir = home.path().join("personas/@pattern-default");
        std::fs::create_dir_all(&persona_dir).unwrap();
        let persona_file = persona_dir.join("persona.kdl");
        std::fs::write(&persona_file, "name \"pattern-default\"\n").unwrap();

        let project = tempfile::tempdir().unwrap();
        let (result_path, agent_id) = resolve_default_persona(project.path()).unwrap();
        assert_eq!(result_path, persona_file);
        assert_eq!(agent_id, "pattern-default");

        // Verify it was NOT overwritten.
        let content = std::fs::read_to_string(&result_path).unwrap();
        assert_eq!(content, "name \"pattern-default\"\n");

        unsafe {
            std::env::remove_var("PATTERN_HOME");
        }
    }

    /// When a project mount exists with a `.pattern.kdl` config that references
    /// a persona, and that persona exists in the project mount, it is resolved
    /// from the project scope.
    #[test]
    fn resolve_finds_project_scoped_persona() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("PATTERN_HOME", home.path().to_str().unwrap());
        }

        // Set up a Mode A mount structure.
        let project = tempfile::tempdir().unwrap();
        pattern_memory::modes::mode_a::init(project.path()).unwrap();

        // Create a persona in the mount.
        let mount_path = project.path().join(".pattern/shared");
        let persona_dir = mount_path.join("personas/@pattern-default");
        std::fs::create_dir_all(&persona_dir).unwrap();
        let persona_path = persona_dir.join("persona.kdl");
        std::fs::write(&persona_path, "name \"pattern-default\"\n").unwrap();

        let (result_path, agent_id) = resolve_default_persona(project.path()).unwrap();
        assert_eq!(result_path, persona_path);
        assert_eq!(agent_id, "pattern-default");

        unsafe {
            std::env::remove_var("PATTERN_HOME");
        }
    }

    /// Bundled default persona KDL is valid — it should contain expected fields.
    #[test]
    fn default_persona_kdl_has_required_fields() {
        assert!(DEFAULT_PERSONA_KDL.contains("name \"pattern-default\""));
        assert!(DEFAULT_PERSONA_KDL.contains("agent-id \"pattern-default\""));
        assert!(DEFAULT_PERSONA_KDL.contains("system-prompt"));
        assert!(DEFAULT_PERSONA_KDL.contains("model provider="));
        assert!(DEFAULT_PERSONA_KDL.contains("memory {"));
    }
}
