//! Pattern CLI entry point.
//!
//! Default invocation (no subcommand) enters a TUI demo. Named subcommands
//! (`pattern mount init`, `pattern mount attach`, `pattern backup create`, …)
//! run as one-shot operations and exit.

mod commands;
mod tui;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result as MietteResult};

// ---------------------------------------------------------------------------
// CLI argument types
// ---------------------------------------------------------------------------

/// Pattern — external executive function for ADHD support.
#[derive(Parser)]
#[command(name = "pattern", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start an interactive chat session with a Pattern agent.
    Chat(ChatCmd),
    /// Launch a multi-agent constellation (one zellij pane per agent).
    Constellation(ConstellationCmd),
    /// Manage memory mounts.
    Mount(MountCmd),
    /// Manage messages.db backups (create, list, restore, info).
    Backup(BackupCmd),
    /// Manage the Pattern daemon (start, stop, status).
    Daemon(commands::daemon::DaemonCmd),
}

// ---------------------------------------------------------------------------
// Constellation subcommand types
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
struct ConstellationCmd {
    /// Agents to include in the constellation (e.g., `@supervisor @writer`).
    ///
    /// If not provided, uses the full agent list from the project config.
    #[arg(value_name = "AGENT")]
    agents: Vec<String>,
}

// ---------------------------------------------------------------------------
// Chat subcommand types
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
struct ChatCmd {
    /// Agent to connect to (e.g., `@supervisor`). Defaults to the project default.
    #[arg(value_name = "AGENT")]
    agent: Option<String>,

    /// Skip zellij auto-launch and connect directly.
    ///
    /// Used internally by panes spawned inside a zellij session to avoid
    /// nesting sessions. Users can also pass this to force a plain TUI.
    #[arg(long)]
    no_auto_launch_zj: bool,

    /// Disable all zellij integration (no auto-launch, no /pane or /float).
    #[arg(long)]
    no_zellij: bool,

    /// Stop the daemon when this TUI exits if no other clients remain.
    ///
    /// Useful during development to ensure stale daemon state from a previous
    /// run does not carry over into the next. When the TUI exits and the
    /// daemon reports zero connected clients, a shutdown is sent automatically.
    #[arg(long)]
    stop_daemon_on_exit: bool,
}

// ---------------------------------------------------------------------------
// Backup subcommand types
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
struct BackupCmd {
    #[command(subcommand)]
    sub: BackupSub,
}

