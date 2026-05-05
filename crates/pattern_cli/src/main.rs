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
    /// Manage provider authentication (login, status, clear).
    Auth(commands::auth::AuthCmd),
    /// Manage plugins (install, list, uninstall).
    Plugin(PluginCmd),
}

// ---------------------------------------------------------------------------
// Plugin command handler
// ---------------------------------------------------------------------------

fn cmd_plugin(cmd: PluginCmd) -> MietteResult<()> {
    use pattern_memory::paths::PatternPaths;
    use pattern_runtime::plugin::registry::{InstallSource, PluginRegistry};
    use std::sync::Arc;

    let paths = Arc::new(PatternPaths::default_paths()
        .map_err(|e| miette::miette!("failed to resolve pattern paths: {e}"))?);
    let project_dir = std::env::current_dir().ok();
    let reg = PluginRegistry::load(paths.clone(), project_dir)
        .map_err(|e| miette::miette!("failed to load plugin registry: {e}"))?;

    match cmd.sub {
        PluginSub::Install { path, scope } => {
            let scope = match scope.as_str() {
                "project" => pattern_core::plugin::PluginScope::Project { private: false },
                _ => pattern_core::plugin::PluginScope::Global,
            };
            // If the path looks like a git URL, clone it first.
            let path = if let Some(url) = path.to_str() {
                if url.starts_with("https://") || url.starts_with("git@") || url.starts_with("ssh://") || url.ends_with(".git") {
                    let cache_base = paths.plugins_cache_root();
                    std::fs::create_dir_all(&cache_base)
                        .map_err(|e| miette::miette!("failed to create cache dir: {e}"))?;
                    let clone_name = url.rsplit('/').next().unwrap_or("plugin").trim_end_matches(".git");
                    let clone_path = cache_base.join(format!(".clone-{clone_name}"));
                    if clone_path.exists() {
                        std::fs::remove_dir_all(&clone_path).ok();
                    }
                    println!("Cloning {url}...");
                    // Try jj first, fall back to git.
                    let jj_result = pattern_memory::jj::JjAdapter::detect()
                        .ok()
                        .flatten()
                        .map(|jj| jj.git_clone(url, &clone_path));
                    match jj_result {
                        Some(Ok(())) => {},
                        _ => {
                            let output = std::process::Command::new("git")
                                .args(["clone", "--depth=1", url, &clone_path.to_string_lossy()])
                                .output()
                                .map_err(|e| miette::miette!("git clone failed: {e}"))?;
                            if !output.status.success() {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                return Err(miette::miette!("git clone failed: {stderr}"));
                            }
                        }
                    }
                    clone_path
                } else {
                    path
                }
            } else {
                path
            };
            // Try direct install first. If no manifest found, scan subdirectories.
            match reg.install(InstallSource::LocalPath(&path), scope.clone()) {
                Ok(lp) => {
                    println!("Installed plugin: {} (scope: {:?})", lp.id, lp.scope);
                }
                Err(_) => {
                    // Scan for plugin subdirectories (multi-plugin repos).
                    let mut found = false;
                    // Check plugins/ subdir first (CC convention).
                    let scan_dir = if path.join("plugins").is_dir() {
                        path.join("plugins")
                    } else {
                        path.clone()
                    };
                    if let Ok(entries) = std::fs::read_dir(&scan_dir) {
                        for entry in entries.flatten() {
                            let sub = entry.path();
                            if !sub.is_dir() { continue; }
                            // Check if this subdir has a manifest.
                            if sub.join("manifest.kdl").exists()
                                || sub.join(".claude-plugin").join("plugin.json").exists()
                            {
                                match reg.install(InstallSource::LocalPath(&sub), scope.clone()) {
                                    Ok(lp) => {
                                        println!("Installed: {} (scope: {:?})", lp.id, lp.scope);
                                        found = true;
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to install {}: {e}", sub.display());
                                    }
                                }
                            }
                        }
                    }
                    if !found {
                        return Err(miette::miette!(
                            "no plugins found at {} (checked for manifest.kdl or .claude-plugin/plugin.json)",
                            path.display()
                        ));
                    }
                }
            }
        }
        PluginSub::List => {
            let plugins = reg.list();
            if plugins.is_empty() {
                println!("No plugins installed.");
            } else {
                for p in &plugins {
                    println!("  {} (scope: {:?}, path: {})",
                        p.id, p.scope, p.source_path.display());
                }
            }
        }
        PluginSub::Uninstall { id, clean } => {
            reg.uninstall(&id, clean)
                .map_err(|e| miette::miette!("uninstall failed: {e}"))?;
            println!("Uninstalled plugin: {id}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin subcommand types
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
struct PluginCmd {
    #[command(subcommand)]
    sub: PluginSub,
}

#[derive(Subcommand)]
enum PluginSub {
    /// Install a plugin from a local path.
    Install {
        /// Path to the plugin directory.
        path: PathBuf,
        /// Install scope (global or project).
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// List installed plugins.
    List,
    /// Uninstall a plugin by ID.
    Uninstall {
        /// Plugin identifier.
        id: String,
        /// Also remove cached files.
        #[arg(long)]
        clean: bool,
    },
}

// ---------------------------------------------------------------------------
// Constellation subcommand types
// ---------------------------------------------------------------------------

#[derive(clap::Args)]
struct ConstellationCmd {
    #[command(subcommand)]
    sub: ConstellationSub,
}

#[derive(Subcommand)]
enum ConstellationSub {
    /// Launch a multi-agent zellij layout (one pane per agent).
    Launch {
        /// Agents to include (e.g., `@supervisor @writer`).
        #[arg(value_name = "AGENT")]
        agents: Vec<String>,
    },
    /// List personas registered in the constellation.
    List {
        /// Optional project-path filter.
        #[arg(long)]
        project: Option<String>,
    },
    /// Promote a `Draft` persona to `Active`.
    Promote {
        /// Persona id to promote.
        #[arg(value_name = "ID")]
        persona_id: String,
    },
    /// Add a relationship edge between two personas.
    Relate {
        /// Source persona id.
        #[arg(value_name = "FROM")]
        from: String,
        /// Target persona id.
        #[arg(value_name = "TO")]
        to: String,
        /// Relationship kind: `supervisor_of`, `specialist_for`, `peer_with`, `observer_of`.
        #[arg(value_name = "KIND")]
        kind: String,
    },
    /// Manage persona groups.
    Groups {
        #[command(subcommand)]
        sub: GroupsSub,
    },
}

#[derive(Subcommand)]
enum GroupsSub {
    /// List groups, optionally filtered by project.
    List {
        #[arg(long)]
        project: Option<String>,
    },
    /// Create a new group.
    Create {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long)]
        project_id: Option<String>,
    },
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
        mode: Option<ModeArg>,

        /// Path to the project root (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,

        /// Project identifier. Optional for `--mode standalone`: when
        /// omitted, an ID is derived from the project directory's
        /// basename (slugified, with a numeric suffix on collision).
        /// Ignored by `--mode in-repo` and `--mode sidecar`.
        #[arg(long)]
        project_id: Option<String>,
    },

    /// Check attachment to a mount
    Check {
        /// Path to start the walk-upward search from (defaults to the current directory).
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },

    /// Link an existing directory to an existing project in the registry.
    ///
    /// Adds `PATH` (default: current directory) to the projects registry
    /// under the project named by `--to`. After linking, future commands
    /// launched from `PATH` (or any subdirectory) resolve to the same
    /// standalone mount as the original project.
    ///
    /// `--to` accepts either a project ID (e.g. `--to my-project`) or
    /// a path that already resolves to a registered project (e.g.
    /// `--to ~/work/my-project`). Path resolution canonicalizes and
    /// walks up — pointing at any subdirectory of a registered project
    /// works.
    ///
    /// Useful for jj workspaces, persistent forks, or sister checkouts
    /// of a standalone project that should share Pattern state with the
    /// primary project root.
    ///
    /// Errors if `--to` matches neither a known project ID nor a path
    /// resolving to one. Idempotent if the same `(PATH, ID)` pair is
    /// already registered.
    Link {
        /// Directory to link (defaults to the current directory).
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,

        /// Existing project to link the path to. Either a project ID
        /// or a filesystem path that resolves to a registered project.
        #[arg(long, value_name = "ID_OR_PATH")]
        to: String,
    },
}

/// Storage mode selection for `mount init`.
///
/// `ValueEnum` maps these to kebab-case CLI values: `in-repo`, `standalone`,
/// `sidecar`.
#[derive(Clone, Copy, ValueEnum, Default)]
enum ModeArg {
    /// In-repo storage; host VCS owns history.
    InRepo,
    /// Separate Pattern-owned jj repository.
    #[default]
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
        Some(Commands::Constellation(cmd)) => match cmd.sub {
            ConstellationSub::Launch { agents } => {
                use tui::zellij::detect::detect as detect_zellij;
                let agents = agents
                    .iter()
                    .map(|a| a.trim_start_matches('@').to_string())
                    .collect();
                let zellij_state = detect_zellij();
                commands::constellation::run_constellation(agents, &zellij_state)?;
            }
            ConstellationSub::List { project } => {
                commands::constellation_registry::cmd_list(project).await?;
            }
            ConstellationSub::Promote { persona_id } => {
                commands::constellation_registry::cmd_promote(persona_id).await?;
            }
            ConstellationSub::Relate { from, to, kind } => {
                commands::constellation_registry::cmd_relate(from, to, kind).await?;
            }
            ConstellationSub::Groups { sub } => match sub {
                GroupsSub::List { project } => {
                    commands::constellation_registry::cmd_groups_list(project).await?;
                }
                GroupsSub::Create { name, project_id } => {
                    commands::constellation_registry::cmd_groups_create(name, project_id).await?;
                }
            },
        },
        Some(Commands::Mount(mount)) => match mount.sub {
            MountSub::Init {
                mode,
                path,
                project_id,
            } => {
                let mode = mode.unwrap_or_default();
                let target = resolve_path(path)?;
                cmd_mount_init(mode, target, project_id)?;
            }
            MountSub::Check { path } => {
                let target = resolve_path(path)?;
                cmd_mount_check(&target)?;
            }
            MountSub::Link { path, to } => {
                let target = resolve_path(path)?;
                cmd_mount_link(&target, &to)?;
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
        Some(Commands::Auth(auth)) => {
            commands::auth::cmd_auth(auth).await?;
        }
        Some(Commands::Plugin(plugin)) => {
            cmd_plugin(plugin)?;
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
            let paths = pattern_memory::paths::PatternPaths::default_paths()
                .map_err(miette::Report::new)?;
            let project_path = path.canonicalize().unwrap_or_else(|_| path.clone());

            // Register first so the canonical id (slugified or explicit)
            // is available to write into the kdl. The kdl ends up with
            // matching id/registry values; the human-readable basename
            // becomes the kdl `name` field.
            let mut registry = pattern_memory::projects::ProjectRegistry::load(&paths)
                .map_err(miette::Report::new)?;
            let id = registry
                .register_project(&project_path, project_id.as_deref())
                .map_err(miette::Report::new)?;
            registry.save(&paths).map_err(miette::Report::new)?;

            let result =
                pattern_memory::modes::in_repo::init(&path, &id).map_err(miette::Report::new)?;

            println!(
                "Mount initialized (in-repo) at {} (project_id={id}, path={})",
                result.mount_path().display(),
                project_path.display()
            );
        }
        ModeArg::Standalone => {
            let adapter = pattern_memory::jj::JjAdapter::detect()
                .map_err(miette::Report::new)?
                .ok_or_else(|| {
                    miette::miette!("standalone mode requires jj but it was not found on PATH")
                })?;
            let paths = pattern_memory::paths::PatternPaths::default_paths()
                .map_err(miette::Report::new)?;

            // Canonicalize the project path. Standalone mode writes nothing
            // into the project repo, so the only way later commands can
            // resolve the mount from this path is via the projects
            // registry — which keys on the canonical path.
            let project_path = path.canonicalize().unwrap_or_else(|_| path.clone());

            // Register the project before init so the resolved id is
            // available for the standalone layout. Errors here include
            // "this path is already registered under a different id"
            // and surface as miette diagnostics.
            let mut registry = pattern_memory::projects::ProjectRegistry::load(&paths)
                .map_err(miette::Report::new)?;
            let id = registry
                .register_project(&project_path, project_id.as_deref())
                .map_err(miette::Report::new)?;
            registry.save(&paths).map_err(miette::Report::new)?;

            let result = pattern_memory::modes::standalone::init(&id, &adapter, &paths)
                .map_err(miette::Report::new)?;
            println!(
                "Mount initialized (standalone) at {} (project_id={id}, path={})",
                result.mount_path().display(),
                project_path.display()
            );
        }
        ModeArg::Sidecar => {
            let adapter = pattern_memory::jj::JjAdapter::detect()
                .map_err(miette::Report::new)?
                .ok_or_else(|| {
                    miette::miette!("sidecar mode requires jj but it was not found on PATH")
                })?;
            let paths = pattern_memory::paths::PatternPaths::default_paths()
                .map_err(miette::Report::new)?;
            let project_path = path.canonicalize().unwrap_or_else(|_| path.clone());

            // Register first so the canonical id is written into the
            // kdl as `id="..."`. The display `name` field gets the
            // raw directory basename in init.
            let mut registry = pattern_memory::projects::ProjectRegistry::load(&paths)
                .map_err(miette::Report::new)?;
            let id = registry
                .register_project(&project_path, project_id.as_deref())
                .map_err(miette::Report::new)?;
            registry.save(&paths).map_err(miette::Report::new)?;

            let result = pattern_memory::modes::sidecar::init(&path, &id, &adapter)
                .map_err(miette::Report::new)?;

            println!(
                "Mount initialized (sidecar) at {} (project_id={id}, path={})",
                result.mount_path().display(),
                project_path.display()
            );
        }
    }
    Ok(())
}

fn cmd_mount_link(path: &std::path::Path, to: &str) -> MietteResult<()> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let paths = pattern_memory::PatternPaths::default_paths().map_err(miette::Report::new)?;
    let mut registry =
        pattern_memory::projects::ProjectRegistry::load(&paths).map_err(miette::Report::new)?;

    // Resolve `to` to a canonical project id. Try id first (exact
    // match); on miss, treat as a filesystem path and walk up to find
    // a registered project. Slug-shaped strings can't appear as paths
    // anyway, so id-first is unambiguous.
    let project_id = if registry.contains_id(to) {
        to.to_owned()
    } else {
        let to_path = std::path::Path::new(to);
        let to_canonical = to_path
            .canonicalize()
            .unwrap_or_else(|_| to_path.to_path_buf());
        match registry.project_id_for_path(&to_canonical) {
            Some(id) => id.to_owned(),
            None => {
                let known: Vec<&str> = registry.project_ids().collect();
                return Err(miette::miette!(
                    "{to:?} is neither a known project id nor a path that resolves to one. \
                     Known projects: {known:?}. \
                     Run `pattern mount init --mode standalone` to create one."
                ));
            }
        }
    };

    registry
        .add_path(&project_id, &canonical)
        .map_err(miette::Report::new)?;
    registry.save(&paths).map_err(miette::Report::new)?;

    println!("Linked {} to project {project_id}", canonical.display());
    Ok(())
}

fn cmd_mount_check(path: &std::path::Path) -> MietteResult<()> {
    let store = pattern_memory::mount::attach(path, None).map_err(miette::Report::new)?;
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
                    Err(_) => SessionResult::offline(),
                }
            }
            Err(_) => SessionResult::offline(),
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
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    )
    .ok();

    let mut terminal = ratatui::init();
    let mut app = tui::app::App::new();

    // Wire up the zellij state so /pane and /float know whether they can act.
    app.set_zellij_state(zellij_state);

    // Wire the daemon's stable partner identity into the app so that outbound
    // messages carry a consistent Author::Partner attribution. If the session
    // was offline or InitSession failed, the app keeps its self-minted id.
    if let Some(pid) = session.partner_id {
        app.set_partner_id(pid);
    }
    // Wire the optional display name for Author::Partner attribution rendering.
    if let Some(name) = session.partner_display_name {
        app.set_partner_display_name(name);
    }

    // Phase 6 T8: seed fronting state for the status bar / panel.
    if let Some(snapshot) = session.fronting_snapshot {
        app.set_fronting_snapshot(snapshot);
    }

    // Phase 6 T8: kick off the initial constellation fetch so the panel has
    // data when the user toggles to it. Subsequent refreshes happen on
    // ConstellationChanged events from the daemon.
    app.refresh_constellation_view();

    // Populate the available agents list and alias index so /front can
    // validate either canonical id or persona-name alias.
    if !session.available_agents.is_empty() {
        app.set_available_agents(session.available_agents);
    }
    if !session.agent_aliases.is_empty() {
        app.set_agent_aliases(session.agent_aliases);
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

    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    )
    .ok();
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
    error: Option<String>,
    available_agents: Vec<smol_str::SmolStr>,
    /// Aliases (persona `name` fields) that resolve to canonical agent ids.
    /// Used by autocomplete and command validation to accept either form.
    agent_aliases: Vec<pattern_server::protocol::AgentAlias>,
    history: Vec<pattern_server::protocol::HistoricalBatch>,
    /// Plugin commands fetched from the daemon for autocomplete registration.
    daemon_commands: Vec<(String, String)>,
    /// Stable partner identity from the daemon. Used to construct
    /// `Author::Partner` origins for `AgentMessage::origin`. The TUI stores
    /// this and passes it as `user_id` in every `SendMessage`.
    partner_id: Option<smol_str::SmolStr>,
    /// Optional human-readable display name for the partner from the daemon.
    /// Sourced from `SessionInfo.partner_display_name`.
    partner_display_name: Option<String>,
    /// Initial fronting snapshot from `SessionInfo.fronting_snapshot`. None
    /// in echo mode or when the mount has no fronting state. Phase 6 T8.
    fronting_snapshot: Option<pattern_server::protocol::FrontingSnapshot>,
}

