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
//!   cache policy.
//!
//! Future tasks (Phase 5 Tasks 3, 8–10) will add `pub mod pipeline`,
//! `pub mod passes`, and related plumbing here.

pub mod profile;

pub use profile::{CacheProfile, CacheStrategy};
