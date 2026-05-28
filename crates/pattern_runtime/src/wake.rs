// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Wake-condition machinery: registered conditions fire activations
//! into a session's mailbox.
//!
//! v3-multi-agent Phase 4 introduces five wake primitives, all of
//! which now have live evaluators:
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
//!   Phase 7 Task 6 closed the Phase 4 deferral: custom conditions
//!   are evaluated by [`custom::CustomEvaluator`] on a read-only
//!   restricted bundle (Observe-class effects only).
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
//! - `custom` — [`CustomEvaluator`] for user-supplied Haskell
//!   conditions (Phase 7 Task 6).

pub mod block_changed;
pub mod custom;
pub mod registry;
pub mod rust_primitives;
pub mod task_dep;

pub use custom::CustomEvaluator;
pub use registry::{PeriodTooShortDetails, WakeCondition, WakeError, WakeRegistry};
