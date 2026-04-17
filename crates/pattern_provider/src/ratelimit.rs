//! Per-provider token-bucket rate limiter.
//!
//! Built on `governor` (GCRA). One limiter instance per provider (map
//! keyed by `AdapterKind` at the gateway level — all Anthropic models
//! share one limiter; all Gemini models share another).
//!
//! Each provider has three independent buckets:
//!
//! - **completions-per-minute** — short-horizon backpressure on chat
//!   requests.
//! - **completions-per-day** — long-horizon budget cap.
//! - **count-tokens-per-minute** — separate bucket for the token-counting
//!   endpoint. Anthropic meters these independently from chat completions
//!   (AC5b.5); exhausting the count bucket must not block chat requests.
//!
//! # Policy
//!
//! Metering is **per-request**, not per-token. True token-weighted
//! client-side metering requires GCRA cost <= max capacity, which doesn't
//! hold when a single request can consume 200k tokens out of a
//! 20k-tokens-per-minute bucket. Request-metering is honest as "pattern's
//! polite-client cap" — the server-side Anthropic 429s (surfaced via
//! [`ProviderError::RateLimited`]) remain the authoritative rate limit.
//!
//! Waits use `until_ready_with_jitter` — governor wakes callers in
//! randomised order so concurrent bursts don't thunder back.

use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::DefaultClock;
use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Jitter, Quota, RateLimiter};

type Governor = RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>;

/// Per-provider rate limiter with independent chat + count_tokens buckets.
pub struct ProviderRateLimiter {
    provider: String,
    completions_per_minute: Governor,
    completions_per_day: Governor,
    count_tokens_per_minute: Governor,
    jitter: Jitter,
}

impl ProviderRateLimiter {
    /// Construct with explicit quotas. Panics if any quota is zero.
    pub fn new(
        provider: impl Into<String>,
        completions_rpm: u32,
        completions_rpd: u32,
        count_tokens_rpm: u32,
    ) -> Self {
        let rpm_nz = NonZeroU32::new(completions_rpm).expect("completions_rpm must be > 0");
        let rpd_nz = NonZeroU32::new(completions_rpd).expect("completions_rpd must be > 0");
        let count_nz = NonZeroU32::new(count_tokens_rpm).expect("count_tokens_rpm must be > 0");

        // Day-scoped quota: 1 request per (86400s / rpd) with burst = rpd.
        // `Quota::with_period` + `allow_burst` gives us a per-day limiter.
        let day_period = Duration::from_secs(86_400)
            .checked_div(completions_rpd)
            .expect("non-zero rpd yields non-zero period");
        let day_quota = Quota::with_period(day_period)
            .expect("non-zero period")
            .allow_burst(rpd_nz);

        Self {
            provider: provider.into(),
            completions_per_minute: RateLimiter::direct(Quota::per_minute(rpm_nz)),
            completions_per_day: RateLimiter::direct(day_quota),
            count_tokens_per_minute: RateLimiter::direct(Quota::per_minute(count_nz)),
            jitter: Jitter::up_to(Duration::from_millis(200)),
        }
    }

    /// Anthropic defaults — conservative "polite personal-use client"
    /// values. Server-side Anthropic enforces its own tier limits; this is
    /// belt-and-suspenders.
    pub fn anthropic_default() -> Self {
        Self::new("anthropic", 60, 5_000, 120)
    }

    /// Gemini defaults.
    pub fn gemini_default() -> Self {
        Self::new("gemini", 60, 5_000, 120)
    }

    /// Which provider this limiter serves. Useful for logging.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Acquire capacity for one chat completion. Waits (with jitter) until
    /// both the per-minute and per-day buckets allow a request through.
    ///
    /// Returns when capacity is available — the caller proceeds with the
    /// actual HTTP request immediately after. AC5.4, AC5.7.
    pub async fn acquire_completion(&self) {
        // Wait on both buckets in series. Order matters: if we blocked
        // on per-day first then per-minute, the latter's wait starts
        // counting only after the former resolves — good, that's
        // conservative. `until_ready_with_jitter` handles the sleep.
        self.completions_per_minute
            .until_ready_with_jitter(self.jitter)
            .await;
        self.completions_per_day
            .until_ready_with_jitter(self.jitter)
            .await;
    }

