//! Wake-condition machinery: registered conditions fire activations
//! into a session's mailbox.
//!
//! v3-multi-agent Phase 4 introduces five wake primitives, four of
//! which ship as Rust evaluators in this module:
//!
//! - [`WakeCondition::TaskTimeout`] — one-shot timer.
//! - [`WakeCondition::Interval`] — recurring periodic timer.
//! - [`WakeCondition::BlockChanged`] — fires when a block's content
//!   changes (any author). Wires through
//!   [`pattern_memory::subscriber`]'s Loro fan-out (T8).
//! - [`WakeCondition::TaskDependencyResolved`] — fires when a
//!   dependency task transitions to `Completed`. Piggybacks on the
//!   `BlockChanged` subscriber and re-reads task status on parent-
//!   block change (T9).
//! - [`WakeCondition::Custom`] — user-supplied Haskell condition.
//!   Phase 4 ships only the registration path; the evaluator that
//!   runs the user's program on its trigger is **scheduled in
//!   Phase 7 Task 6** (see
//!   `docs/implementation-plans/2026-04-19-v3-multi-agent/phase_07.md`).
//!
//! All evaluator tasks deliver wake activations as
//! [`crate::mailbox::MailboxInput`] with an `Author::System { reason:
//! ... }` origin carrying the structured payload (block ref, span)
//! directly on the variant. There is no separate "wake reason" axis
//! on the turn input — `SystemReason` answers the "why is this turn
//! happening" question on its own.
//!
//! # Module layout
//!
//! - `registry` — [`WakeRegistry`], [`WakeCondition`], [`WakeError`],
//!   plus the internal `wake_mailbox_input` synthesiser.
//! - `rust_primitives` — `tokio::time`-based evaluators for
//!   `TaskTimeout` and `Interval`.

pub mod block_changed;
pub mod registry;
pub mod rust_primitives;

pub use registry::{WakeCondition, WakeError, WakeRegistry};
