//! Task 18 — AC2.8: GHC / JIT crash surfacing + session poisoning.
//!
//! Two-part coverage:
//!
//! 1. **Error-map layer.** User-visible JIT signals / heap-bridge / yield
//!    signals all map to `RuntimeError::RuntimeCrashed`. We can't reliably
//!    drive the JIT into a signal from a test program, so we inject fake
//!    `JitError` variants through `map_jit_error` and assert the mapped
//!    outcome. (The `error_map` module has a full unit-test matrix; these
//!    integration assertions pin the *public* surface so future crate-
//!    reshuffling can't silently drop coverage of the path the orchestrator
//!    consumes.)
//!
//! 2. **Session poisoning.** If a session becomes poisoned, subsequent
//!    `step()` calls short-circuit with `RuntimeError::SessionPoisoned`
//!    rather than running another turn. The real path that flips the flag
//!    (join-error during hard-abandon) is inherently racy, so we use the
//!    `__poison_for_tests` hook to deterministically trigger the
//!    short-circuit and verify the surfaced error.

use std::sync::Arc;

use pattern_core::error::RuntimeError;
use pattern_core::traits::{AgentRuntime, Session};
use pattern_core::types::ids::new_id;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaConfig;
use pattern_core::types::turn::TurnInput;
use pattern_runtime::TidepoolRuntime;
use pattern_runtime::testing::InMemoryMemoryStore;
use pattern_runtime::tidepool::error_map::{JitOutcome, map_jit_error};
use tidepool_codegen::jit_machine::JitError;
use tidepool_codegen::signal_safety::SignalError;
use tidepool_codegen::yield_type::YieldError;

fn fresh_turn_input() -> TurnInput {
    TurnInput {
        turn_id: new_id(),
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![],
    }
}

/// AC2.8 part 1: a raw JIT signal (e.g., SIGSEGV during codegen or heap
/// bridge) maps to `RuntimeError::RuntimeCrashed`. This is the error
/// shape the session orchestrator will see on a genuine GHC-level crash.
#[test]
fn jit_signal_maps_to_runtime_crashed() {
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
    use tidepool_codegen::heap_bridge::BridgeError;
    let outcome = map_jit_error(JitError::HeapBridge(BridgeError::UnexpectedHeapTag(0xff)));
    assert!(
        matches!(outcome, JitOutcome::Runtime(RuntimeError::RuntimeCrashed)),
        "expected RuntimeCrashed on HeapBridge error",
    );
}

/// AC2.8 part 2: once a session is poisoned, `step()` must short-circuit
/// with `RuntimeError::SessionPoisoned` rather than running another
/// turn.
///
/// We use `__poison_for_tests` to deterministically flip the flag. The
/// real-world path is the JoinError branch in
/// `session::run_turn`'s hard-abandon arm — reproducing that
/// deterministically in a test would require racing a blocking-task
/// panic with the watchdog escalation, which is both slow and flaky.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ghc_crash_poisons_session() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let memory = Arc::new(InMemoryMemoryStore::new());
    let runtime = TidepoolRuntime::with_default_sdk(memory);
    // A well-behaved program so the only way `step` can fail is via
    // the poison short-circuit we flip below.
    let persona = PersonaConfig::new(
        "ghc-crash-poison",
        "GhcCrashPoison",
        include_str!("fixtures/time_log.hs"),
    );
    let mut session = runtime.open_session(persona, None).await.expect("open");

    // Sanity: an un-poisoned step succeeds.
    session
        .step(fresh_turn_input())
        .await
        .expect("first step runs fine on a fresh session");

    // Flip the poison flag as if a hard-abandon join-error had fired.
    session.__poison_for_tests();

    // Subsequent step must short-circuit with SessionPoisoned (not a
    // fresh timeout, not a fresh run).
    let err = session
        .step(fresh_turn_input())
        .await
        .expect_err("step on poisoned session should error");

    match err {
        RuntimeError::SessionPoisoned { reason } => {
            assert!(
                !reason.is_empty(),
                "SessionPoisoned.reason should be populated, got empty string",
            );
        }
        other => panic!("expected SessionPoisoned, got {other:?}"),
    }

    // And again — the poison is sticky.
    let err2 = session
        .step(fresh_turn_input())
        .await
        .expect_err("poison is sticky across subsequent steps");
    assert!(
        matches!(err2, RuntimeError::SessionPoisoned { .. }),
        "expected repeated SessionPoisoned, got {err2:?}",
    );
}
