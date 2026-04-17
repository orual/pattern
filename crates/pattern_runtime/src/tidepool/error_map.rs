//! Tidepool error → Pattern RuntimeError translation.
//!
//! This is the single centralised place where tidepool errors are converted into
//! `pattern_core::error::RuntimeError` variants. All code that calls tidepool APIs
//! routes through these functions; no other crate in Pattern should match on raw
//! tidepool error types.
//!
//! # Mapping policy
//!
//! See the error-mapping table in `docs/implementation-plans/2026-04-16-v3-foundation/phase_03.md`
//! (executor context section) for the full rationale for each mapping decision.
//! The key split:
//! - Compile-time errors (extractor failures, IO, missing outputs) → `RuntimeError::GhcPanic`
//! - JIT-time infrastructure failures (signal, heap bridge, missing con tags) → `RuntimeError::RuntimeCrashed` or `GhcPanic`
//! - Effect-level overflow (too many response nodes) → `RuntimeError::EffectOverflow`
//! - Agent-logic errors (Haskell `error`/`undefined`) → not `RuntimeError`; caller decides disposition
//! - SDK handler failures → `SdkError` (handler-local; not a runtime crash)

use pattern_core::error::RuntimeError;
use tidepool_codegen::jit_machine::JitError;
use tidepool_runtime::CompileError;

/// Map a `tidepool_runtime::CompileError` to a `pattern_core::error::RuntimeError`.
///
/// All compile errors are surfaced as `RuntimeError::GhcPanic` because they represent
/// a failure in the extractor pipeline (GHC parse/type-check/Core translation), which
/// is an implementation-level problem rather than an agent-logic problem.
pub fn map_compile_error(_e: CompileError) -> RuntimeError {
    // phase: 3; AC: AC2.8
    todo!("implement per error-mapping table in phase_03.md")
}

/// Map a `tidepool_codegen::JitError` to a `pattern_core::error::RuntimeError`.
///
/// Returns `Err(RuntimeError)` for infrastructure-level failures.
/// Returns `Ok(AgentError)` for agent-logic errors (Haskell `error`/`undefined`) via
/// the [`JitOutcome`] type, since these should be surfaced to the agent orchestrator
/// rather than treated as crashes.
///
/// # Design note
///
/// `JitError::Effect(EffectError)` is not translated here — it propagates as [`SdkError`]
/// at the handler call site, not through this function.
pub fn map_jit_error(_e: JitError) -> JitOutcome {
    // phase: 3; AC: AC2.7, AC2.8, AC2.9
    todo!("implement per error-mapping table in phase_03.md")
}

/// Outcome of a JIT execution.
///
/// A JIT call can either produce a `RuntimeError` (infrastructure failure) or an
/// `AgentError` (agent-logic error from Haskell's `error`/`undefined`) that should
/// be surfaced to the agent orchestrator rather than treated as a crash.
pub enum JitOutcome {
    /// An infrastructure-level failure that the runtime cannot recover from.
    Runtime(RuntimeError),
    /// The agent program called Haskell's `error` or `undefined`.
    ///
    /// This is an agent-logic error, not a runtime crash. The message (if any) is
    /// preserved for the orchestrator to surface to the agent's turn log.
    AgentError(AgentRuntimeError),
}

/// An error that originated in agent Haskell code rather than in the Tidepool
/// runtime infrastructure.
///
/// Produced when the agent calls Haskell's `error` or forces `undefined`.
/// The orchestrator decides how to handle this (typically log + abort the turn).
#[non_exhaustive]
#[derive(Debug)]
pub struct AgentRuntimeError {
    /// The message passed to Haskell's `error`, if present.
    pub message: Option<String>,
}

/// An error that originated in a Pattern SDK effect handler.
///
/// Produced when a `JitError::Effect(EffectError)` fires during `machine.run`.
/// This is a handler-local failure (e.g., bridge mismatch, missing constructor),
/// not a runtime crash. The error is surfaced through the SDK handler infrastructure
/// rather than through `RuntimeError`.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
#[error("SDK effect handler error: {0}")]
pub struct SdkError(pub tidepool_effect::EffectError);
