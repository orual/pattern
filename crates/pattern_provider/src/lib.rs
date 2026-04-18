//! Pattern v3 LLM provider: multi-provider gateway over a rebased `rust-genai`
//! fork (v0.6.0-beta.17 base with pattern-v3-foundation patches).
//!
//! One [`gateway::PatternGatewayClient`] holds a single `genai::Client` plus
//! per-[`genai::adapter::AdapterKind`] pattern-side state — credential tiers,
//! request shaper, rate limiter, session UUID — and dispatches on the per-call
//! model string. A single gateway instance can hit Anthropic + Gemini + OpenAI
//! (+ any other genai-supported provider) based solely on which model is
//! requested.
//!
//! Absorbs the Anthropic-facing responsibilities of the retired `pattern_auth`
//! crate. See `docs/plans/rewrite-v3-portlist.md` for the retirement timeline
//! and `docs/implementation-plans/2026-04-16-v3-foundation/phase_04.md` for
//! the full task list.
//!
//! Populated incrementally across v3 foundation phase 4. Phase 5 wires the
//! gateway into `pattern_runtime` via the request composer that emits the
//! three-segment cache layout defined in the v3 foundation design.

pub mod auth;
pub mod compose;
#[cfg(feature = "subscription-oauth")]
pub mod creds_store;
pub mod gateway;
pub mod ratelimit;
pub mod session_uuid;
pub mod shaper;
pub mod token_count;

pub use gateway::{PatternGatewayClient, PatternGatewayClientBuilder, RetryPolicy};

// Note: the `auth` module is always compiled, but its internal submodules
// (session_pickup, pkce) are feature-gated. `api_key` and the top-level
// CredentialTier machinery are always available. See auth.rs for details.
