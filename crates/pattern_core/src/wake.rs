//! Wake conditions and reason codes for agent activations.
//!
//! v3-multi-agent Phase 4 introduces *wake* — a way for the runtime to
//! activate an idle agent on causes other than a direct inbound
//! message. The four built-in Rust primitives are timer-based
//! ([`WakeReason::TaskTimeout`], [`WakeReason::Interval`]) or memory-
//! subscriber-based ([`WakeReason::BlockChanged`],
//! [`WakeReason::TaskDependencyResolved`]). [`WakeReason::Custom`]
//! reserves a slot for Haskell-registered conditions whose evaluator
//! is deferred — Phase 4 ships the registration path; the evaluator
//! that runs the user's Haskell condition on a timer is future work.
//!
//! [`WakeReason`] is carried on a `TurnInput` (see `types::turn`) so
//! the agent can branch on its own activation cause. Defined here in
//! `pattern_core` (rather than `pattern_runtime`) because it travels
//! across the runtime/Haskell boundary as turn-input metadata.

use jiff::Span;
use serde::{Deserialize, Serialize};

use crate::types::block_ref::BlockRef;

/// Why an agent's mailbox triggered a turn.
///
/// Set on [`crate::types::TurnInput::wake`] when the activation came
/// from a wake condition rather than a direct message. Message-driven
/// turns leave it `None`.
///
/// `#[non_exhaustive]` — future phases may add transport-specific
/// wake variants without breaking match arms.
/// Note: no `PartialEq`/`Eq` derives — [`jiff::Span`] does not implement
/// `PartialEq<Span>` because span equality is calendar-dependent (e.g.
/// "1 month" cannot be compared with "30 days" without a reference
/// instant). Compare individual fields explicitly when needed; tests
/// use `format!("{x:?}") == format!("{y:?}")` for shape equality.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WakeReason {
    /// A task's deadline elapsed without the agent completing it.
    TaskTimeout {
        /// The task whose timer fired.
        task: BlockRef,
        /// How long the timer was set for. Echoed back so the agent
        /// can branch on duration without re-reading the task.
        elapsed: Span,
    },
    /// A dependency task transitioned to `Completed`. The agent's
    /// blocked task can now proceed.
    TaskDependencyResolved {
        /// The dependency that just resolved.
        task: BlockRef,
    },
    /// A specific block's content changed (any author). Used when the
    /// agent registered explicit interest in a memory location.
    BlockChanged {
        /// The block whose content changed.
        block: BlockRef,
    },
    /// A periodic timer fired. The agent registered an interval and
    /// requested a wake on every tick.
    Interval {
        /// The interval period (echoed back for symmetry with
        /// [`Self::TaskTimeout`]).
        period: Span,
    },
    /// A Haskell-registered condition fired.
    ///
    /// Phase 4 only ships the *registration* path for custom
    /// conditions; the evaluator that runs the user's Haskell
    /// condition on a timer is deferred. This variant is present so
    /// the type is forward-complete — it will not be emitted by Phase 4
    /// runtime code.
    Custom {
        /// User-supplied identifier from `ctx.wake.register`.
        id: String,
    },
}

impl WakeReason {
    /// Short label for log lines and observability events.
    ///
    /// Stable identifier; kept in sync with the variant names so
    /// downstream consumers (TUI rendering, metrics tags) don't have
    /// to format Debug output.
    pub fn label(&self) -> &'static str {
        match self {
            Self::TaskTimeout { .. } => "task-timeout",
            Self::TaskDependencyResolved { .. } => "task-dependency-resolved",
            Self::BlockChanged { .. } => "block-changed",
            Self::Interval { .. } => "interval",
            Self::Custom { .. } => "custom",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn br(label: &str) -> BlockRef {
        BlockRef::new(label, "test-block-id")
    }

    #[test]
    fn round_trip_each_variant_via_serde_json() {
        let cases = vec![
            WakeReason::TaskTimeout {
                task: br("planning"),
                elapsed: Span::new().minutes(30),
            },
            WakeReason::TaskDependencyResolved {
                task: br("ship-it"),
            },
            WakeReason::BlockChanged {
                block: br("notes"),
            },
            WakeReason::Interval {
                period: Span::new().minutes(5),
            },
            WakeReason::Custom {
                id: "user-cond-1".into(),
            },
        ];

        for case in &cases {
            let json = serde_json::to_string(case).expect("serialize");
            let back: WakeReason = serde_json::from_str(&json).expect("deserialize");
            // `jiff::Span` is calendar-dependent and does not implement
            // `PartialEq<Span>` — compare via Debug for shape equality.
            assert_eq!(
                format!("{back:?}"),
                format!("{case:?}"),
                "round trip mismatch for {case:?}"
            );
        }
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(
            WakeReason::TaskTimeout {
                task: br("x"),
                elapsed: Span::new()
            }
            .label(),
            "task-timeout"
        );
        assert_eq!(
            WakeReason::Interval {
                period: Span::new()
            }
            .label(),
            "interval"
        );
        assert_eq!(
            WakeReason::Custom { id: "x".into() }.label(),
            "custom"
        );
    }
}
