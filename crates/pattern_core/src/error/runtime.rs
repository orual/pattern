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
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Which cancellation path produced a [`RuntimeError::Timeout`].
///
/// See the v3-foundation Phase 3 Task 16 description for the two-path
/// cancellation design. The distinction matters to callers because
/// [`CancelPath::Soft`] leaves the session usable while
/// [`CancelPath::HardAbandon`] poisons it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelPath {
    /// Soft cancel fired at an effect boundary; the session remains usable.
    Soft,
    /// Hard abandon fired — the blocking thread was detached and the session
    /// is poisoned. Callers must open a fresh session.
    HardAbandon,
}

impl std::fmt::Display for CancelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelPath::Soft => f.write_str("soft"),
            CancelPath::HardAbandon => f.write_str("hard_abandon"),
        }
    }
}

/// Kinds of sandbox constraint a runtime may enforce on agent programs.
///
/// Surfaced through [`RuntimeError::SandboxConstraintViolated`]. These are
/// learned operational constraints we surface back to the agent rather than
/// program bugs — the agent can iterate on its program to avoid the
/// constraint.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxConstraint {
    /// Program uses IO types. Pattern's Tidepool-backed sandbox only
    /// accepts pure functional code — all side effects go through the
    /// SDK effect algebra, not `IO`.
    NoIoAllowed,
    // Future: NoUnsafeFfi, ExcessiveRecursion, etc.
}

