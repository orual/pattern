//! Two-path cancellation harness for Tidepool execution (Phase 3 Task 16).
//!
//! Tidepool has no public interrupt API. Pattern's approach:
//! 1. Soft cancel via shared atomic flag checked by every effect handler.
//! 2. Hard abandon (last resort) when no effect yields observed for long enough.
//!
//! Budget consumption pauses while the JIT is inside an effect handler
//! (handler owns its own timeout, typically for I/O). Budget counts
//! time-in-JIT-compute, not wall-clock-including-I/O.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pattern_core::types::snapshot::PersonaConfig;

/// Sentinel string embedded in `EffectError::Handler(...)` to mark a
/// handler-side cooperative cancellation. The harness matches on this to
/// map the resulting JIT error back to a soft-cancel outcome.
///
/// We use a sentinel rather than extending `tidepool_effect::EffectError`
/// with a `Cancelled` variant because `EffectError` is owned by the
/// upstream `tidepool-effect` crate; adding variants requires coordinating
/// an upstream change. This sentinel is a stable, greppable marker that
/// only Pattern's own handlers ever emit.
pub const CANCELLED_SENTINEL: &str = "__pattern_cancelled__";

/// Per-turn execution budget. Derived from [`PersonaConfig`] fields at
/// session-open time; persisted on [`crate::session::SessionContext`] for
/// the lifetime of the session.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Wall-clock budget for time-in-JIT-compute. Default 30s per turn.
    pub wall: Duration,
    /// CPU budget for time-in-JIT-compute. Default 10s per turn.
    ///
    /// On non-Linux platforms the harness falls back to wall-time
    /// accumulation for CPU tracking (see `sample_thread_cpu`).
    pub cpu: Duration,
    /// When no effect invocations observed for this long beyond the cpu
    /// budget, escalate to hard-abandon. Default: 2× cpu budget.
    pub hard_abandon_threshold: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        let cpu = Duration::from_secs(10);
        Self {
            wall: Duration::from_secs(30),
            cpu,
            hard_abandon_threshold: cpu * 2,
        }
    }
}

impl Budget {
    /// Derive a budget from a [`PersonaConfig`], filling unset fields with
    /// the defaults defined in [`Budget::default`].
    pub fn from_persona(persona: &PersonaConfig) -> Self {
        let defaults = Self::default();
        let wall = persona
            .wall_budget_ms
            .map(Duration::from_millis)
            .unwrap_or(defaults.wall);
        let cpu = persona
            .cpu_budget_ms
            .map(Duration::from_millis)
            .unwrap_or(defaults.cpu);
        let hard_abandon_threshold = persona
            .hard_abandon_ms
            .map(Duration::from_millis)
            .unwrap_or(cpu * 2);
        Self {
            wall,
            cpu,
            hard_abandon_threshold,
        }
    }
}

/// Counter shared with every effect handler. Handlers increment on entry,
/// decrement on exit. The watchdog reads this to determine whether budget
/// should accumulate (budget pauses while a handler is executing so slow
/// I/O does not spuriously trigger a soft-cancel).
#[derive(Debug, Default)]
pub struct HandlerGate {
    in_flight: AtomicU32,
    /// Monotonic counter of handler entries. Allows the watchdog to detect
    /// "no effect yields observed since time T" by comparing two samples.
    entries: AtomicU64,
}

impl HandlerGate {
    /// Construct a fresh gate with zero handlers in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Called by a handler on entry.
    pub fn enter(&self) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        self.entries.fetch_add(1, Ordering::SeqCst);
    }

    /// Called by a handler on exit.
    pub fn exit(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    /// True iff at least one handler is currently executing.
    pub fn in_handler(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) > 0
    }

    /// Snapshot the monotonic entry counter. Used by the watchdog to
    /// detect progress between samples.
    pub fn entry_count(&self) -> u64 {
        self.entries.load(Ordering::SeqCst)
    }
}

/// RAII guard: calls `HandlerGate::enter` on construction and
/// `HandlerGate::exit` on drop. Handlers use this to make gate state
/// panic-safe.
pub struct HandlerGuard<'a> {
    gate: &'a HandlerGate,
}

impl<'a> HandlerGuard<'a> {
    /// Enter the gate; the guard's drop will exit.
    pub fn enter(gate: &'a HandlerGate) -> Self {
        gate.enter();
        Self { gate }
    }
}

impl<'a> Drop for HandlerGuard<'a> {
    fn drop(&mut self) {
        self.gate.exit();
    }
}