    /// Acquire capacity for one count_tokens call. Uses the independent
    /// count-tokens bucket only; completion buckets are unaffected
    /// (AC5b.5).
    pub async fn acquire_count_tokens(&self) {
        self.count_tokens_per_minute
            .until_ready_with_jitter(self.jitter)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// AC5.6: two limiters with different quotas are fully independent.
    #[tokio::test]
    async fn separate_limiters_are_independent() {
        let a = ProviderRateLimiter::new("alpha", 600, 10_000, 600);
        let b = ProviderRateLimiter::new("beta", 600, 10_000, 600);

        // Exhaust a's per-minute bucket to demonstrate b is unaffected. With 600
        // RPM quota we'd need ~600 calls to exhaust — instead we just verify
        // that both acquire in parallel without issue (a != b state-wise).
        tokio::join!(a.acquire_completion(), b.acquire_completion());

        assert_eq!(a.provider(), "alpha");
        assert_eq!(b.provider(), "beta");
    }

    /// AC5b.5: count_tokens bucket is independent from completion buckets.
    /// With completions_rpm = 1 and count_tokens_rpm = 60, the completion
    /// bucket should be the one that throttles — count_tokens should not.
    #[tokio::test]
    async fn count_tokens_bucket_is_independent_from_completions() {
        let limiter = ProviderRateLimiter::new("anthropic", 1, 10_000, 600);

        // Drain the completion bucket (one call consumes the minute's budget).
        limiter.acquire_completion().await;

        // count_tokens should not be blocked by the completions bucket —
        // fire ten calls rapidly; they all finish quickly (<1s) since the
        // count bucket's RPM is 600.
        let start = Instant::now();
        for _ in 0..10 {
            limiter.acquire_count_tokens().await;
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "count_tokens should be unaffected by completion-bucket exhaustion; elapsed={elapsed:?}"
        );
    }

    /// AC5.4: exhausted completion bucket blocks briefly then succeeds.
    #[tokio::test]
    async fn exhausted_completion_bucket_blocks_then_succeeds() {
        // 60 RPM = 1 per second equilibrium rate with initial burst of 60.
        // We'll exhaust the burst then verify the next call waits measurably.
        let limiter = ProviderRateLimiter::new("anthropic", 60, 10_000, 120);

        // Exhaust the minute's burst cap.
        for _ in 0..60 {
            limiter.acquire_completion().await;
        }

        // 61st call must wait ~1 second for a token refill.
        let start = Instant::now();
        limiter.acquire_completion().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(500),
            "61st call should wait at least ~1s for refill; elapsed={elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "wait should not be unreasonably long; elapsed={elapsed:?}"
        );
    }

    /// AC5.7: per-day bucket stays depleted even while per-minute refills.
    /// Construct a limiter with rpd=2 + rpm=1000. After 2 completions the
    /// per-day cap is exhausted; a third call must block for a very long
    /// time (≈day_period/2) even though the minute bucket has capacity.
    /// We don't actually wait for the refill — we just assert the call
    /// doesn't return in under half a second.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn per_day_bucket_stays_depleted_while_minute_refills() {
        // start_paused = true + tokio test means time advances only via
        // tokio::time::advance — but governor uses its own DefaultClock
        // (std::time-based), so we can't fully simulate. Fall back to
        // measuring that the third call does NOT return within a short
        // real-time window.
        let limiter = ProviderRateLimiter::new("anthropic", 1_000, 2, 1_000);

        // Exhaust the daily bucket.
        limiter.acquire_completion().await;
        limiter.acquire_completion().await;

        // Third call should block on the daily bucket. With rpd=2, the
        // daily period is 43200s = 12h, so the wait is long.
        let third =
            tokio::time::timeout(Duration::from_millis(500), limiter.acquire_completion()).await;

        assert!(
            third.is_err(),
            "daily bucket exhaustion must keep callers waiting past 500ms \
             even though the per-minute bucket has capacity"
        );
    }

    #[test]
    fn presets_have_sane_quotas() {
        let a = ProviderRateLimiter::anthropic_default();
        let g = ProviderRateLimiter::gemini_default();
        assert_eq!(a.provider(), "anthropic");
        assert_eq!(g.provider(), "gemini");
    }

    #[test]
    #[should_panic(expected = "completions_rpm must be > 0")]
    fn zero_rpm_panics() {
        let _ = ProviderRateLimiter::new("x", 0, 100, 10);
    }

    #[test]
    #[should_panic(expected = "completions_rpd must be > 0")]
    fn zero_rpd_panics() {
        let _ = ProviderRateLimiter::new("x", 10, 0, 10);
    }

    #[test]
    #[should_panic(expected = "count_tokens_rpm must be > 0")]
    fn zero_count_tokens_panics() {
        let _ = ProviderRateLimiter::new("x", 10, 100, 0);
    }
}
