//! Task 18 — AC2.8: GHC / JIT crash surfacing.
//!
//! Error-map layer tests: user-visible JIT signals / heap-bridge / yield
//! signals all map to `RuntimeError::RuntimeCrashed`. We can't reliably
//! drive the JIT into a signal from a test program, so we inject fake
//! `JitError` variants through `map_jit_error` and assert the mapped
//! outcome.
//!
//! The session-poisoning test (`ghc_crash_poisons_session`) was retired in
//! Phase 6 Task B alongside the legacy `Session::step` / `run_turn` path
//! that it exercised. Session poisoning behaviour is now tested through the
//! agent-loop path where applicable.

use pattern_runtime::tidepool::error_map::{JitOutcome, map_jit_error};
use tidepool_codegen::jit_machine::JitError;
use tidepool_codegen::signal_safety::SignalError;
use tidepool_codegen::yield_type::YieldError;

/// AC2.8 part 1: a raw JIT signal (e.g., SIGSEGV during codegen or heap
/// bridge) maps to `RuntimeError::RuntimeCrashed`. This is the error
/// shape the session orchestrator will see on a genuine GHC-level crash.
#[test]
fn jit_signal_maps_to_runtime_crashed() {
    use pattern_core::error::RuntimeError;
    // 11 = SIGSEGV. The specific signal number is not important; the
    // mapping normalises all `Signal(_)` to `RuntimeCrashed`.
    let outcome = map_jit_error(JitError::Signal(SignalError(11)));
    assert!(
        matches!(outcome, JitOutcome::Runtime(RuntimeError::RuntimeCrashed)),
        "expected RuntimeCrashed on SIGSEGV-ish Signal",
    );
}

/// AC2.8 part 1 (yield path): a signal surfaced through the yield
/// machinery (e.g., signal raised inside a handler frame) also maps to
/// `RuntimeError::RuntimeCrashed`.
#[test]
fn yield_signal_maps_to_runtime_crashed() {
    use pattern_core::error::RuntimeError;
    let outcome = map_jit_error(JitError::Yield(YieldError::Signal(11)));
    assert!(
        matches!(outcome, JitOutcome::Runtime(RuntimeError::RuntimeCrashed)),
        "expected RuntimeCrashed on YieldError::Signal",
    );
}

/// AC2.8 part 1 (heap path): a bridge-error during Haskell value decode
/// indicates a broken runtime contract and also surfaces as a crash.
#[test]
fn heap_bridge_maps_to_runtime_crashed() {
    use pattern_core::error::RuntimeError;
    use tidepool_codegen::heap_bridge::BridgeError;
    let outcome = map_jit_error(JitError::HeapBridge(BridgeError::UnexpectedHeapTag(0xff)));
    assert!(
        matches!(outcome, JitOutcome::Runtime(RuntimeError::RuntimeCrashed)),
        "expected RuntimeCrashed on HeapBridge error",
    );
}
