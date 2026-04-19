//! JIT effect machine wrapper.
//!
//! [`SessionMachine`] wraps `tidepool_codegen::JitEffectMachine` with session-scoped
//! state and thread-safety documentation. One `SessionMachine` per session: compiled
//! once at `Session::open`, re-run on every turn via `run`.

use pattern_core::error::RuntimeError;
use tidepool_codegen::jit_machine::{CancelHandle, JitEffectMachine};
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

use super::compile::CompiledProgram;

/// Wraps a tidepool `JitEffectMachine` with session-scoped nursery + Send assertions.
///
/// `JitEffectMachine` internally uses an `unsafe impl Send` on its hot loop;
/// this wrapper documents Pattern's contract: at most one thread mutates it at a
/// time (enforced by `&mut self` on `run`). Multiple `SessionMachine`s across
/// distinct sessions run concurrently without interference (AC2.10).
///
pub struct SessionMachine {
    inner: JitEffectMachine,
    data_cons: DataConTable,
    /// GC nursery size in bytes. Retained for diagnostic / future resize use.
    #[allow(dead_code)]
    nursery_size: usize,
}

impl SessionMachine {
    /// JIT-compile a `CompiledProgram` into an executable machine.
    ///
    /// `nursery_size` is the GC nursery heap size in bytes. A value of `1 << 20` (1 MiB)
    /// is a reasonable default for most agents; increase if agents use large data structures.
    pub fn new(program: CompiledProgram, nursery_size: usize) -> Result<Self, RuntimeError> {
        let inner = JitEffectMachine::compile(&program.core, &program.data_cons, nursery_size)
            .map_err(|e| match crate::tidepool::error_map::map_jit_error(e) {
                crate::tidepool::error_map::JitOutcome::Runtime(rt) => rt,
                crate::tidepool::error_map::JitOutcome::AgentError(ae) => {
                    RuntimeError::CompileInternal {
                        reason: ae
                            .message
                            .unwrap_or_else(|| "agent error during JIT compile".into()),
                    }
                }
                crate::tidepool::error_map::JitOutcome::Sdk(sdk) => RuntimeError::CompileInternal {
                    reason: sdk.to_string(),
                },
            })?;
        Ok(Self {
            inner,
            data_cons: program.data_cons,
            nursery_size,
        })
    }

    /// Run the compiled program to completion, dispatching effects through `handlers`.
    ///
    /// `user` is the per-turn user context threaded through all effect dispatch calls.
    /// Re-runnable without recompile: each call is an independent turn.
    pub fn run<U, H>(&mut self, handlers: &mut H, user: &U) -> Result<Value, RuntimeError>
    where
        H: DispatchEffect<U>,
    {
        self.inner
            .run(&self.data_cons, handlers, user)
            .map_err(|e| match crate::tidepool::error_map::map_jit_error(e) {
                crate::tidepool::error_map::JitOutcome::Runtime(rt) => rt,
                crate::tidepool::error_map::JitOutcome::AgentError(_ae) => {
                    // Agent called Haskell `error` — surface as a runtime crash for now.
                    // Phase 4 will introduce proper agent-error handling at the orchestrator.
                    RuntimeError::RuntimeCrashed
                }
                crate::tidepool::error_map::JitOutcome::Sdk(sdk) => {
                    // SDK handler failure during run — route to the
                    // dedicated SdkHandlerFailed variant so callers can
                    // match on the category without string-scanning a
                    // generic CompileInternal message. `handler` and
                    // `reason` are extracted from the underlying
                    // EffectError; see `sdk_failure_parts` for the
                    // contract when the handler id isn't surfaced.
                    let (handler, reason) = sdk_failure_parts(&sdk);
                    RuntimeError::SdkHandlerFailed { handler, reason }
                }
            })
    }

    /// Access the data constructor table used by this machine.
    ///
    /// Needed for `FromCore::from_value` round-trips on the result value.
    pub fn table(&self) -> &DataConTable {
        &self.data_cons
    }

