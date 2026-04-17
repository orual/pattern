//! Runtime errors for the agent execution loop.
//!
//! This file defines errors that can occur during agent-loop execution: budget
//! exhaustion, unrecoverable crashes, and checkpoint failures. These errors
//! originate inside the Tidepool runtime (Phase 3) and are surfaced through
//! [`super::core::CoreError::Runtime`].
//!
//! # Pre-v3 CoreError variants replaced by this file
//!
//! None of the pre-v3 `CoreError` variants map directly here; `RuntimeError`
//! variants are new for v3.

use miette::Diagnostic;
use thiserror::Error;

/// Errors that originate in the agent execution loop.
///
/// All variants are `#[non_exhaustive]` at the enum level; new runtime error
/// kinds may be added in minor versions without breaking existing match arms.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum RuntimeError {
    /// The agent turn exceeded its wall-clock or CPU time budget.
    ///
    /// Both budgets are reported so callers can distinguish a CPU-heavy turn
    /// (small `wall_ms`, large `cpu_ms`) from one that blocked on I/O.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::Timeout { wall_ms: 30_000, cpu_ms: 10_000 };
    /// assert!(err.to_string().contains("wall"));
    /// assert!(err.to_string().contains("30000"));
    /// ```
    #[error("agent turn timed out: wall {wall_ms}ms, cpu {cpu_ms}ms")]
    #[diagnostic(
        code(pattern_core::runtime::timeout),
        help("increase the turn budget or reduce the agent's workload per turn")
    )]
    Timeout {
        /// Elapsed wall-clock time in milliseconds.
        wall_ms: u64,
        /// Elapsed CPU time in milliseconds.
        cpu_ms: u64,
    },

    /// The agent attempted to emit more effects in one turn than the budget
    /// permits.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::EffectOverflow;
    /// assert!(err.to_string().contains("overflow"));
    /// ```
    #[error("effect overflow: too many effects emitted in a single turn")]
    #[diagnostic(
        code(pattern_core::runtime::effect_overflow),
        help("split the agent's workload across multiple turns")
    )]
    EffectOverflow,

    /// The underlying Tidepool runtime panicked.
    ///
    /// This indicates a bug in the Tidepool runtime, not in the agent program.
    /// The `reason` field contains the panic message if it could be captured.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::GhcPanic { reason: "out of memory".to_string() };
    /// assert!(err.to_string().contains("out of memory"));
    /// ```
    #[error("tidepool runtime panicked: {reason}")]
    #[diagnostic(
        code(pattern_core::runtime::ghc_panic),
        help("this is a runtime bug; report it with the reason string")
    )]
    GhcPanic {
        /// The panic message captured from the runtime, if available.
        reason: String,
    },

    /// The Tidepool runtime process crashed unexpectedly.
    ///
    /// Distinct from [`RuntimeError::GhcPanic`]: a crash means the process
    /// exited without a catchable panic (e.g., segfault, OOM kill).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::RuntimeCrashed;
    /// assert!(err.to_string().contains("crashed"));
    /// ```
    #[error("tidepool runtime crashed unexpectedly")]
    #[diagnostic(
        code(pattern_core::runtime::crashed),
        help("check system resources; the runtime process was killed externally")
    )]
    RuntimeCrashed,

    /// A checkpoint could not be written or verified.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::CheckpointFailed { reason: "disk full".to_string() };
    /// assert!(err.to_string().contains("disk full"));
    /// ```
    #[error("checkpoint failed: {reason}")]
    #[diagnostic(
        code(pattern_core::runtime::checkpoint_failed),
        help("check disk space and permissions for the checkpoint store")
    )]
    CheckpointFailed {
        /// Human-readable description of why the checkpoint failed.
        reason: String,
    },

    /// The runtime environment failed a preflight check before any compilation started.
    ///
    /// Returned by `pattern_runtime::preflight::check()` when a required binary
    /// (e.g., `tidepool-extract`) is missing or non-functional.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::PreflightFailed { reason: "tidepool-extract not found".to_string() };
    /// assert!(err.to_string().contains("tidepool-extract"));
    /// ```
    #[error("runtime preflight failed: {reason}")]
    #[diagnostic(
        code(pattern_core::runtime::preflight_failed),
        help(
            "install tidepool-extract and ensure it is on PATH, or set $TIDEPOOL_EXTRACT to its absolute path; see crates/pattern_runtime/CLAUDE.md for setup instructions"
        )
    )]
    PreflightFailed {
        /// Human-readable description of what the preflight check found wrong.
        reason: String,
    },
}
