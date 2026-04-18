//! Composer pipeline — transforms a partial request into a final
//! `genai::chat::ChatRequest` using a sequence of `ComposerPass`
//! implementations (defined in Phase 5 Task 3).
//!
//! # Three-segment cache layout
//!
//! Anthropic's prompt-cache implementation is segment-aware. The
//! composer emits exactly three cache-breakpoint segments per turn:
//!
//! 1. **Segment 1** — system prompt + base instructions + tool schemas.
//!    Long-lived stable content; `Ephemeral1h` by default.
//! 2. **Segment 2** — message-history boundary. Prior-turn messages +
//!    any memory-change pseudo-messages emitted this turn;
//!    `Ephemeral5m` by default.
//! 3. **Segment 3** — `[memory:current_state]` pseudo-turn carrying
//!    current block state. Shorter TTL because block edits invalidate
//!    it; `Ephemeral5m` by default.
//!
//! The [`CacheProfile`] captures the per-segment TTL policy and is
//! latched at session open to prevent mid-session TTL-flip cache busts
//! (empirically observed at ~20K tokens per flip on Anthropic's
//! subscription tier).
//!
//! # Module layout
//!
//! - [`profile`] — [`CacheProfile`] + [`CacheStrategy`]. Session-latched
//!   cache policy (Phase 5 Task 2).
//! - [`pipeline`] — [`pipeline::ComposerPass`] trait, [`pipeline::compose`]
//!   orchestrator, and [`pipeline::finalize`] request assembly
//!   (Phase 5 Task 3).
//! - [`partial_request`] — [`partial_request::PartialRequest`], the
//!   mutable request being assembled by composer passes.
//! - [`breakpoints`] — [`breakpoints::BreakpointLocation`],
//!   [`breakpoints::BreakpointPlacement`], and
//!   [`breakpoints::BreakpointTracker`] — `cache_control` placement
//!   + Anthropic's 4-marker-per-request budget enforcement.
//!
//! Future tasks (Phase 5 Tasks 8–10) will add `pub mod passes` with
//! the concrete three-segment pass implementations.

pub mod breakpoints;
pub mod current_state;
pub mod partial_request;
pub mod pipeline;
pub mod profile;
pub mod pseudo_messages;

// Convenience re-exports so call sites can type `compose::ComposerPass`
// instead of `compose::pipeline::ComposerPass`.
pub use breakpoints::{BreakpointLocation, BreakpointPlacement, BreakpointTracker};
pub use partial_request::PartialRequest;
pub use pipeline::{ComposerPass, compose, finalize};
pub use profile::{CacheProfile, CacheStrategy};
pub use current_state::render_current_state;
pub use pseudo_messages::{render_change_event, render_change_events};
