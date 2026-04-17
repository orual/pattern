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
//! - Agent-logic errors (Haskell `error`/`undefined`) → `JitOutcome::AgentError`; not a runtime crash
//! - SDK handler failures → `SdkError` (handler-local; not a runtime crash)

use pattern_core::error::RuntimeError;
use tidepool_codegen::jit_machine::JitError;
use tidepool_codegen::yield_type::YieldError;
use tidepool_runtime::CompileError;

/// Map a `tidepool_runtime::CompileError` to a `pattern_core::error::RuntimeError`.
///
/// All compile errors surface as `RuntimeError::GhcPanic` because they represent a
/// failure in the extractor pipeline (GHC parse/type-check/Core translation), not an
/// agent-logic problem.
pub fn map_compile_error(e: CompileError) -> RuntimeError {
    match e {
        // GHC parse/type-check/Core failure: stderr captured by extractor.
        CompileError::ExtractFailed(stderr) => RuntimeError::GhcPanic { reason: stderr },

        // Extractor setup or IO sandbox issues: surface the OS error as reason.
        CompileError::Io(io_err) => RuntimeError::GhcPanic {
            reason: io_err.to_string(),
        },
        CompileError::ReadError(read_err) => RuntimeError::GhcPanic {
            reason: read_err.to_string(),
        },
        CompileError::MissingOutput(path) => RuntimeError::GhcPanic {
            reason: format!("missing extractor output: {}", path.display()),
        },

        // Agent tried to use IO operations (unsafePerformIO etc.) in a sandbox-only context.
        CompileError::IOTypeDetected => RuntimeError::GhcPanic {
            reason: "IO type detected in result binding: IO operations are not permitted in the Tidepool sandbox".to_string(),
        },
    }
}

/// Map a `tidepool_codegen::JitError` to a `JitOutcome`.
///
/// Returns `JitOutcome::Runtime(RuntimeError)` for infrastructure-level failures.
/// Returns `JitOutcome::AgentError` for agent-logic errors (Haskell `error`/`undefined`)
/// since these should be surfaced to the agent orchestrator rather than treated as crashes.
///
/// # Design note
///
/// `JitError::Effect(EffectError)` is not translated to `RuntimeError` — it wraps the
/// `EffectError` as `SdkError`. The caller is responsible for deciding disposition
/// (e.g., propagating to the handler's error channel).
pub fn map_jit_error(e: JitError) -> JitOutcome {
    match e {
        // JIT-time signal during codegen or heap bridge (SIGILL, SIGSEGV, etc.).
        JitError::Signal(_) => JitOutcome::Runtime(RuntimeError::RuntimeCrashed),

        // Heap-object conversion failed at the result boundary.
        JitError::HeapBridge(_) => JitOutcome::Runtime(RuntimeError::RuntimeCrashed),

        // Agent DSL missing required freer-simple constructor tags.
        JitError::MissingConTags(name) => JitOutcome::Runtime(RuntimeError::GhcPanic {
            reason: format!("missing freer-simple constructor: {name}"),
        }),

        // Effect handler response exceeded the node budget (dedicated variant as of 746da8b).
        JitError::EffectResponseTooLarge { .. } => {
            JitOutcome::Runtime(RuntimeError::EffectOverflow)
        }

        // SDK handler failure — not a runtime crash; surface to handler infrastructure.
        JitError::Effect(effect_err) => JitOutcome::Sdk(SdkError(effect_err)),

        // Codegen/pipeline failures — generally happen at compile time, not run time.
        JitError::Pipeline(pipeline_err) => JitOutcome::Runtime(RuntimeError::GhcPanic {
            reason: pipeline_err.to_string(),
        }),
        JitError::Compilation(emit_err) => JitOutcome::Runtime(RuntimeError::GhcPanic {
            reason: emit_err.to_string(),
        }),

        // Yield-level errors.
        JitError::Yield(y) => map_yield_error(y),
    }
}

/// Map a `tidepool_codegen::yield_type::YieldError` to a `JitOutcome`.
///
/// Called by `map_jit_error` for the `JitError::Yield` arm.
fn map_yield_error(y: YieldError) -> JitOutcome {
    match y {
        // Agent called Haskell's `error` — surface to orchestrator, not a crash.
        YieldError::UserError => JitOutcome::AgentError(AgentRuntimeError { message: None }),
        YieldError::UserErrorMsg(msg) => {
            JitOutcome::AgentError(AgentRuntimeError { message: Some(msg) })
        }
        // Haskell's `undefined` — semantically same as `error` with no message.
        YieldError::Undefined => JitOutcome::AgentError(AgentRuntimeError { message: None }),

        // Unrecoverable resource exhaustion.
        YieldError::StackOverflow | YieldError::HeapOverflow => {
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        }

        // Fatal signal during JIT execution.
        YieldError::Signal(_) => JitOutcome::Runtime(RuntimeError::RuntimeCrashed),

        // Runtime-semantic errors: the agent's program is broken at a semantic level.
        YieldError::DivisionByZero
        | YieldError::Overflow
        | YieldError::BlackHole
        | YieldError::BadThunkState(_)
        | YieldError::NullFunPtr
        | YieldError::BadFunPtrTag(_)
        | YieldError::UnresolvedVar(_)
        | YieldError::TypeMetadata => JitOutcome::Runtime(RuntimeError::RuntimeCrashed),

        // Heap-parse errors at the result boundary — implementation bugs in the bridge.
        YieldError::UnexpectedTag(_)
        | YieldError::UnexpectedConTag(_)
        | YieldError::BadValFields(_)
        | YieldError::BadEFields(_)
        | YieldError::BadUnionFields(_)
        | YieldError::NullPointer => JitOutcome::Runtime(RuntimeError::RuntimeCrashed),
    }
}

