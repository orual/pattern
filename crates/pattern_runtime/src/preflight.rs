// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Preflight checks for the Pattern runtime environment.
//!
//! Verifies that `tidepool-extract` is reachable before any agent session opens.
//! Returns a structured, actionable diagnostic if the environment is not set up
//! correctly.
//!
//! # Usage
//!
//! Call `preflight::check()` at binary startup before opening any `Session`.
//! `Session::open` (Task 14) will call this automatically, but an explicit call
//! at startup gives faster feedback with a clear error message before other
//! initialisation happens.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use pattern_core::error::RuntimeError;

/// The environment variable that overrides the PATH-based binary search.
const ENV_TIDEPOOL_EXTRACT: &str = "TIDEPOOL_EXTRACT";

/// The name of the binary to search for on PATH.
const BINARY_NAME: &str = "tidepool-extract";

/// Maximum time allowed for `tidepool-extract --version` to respond.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Check that the runtime environment is ready to compile and run Tidepool programs.
///
/// Currently verifies:
/// 1. `tidepool-extract` is reachable (via `$TIDEPOOL_EXTRACT` or `$PATH`).
/// 2. The binary responds to `--version` without error.
///
/// Returns `Ok(())` if the environment is ready. Returns a [`RuntimeError::PreflightFailed`]
/// with an actionable `miette` diagnostic on failure.
pub fn check() -> Result<(), RuntimeError> {
    let binary_path = resolve_binary()?;
    verify_binary(&binary_path)?;
    Ok(())
}

/// Resolve the path to `tidepool-extract`.
///
/// Resolution order:
/// 1. `$TIDEPOOL_EXTRACT` env var if set (absolute path to the binary).
/// 2. `tidepool-extract` on `$PATH` via `which`.
fn resolve_binary() -> Result<PathBuf, RuntimeError> {
    // Check $TIDEPOOL_EXTRACT first.
    if let Some(path_str) = std::env::var_os(ENV_TIDEPOOL_EXTRACT) {
        let path = PathBuf::from(&path_str);
        if path.is_file() {
            return Ok(path);
        }
        // The env var is set but doesn't point at a real file — report it explicitly
        // so the user knows to fix their $TIDEPOOL_EXTRACT rather than wondering if
        // the binary just isn't on PATH.
        return Err(RuntimeError::PreflightFailed {
            reason: format!(
                "TIDEPOOL_EXTRACT is set to {path:?} but that path does not exist or is not a file.\n\
                 \n\
                 To fix:\n\
                 • If using the Nix devshell: re-enter via `nix develop` — it sets this automatically.\n\
                 • Otherwise: set TIDEPOOL_EXTRACT to the absolute path of the tidepool-extract binary.\n\
                 \n\
                 See crates/pattern_runtime/CLAUDE.md for full setup instructions.",
                path = path,
            ),
        });
    }

    // Fall back to PATH lookup.
    match which::which(BINARY_NAME) {
        Ok(path) => Ok(path),
        Err(_) => Err(RuntimeError::PreflightFailed {
            reason: "tidepool-extract not found on PATH (or TIDEPOOL_EXTRACT env var)\n\
                 \n\
                 Pattern v3 agents require the tidepool-extract GHC plugin binary to compile\n\
                 agent programs. To install:\n\
                 \n\
                 \u{2022} Nix users: `nix develop` in the pattern repo root.\n\
                 \u{2022} Manual: see crates/pattern_runtime/CLAUDE.md for GHC 9.12 + cabal setup.\n\
                 \n\
                 Expected one of:\n\
                 \u{2022} `tidepool-extract` on PATH\n\
                 \u{2022} $TIDEPOOL_EXTRACT set to an executable path"
                .to_string(),
        }),
    }
}

