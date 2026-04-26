//! Rust evaluator tasks for the timer-based wake conditions.
//!
//! `TaskTimeout` (one-shot) and `Interval` (recurring) — both ride
//! `tokio::time` primitives. The evaluators send
//! [`crate::mailbox::MailboxInput`] into the session's mailbox, with
//! an `Author::System { reason: ... }` origin carrying the
//! structured payload (the timed-out task ref, the interval period)
//! directly on the variant.
//!
//! Block-subscriber evaluators (BlockChanged, TaskDependencyResolved)
//! land in T8/T9 alongside the `pattern_memory::subscriber` fan-out
//! hook.

use std::time::Duration;

use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::origin::{SpanCompare, SystemReason};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::mailbox::MailboxInput;

use super::registry::{WakeError, wake_mailbox_input};

/// Convert a `jiff::Span` carrying only wall-clock units (h/m/s/ms/us/ns
/// + days) into a `std::time::Duration`. Returns
/// [`WakeError::NonWallClockSpan`] when the span carries calendar
/// units (years/months/weeks) that need a reference instant to
/// resolve.
pub(super) fn span_to_duration(span: jiff::Span) -> Result<Duration, WakeError> {
    // Try direct conversion. `Span::try_into` for Duration only
    // succeeds on spans without calendar units.
    Duration::try_from(span).map_err(|e| WakeError::NonWallClockSpan(e.to_string()))
}

/// Validate that an interval period meets the registry's minimum.
///
/// Used by [`super::registry::WakeRegistry::register`] before spawning
/// the evaluator task. Pulled out here so the same check applies to
/// future call sites (e.g. when a custom-wake registration falls
/// back to an interval pulse).
pub(super) fn validate_period(period: jiff::Span, min: jiff::Span) -> Result<(), WakeError> {
    let req = span_to_duration(period)?;
    let min_dur = span_to_duration(min)?;
    if req < min_dur {
        return Err(WakeError::PeriodTooShort {
            requested: period,
            minimum: min,
        });
    }
    Ok(())
}

/// Spawn the evaluator task for a one-shot
/// [`super::WakeCondition::TaskTimeout`].
///
/// The task sleeps the deadline and then sends one wake activation.
/// On send failure (mailbox closed) it exits silently.
pub(super) fn spawn_task_timeout(
    task: BlockRef,
    deadline: jiff::Span,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
) -> Result<JoinHandle<()>, WakeError> {
    let dur = span_to_duration(deadline)?;
    let span_compare = SpanCompare(deadline);
    Ok(tokio::spawn(async move {
        tokio::time::sleep(dur).await;
        let body = format!(
            "wake: task timeout — {} elapsed without completion",
            task.label
        );
        let input = wake_mailbox_input(
            SystemReason::TaskTimeout {
                task: task.clone(),
                elapsed: span_compare,
            },
            &body,
        );
        // Send failure means the mailbox has been dropped; exit
        // silently.
        let _ = mailbox_tx.send(input);
    }))
}