#[derive(Subcommand)]
enum BackupSub {
    /// Create an immediate snapshot of messages.db for the nearest mount.
    Create {
        /// Path to start the mount search from (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// List all snapshots for the nearest mount (newest first).
    List {
        /// Path to start the mount search from (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Restore messages.db from a snapshot.
    ///
    /// The current state is saved to a `.pre-restore-<ts>` file before the
    /// swap. Supported TIMESTAMP values: `latest`, an exact filename stem
    /// (`2026-04-19T120000Z`), or a date prefix (`2026-04-19`).
    Restore {
        /// Snapshot to restore: `latest`, exact timestamp, or date prefix.
        #[arg(value_name = "TIMESTAMP")]
        spec: String,
        /// Path to start the mount search from (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Show metadata and integrity status for a specific snapshot.
    Info {
        /// Snapshot to inspect: `latest`, exact timestamp, or date prefix.
        #[arg(value_name = "TIMESTAMP")]
        spec: String,
        /// Path to start the mount search from (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct MountCmd {
    #[command(subcommand)]
    sub: MountSub,
}

#[derive(Subcommand)]
enum MountSub {
    /// Initialize a new mount with the specified storage mode.
    Init {
        /// Storage mode: `in-repo` (host VCS owns history),
        /// `standalone` (separate Pattern-owned jj repo),
        /// or `sidecar` (jj alongside host git in the same working copy).
        #[arg(value_enum, long)]
        mode: ModeArg,

        /// Path to the project root (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,

        /// Project identifier (required for `--mode standalone`).
        #[arg(long)]
        project_id: Option<String>,
    },

    /// Attach to a mount (smoke test — attaches then immediately detaches).
    Attach {
        /// Path to start the walk-upward search from (defaults to the current directory).
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

/// Storage mode selection for `mount init`.
///
/// `ValueEnum` maps these to kebab-case CLI values: `in-repo`, `standalone`,
/// `sidecar`.
#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    /// In-repo storage; host VCS owns history.
    InRepo,
    /// Separate Pattern-owned jj repository.
    Standalone,
    /// Sidecar jj alongside host git in the same working copy.
    Sidecar,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> MietteResult<()> {
    // Set up tracing to a log file (not stderr — would corrupt TUI).
    // Use the daemon state dir so TUI logs live alongside the daemon log
    // at `~/.pattern/daemon/tui.log`.
    let log_path = pattern_server::state::DaemonState::state_dir().join("tui.log");
    std::fs::create_dir_all(pattern_server::state::DaemonState::state_dir()).ok();
    let log_file = std::fs::File::create(&log_path).ok();
    if let Some(file) = log_file {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "pattern=info".into());
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Chat(cmd)) => run_chat(cmd).await?,
        Some(Commands::Constellation(cmd)) => {
            use tui::zellij::detect::detect as detect_zellij;
            let agents = cmd
                .agents
                .iter()
                .map(|a| a.trim_start_matches('@').to_string())
                .collect();
            let zellij_state = detect_zellij();
            commands::constellation::run_constellation(agents, &zellij_state)?;
        }
        Some(Commands::Mount(mount)) => match mount.sub {
            MountSub::Init {
                mode,
                path,
                project_id,
            } => {
                let target = resolve_path(path)?;
                cmd_mount_init(mode, target, project_id)?;
            }
            MountSub::Attach { path } => {
                let target = resolve_path(path)?;
                cmd_attach(&target)?;
            }
        },
        Some(Commands::Backup(backup)) => match backup.sub {
            BackupSub::Create { path } => {
                commands::backup::cmd_backup_create(path)?;
            }
            BackupSub::List { path } => {
                commands::backup::cmd_backup_list(path)?;
            }
            BackupSub::Restore { spec, path } => {
                commands::backup::cmd_backup_restore(spec, path)?;
            }
            BackupSub::Info { spec, path } => {
                commands::backup::cmd_backup_info(spec, path)?;
            }
        },
        Some(Commands::Daemon(daemon)) => {
            commands::daemon::cmd_daemon(daemon)?;
        }
        None => {
            // Default: enter chat mode with all defaults (auto-zellij enabled).
            run_chat(ChatCmd {
                agent: None,
                no_auto_launch_zj: false,
                no_zellij: false,
                stop_daemon_on_exit: false,
            })
            .await?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn cmd_mount_init(mode: ModeArg, path: PathBuf, project_id: Option<String>) -> MietteResult<()> {
    match mode {
        ModeArg::InRepo => {
            let result =
                pattern_memory::modes::in_repo::init(&path).map_err(miette::Report::new)?;
            println!(
                "Mount initialized (in-repo) at {}",
                result.mount_path().display()
            );
        }
        ModeArg::Standalone => {
            let id = project_id.ok_or_else(|| {
                miette::miette!("--project-id is required for `--mode standalone`")
            })?;
            let adapter = pattern_memory::jj::JjAdapter::detect()
                .map_err(miette::Report::new)?
                .ok_or_else(|| {
                    miette::miette!("standalone mode requires jj but it was not found on PATH")
                })?;
            let paths = pattern_memory::paths::PatternPaths::default_paths()
                .map_err(miette::Report::new)?;
            let result = pattern_memory::modes::standalone::init(&id, &adapter, &paths)
                .map_err(miette::Report::new)?;
            println!(
                "Mount initialized (standalone) at {}",
                result.mount_path().display()
            );
        }
        ModeArg::Sidecar => {
            let adapter = pattern_memory::jj::JjAdapter::detect()
                .map_err(miette::Report::new)?
                .ok_or_else(|| {
                    miette::miette!("sidecar mode requires jj but it was not found on PATH")
                })?;
            let result = pattern_memory::modes::sidecar::init(&path, &adapter)
                .map_err(miette::Report::new)?;
            println!(
                "Mount initialized (sidecar) at {}",
                result.mount_path().display()
            );
        }
    }
    Ok(())
}

fn cmd_attach(path: &std::path::Path) -> MietteResult<()> {
    let store = pattern_memory::mount::attach(path).map_err(miette::Report::new)?;
    println!(
        "Attached: mode={:?} mount={}",
        store.mode,
        store.mount_path.display()
    );
    // Immediately detach — this is a smoke test, not a persistent session.
    store.detach();
    println!("Detached cleanly.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve an optional path argument, defaulting to the current directory.
fn resolve_path(path: Option<PathBuf>) -> MietteResult<PathBuf> {
    match path {
        Some(p) => Ok(p),
        None => std::env::current_dir().into_diagnostic(),
    }
}

// ---------------------------------------------------------------------------
// Chat mode
// ---------------------------------------------------------------------------

/// Enter the interactive chat TUI.
///
/// Detects the zellij environment at startup and:
/// - If zellij is available and `--no-auto-launch-zj` / `--no-zellij` are not
///   set, hands off to `zellij attach --create` and returns (the actual TUI
///   runs inside the zellij pane with `--no-auto-launch-zj` set).
/// - If already inside a zellij session (or bypass flags are set), connects
///   to the daemon and runs the TUI directly.
///
/// The optional `agent` from the `ChatCmd` overrides the project default.
async fn run_chat(cmd: ChatCmd) -> MietteResult<()> {
    use pattern_server::client::DaemonClient;
    use std::time::Duration;
    use tui::zellij::detect::{ZellijState, detect as detect_zellij};

    // Strip optional leading `@` from the agent argument for normalisation.
    let agent_override = cmd
        .agent
        .as_deref()
        .map(|a| a.trim_start_matches('@').to_string());

    let zellij_state = if cmd.no_zellij {
        ZellijState::NotAvailable
    } else {
        detect_zellij()
    };

    // Auto-launch: if zellij is available and we're not inside a session yet,
    // and neither bypass flag is set, generate a layout and hand off to zellij.
    // The spawned pane will re-invoke `pattern chat --no-auto-launch-zj`.
    if !cmd.no_auto_launch_zj && matches!(zellij_state, ZellijState::Available) {
        return tui::zellij::session::auto_launch_session(agent_override.as_deref());
    }

    // Resolve the default persona agent_id from project config, then apply
    // any override from the command line.
    let agent_id = agent_override.unwrap_or_else(resolve_default_agent_id);
    let project_path = std::env::current_dir().unwrap_or_default();

    // Ensure the default persona exists on disk so the daemon can discover it.
    commands::daemon::ensure_default_persona(&project_path).ok();

    // Connect to daemon, auto-starting if needed.
    let session = match DaemonClient::connect().await {
        Ok(client) => init_session_and_subscribe(&client, &project_path, &agent_id).await,
        Err(_) => match commands::daemon::ensure_daemon_running() {
            Ok(_addr) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                match DaemonClient::connect().await {
                    Ok(client) => {
                        init_session_and_subscribe(&client, &project_path, &agent_id).await
                    }
                    Err(_) => SessionResult::offline(agent_id.clone()),
                }
            }
            Err(_) => SessionResult::offline(agent_id.clone()),
        },
    };

    // Set up a panic hook that restores the terminal before printing the
    // panic message. Without this, panics leave the terminal in raw mode.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        ratatui::restore();
        original_hook(panic_info);
    }));

    // Enable mouse capture so clicks can toggle collapsible sections and the
    // panel. Text selection is handled via Ctrl+S selection mode instead of
    // native terminal selection.
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).ok();

    let mut terminal = ratatui::init();
    let mut app = tui::app::App::new(smol_str::SmolStr::from(session.resolved_agent.as_str()));

    // Wire up the zellij state so /pane and /float know whether they can act.
    app.set_zellij_state(zellij_state);

    // Populate the available agents list so /front can validate names.
    if !session.available_agents.is_empty() {
        app.set_available_agents(session.available_agents);
    }

    // Register any plugin commands the daemon reported on session init.
    if !session.daemon_commands.is_empty() {
        app.set_daemon_commands(session.daemon_commands);
    }

    // Load conversation history from the daemon.
    if !session.history.is_empty() {
        app.load_history(session.history);
    }

    // Surface any session initialization error as the first system message.
    if let Some(err) = session.error {
        app.push_system_message(format!("warning: {err}"));
    }

    // Retain a client reference before moving it into the event loop, so we
    // can check the client count after the TUI exits (--stop-daemon-on-exit).
    let shutdown_client = if cmd.stop_daemon_on_exit {
        session.client.clone()
    } else {
        None
    };

    let result = app
        .run(&mut terminal, session.event_rx, session.client)
        .await;
    ratatui::restore();

    // Disable mouse capture after restoring the terminal.
    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture).ok();

    // AC6.7: if --stop-daemon-on-exit was passed and we were the last client,
    // send a shutdown request to the daemon so stale state does not persist.
    if let Some(client) = shutdown_client {
        match client.client_count().await {
            Ok(0) => {
                // We were the last client — ask the daemon to shut down via
                // the dedicated Shutdown RPC (not RunCommand).
                client.shutdown().await.ok();
            }
            Ok(_) => {
                // Other clients remain — leave the daemon running.
            }
            Err(_) => {
                // Failed to check — leave the daemon running to be safe.
            }
        }
    }

    result
}

/// Result of a successful (or degraded) `InitSession` + subscribe.
struct SessionResult {
    client: Option<pattern_server::client::DaemonClient>,
    event_rx: Option<tui::app::DaemonEventReceiver>,
    resolved_agent: String,
    error: Option<String>,
    available_agents: Vec<smol_str::SmolStr>,
    history: Vec<pattern_server::protocol::HistoricalBatch>,
    /// Plugin commands fetched from the daemon for autocomplete registration.
    daemon_commands: Vec<(String, String)>,
}

impl SessionResult {
    /// Construct an offline (no daemon) session result.
    fn offline(agent_id: String) -> Self {
        Self {
            client: None,
            event_rx: None,
            resolved_agent: agent_id,
            error: None,
            available_agents: vec![],
            history: vec![],
            daemon_commands: vec![],
        }
    }
}

/// Send `InitSession`, fetch history, then subscribe to the resolved agent's output.
///
/// On RPC failure or when the daemon reports a mount error, `error` is set —
/// callers should surface it as a system message in the TUI.
async fn init_session_and_subscribe(
    client: &pattern_server::client::DaemonClient,
    project_path: &std::path::Path,
    default_agent: &str,
) -> SessionResult {
    match client
        .init_session(project_path.to_path_buf(), default_agent.into())
        .await
    {
        Ok(info) => {
            // Surface any mount failure reported by the daemon as a session
            // error. The TUI will show it as a system message on startup.
            if let Some(ref err) = info.error {
                tracing::warn!("InitSession reported error: {err}");
            }
            let resolved = info.agent_id.clone();

            // Fetch history and daemon-registered commands in parallel.
            let (history, daemon_commands) = tokio::join!(
                async {
                    client
                        .get_history(resolved.clone())
                        .await
                        .map(|resp| resp.batches)
                        .unwrap_or_default()
                },
                async {
                    client
                        .list_commands()
                        .await
                        .map(|cmds| {
                            cmds.into_iter()
                                .map(|c| (c.name, c.description))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                },
            );

            let rx = client.subscribe_output(resolved.clone()).await.ok();
            SessionResult {
                client: Some(client.clone()),
                event_rx: rx,
                resolved_agent: resolved.to_string(),
                error: info.error,
                available_agents: info.available_agents,
                history,
                daemon_commands,
            }
        }
        Err(e) => {
            tracing::warn!("InitSession failed, falling back to default agent: {e}");
            let rx = client.subscribe_output(default_agent.into()).await.ok();
            SessionResult {
                client: Some(client.clone()),
                event_rx: rx,
                resolved_agent: default_agent.to_string(),
                error: Some(format!("session init failed: {e}")),
                available_agents: vec![],
                history: vec![],
                daemon_commands: vec![],
            }
        }
    }
}

/// Resolve the default persona agent_id from project config.
///
/// Strategy:
/// 1. Find the project mount via `find_mount(cwd)`.
/// 2. Parse `.pattern.kdl` to get the `personas.default` handle.
/// 3. Strip leading `@` to normalize.
/// 4. Fall back to `"pattern-default"` if no config found.
fn resolve_default_agent_id() -> String {
    use pattern_memory::config::load_mount_config;
    use pattern_memory::mount::find_mount;

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return "pattern-default".to_string(),
    };

    let mount = match find_mount(&cwd) {
        Ok(m) => m,
        Err(_) => return "pattern-default".to_string(),
    };

    let config_path = mount.join(".pattern.kdl");
    let config = match load_mount_config(&config_path) {
        Ok(c) => c,
        Err(_) => return "pattern-default".to_string(),
    };

    config
        .personas
        .entries
        .iter()
        .find(|b| b.slot == "default")
        .map(|b| b.persona.trim_start_matches('@').to_string())
        .unwrap_or_else(|| "pattern-default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Verify that `--stop-daemon-on-exit` is accepted by `ChatCmd` and sets
    /// the flag correctly. This exercises the clap derive macro and confirms
    /// AC6.7 is reachable from the command line.
    #[test]
    fn chat_stop_flag_parses() {
        // Wrap ChatCmd in a minimal Parser to call try_parse_from.
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            cmd: ChatCmd,
        }

        let w = Wrapper::try_parse_from(["pattern", "--stop-daemon-on-exit"]).unwrap();
        assert!(
            w.cmd.stop_daemon_on_exit,
            "--stop-daemon-on-exit must set stop_daemon_on_exit to true"
        );
        assert!(!w.cmd.no_auto_launch_zj);
        assert!(!w.cmd.no_zellij);
        assert!(w.cmd.agent.is_none());
    }

    /// Verify that all fields of ChatCmd default correctly when no flags are passed.
    #[test]
    fn chat_cmd_defaults() {
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            cmd: ChatCmd,
        }

        let w = Wrapper::try_parse_from(["pattern"]).unwrap();
        assert!(!w.cmd.stop_daemon_on_exit);
        assert!(!w.cmd.no_auto_launch_zj);
        assert!(!w.cmd.no_zellij);
        assert!(w.cmd.agent.is_none());
    }
}
