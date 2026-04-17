//! Request shaper — per-provider (for now; model-group / cost-tier dispatch
//! can slot in later without reworking the trait).
//!
//! Each supported provider gets a [`RequestShaper`] trait-object instance
//! registered with [`crate::gateway::PatternGatewayClient`]. The gateway
//! invokes the shaper after credential resolution and before rate-limit
//! acquisition. Shapers can:
//!
//! - inject identification headers (pattern identifies honestly by default)
//! - rewrite or restructure the system prompt (Anthropic's
//!   `SubscriptionRoutingShape` uses `ChatRequest::system_blocks` from the
//!   fork patch to emit the three-block structural shape)
//! - attach beta/extra headers per `ShaperConfig`
//!
//! Two shapers ship in Phase 4:
//!
//! - [`HonestPatternShaper`] — Anthropic, with a `ShaperCompatMode` escalation
//!   ladder: `HonestPattern` (cleanest), `SubscriptionRoutingShape`
//!   (provisional default pending Task 20 verification), `FullSurfaceImpersonation`
//!   (future-gated, requires explicit sign-off).
//! - `NoOpShaper` — default for Gemini and any future provider that doesn't
//!   need shaping.
//!
//! Phase 4 Task 12 populates this module. See phase_04.md for the detailed
//! shaper contract and the `ShaperConfig` fields.