    /// Obtain an external cancel handle. Clone-able, `Send + Sync`. Flipping
    /// it via [`CancelHandle::cancel`] causes the JIT to observe cancellation
    /// at its next GC safepoint and return with
    /// `JitError::Yield(YieldError::Cancelled)`, which `error_map` converts
    /// into `RuntimeError::Timeout { path: CancelPath::HardAbandon }` with
    /// placeholder wall/cpu — session.rs fills in real bookkeeping.
    ///
    /// The flag is per-machine, not per-run. Call [`CancelHandle::reset`]
    /// between turns if a cancelled run is followed by a reuse.
    pub fn cancel_handle(&self) -> CancelHandle {
        self.inner.cancel_handle()
    }
}

/// Extract `(handler, reason)` from an [`crate::tidepool::error_map::SdkError`]
/// for surfacing as [`RuntimeError::SdkHandlerFailed`].
///
/// `tidepool_effect::EffectError` does not carry a structured handler
/// identity — it's one of a few flat variants with `{error}` interpolated
/// messages. Pattern's own handlers conventionally prefix their
/// `EffectError::Handler(...)` strings with `"Pattern.<Module>..."` so
/// the module name is recoverable via a light parse. For non-`Handler`
/// variants (Eval / Bridge / Unhandled / etc.) we fall back to
/// `handler = "unknown"` and use the `Display` as the full reason.
///
/// TODO(post-foundation): once `tidepool-effect` surfaces a dedicated
/// handler-id on `EffectError`, thread it through here and drop the
/// string parse.
fn sdk_failure_parts(sdk: &crate::tidepool::error_map::SdkError) -> (String, String) {
    use tidepool_effect::EffectError;
    match &sdk.0 {
        EffectError::Handler(msg) => parse_pattern_handler(msg),
        other => ("unknown".to_string(), other.to_string()),
    }
}

/// Heuristic: if `msg` starts with `Pattern.<Module>` (optionally
/// followed by `.<Op>` and then `:` or whitespace), return
/// `("Pattern.<Module>", rest_of_message)`. Otherwise return
/// `("unknown", msg)`.
fn parse_pattern_handler(msg: &str) -> (String, String) {
    let Some(rest) = msg.strip_prefix("Pattern.") else {
        return ("unknown".to_string(), msg.to_string());
    };
    // Take the first path segment: up to the next '.', ':', or whitespace.
    let end = rest
        .find(|c: char| c == '.' || c == ':' || c.is_whitespace())
        .unwrap_or(rest.len());
    let module = &rest[..end];
    if module.is_empty() {
        return ("unknown".to_string(), msg.to_string());
    }
    (format!("Pattern.{module}"), msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_effect::EffectError;

    #[test]
    fn parse_pattern_handler_extracts_module_prefix() {
        let (h, _r) = parse_pattern_handler("Pattern.Memory.Get: no block named \"x\"");
        assert_eq!(h, "Pattern.Memory");
    }

    #[test]
    fn parse_pattern_handler_handles_bare_module() {
        let (h, _r) = parse_pattern_handler("Pattern.File is not implemented");
        assert_eq!(h, "Pattern.File");
    }

    #[test]
    fn parse_pattern_handler_falls_back_on_no_prefix() {
        let (h, r) = parse_pattern_handler("nothing useful here");
        assert_eq!(h, "unknown");
        assert_eq!(r, "nothing useful here");
    }

    #[test]
    fn sdk_failure_parts_extracts_handler_from_handler_variant() {
        let sdk = crate::tidepool::error_map::SdkError(EffectError::Handler(
            "Pattern.Memory.Put: boom".into(),
        ));
        let (h, r) = sdk_failure_parts(&sdk);
        assert_eq!(h, "Pattern.Memory");
        assert!(r.contains("boom"));
    }

    #[test]
    fn sdk_failure_parts_falls_back_for_non_handler_variant() {
        let sdk = crate::tidepool::error_map::SdkError(EffectError::UnhandledEffect { tag: 42 });
        let (h, _r) = sdk_failure_parts(&sdk);
        assert_eq!(h, "unknown");
    }
}