impl std::fmt::Display for SandboxConstraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxConstraint::NoIoAllowed => f.write_str("no_io_allowed"),
        }
    }
}

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
    /// let err = RuntimeError::Timeout {
    ///     wall_ms: 30_000,
    ///     cpu_ms: 10_000,
    ///     path: pattern_core::error::CancelPath::Soft,
    /// };
    /// assert!(err.to_string().contains("wall"));
    /// assert!(err.to_string().contains("30000"));
    /// ```
    #[error("agent turn timed out ({path}): wall {wall_ms}ms, cpu {cpu_ms}ms")]
    #[diagnostic(
        code(pattern_core::runtime::timeout),
        help("increase the turn budget or reduce the agent's workload per turn")
    )]
    Timeout {
        /// Elapsed wall-clock time in milliseconds.
        wall_ms: u64,
        /// Elapsed CPU time in milliseconds.
        cpu_ms: u64,
        /// Which cancellation path produced this timeout.
        path: CancelPath,
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

    /// The agent program failed to parse or type-check.
    ///
    /// Source-level errors from the Haskell extractor pipeline (GHC parse /
    /// type-check / Core translation). The `diagnostics` field carries the
    /// extractor's stderr verbatim, which the program author should use to
    /// fix their code.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::ProgramCompileFailed {
    ///     diagnostics: "Main.hs:3:1: parse error".to_string(),
    /// };
    /// assert!(err.to_string().contains("parse error"));
    /// ```
    #[error("agent program compile failed:\n{diagnostics}")]
    #[diagnostic(code(pattern_runtime::program_compile_failed))]
    ProgramCompileFailed {
        /// Raw extractor diagnostics (GHC stderr) describing the failure.
        diagnostics: String,
    },

    /// The agent program references a primitive the runtime doesn't provide.
    ///
    /// Typically an SDK/runtime version mismatch — the Haskell SDK refers to
    /// a freer-simple constructor (effect variant, data constructor) that the
    /// embedded runtime hasn't registered. Surfaces the constructor name so
    /// operators can diagnose which SDK/runtime pair is out of sync.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::MissingRuntimePrimitive { name: "Notify".to_string() };
    /// assert!(err.to_string().contains("Notify"));
    /// ```
    #[error("missing runtime primitive: {name}")]
    #[diagnostic(
        code(pattern_runtime::missing_runtime_primitive),
        help(
            "the agent SDK refers to `{name}` but the runtime doesn't provide it. check SDK/runtime version alignment."
        )
    )]
    MissingRuntimePrimitive {
        /// Name of the missing constructor / primitive.
        name: String,
    },

    /// Runtime-internal failure during compilation.
    ///
    /// Covers codegen / linking / supporting-file resolution / substrate IO
    /// errors inside the compile pipeline. Not a user program bug — file
    /// against the runtime crate.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::CompileInternal {
    ///     reason: "failed to spawn tidepool-extract".to_string(),
    /// };
    /// assert!(err.to_string().contains("tidepool-extract"));
    /// ```
    #[error("runtime-internal compile failure: {reason}")]
    #[diagnostic(code(pattern_runtime::compile_internal))]
    CompileInternal {
        /// Human-readable description of the internal failure.
        reason: String,
    },

    /// The agent program violates a substrate sandbox constraint.
    ///
    /// Surface `detail` back to the agent (not the author) so it can iterate —
    /// this is a learned operational constraint, not a bug. `constraint`
    /// identifies the kind of constraint violated and is stable for matching;
    /// `detail` is free-form and may be surfaced directly to the agent.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::{RuntimeError, SandboxConstraint};
    ///
    /// let err = RuntimeError::SandboxConstraintViolated {
    ///     constraint: SandboxConstraint::NoIoAllowed,
    ///     detail: "agent program uses IO types; use SDK effects instead".to_string(),
    /// };
    /// assert!(err.to_string().contains("IO"));
    /// ```
    #[error("sandbox constraint violated: {detail}")]
    #[diagnostic(code(pattern_runtime::sandbox_constraint_violated))]
    SandboxConstraintViolated {
        /// The kind of sandbox constraint violated.
        constraint: SandboxConstraint,
        /// Human-readable detail for the agent.
        detail: String,
    },

    /// The Tidepool runtime process crashed unexpectedly.
    ///
    /// A crash means the process exited without a catchable panic
    /// (e.g., segfault, OOM kill, fatal JIT signal). Distinct from the
    /// compile-failure variants: these happen mid-execution.
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

    /// The Haskell SDK directory could not be found at the expected location.
    ///
    /// Returned by `SdkLocation::resolve()` (pattern_runtime) when the
    /// configured directory does not exist. `hint` provides actionable
    /// guidance (e.g., set `PATTERN_SDK_DIR`).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    /// use std::path::PathBuf;
    ///
    /// let err = RuntimeError::SdkNotFound {
    ///     path: PathBuf::from("/missing/haskell"),
    ///     hint: "Set PATTERN_SDK_DIR".to_string(),
    /// };
    /// assert!(err.to_string().contains("/missing/haskell"));
    /// ```
    #[error("SDK directory not found: {}", path.display())]
    #[diagnostic(code(pattern_runtime::sdk_not_found), help("{hint}"))]
    SdkNotFound {
        /// The path that was expected to contain the SDK.
        path: std::path::PathBuf,
        /// Actionable guidance for the operator.
        hint: String,
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

    /// The session was poisoned by a hard-abandoned turn and can no longer
    /// be stepped. Callers must open a fresh session.
    ///
    /// Produced by the Phase 3 Task 16 two-path cancellation harness when
    /// a runaway compute turn had to be abandoned in the background.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::SessionPoisoned {
    ///     reason: "previous turn hard-abandoned".into(),
    /// };
    /// assert!(err.to_string().contains("poisoned"));
    /// ```
    #[error("session poisoned: {reason}")]
    #[diagnostic(
        code(pattern_core::runtime::session_poisoned),
        help("open a fresh session — compile is cached so this is cheap")
    )]
    SessionPoisoned {
        /// Why the session was poisoned.
        reason: String,
    },

    /// A tokio task joined with an error (panic or cancellation propagation).
    ///
    /// Produced by the cancellation harness when the blocking task hosting
    /// the JIT fails to join cleanly. The underlying task error is preserved
    /// as a human-readable string.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::JoinError { reason: "task panicked".into() };
    /// assert!(err.to_string().contains("join"));
    /// ```
    #[error("join error: {reason}")]
    #[diagnostic(code(pattern_core::runtime::join_error))]
    JoinError {
        /// Human-readable description of the join failure.
        reason: String,
    },

    /// The cancellation watchdog itself failed (e.g., its task panicked).
    ///
    /// Should not occur in practice; surfaced defensively so callers can
    /// distinguish a watchdog bug from a genuine timeout.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::WatchdogFailure;
    /// assert!(err.to_string().contains("watchdog"));
    /// ```
    #[error("cancellation watchdog failed")]
    #[diagnostic(code(pattern_core::runtime::watchdog_failure))]
    WatchdogFailure,

    /// An SDK effect handler reported a failure during turn execution.
    ///
    /// Produced when a handler returns `EffectError::Handler(...)` (or any
    /// other effect error) that is not a cancellation sentinel. The raw
    /// message is preserved for diagnostics.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::EffectHandlerFailed {
    ///     reason: "Pattern.Memory.Search(...) not yet wired".into(),
    /// };
    /// assert!(err.to_string().contains("Pattern.Memory"));
    /// ```
    #[error("effect handler failed: {reason}")]
    #[diagnostic(code(pattern_core::runtime::effect_handler_failed))]
    EffectHandlerFailed {
        /// Human-readable description of the handler failure.
        reason: String,
    },

    /// A Pattern SDK handler failed during JIT execution.
    ///
    /// Distinct from [`Self::CompileInternal`], which describes codegen /
    /// pipeline / substrate failures: this variant carries a handler
    /// identity and message surfaced from a tidepool-effect `EffectError`
    /// that bubbled out of the JIT run path. Routing SDK handler failures
    /// here rather than into `CompileInternal` gives callers a category
    /// they can match on without string-matching on an opaque reason.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::RuntimeError;
    ///
    /// let err = RuntimeError::SdkHandlerFailed {
    ///     handler: "Pattern.File".into(),
    ///     reason: "not yet implemented".into(),
    /// };
    /// assert!(err.to_string().contains("Pattern.File"));
    /// assert!(err.to_string().contains("not yet implemented"));
    /// ```
    #[error("SDK handler {handler} failed: {reason}")]
    #[diagnostic(code(pattern_core::runtime::sdk_handler_failed))]
    SdkHandlerFailed {
        /// Best-effort handler identity extracted from the effect error
        /// message (e.g. `"Pattern.File"`). Falls back to `"unknown"` if
        /// the upstream effect error did not carry a handler tag.
        handler: String,
        /// Human-readable reason surfaced by the handler.
        reason: String,
    },
}