/// Run `tidepool-extract` (bare invocation, prints usage) and verify it exits successfully.
///
/// Note: `tidepool-extract` does not support `--version` or `--help` flags.
/// A bare invocation prints usage and exits 0, which is sufficient to verify
/// the binary is functional.
fn verify_binary(path: &PathBuf) -> Result<(), RuntimeError> {
    // Spawn the process with a short timeout. We can't use tokio here since
    // `check()` is sync (called before the runtime starts). Instead we spawn
    // and poll with a deadline — standard library only.
    let mut child = Command::new(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RuntimeError::PreflightFailed {
            reason: format!(
                "failed to spawn {path:?}: {e}\n\
                 \n\
                 The binary may not be executable. Check file permissions.",
                path = path,
                e = e
            ),
        })?;

    // Wait with a timeout. `std::process::Child::wait_timeout` is not stable,
    // so we poll in a tight loop with a sleep. This is only used during startup
    // (not in the hot path), so the overhead is acceptable.
    let deadline = std::time::Instant::now() + VERSION_TIMEOUT;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(RuntimeError::PreflightFailed {
                        reason: format!(
                            "tidepool-extract timed out after {}s\n\
                             \n\
                             The binary may be corrupt or the system may be under heavy load.",
                            VERSION_TIMEOUT.as_secs()
                        ),
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(RuntimeError::PreflightFailed {
                    reason: format!("error waiting for tidepool-extract: {e}"),
                });
            }
        }
    };

    if exit_status.success() {
        return Ok(());
    }

    // Collect stderr for the error message.
    let stderr = child
        .stderr
        .take()
        .and_then(|mut s| {
            use std::io::Read;
            let mut buf = String::new();
            s.read_to_string(&mut buf).ok().map(|_| buf)
        })
        .unwrap_or_default();

    Err(RuntimeError::PreflightFailed {
        reason: format!(
            "tidepool-extract exited with status {exit_status}\n\
             \n\
             stderr:\n\
             {stderr}",
            exit_status = exit_status,
            stderr = if stderr.is_empty() {
                "(empty)".to_string()
            } else {
                stderr
            },
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::error::RuntimeError;

    /// Smoke test: succeeds when tidepool-extract is on PATH.
    ///
    /// Ignored by default because it requires the binary to be present (Nix devshell or
    /// manual install). Run explicitly with:
    ///
    /// ```sh
    /// cargo nextest run -p pattern-runtime preflight -- --ignored
    /// ```
    #[test]
    fn succeeds_when_extract_on_path() {
        // The devshell exports `$TIDEPOOL_EXTRACT` pointing at the
        // flake-provided binary; CI runs inside the same devshell. If this
        // ever fails outside those environments, the fix is to activate
        // `nix develop` first, not to re-ignore this test.
        super::check().expect("preflight should succeed when tidepool-extract is available");
    }

    /// Verifies that when `tidepool-extract` is not findable, the error message
    /// contains actionable install instructions.
    ///
    /// Forces failure by clearing PATH and unsetting TIDEPOOL_EXTRACT. The resolution
    /// logic tries $TIDEPOOL_EXTRACT first, then PATH — with both absent, it must fail
    /// with a message containing install hints.
    #[test]
    fn fails_with_actionable_message_when_missing() {
        // Temporarily override PATH to an empty string and unset TIDEPOOL_EXTRACT.
        // We manipulate env for this process's test thread. `std::env::set_var` is unsafe
        // in multi-threaded tests (per Rust 1.83+), but nextest runs each test in its own
        // process by default, so this is safe here.
        //
        // Save original values so we restore them at the end, regardless of assertion outcome.
        let original_path = std::env::var_os("PATH");
        let original_extract = std::env::var_os(ENV_TIDEPOOL_EXTRACT);

        unsafe {
            std::env::set_var("PATH", "");
            std::env::remove_var(ENV_TIDEPOOL_EXTRACT);
        }

        let result = resolve_binary();

        // Restore before asserting so any panic doesn't permanently corrupt state.
        unsafe {
            match original_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
            match original_extract {
                Some(v) => std::env::set_var(ENV_TIDEPOOL_EXTRACT, v),
                None => std::env::remove_var(ENV_TIDEPOOL_EXTRACT),
            }
        }

        let err = result.expect_err("should fail when tidepool-extract is not findable");
        let RuntimeError::PreflightFailed { reason } = err else {
            panic!("expected PreflightFailed, got {err:?}");
        };

        // The error message must contain actionable install instructions.
        assert!(
            reason.contains("nix develop") || reason.contains("CLAUDE.md"),
            "error message should contain install hint; got:\n{reason}"
        );
        assert!(
            reason.contains("tidepool-extract"),
            "error message should name the missing binary; got:\n{reason}"
        );
    }
}
