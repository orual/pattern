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

use pattern_core::error::RuntimeError;

/// Check that the runtime environment is ready to compile and run Tidepool programs.
///
/// Currently verifies:
/// 1. `tidepool-extract` is reachable (via `$TIDEPOOL_EXTRACT` or `$PATH`).
/// 2. The binary responds to `--version` without error.
///
/// Returns `Ok(())` if the environment is ready. Returns a `RuntimeError` with
/// an actionable `miette` diagnostic on failure.
pub fn check() -> Result<(), RuntimeError> {
    // phase: 3; AC: AC2.1
    todo!("implement tidepool-extract availability check with actionable diagnostic")
}

#[cfg(test)]
mod tests {
    /// Smoke test: succeeds when tidepool-extract is on PATH.
    ///
    /// Ignored by default because it requires the binary to be present (Nix devshell or
    /// manual install). Run explicitly with:
    ///
    /// ```sh
    /// cargo nextest run -p pattern-runtime preflight -- --ignored
    /// ```
    #[test]
    #[ignore = "requires tidepool-extract on PATH or $TIDEPOOL_EXTRACT set"]
    fn succeeds_when_extract_on_path() {
        super::check().expect("preflight should succeed when tidepool-extract is available");
    }

    /// Verifies that when `tidepool-extract` is not findable, the error message
    /// contains actionable install instructions.
    #[test]
    fn fails_with_actionable_message_when_missing() {
        // This test will be filled in during Task 5 when the real implementation lands.
        // phase: 3; AC: AC2.1
        todo!("implement after preflight::check is fleshed out in Task 5")
    }
}