/// Outcome of a JIT execution dispatch.
///
/// A JIT call can produce one of three outcomes:
/// - A `RuntimeError` (infrastructure failure, unrecoverable).
/// - An `AgentError` (agent-logic error from Haskell's `error`/`undefined`).
/// - An `SdkError` (SDK effect handler failure, handler-local).
pub enum JitOutcome {
    /// An infrastructure-level failure that the runtime cannot recover from.
    Runtime(RuntimeError),
    /// The agent program called Haskell's `error` or `undefined`.
    ///
    /// This is an agent-logic error, not a runtime crash. The message (if any) is
    /// preserved for the orchestrator to surface to the agent's turn log.
    AgentError(AgentRuntimeError),
    /// An SDK effect handler reported a failure.
    ///
    /// Not a runtime crash — the handler infrastructure decides disposition.
    Sdk(SdkError),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::heap_bridge::BridgeError;
    use tidepool_codegen::jit_machine::JitError;
    use tidepool_codegen::yield_type::YieldError;
    use tidepool_effect::EffectError;
    use tidepool_runtime::CompileError;

    // --- CompileError tests ---

    #[test]
    fn extract_failed_becomes_ghc_panic() {
        let input = CompileError::ExtractFailed("kaboom".into());
        let mapped = map_compile_error(input);
        assert!(
            matches!(mapped, RuntimeError::GhcPanic { ref reason } if reason.contains("kaboom"))
        );
    }

    #[test]
    fn io_error_becomes_ghc_panic() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let mapped = map_compile_error(CompileError::Io(io_err));
        assert!(matches!(mapped, RuntimeError::GhcPanic { .. }));
    }

    #[test]
    fn missing_output_becomes_ghc_panic() {
        let path = std::path::PathBuf::from("/tmp/missing.cbor");
        let mapped = map_compile_error(CompileError::MissingOutput(path));
        assert!(
            matches!(mapped, RuntimeError::GhcPanic { ref reason } if reason.contains("missing extractor output"))
        );
    }

    #[test]
    fn io_type_detected_becomes_ghc_panic() {
        let mapped = map_compile_error(CompileError::IOTypeDetected);
        assert!(
            matches!(mapped, RuntimeError::GhcPanic { ref reason } if reason.contains("IO type"))
        );
    }

    // --- JitError tests ---

    #[test]
    fn signal_becomes_runtime_crashed() {
        use tidepool_codegen::signal_safety::SignalError;
        let e = JitError::Signal(SignalError(11)); // SIGSEGV
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn heap_bridge_error_becomes_runtime_crashed() {
        let e = JitError::HeapBridge(BridgeError::UnexpectedHeapTag(0xff));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn missing_con_tags_becomes_ghc_panic_with_name() {
        let e = JitError::MissingConTags("MyEffect");
        let outcome = map_jit_error(e);
        assert!(
            matches!(outcome, JitOutcome::Runtime(RuntimeError::GhcPanic { ref reason }) if reason.contains("MyEffect"))
        );
    }

    #[test]
    fn effect_response_too_large_becomes_effect_overflow() {
        let e = JitError::EffectResponseTooLarge {
            nodes: 20_000,
            limit: 10_000,
        };
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::EffectOverflow)
        ));
    }

    #[test]
    fn effect_error_becomes_sdk_error() {
        let e = JitError::Effect(EffectError::Handler("oops".to_string()));
        let outcome = map_jit_error(e);
        assert!(matches!(outcome, JitOutcome::Sdk(_)));
    }

    // --- YieldError tests ---

    #[test]
    fn user_error_becomes_agent_error_no_message() {
        let e = JitError::Yield(YieldError::UserError);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::AgentError(AgentRuntimeError { message: None })
        ));
    }

    #[test]
    fn user_error_msg_becomes_agent_error_with_message() {
        let e = JitError::Yield(YieldError::UserErrorMsg("agent blew up".to_string()));
        let outcome = map_jit_error(e);
        assert!(
            matches!(outcome, JitOutcome::AgentError(AgentRuntimeError { ref message }) if message.as_deref() == Some("agent blew up"))
        );
    }

    #[test]
    fn undefined_becomes_agent_error() {
        // Haskell's `undefined` is semantically equivalent to `error` with no message;
        // surfaced as AgentError rather than a crash.
        let e = JitError::Yield(YieldError::Undefined);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::AgentError(AgentRuntimeError { message: None })
        ));
    }

    #[test]
    fn stack_overflow_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::StackOverflow);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn heap_overflow_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::HeapOverflow);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn yield_signal_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::Signal(11));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn division_by_zero_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::DivisionByZero);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn overflow_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::Overflow);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn blackhole_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::BlackHole);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn bad_thunk_state_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::BadThunkState(42));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn null_fun_ptr_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::NullFunPtr);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn bad_fun_ptr_tag_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::BadFunPtrTag(99));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn unresolved_var_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::UnresolvedVar(0xdeadbeef));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn type_metadata_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::TypeMetadata);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn unexpected_tag_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::UnexpectedTag(0xff));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn unexpected_con_tag_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::UnexpectedConTag(99));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn bad_val_fields_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::BadValFields(0));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn bad_e_fields_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::BadEFields(3));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn bad_union_fields_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::BadUnionFields(1));
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }

    #[test]
    fn null_pointer_becomes_runtime_crashed() {
        let e = JitError::Yield(YieldError::NullPointer);
        let outcome = map_jit_error(e);
        assert!(matches!(
            outcome,
            JitOutcome::Runtime(RuntimeError::RuntimeCrashed)
        ));
    }
}
