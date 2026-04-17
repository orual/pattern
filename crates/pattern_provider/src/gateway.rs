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
//! # Task 18 behavioural requirements (to implement)
//!
//! - **429 with exponential backoff.** When the server returns 429,
//!   retry with exponential backoff (1s, 2s, 4s, 8s...) capped at some
//!   ceiling, with jitter. Retries MUST respect any `Retry-After` header
//!   the server sends (prefer server value over computed backoff).
//! - **Subscription-tier 5-hour cap.** Anthropic's subscription tier
//!   sends a cap-reset header when the 5-hour window has been hit (e.g.
//!   `anthropic-ratelimit-unified-5h-reset` with a UNIX epoch seconds
//!   value). The gateway must parse this header and either:
//!     - surface the reset time to the user and fail with a clear error,
//!       OR
//!     - wait until the reset time before retrying (policy configurable
//!       via `ShaperConfig` or a separate `RetryPolicy` knob).
//!   Distinct from transient 429s (which backoff handles). The reset
//!   timestamp surfaces via `ProviderError::RateLimited { retry_after }`
//!   where `retry_after` is computed from `reset_at - now()`.
//!
//! Phase 4 Task 18 populates this type. See phase_04.md for the
//! `ProviderClient` trait shape (from `pattern_core` Phase 2) and the
//! full method signatures.