/// Outcome of a bounded execution: either normal completion or a
/// cancellation produced by the watchdog.
#[derive(Debug)]
#[allow(dead_code)]
pub enum BoundedOutcome {
    /// The JIT completed normally. Soft-cancel did not fire.
    Completed,
    /// The watchdog fired a soft cancel; the JIT cooperated by returning
    /// at the next effect boundary. Session remains usable.
    SoftCancelled {
        /// Observed wall budget consumption in ms.
        wall_ms: u64,
        /// Observed cpu budget consumption in ms.
        cpu_ms: u64,
    },
    /// The watchdog escalated to hard-abandonment; the blocking task has
    /// been detached. Session is poisoned.
    HardAbandoned {
        /// Observed wall budget consumption in ms.
        wall_ms: u64,
        /// Observed cpu budget consumption in ms.
        cpu_ms: u64,
    },
}

/// Shared state driving the cancellation handshake. Constructed once per
/// session and lives on [`crate::session::SessionContext`] for the
/// session's lifetime.
#[derive(Debug, Default)]
pub struct CancelState {
    /// Set by the watchdog when budget is exhausted; checked by every
    /// effect handler at entry. Handlers returning on-cancelled propagate
    /// an `EffectError::Handler(CANCELLED_SENTINEL)`.
    pub cancellation: AtomicBool,
    /// Handler-in-flight counter used by the watchdog to pause budget.
    pub gate: HandlerGate,
}

impl CancelState {
    /// Construct a fresh cancel state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset flag between turns so a second `step` starts clean.
    pub fn reset(&self) {
        self.cancellation.store(false, Ordering::SeqCst);
    }

    /// True iff a soft cancel has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::SeqCst)
    }
}

/// Spawn the watchdog task. Returns a handle that the session drops
/// (aborting the watchdog) when the JIT completes normally.
///
/// The watchdog samples every `sample_interval`. It accumulates budget
/// only while `gate.in_handler()` is false (i.e., only when the JIT is
/// running agent compute, not waiting on a handler).
///
/// Hard-abandonment fires when the soft-cancel flag has been set AND no
/// new handler entries have been observed for `hard_abandon_threshold`.
/// That means the JIT is spinning in pure compute without yielding, so
/// the cooperative flag cannot be observed.
pub fn spawn_watchdog(
    state: Arc<CancelState>,
    budget: Budget,
    sample_interval: Duration,
) -> tokio::task::JoinHandle<BoundedOutcome> {
    tokio::spawn(async move {
        let mut jit_wall_accumulated = Duration::ZERO;
        let mut jit_cpu_accumulated = Duration::ZERO;
        let mut last_sample = Instant::now();
        let mut last_entry_count = state.gate.entry_count();
        let mut last_entry_observed_at = Instant::now();
        let mut soft_fired_at: Option<Instant> = None;

        loop {
            tokio::time::sleep(sample_interval).await;
            let now = Instant::now();
            let interval = now.duration_since(last_sample);
            last_sample = now;

            // Track entry-count progress. Any entry observed since last
            // sample counts as progress; reset the no-yield clock.
            let entries = state.gate.entry_count();
            if entries != last_entry_count {
                last_entry_count = entries;
                last_entry_observed_at = now;
            }

            // Only accumulate budget when the JIT is actively running
            // compute (no handler in flight).
            if !state.gate.in_handler() {
                jit_wall_accumulated += interval;
                jit_cpu_accumulated += sample_thread_cpu().unwrap_or(interval);
            }

            // Primary budget check.
            if jit_wall_accumulated >= budget.wall || jit_cpu_accumulated >= budget.cpu {
                if soft_fired_at.is_none() {
                    state.cancellation.store(true, Ordering::SeqCst);
                    soft_fired_at = Some(now);
                    tracing::info!(
                        wall_ms = jit_wall_accumulated.as_millis() as u64,
                        cpu_ms = jit_cpu_accumulated.as_millis() as u64,
                        "soft cancel fired; JIT will exit at next effect boundary"
                    );
                }

                // Escalate to hard-abandon if we've been waiting too long
                // without ANY effect entry (which is our cooperative
                // signal). We measure from the later of
                // soft-fired and last-entry-observed.
                let reference = soft_fired_at
                    .map(|t| t.max(last_entry_observed_at))
                    .unwrap_or(last_entry_observed_at);
                if now.duration_since(reference) > budget.hard_abandon_threshold {
                    return BoundedOutcome::HardAbandoned {
                        wall_ms: jit_wall_accumulated.as_millis() as u64,
                        cpu_ms: jit_cpu_accumulated.as_millis() as u64,
                    };
                }
            }
        }
    })
}

