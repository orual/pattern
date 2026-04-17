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
                    // SDK handler failure during run — escalate to compile-internal for now.
                    RuntimeError::CompileInternal {
                        reason: sdk.to_string(),
                    }
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
