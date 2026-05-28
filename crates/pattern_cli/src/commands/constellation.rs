// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Constellation layout command — launch one agent pane per persona.
//!
//! A constellation is a zellij session where each configured agent gets its
//! own tiled pane running `pattern chat @agent --no-auto-launch-zj`. This gives a
//! side-by-side view of all active personas.
//!
//! [`run_constellation`] is the top-level entry point. It:
//! 1. Validates the agent list (at least one required).
//! 2. Generates a multi-pane KDL layout via [`PatternLayout::constellation`].
//! 3. Writes the layout and hands off to `zellij attach --create`.

use miette::{Result as MietteResult, miette};

use crate::tui::zellij::detect::{ZellijState, session_name_for_project};
use crate::tui::zellij::layout::PatternLayout;

/// Launch a constellation session: one zellij pane per agent.
///
/// `agents` is the ordered list of agent names (without `@` prefix). If
/// empty, returns an error — a constellation requires at least one agent.
///
/// When already inside a zellij session ([`ZellijState::InSession`]), returns
/// an error rather than nesting sessions. When zellij is not available
/// ([`ZellijState::NotAvailable`]), also returns an error.
pub fn run_constellation(agents: Vec<String>, zellij_state: &ZellijState) -> MietteResult<()> {
    // Validate inputs before checking system requirements so callers get
    // actionable errors regardless of environment.
    if agents.is_empty() {
        return Err(miette!("constellation requires at least one agent"));
    }

    match zellij_state {
        ZellijState::InSession { .. } => {
            return Err(miette!(
                "already inside a zellij session — cannot nest a constellation"
            ));
        }
        ZellijState::NotAvailable => {
            return Err(miette!(
                "zellij is not available — install zellij to use constellations"
            ));
        }
        ZellijState::Available => {}
    }

    let pattern_bin = crate::tui::zellij::locate_pattern_binary();

    // Start (or reuse) the detached daemon before launching zellij so the
    // layout's log-tail tab has a daemon to follow.
    let _ = crate::commands::daemon::ensure_daemon_running();

    let log_path_buf = pattern_server::state::DaemonState::log_path();
    let log_path = log_path_buf.to_str().ok_or_else(|| {
        miette!(
            "daemon log path is not valid UTF-8: {}",
            log_path_buf.display()
        )
    })?;
    let layout = PatternLayout::constellation(&pattern_bin, &agents).with_daemon(log_path);
    let layout_path = layout
        .write_layout()
        .map_err(|e| miette!("failed to write constellation layout: {e}"))?;

    let session_name = session_name_for_project(None);
    let layout_str = layout_path
        .to_str()
        .ok_or_else(|| miette!("layout path is not valid UTF-8: {}", layout_path.display()))?;
    let status = std::process::Command::new("zellij")
        .args([
            "attach",
            "--create",
            &session_name,
            "options",
            "--default-layout",
            layout_str,
        ])
        .status()
        .map_err(|e| miette!("failed to launch zellij constellation: {e}"))?;

    if !status.success() {
        return Err(miette!("zellij exited with {status}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constellation_requires_at_least_one_agent() {
        // NotAvailable is used here because the empty-agents check now runs
        // before the zellij-availability check, so the environment state
        // does not affect which error is returned for an empty agent list.
        let state = ZellijState::NotAvailable;
        let result = run_constellation(vec![], &state);
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("at least one agent"), "got: {msg}");
    }

    #[test]
    fn constellation_rejects_nested_sessions() {
        let state = ZellijState::InSession {
            session_name: "test".into(),
        };
        let result = run_constellation(vec!["alpha".into()], &state);
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("already inside"), "got: {msg}");
    }

    #[test]
    fn constellation_rejects_unavailable_zellij() {
        let state = ZellijState::NotAvailable;
        let result = run_constellation(vec!["alpha".into()], &state);
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("not available"), "got: {msg}");
    }
}