impl SessionResult {
    /// Construct an offline (no daemon) session result.
    fn offline() -> Self {
        Self {
            client: None,
            event_rx: None,
            error: None,
            available_agents: vec![],
            agent_aliases: vec![],
            history: vec![],
            daemon_commands: vec![],
            partner_id: None,
            partner_display_name: None,
            fronting_snapshot: None,
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
                    let h = client
                        .get_history(resolved.clone())
                        .await
                        .map(|resp| resp.batches);
                    h.unwrap_or_default()
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

            // Phase 6 T8: subscribe mount-wide so the TUI receives every
            // agent's events for the project plus daemon-level
            // FrontingChanged / ConstellationChanged notifications.
            let rx = client.subscribe_all(project_path.to_path_buf()).await.ok();
            SessionResult {
                client: Some(client.clone()),
                event_rx: rx,
                error: info.error,
                available_agents: info.available_agents,
                agent_aliases: info.agent_aliases,
                history,
                daemon_commands,
                partner_id: Some(info.partner_id),
                partner_display_name: info.partner_display_name,
                fronting_snapshot: info.fronting_snapshot,
            }
        }
        Err(e) => {
            tracing::warn!("InitSession failed, falling back to default agent: {e}");
            let rx = client.subscribe_all(project_path.to_path_buf()).await.ok();
            SessionResult {
                client: Some(client.clone()),
                event_rx: rx,
                error: Some(format!("session init failed: {e}")),
                available_agents: vec![],
                agent_aliases: vec![],
                history: vec![],
                daemon_commands: vec![],
                partner_id: None,
                partner_display_name: None,
                fronting_snapshot: None,
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
