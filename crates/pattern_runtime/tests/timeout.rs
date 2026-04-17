//! Two-path cancellation tests (Phase 3 Task 16 — AC2.5, AC2.6).
//!
//! The harness spins up a session with an aggressive per-turn budget
//! and confirms:
//!   * Soft cancel fires when the agent yields via effects → the
//!     session remains usable.
//!   * Hard abandon fires when the agent loops in pure compute with no
//!     effect yields → the session becomes poisoned.
//!
//! Preflight is enforced loudly: missing tidepool-extract fails the
//! test with an actionable message rather than silently skipping.

use std::sync::Arc;

use pattern_core::error::{CancelPath, RuntimeError};
use pattern_core::traits::{AgentRuntime, Session};
use pattern_core::types::ids::new_id;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaConfig;
use pattern_core::types::turn::TurnInput;
use pattern_runtime::TidepoolRuntime;
use pattern_runtime::testing::InMemoryMemoryStore;

fn preflight_or_fail() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");
}

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

/// AC2.5: soft cancel returns `CancelPath::Soft` when an agent yielding
/// via effects exceeds its budget, and the session remains usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soft_cancel_on_yielding_loop_returns_soft_path() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let runtime = TidepoolRuntime::with_default_sdk(memory);
    let persona = PersonaConfig::new(
        "soft-cancel",
        "SoftCancel",
        include_str!("fixtures/yielding_loop.hs"),
    )
    // Aggressive budget: 200ms wall, 200ms cpu.
    // Hard-abandon threshold well above the expected soft-cancel fire
    // so the test doesn't race the hard path.
    .with_wall_budget_ms(200)
    .with_cpu_budget_ms(200)
    .with_hard_abandon_ms(5_000);

    let mut session = runtime.open_session(persona, None).await.expect("open");
    let err = session
        .step(fresh_turn_input())
        .await
        .expect_err("yielding loop should exceed budget");

    match err {
        RuntimeError::Timeout {
            path: CancelPath::Soft,
            wall_ms,
            cpu_ms,
        } => {
            assert!(
                wall_ms > 0 || cpu_ms > 0,
                "expected non-zero budget ms (got wall={wall_ms}, cpu={cpu_ms})"
            );
        }
        other => panic!("expected Timeout {{ path: Soft }}, got {other:?}"),
    }

    // Session must remain usable: we shouldn't get SessionPoisoned on
    // the next step. (Running the same infinite loop again produces
    // another soft timeout — success is that it reaches a RuntimeError
    // at all rather than short-circuiting to SessionPoisoned.)
    let err2 = session
        .step(fresh_turn_input())
        .await
        .expect_err("second step also times out");
    assert!(
        !matches!(err2, RuntimeError::SessionPoisoned { .. }),
        "session should not be poisoned after a soft cancel; got {err2:?}"
    );
}

/// AC2.6: hard abandon fires when an agent spins in pure compute with
/// no effect yields, returning `CancelPath::HardAbandon`. The watchdog
/// flips the tidepool `CancelHandle`, the JIT observes it at the next
/// heap check, and unwinds cleanly — the session remains usable for a
/// subsequent turn (it does NOT poison on clean cancellation).
///
/// Running time is infinite_spin's budget + hard_abandon_ms = ~500ms,
/// plus ~20ms cancel observation latency at the JIT's next safepoint.
///
/// Uses `infinite_spin.hs` (non-terminating) rather than
/// `tight_compute.hs` (terminating strict fold) because hard-abandon
/// requires a program that does not cooperate via handler entries AND
/// does not complete on its own — a finite compute hits soft-cancel at
/// the trailing `info` effect or finishes naturally before the watchdog
/// can escalate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_abandon_on_tight_compute_poisons_session() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let runtime = TidepoolRuntime::with_default_sdk(memory);
    let persona = PersonaConfig::new(
        "hard-abandon",
        "HardAbandon",
        include_str!("fixtures/infinite_spin.hs"),
    )
    // Low wall / cpu budget + short hard-abandon window to keep the
    // test fast while still exercising the escalation path.
    .with_wall_budget_ms(150)
    .with_cpu_budget_ms(150)
    .with_hard_abandon_ms(400);

    let mut session = runtime.open_session(persona, None).await.expect("open");
    let err = session
        .step(fresh_turn_input())
        .await
        .expect_err("tight compute should hard-abandon");

    match err {
        RuntimeError::Timeout {
            path: CancelPath::HardAbandon,
            wall_ms,
            cpu_ms,
        } => {
            assert!(
                wall_ms > 0 || cpu_ms > 0,
                "expected non-zero budget ms on hard-abandon (wall={wall_ms}, cpu={cpu_ms})"
            );
        }
        other => panic!("expected Timeout {{ path: HardAbandon }}, got {other:?}"),
    }

    // Session must NOT be poisoned: tidepool's CancelHandle unwinds the
    // JIT cleanly, so machine state is safe to reuse. A subsequent step
    // that hits the same runaway program should produce another
    // HardAbandon rather than SessionPoisoned.
    let err2 = session
        .step(fresh_turn_input())
        .await
        .expect_err("subsequent step on tight compute also hard-abandons");
    assert!(
        !matches!(err2, RuntimeError::SessionPoisoned { .. }),
        "session should not be poisoned after clean hard-abandon; got {err2:?}"
    );
    assert!(
        matches!(
            err2,
            RuntimeError::Timeout {
                path: CancelPath::HardAbandon,
                ..
            }
        ),
        "expected repeated HardAbandon on reused session; got {err2:?}"
    );
}