/// Sample the calling thread's CPU time. Currently falls back to
/// returning `None` on all platforms — the watchdog then uses the wall
/// interval as a CPU estimate. Linux-specific `/proc/self/task/<tid>/stat`
/// sampling is future work (AC2.6 notes this on Linux only).
///
/// This is pub(crate) so tests can stub/verify behaviour if needed; it is
/// not part of the crate's public API.
pub(crate) fn sample_thread_cpu() -> Option<Duration> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_from_persona_uses_defaults_when_unset() {
        let persona = PersonaConfig::new("a", "A", "x");
        let b = Budget::from_persona(&persona);
        let defaults = Budget::default();
        assert_eq!(b.wall, defaults.wall);
        assert_eq!(b.cpu, defaults.cpu);
        assert_eq!(b.hard_abandon_threshold, defaults.hard_abandon_threshold);
    }

    #[test]
    fn budget_from_persona_applies_overrides() {
        let persona = PersonaConfig::new("a", "A", "x")
            .with_wall_budget_ms(1000)
            .with_cpu_budget_ms(500)
            .with_hard_abandon_ms(2000);
        let b = Budget::from_persona(&persona);
        assert_eq!(b.wall, Duration::from_millis(1000));
        assert_eq!(b.cpu, Duration::from_millis(500));
        assert_eq!(b.hard_abandon_threshold, Duration::from_millis(2000));
    }

    #[test]
    fn handler_gate_enter_exit_balances() {
        let g = HandlerGate::new();
        assert!(!g.in_handler());
        {
            let _h = HandlerGuard::enter(&g);
            assert!(g.in_handler());
        }
        assert!(!g.in_handler());
    }

    #[test]
    fn handler_gate_entries_are_monotonic() {
        let g = HandlerGate::new();
        assert_eq!(g.entry_count(), 0);
        HandlerGuard::enter(&g);
        assert_eq!(g.entry_count(), 1);
        HandlerGuard::enter(&g);
        assert_eq!(g.entry_count(), 2);
    }

    #[test]
    fn cancel_state_reset_clears_flag() {
        let s = CancelState::new();
        s.cancellation.store(true, Ordering::SeqCst);
        assert!(s.is_cancelled());
        s.reset();
        assert!(!s.is_cancelled());
    }

    /// Use `CANCELLED_SENTINEL` to produce a handler error and match on
    /// the marker, confirming the sentinel is stable in a cancelled
    /// effect-error message.
    #[test]
    fn cancelled_sentinel_is_stable_identifier() {
        let msg = format!("{}: cancelled at Pattern.Time.Now", CANCELLED_SENTINEL);
        assert!(msg.contains(CANCELLED_SENTINEL));
    }

    /// When budget is exhausted and no handler entries are observed,
    /// the watchdog escalates to HardAbandoned after the
    /// hard_abandon_threshold elapses.
    ///
    /// This tests the watchdog in isolation — no JIT, no blocking
    /// thread to leak. We drive `CancelState` from the test itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_escalates_to_hard_abandon_when_no_effects_seen() {
        let state = Arc::new(CancelState::new());
        let budget = Budget {
            wall: Duration::from_millis(50),
            cpu: Duration::from_millis(50),
            hard_abandon_threshold: Duration::from_millis(100),
        };
        let handle = spawn_watchdog(state.clone(), budget, Duration::from_millis(10));
        let outcome = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("watchdog should terminate within 2s")
            .expect("watchdog task should not panic");
        match outcome {
            BoundedOutcome::HardAbandoned { wall_ms, cpu_ms } => {
                assert!(
                    wall_ms >= 50,
                    "expected wall budget consumed ({wall_ms}ms)"
                );
                assert!(
                    cpu_ms >= 50,
                    "expected cpu budget consumed ({cpu_ms}ms)"
                );
                assert!(state.is_cancelled(), "soft-cancel flag should be set too");
            }
            other => panic!("expected HardAbandoned, got {other:?}"),
        }
    }

    /// When handlers keep entering the gate (simulating cooperative
    /// yielding), the watchdog does NOT escalate to hard-abandon — it
    /// just waits for the cooperative soft-cancel to be observed.
    /// We verify this indirectly by asserting the watchdog is still
    /// running after several budget periods have elapsed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_does_not_escalate_while_handlers_yield() {
        let state = Arc::new(CancelState::new());
        let budget = Budget {
            wall: Duration::from_millis(50),
            cpu: Duration::from_millis(50),
            hard_abandon_threshold: Duration::from_millis(500),
        };
        let handle = spawn_watchdog(state.clone(), budget, Duration::from_millis(10));

        // Simulate an agent that enters a handler, runs briefly, and
        // loops back into another handler — i.e. it's yielding.
        let yielder = {
            let state = state.clone();
            tokio::spawn(async move {
                for _ in 0..20 {
                    let _g = HandlerGuard::enter(&state.gate);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
        };
        yielder.await.expect("yielder panic");

        // By now ~200ms has elapsed but handlers entered every 10ms
        // keeping `last_entry_observed_at` fresh. Watchdog should
        // still be waiting (not yet HardAbandoned).
        assert!(
            !handle.is_finished(),
            "watchdog should not have escalated while handlers yielded"
        );
        handle.abort();
    }
}
