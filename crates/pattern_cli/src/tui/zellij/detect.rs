// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Zellij environment detection.
//!
//! Determines whether the process is running inside a zellij session, whether
//! zellij is available on PATH, or neither. Used at TUI startup to decide
//! whether to auto-launch a session or run standalone.

/// The three possible zellij environment states at process startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZellijState {
    /// The process is running inside an active zellij session.
    InSession { session_name: String },
    /// Zellij binary is available on PATH but we are not inside a session.
    Available,
    /// Zellij is not on PATH.
    NotAvailable,
}

/// Detect the current zellij environment state.
///
/// Checks `$ZELLIJ_SESSION_NAME` first (set by zellij for all child
/// processes), then falls back to PATH lookup via `which`.
pub fn detect() -> ZellijState {
    if let Ok(name) = std::env::var("ZELLIJ_SESSION_NAME")
        && !name.is_empty()
    {
        return ZellijState::InSession { session_name: name };
    }
    if which::which("zellij").is_ok() {
        ZellijState::Available
    } else {
        ZellijState::NotAvailable
    }
}

/// Derive a deterministic zellij session name for the current project.
///
/// Uses the last component of `dir` (or `$PWD` when `dir` is `None`),
/// normalised to lowercase with spaces replaced by hyphens. Falls back to
/// `"pattern-default"` when the path cannot be determined.
///
/// Accepting an explicit path makes the function testable without touching
/// the process's working directory.
pub fn session_name_for_project(dir: Option<&std::path::Path>) -> String {
    let dir_name = match dir {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "default".into()),
        None => std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "default".into()),
    };
    let normalised = dir_name.to_lowercase().replace(' ', "-");
    format!("pattern-{normalised}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_in_session_when_env_var_set() {
        // SAFETY: test-only env mutation; nextest runs tests in separate
        // processes so there is no risk of interfering with other tests.
        unsafe { std::env::set_var("ZELLIJ_SESSION_NAME", "test-session") };
        let state = detect();
        unsafe { std::env::remove_var("ZELLIJ_SESSION_NAME") };
        assert_eq!(
            state,
            ZellijState::InSession {
                session_name: "test-session".into()
            }
        );
    }

    #[test]
    fn detect_not_in_session_when_env_var_absent() {
        unsafe { std::env::remove_var("ZELLIJ_SESSION_NAME") };
        let state = detect();
        // May be Available or NotAvailable depending on the environment.
        assert!(matches!(
            state,
            ZellijState::Available | ZellijState::NotAvailable
        ));
    }

    #[test]
    fn session_name_is_prefixed() {
        let name = session_name_for_project(None);
        assert!(
            name.starts_with("pattern-"),
            "session name must start with 'pattern-', got: {name}"
        );
        assert!(!name.is_empty());
    }

    #[test]
    fn session_name_derives_from_dir() {
        let name = session_name_for_project(Some(std::path::Path::new("/tmp/myproject")));
        assert_eq!(
            name, "pattern-myproject",
            "expected 'pattern-myproject', got: {name}"
        );
    }

    #[test]
    fn session_name_none_uses_current_dir() {
        // Passing None should read the current directory and derive from it,
        // equivalent to passing Some(current_dir).
        let from_none = session_name_for_project(None);
        let from_cwd = session_name_for_project(Some(&std::env::current_dir().unwrap()));
        assert_eq!(from_none, from_cwd);
    }
}
