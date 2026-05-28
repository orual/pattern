// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Zellij pane spawning for `/pane` and `/float` commands.
//!
//! These functions shell out to `zellij action new-pane` and are only useful
//! when the process is already running inside a zellij session
//! ([`super::detect::ZellijState::InSession`]). The app checks session state
//! before calling these; callers outside a session get a clear error message.

use std::process::Command;

use super::locate_pattern_binary;

/// Build the `zellij action new-pane` argument list for a tiled pane.
///
/// `pattern_bin` is the path (or name) of the pattern binary zellij should
/// launch inside the new pane. Use [`locate_pattern_binary`] in production
/// to resolve the currently-running binary; tests pass a literal.
///
/// The `--no-auto-launch-zj` flag prevents the spawned pane from trying to
/// re-launch zellij (since it's already inside a session).
pub(crate) fn build_pane_args(pattern_bin: &str, agent: &str) -> Vec<String> {
    vec![
        "action".into(),
        "new-pane".into(),
        "--name".into(),
        format!("@{agent}"),
        "--".into(),
        pattern_bin.into(),
        "chat".into(),
        format!("@{agent}"),
        "--no-auto-launch-zj".into(),
    ]
}

/// Build the `zellij action new-pane --floating` argument list.
///
/// See [`build_pane_args`] for the `pattern_bin` contract.
///
/// The `--no-auto-launch-zj` flag prevents the spawned pane from trying to
/// re-launch zellij (since it's already inside a session).
pub(crate) fn build_float_args(pattern_bin: &str, agent: &str) -> Vec<String> {
    vec![
        "action".into(),
        "new-pane".into(),
        "--floating".into(),
        "--name".into(),
        format!("@{agent}"),
        "--".into(),
        pattern_bin.into(),
        "chat".into(),
        format!("@{agent}"),
        "--no-auto-launch-zj".into(),
    ]
}

/// Spawn a new tiled pane running `pattern chat @agent --no-auto-launch-zj`.
///
/// The `--no-auto-launch-zj` flag prevents the spawned pane from trying to
/// re-launch zellij (since it's already inside a session). The pattern
/// binary path is resolved via [`locate_pattern_binary`] so the spawned
/// pane invokes the same build as the caller.
pub fn spawn_tiled(agent: &str) -> Result<(), String> {
    let bin = locate_pattern_binary();
    let status = Command::new("zellij")
        .args(build_pane_args(&bin, agent))
        .status()
        .map_err(|e| format!("failed to spawn pane: {e}"))?;

    if !status.success() {
        return Err(format!("zellij new-pane exited with {status}"));
    }
    Ok(())
}

/// Spawn a new floating pane running `pattern chat @agent --no-auto-launch-zj`.
pub fn spawn_floating(agent: &str) -> Result<(), String> {
    let bin = locate_pattern_binary();
    let status = Command::new("zellij")
        .args(build_float_args(&bin, agent))
        .status()
        .map_err(|e| format!("failed to spawn floating pane: {e}"))?;

    if !status.success() {
        return Err(format!("zellij floating pane exited with {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: the `/pane` outside-zellij behaviour is covered by an integration
    // test in `tests/zellij_integration.rs::pane_command_outside_zellij_shows_system_error`.
    // Keeping it there avoids duplication and exercises the public App API.

    /// `build_pane_args` produces args with the resolved pattern binary,
    /// `@agent`, `--no-auto-launch-zj`, and does NOT include the obsolete
    /// `--connect` flag.
    #[test]
    fn pane_command_constructs_correct_args() {
        let args = build_pane_args("/abs/path/to/pattern", "supervisor");
        assert!(
            args.contains(&"/abs/path/to/pattern".to_string()),
            "expected pattern binary path in args: {args:?}"
        );
        assert!(
            args.contains(&"@supervisor".to_string()),
            "expected @supervisor in args: {args:?}"
        );
        assert!(
            args.contains(&"--no-auto-launch-zj".to_string()),
            "expected --no-auto-launch-zj in args: {args:?}"
        );
        assert!(
            !args.contains(&"--connect".to_string()),
            "--connect must not appear in args: {args:?}"
        );
    }

    /// `build_float_args` includes `--floating` in addition to the base args.
    #[test]
    fn float_command_adds_floating_flag() {
        let args = build_float_args("/abs/path/to/pattern", "supervisor");
        assert!(
            args.contains(&"--floating".to_string()),
            "expected --floating in float args: {args:?}"
        );
        assert!(
            args.contains(&"@supervisor".to_string()),
            "expected @supervisor in float args: {args:?}"
        );
        assert!(
            args.contains(&"--no-auto-launch-zj".to_string()),
            "expected --no-auto-launch-zj in float args: {args:?}"
        );
    }
}
