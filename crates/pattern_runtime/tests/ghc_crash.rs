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
//!    lives in `session::run_turn`'s hard-abandon arm, specifically the
//!    `join_result.is_err() => inner.poisoned = true` branch. Reaching
//!    that branch deterministically from an integration test is
//!    infeasible at the protocol level:
//!
//!      * The hard-abandon arm only runs after the watchdog escalates.
//!      * Watchdog escalation requires the JIT to have NOT entered any
//!        handler for `hard_abandon_threshold` milliseconds — i.e. pure
//!        compute with no yields.
//!      * A `JoinError` from `tokio::task::spawn_blocking` requires the
//!        blocking task to panic (or be aborted by the runtime; we do
//!        not abort it).
//!      * A panic inside the spawn_blocking task can only come from
//!        (a) a handler panic — ruled out, because no handler is
//!        executing during pure compute, OR (b) an internal tidepool
//!        panic — we cannot reliably induce one from agent-level
//!        code, and doing so would defeat the controlled-test
//!        premise anyway.
//!
//!    Reproducing this at the integration level would require either a
//!    test-only side channel that forces a spawn_blocking panic post
//!    hard-abandon (equivalent to the existing `__poison_for_tests`
//!    hook, at more cost), or a custom instrumented `run_turn` that
//!    only exists for tests. Both are strictly worse than the hook:
//!    the hook is small, well-commented, and asserts the same
//!    observable outcome (subsequent steps short-circuit with
//!    `SessionPoisoned`). This tests the short-circuit, not the
//!    production poisoning path.

use std::sync::Arc;

use pattern_core::error::RuntimeError;
use pattern_core::traits::{AgentRuntime, Session};
use pattern_core::types::ids::{BatchId, new_id};
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaConfig;
use pattern_core::types::turn::TurnInput;
use pattern_runtime::TidepoolRuntime;
use pattern_runtime::testing::{InMemoryMemoryStore, NopProviderClient};
use pattern_runtime::tidepool::error_map::{JitOutcome, map_jit_error};
use tidepool_codegen::jit_machine::JitError;
use tidepool_codegen::signal_safety::SignalError;
use tidepool_codegen::yield_type::YieldError;

fn fresh_turn_input() -> TurnInput {
    TurnInput {
        turn_id: new_id(),
        batch_id: BatchId::from(new_id()),
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
/// We use `__poison_for_tests` to deterministically flip the flag.
/// This tests the short-circuit, not the production poisoning path.
/// See the module-level doc for the detailed infeasibility argument
/// (mutually exclusive protocol-level conditions rule out driving the
/// real JoinError-during-hard-abandon branch from integration tests).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ghc_crash_poisons_session() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory, provider);
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
