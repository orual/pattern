//! Concrete composer-pass implementations for the three-segment cache layout.
//!
//! Each pass is a [`super::ComposerPass`] that appends content to a
//! [`super::PartialRequest`] and places one cache-breakpoint marker. The
//! canonical execution order is:
//!
//! 1. [`segment_1::Segment1Pass`] — system prompt + tool schemas.
//! 2. [`segment_2::Segment2Pass`] — prior-turn history + summary-head +
//!    memory-change pseudo-messages.
//! 3. [`segment_3::Segment3Pass`] — `[memory:current_state]` pseudo-turn.
//!
//! After all three passes, the caller appends fresh user input to
//! `partial.messages` (uncached), then calls [`super::finalize`] to apply
//! breakpoint markers and assemble the final
//! [`pattern_core::types::provider::CompletionRequest`].
//!
//! # Ordering matters
//!
//! The cache-breakpoint indices are positional — a pass that records
//! `BreakpointLocation::MessageBlock(5)` expects index 5 to remain stable.
//! Running passes out of order will misplace markers. The canonical order
//! above is enforced by convention (and documented here) rather than by
//! type-level sequencing; tests verify the combined pipeline produces the
//! correct marker count and placement.

pub mod segment_1;
pub mod segment_2;
pub mod segment_3;

pub use segment_1::Segment1Pass;
