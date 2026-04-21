//! Pattern CLI entry point.
//!
//! Default invocation (no subcommand) enters a TUI demo. Named subcommands
//! (`pattern mount init`, `pattern mount attach`, `pattern backup create`, …)
//! run as one-shot operations and exit.

mod commands;
mod tui;

use std::io;
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
    /// Manage memory mounts.
    Mount(MountCmd),
    /// Manage messages.db backups (create, list, restore, info).
    Backup(BackupCmd),
    /// Manage the Pattern daemon (start, stop, status).
    Daemon(commands::daemon::DaemonCmd),
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
        /// Storage mode: `a` (in-repo, host VCS), `b` (separate pattern-jj repo),
        /// or `c` (sidecar jj inside host git project).
        #[arg(value_enum, long)]
        mode: ModeArg,

        /// Path to the project root (defaults to the current directory).
        #[arg(long)]
        path: Option<PathBuf>,

        /// Project identifier (required for Mode B).
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
#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    /// In-repo storage; host VCS owns history.
    A,
    /// Separate Pattern-owned jj repository.
    B,
    /// Sidecar jj inside host git project.
    C,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> MietteResult<()> {
    let cli = Cli::parse();

    match cli.command {
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
            // Default: enter TUI mode.
            run_tui()?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn cmd_mount_init(mode: ModeArg, path: PathBuf, project_id: Option<String>) -> MietteResult<()> {
    match mode {
        ModeArg::A => {
            let result = pattern_memory::modes::mode_a::init(&path).map_err(miette::Report::new)?;
            println!(
                "Mount initialized (Mode A) at {}",
                result.mount_path().display()
            );
        }
        ModeArg::B => {
            let id =
                project_id.ok_or_else(|| miette::miette!("--project-id is required for Mode B"))?;
            let adapter = pattern_memory::jj::JjAdapter::detect()
                .map_err(miette::Report::new)?
                .ok_or_else(|| {
                    miette::miette!("Mode B requires jj but it was not found on PATH")
                })?;
            let paths = pattern_memory::paths::PatternPaths::default_paths()
                .map_err(miette::Report::new)?;
            let result = pattern_memory::modes::mode_b::init(&id, &adapter, &paths)
                .map_err(miette::Report::new)?;
            println!(
                "Mount initialized (Mode B) at {}",
                result.mount_path().display()
            );
        }
        ModeArg::C => {
            let adapter = pattern_memory::jj::JjAdapter::detect()
                .map_err(miette::Report::new)?
                .ok_or_else(|| {
                    miette::miette!("Mode C requires jj but it was not found on PATH")
                })?;
            let result = pattern_memory::modes::mode_c::init(&path, &adapter)
                .map_err(miette::Report::new)?;
            println!(
                "Mount initialized (Mode C) at {}",
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
// TUI mode (ratatui textarea demo)
// ---------------------------------------------------------------------------

fn run_tui() -> MietteResult<()> {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use ratatui::crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::prelude::*;
    use ratatui::{Terminal, crossterm};
    use ratatui_textarea::{Input, Key, TextArea};
    use ratatui_widgets::block::Block;
    use ratatui_widgets::borders::Borders;

    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    enable_raw_mode().into_diagnostic()?;
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture).into_diagnostic()?;
    let backend = CrosstermBackend::new(stdout);
    let mut term = Terminal::new(backend).into_diagnostic()?;

    let mut textarea = TextArea::default();
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title("Pattern TUI (press Esc to exit)"),
    );

    loop {
        term.draw(|f| {
            f.render_widget(&textarea, f.area());
        })
        .into_diagnostic()?;
        match crossterm::event::read().into_diagnostic()?.into() {
            Input { key: Key::Esc, .. } => break,
            input => {
                textarea.input(input);
            }
        }
    }

    disable_raw_mode().into_diagnostic()?;
    crossterm::execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .into_diagnostic()?;
    term.show_cursor().into_diagnostic()?;

    Ok(())
}
