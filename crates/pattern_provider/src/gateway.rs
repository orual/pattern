//! [`PatternGatewayClient`] — the `pattern_core::traits::ProviderClient` impl.
//!
//! Wraps one `genai::Client` and dispatches on `AdapterKind` for every
//! pattern-side concern:
//!
//! - **credentials**: per-provider tier chain resolved through the
//!   [`crate::auth`] module.
//! - **request shaping**: per-provider shaper from [`crate::shaper`]
//!   (`HonestPatternShaper` for Anthropic, `NoOpShaper` default).
//! - **rate limiting**: per-provider bucket from [`crate::ratelimit`].
//! - **session UUID**: one per-persona UUID, rotates on compaction boundary
//!   per [`crate::session_uuid`].
//! - **token counting**: async `count_tokens` wrapper from
//!   [`crate::token_count`]; per-provider endpoint shape.
//!
//! The per-call model string drives `AdapterKind` inference inside genai;
//! the gateway looks up the same `AdapterKind` in its per-provider maps to
//! pick which credential chain / shaper / bucket applies.
//!
//! Phase 4 Task 18 populates this type. See phase_04.md for the
//! `ProviderClient` trait shape (from `pattern_core` Phase 2) and the
//! full method signatures.