/// Spawn the evaluator task for a recurring
/// [`super::WakeCondition::Interval`].
///
/// The task uses `tokio::time::interval` and emits one wake
/// activation per tick. Exits when the mailbox channel closes.
pub(super) fn spawn_interval(
    period: jiff::Span,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
) -> Result<JoinHandle<()>, WakeError> {
    let dur = span_to_duration(period)?;
    let span_compare = SpanCompare(period);
    Ok(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(dur);
        // Skip the immediate first tick — interval semantics fire
        // "every period", not "once now then every period".
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // consume the immediate tick
        loop {
            ticker.tick().await;
            let body = format!("wake: interval tick");
            let input = wake_mailbox_input(
                SystemReason::Interval {
                    period: span_compare.clone(),
                },
                &body,
            );
            if mailbox_tx.send(input).is_err() {
                // Mailbox dropped — exit cleanly.
                return;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::{WakeCondition, WakeError, WakeRegistry};
    use pattern_core::types::origin::{Author, SystemReason};
    use std::time::Duration;

    /// Helper: build a registry with a tight min-period so tests
    /// can exercise sub-1s timers without bumping into the
    /// production safeguard.
    fn fast_registry() -> (WakeRegistry, mpsc::UnboundedReceiver<MailboxInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let reg = WakeRegistry::new(tx).with_min_period(jiff::Span::new().milliseconds(10));
        (reg, rx)
    }

    fn br(label: &str) -> BlockRef {
        BlockRef::new(label, "test-block-id")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_timeout_fires_after_deadline() {
        let (reg, mut rx) = fast_registry();
        let _ = reg
            .register(
                "tt-1".into(),
                WakeCondition::TaskTimeout {
                    task: br("planning"),
                    deadline: SpanCompare(jiff::Span::new().milliseconds(100)),
                },
            )
            .expect("register");

        // Wait up to 1s for the wake to fire.
        let input = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("wake should fire within 1s")
            .expect("mailbox channel open");

        match input.from.author {
            Author::System {
                reason: SystemReason::TaskTimeout { task, .. },
            } => {
                assert_eq!(task.label, "planning");
            }
            other => panic!("expected TaskTimeout, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interval_fires_repeatedly() {
        let (reg, mut rx) = fast_registry();
        let _ = reg
            .register(
                "iv-1".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(50)),
                },
            )
            .expect("register");

        // Collect three ticks within 1s.
        let mut ticks = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while ticks < 3 && std::time::Instant::now() < deadline {
            if let Ok(Some(input)) = tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                rx.recv(),
            )
            .await
            {
                match input.from.author {
                    Author::System {
                        reason: SystemReason::Interval { .. },
                    } => ticks += 1,
                    other => panic!("expected Interval, got {other:?}"),
                }
            }
        }
        assert!(
            ticks >= 3,
            "interval should have fired at least 3 times within 1s, got {ticks}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_conditions_fire_independently() {
        let (reg, mut rx) = fast_registry();
        let _ = reg
            .register(
                "tt".into(),
                WakeCondition::TaskTimeout {
                    task: br("planning"),
                    deadline: SpanCompare(jiff::Span::new().milliseconds(80)),
                },
            )
            .expect("register tt");
        let _ = reg
            .register(
                "iv".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(100)),
                },
            )
            .expect("register iv");

        // Within 500ms we should see at least one of each kind.
        let mut saw_timeout = false;
        let mut saw_interval = false;
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while !(saw_timeout && saw_interval) && std::time::Instant::now() < deadline {
            if let Ok(Some(input)) = tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                rx.recv(),
            )
            .await
            {
                match input.from.author {
                    Author::System {
                        reason: SystemReason::TaskTimeout { .. },
                    } => saw_timeout = true,
                    Author::System {
                        reason: SystemReason::Interval { .. },
                    } => saw_interval = true,
                    _ => {}
                }
            }
        }
        assert!(saw_timeout, "TaskTimeout did not fire within 500ms");
        assert!(saw_interval, "Interval did not fire within 500ms");
    }

    #[tokio::test]
    async fn subsecond_interval_rejected_at_register() {
        let (tx, _rx) = mpsc::unbounded_channel();
        // Production registry — default min_period 1s.
        let reg = WakeRegistry::new(tx);
        let err = reg
            .register(
                "iv".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(500)),
                },
            )
            .expect_err("subsecond interval must be rejected");
        assert!(
            matches!(err, WakeError::PeriodTooShort { .. }),
            "expected PeriodTooShort, got {err:?}"
        );
    }

    #[tokio::test]
    async fn unregister_aborts_evaluator() {
        let (reg, mut rx) = fast_registry();
        let _ = reg
            .register(
                "iv".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(20)),
                },
            )
            .expect("register");

        // Confirm at least one fire.
        let _ = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("at least one tick");

        // Unregister.
        assert!(reg.unregister(&"iv".into()), "id was registered");
        assert_eq!(reg.len(), 0);

        // Drain any in-flight events. Then assert no further fires
        // for 250ms (well past two periods).
        while rx.try_recv().is_ok() {}
        let result = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
        assert!(
            result.is_err(),
            "no further wakes should fire after unregister; got {result:?}"
        );
    }

    #[tokio::test]
    async fn duplicate_id_rejected() {
        let (reg, _rx) = fast_registry();
        let _ = reg
            .register(
                "id".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(500)),
                },
            )
            .expect("first register");
        let err = reg
            .register(
                "id".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(500)),
                },
            )
            .expect_err("duplicate must be rejected");
        assert!(matches!(err, WakeError::DuplicateId { .. }));
    }

    #[tokio::test]
    async fn registry_drop_aborts_all_tasks() {
        // Keep an extra sender clone outside the registry so the
        // channel doesn't close on registry drop. We want to
        // distinguish "tasks aborted, no further wake events" from
        // "channel closed, recv returns None".
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _keepalive = tx.clone();
        let reg = WakeRegistry::new(tx).with_min_period(jiff::Span::new().milliseconds(10));
        let _ = reg
            .register(
                "iv".into(),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(20)),
                },
            )
            .expect("register");

        // Confirm a tick.
        let _ = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("at least one tick");

        // Drop the registry.
        drop(reg);

        // Drain in-flight, then assert silence (timeout, not channel
        // close) for the next 250ms — well past two periods.
        while rx.try_recv().is_ok() {}
        let result = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
        assert!(
            result.is_err(),
            "no further wakes should fire after registry drop; got {result:?}"
        );
    }
}
