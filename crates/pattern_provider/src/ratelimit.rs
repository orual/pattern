//! Per-provider token-bucket rate limiter.
//!
//! One bucket per [`genai::adapter::AdapterKind`] (e.g. all Anthropic models
//! share one bucket, all Gemini models share another). Built on `governor`'s
//! GCRA rate limiter. Separate buckets for chat completions vs `count_tokens`
//! per AC5b.5 — Anthropic meters these independently.
//!
//! On bucket exhaustion: queue the request with jitter, retry when refill
//! allows. Exhaustion surfaces as a visible delay to the caller but never
//! as a hard failure (unless the wait exceeds the caller's cancellation
//! window, which it doesn't implement here — cancellation is the caller's
//! concern via `tokio::select!` or similar).
//!
//! Phase 4 Task 14 populates this module.