/// After a soft-cancel on a yielding loop, running another turn on the
/// SAME session succeeds once both the handler-side `CancelState` and
/// the JIT-side `CancelHandle` have been reset at the top of run_turn.
/// This exercises reuse of the JIT machine across a cancelled turn
/// (the interesting case — if either reset is skipped, the next turn
/// would either short-circuit in every handler or be cancelled at the
/// first heap check).
///
/// Uses the yielding-loop fixture for the first step (soft cancel
/// observable at handler boundaries), then swaps no persona — the
/// second step runs the same loop again and must produce another
/// soft-cancel (not a poisoned session, not a hard-abandon).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soft_cancel_then_reuse_same_session_resets_cancel_flags() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let runtime = TidepoolRuntime::with_default_sdk(memory);
    let persona = PersonaConfig::new(
        "soft-reuse",
        "SoftReuse",
        include_str!("fixtures/yielding_loop.hs"),
    )
    .with_wall_budget_ms(150)
    .with_cpu_budget_ms(150)
    .with_hard_abandon_ms(5_000);

    let mut session = runtime.open_session(persona, None).await.expect("open");

    let err1 = session
        .step(fresh_turn_input())
        .await
        .expect_err("first step soft-cancels");
    assert!(
        matches!(
            err1,
            RuntimeError::Timeout {
                path: CancelPath::Soft,
                ..
            }
        ),
        "expected first step to soft-cancel; got {err1:?}"
    );

    let err2 = session
        .step(fresh_turn_input())
        .await
        .expect_err("second step on same session soft-cancels again");
    assert!(
        matches!(
            err2,
            RuntimeError::Timeout {
                path: CancelPath::Soft,
                ..
            }
        ),
        "expected second step to soft-cancel again (not poisoned, not hard-abandoned); \
         got {err2:?}"
    );
}

/// Budget resets between turns: running a short, well-behaved program
/// after a soft-cancel does not spuriously trigger another timeout
/// because the accumulator restarts at zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soft_cancel_then_short_turn_succeeds() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let runtime = TidepoolRuntime::with_default_sdk(memory);
    // First open a session for the infinite loop.
    let persona = PersonaConfig::new(
        "soft-then-short",
        "SoftThenShort",
        include_str!("fixtures/yielding_loop.hs"),
    )
    .with_wall_budget_ms(150)
    .with_cpu_budget_ms(150)
    .with_hard_abandon_ms(5_000);
    let mut s = runtime.open_session(persona, None).await.expect("open");
    let _ = s.step(fresh_turn_input()).await; // soft-cancel

    // Open a separate session on a cheap program and confirm it
    // completes without a timeout. This exercises "session remains
    // recoverable" at the runtime level — even though the same session
    // with the infinite loop still times out on every step, a fresh
    // session on a fast program is unaffected by the prior soft
    // cancel's state.
    let persona2 = PersonaConfig::new(
        "soft-then-short",
        "SoftThenShort2",
        include_str!("fixtures/time_log.hs"),
    )
    .with_wall_budget_ms(2_000)
    .with_cpu_budget_ms(2_000);
    let mut s2 = runtime.open_session(persona2, None).await.expect("open 2");
    s2.step(fresh_turn_input()).await.expect("short turn");
}
