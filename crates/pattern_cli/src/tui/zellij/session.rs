//! Zellij session lifecycle: auto-launch and attachment.
//!
//! [`auto_launch_session`] is called from `main.rs` when the process starts
//! outside a zellij session but zellij is available. It generates a KDL
//! layout, writes it to disk, then hands off to `zellij attach --create`.

use std::process::Command;

use super::detect::session_name_for_project;
use super::layout::PatternLayout;

/// Auto-launch (or reattach to) a `pattern-{project}` zellij session.
///
/// Generates a single-pane layout, writes it to `~/.pattern/daemon/layout.kdl`,
/// then runs `zellij attach --create <session>`. Returns when the zellij
/// process exits. The caller should return immediately after this — there
/// is nothing more for the parent process to do.
pub fn auto_launch_session(agent: Option<&str>) -> miette::Result<()> {
    let session_name = session_name_for_project(None);
    let pattern_bin = super::locate_pattern_binary();

    // Start (or reuse) the detached daemon BEFORE launching zellij. The layout
    // will attach a `tail -F` viewer to the daemon log, so we want the daemon
    // (and its log file) to exist by the time zellij reads the layout — and
    // we want the daemon's lifecycle kept outside zellij so exiting the
    // session doesn't kill it or leave behind a stale `pattern-daemon` tab.
    // Failures here are non-fatal: the chat pane's own `ensure_daemon_running`
    // will retry, and the log tab's `tail -F` handles a file that doesn't
    // exist yet.
    let _ = crate::commands::daemon::ensure_daemon_running();

    let log_path_buf = pattern_server::state::DaemonState::log_path();
    let log_path = log_path_buf.to_str().ok_or_else(|| {
        miette::miette!(
            "daemon log path is not valid UTF-8: {}",
            log_path_buf.display()
        )
    })?;
    let layout = PatternLayout::single(&pattern_bin, agent).with_daemon(log_path);
    let layout_path = layout
        .write_layout()
        .map_err(|e| miette::miette!("failed to write zellij layout: {e}"))?;

    let layout_str = layout_path.to_str().ok_or_else(|| {
        miette::miette!("layout path is not valid UTF-8: {}", layout_path.display())
    })?;
    let status = Command::new("zellij")
        .args([
            "attach",
            "--create",
            &session_name,
            "options",
            "--default-layout",
            layout_str,
        ])
        .status()
        .map_err(|e| miette::miette!("failed to launch zellij: {e}"))?;

    if !status.success() {
        return Err(miette::miette!("zellij exited with {status}"));
    }

    Ok(())
}
