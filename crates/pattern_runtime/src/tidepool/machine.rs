//! JIT effect machine wrapper.
//!
//! [`SessionMachine`] wraps `tidepool_codegen::JitEffectMachine` with session-scoped
//! state and thread-safety documentation. One `SessionMachine` per session: compiled
//! once at `Session::open`, re-run on every turn via `run`.

use pattern_core::error::RuntimeError;
use tidepool_codegen::jit_machine::JitEffectMachine;
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
/// Fields are set in `new()` (Task 7+; body currently `todo!`).
#[allow(dead_code)]
pub struct SessionMachine {
    inner: JitEffectMachine,
    data_cons: DataConTable,
    /// GC nursery size in bytes. Retained for documentation; set at `new` time.
    nursery_size: usize,
}

impl SessionMachine {
    /// JIT-compile a `CompiledProgram` into an executable machine.
    ///
    /// `nursery_size` is the GC nursery heap size in bytes. A value of `1 << 20` (1 MiB)
    /// is a reasonable default for most agents; increase if agents use large data structures.
    pub fn new(_program: CompiledProgram, _nursery_size: usize) -> Result<Self, RuntimeError> {
        // JitEffectMachine::compile(&program.core, &program.data_cons, nursery_size)
        // .map_err(crate::tidepool::error_map::map_jit_error)
        // phase: 3; AC: AC2.1
        todo!("implement per tidepool_codegen::JitEffectMachine::compile")
    }

    /// Run the compiled program to completion, dispatching effects through `handlers`.
    ///
    /// `user` is the per-turn user context threaded through all effect dispatch calls.
    /// Re-runnable without recompile: each call is an independent turn.
    pub fn run<U, H>(&mut self, _handlers: &mut H, _user: &U) -> Result<Value, RuntimeError>
    where
        H: DispatchEffect<U>,
    {
        // self.inner.run(&self.data_cons, handlers, user)
        //   .map_err(crate::tidepool::error_map::map_jit_error)
        // phase: 3; AC: AC2.1
        todo!("implement per JitEffectMachine::run wrapper")
    }
}
