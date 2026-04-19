//! Session-stable cache policy. Latched at session open; never mutated
//! mid-session.
//!
//! # Why session-latched
//!
//! Empirically, mid-session TTL or scope flips bust the server-side
//! prompt cache wholesale (observed ~20K tokens per flip on Anthropic's
//! subscription tier). Cache-friendly design is to lock the policy at
//! the point of session open and make it immutable for the session's
//! duration.
//!
//! # genai types
//!
//! Uses `genai::chat::CacheControl` directly per the Phase 5 Task 1
//! decision — no pattern-side mirror, no `From`-conversion layer.
//! `CacheControl` is already re-exported from
//! `pattern_core::types::provider` for callers that want it without
//! pulling genai directly.

use genai::chat::CacheControl;

/// Session-latched cache policy. See module docs for rationale.
#[derive(Debug, Clone)]
pub struct CacheProfile {
    /// TTL for segment 1 (system + instructions + tools). Default
    /// `Ephemeral1h` for the long-lived-stable content — identity,
    /// base instructions, tool schemas don't churn within a session.
    /// Downgrades to `Ephemeral5m` when `allow_extended_ttl` is false.
    pub segment_1_ttl: CacheControl,

    /// TTL for segment 2 (message-history boundary). Default
    /// `Ephemeral5m`. Segment 2 carries prior-turn messages + any
    /// memory-change pseudo-messages emitted this turn.
    pub segment_2_ttl: CacheControl,

    /// TTL for segment 3 (memory pseudo-turn). Default `Ephemeral5m`.
    /// Segment 3 is the `[memory:current_state]` pseudo-turn carrying
    /// current block state; naturally shorter TTL since block edits
    /// invalidate it.
    pub segment_3_ttl: CacheControl,

    /// Whether extended-TTL (`Ephemeral1h` / `Ephemeral24h`) is
    /// permitted for this session. Latched from subscription-tier
    /// status at session open:
    ///
    /// - OAuth subscription tier with `extended-cache-ttl-2025-04-11`
    ///   beta available → `true`
    /// - API-key tier with the beta available → `true`
    /// - Subscription-in-overage (future billing-aware plan) → `false`
    ///
    /// When `false`, [`segment_1_control`](Self::segment_1_control)
    /// downgrades `Ephemeral1h` / `Ephemeral24h` → `Ephemeral5m` with a
    /// `tracing::warn` so cache-break-detection can attribute the
    /// downgrade if it surfaces as a cache bust.
    pub allow_extended_ttl: bool,

    /// Cache-placement strategy. Phase 5 only supports
    /// [`CacheStrategy::Default`]; other variants are reserved for future
    /// phases and stored as metadata but not yet interpreted by any
    /// composer pass (the default three-segment layout applies regardless).
    pub strategy: CacheStrategy,
}

/// Cache-placement strategy enum. `#[non_exhaustive]` because future
/// strategies (MCP-aware, Bedrock extra-body, etc.) will be added in
/// subsequent phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheStrategy {
    /// Three-segment layout: system+tools → history+pseudo-msgs →
    /// memory-current-state. Phase 5 default.
    Default,

    /// Reserved for future MCP-aware cache reference integration (see
    /// post-foundation plugin-system plan). Currently stored but not
    /// interpreted by any composer pass — the default three-segment
    /// layout applies regardless of this variant.
    McpAware,

    /// Reserved for future Bedrock provider integration (see
    /// post-foundation cloud-provider plan). Currently stored but not
    /// interpreted by any composer pass — the default three-segment
    /// layout applies regardless of this variant.
    BedrockExtraBody,
}

impl CacheProfile {
    /// Default profile for an OAuth subscription-tier session with
    /// extended-cache-ttl beta available.
    ///
    /// All three segments default to `Ephemeral1h`. Rationale + evidence:
    /// see `docs/notes/2026-04-18-cache-ttl-research.md`. Short version:
    ///
    /// - **Segment 1** — identity + tools + instructions. Changes rarely
    ///   (persona edits, tool-registry tweaks). Long TTL is the point.
    /// - **Segment 2** — message history + recent-edit pseudo-messages.
    ///   Messages are append-only within a range; a given prefix is
    ///   effectively immutable once emitted. 1h TTL lets segment 2
    ///   survive the real-world idle periods (tool latency, user
    ///   think-time, scheduled wakeups, sleeptime consolidations) that
    ///   routinely exceed 5m.
    /// - **Segment 3** — `[memory:current_state]` pseudo-turn rendering
    ///   current blocks. Changes only on block edits, not every turn;
    ///   long TTL lets it cache across multi-hour activations.
    ///
    /// All-1h side-steps Anthropic's TTL-ordering constraint (1h entries
    /// must precede 5m in the wire format) — with all markers at the
    /// same TTL, any placement order is valid, giving the composer
    /// maximum flexibility.
    ///
    /// A 5m variant is deliberately NOT offered as a default. Claude Code's
    /// silent downgrade from 1h to 5m on 2026-03-06 caused ~17–32% cost
    /// inflation before being reverted — the research note captures the
    /// evidence trail. A mode-aware override for sustained chat-burst
    /// agents is plausible future work but not part of the foundation.
    pub fn default_anthropic_subscriber() -> Self {
        Self {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral1h,
            segment_3_ttl: CacheControl::Ephemeral1h,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        }
    }

    /// Default profile for an API-key-tier session. Same defaults as
    /// the subscription-tier path — Pattern doesn't model scope (single
    /// user, single org) so API-key and subscription-OAuth shapes are
    /// identical at the profile level.
    pub fn default_api_key() -> Self {
        Self {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral1h,
            segment_3_ttl: CacheControl::Ephemeral1h,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        }
    }

    /// Shared downgrade helper. When `allow_extended_ttl` is false and
    /// the requested control is an extended-TTL variant, emit a
    /// `tracing::warn` and downgrade to `Ephemeral5m`. Otherwise return
    /// the control unchanged.
    fn downgrade_if_needed(&self, segment: &'static str, requested: &CacheControl) -> CacheControl {
        match (self.allow_extended_ttl, requested) {
            (false, CacheControl::Ephemeral1h | CacheControl::Ephemeral24h) => {
                tracing::warn!(
                    segment,
                    requested = ?requested,
                    applied = "Ephemeral5m",
                    "extended TTL not permitted; downgrading",
                );
                CacheControl::Ephemeral5m
            }
            _ => requested.clone(),
        }
    }

    /// Resolve the effective segment-1 `CacheControl`, respecting
    /// `allow_extended_ttl`.
    pub fn segment_1_control(&self) -> CacheControl {
        self.downgrade_if_needed("segment_1", &self.segment_1_ttl)
    }

    /// Resolve the effective segment-2 `CacheControl`, respecting
    /// `allow_extended_ttl`.
    pub fn segment_2_control(&self) -> CacheControl {
        self.downgrade_if_needed("segment_2", &self.segment_2_ttl)
    }

    /// Resolve the effective segment-3 `CacheControl`, respecting
    /// `allow_extended_ttl`.
    pub fn segment_3_control(&self) -> CacheControl {
        self.downgrade_if_needed("segment_3", &self.segment_3_ttl)
    }

    /// True if any effective segment control requires the
    /// `extended-cache-ttl-2025-04-11` beta header (i.e., uses
    /// `Ephemeral1h` or `Ephemeral24h`). The shaper / gateway is
    /// responsible for ensuring the header is present; the composer's
    /// finalize pass validates it.
    pub fn requires_extended_ttl_beta(&self) -> bool {
        [
            self.segment_1_control(),
            self.segment_2_control(),
            self.segment_3_control(),
        ]
        .iter()
        .any(|cc| matches!(cc, CacheControl::Ephemeral1h | CacheControl::Ephemeral24h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    // --- Test 1: default_anthropic_subscriber returns expected defaults ---

    #[test]
    fn default_anthropic_subscriber_returns_expected_defaults() {
        let profile = CacheProfile::default_anthropic_subscriber();
        // All-1h: long-lived cache for long-running agent activations;
        // side-steps the 1h-before-5m wire-format ordering constraint.
        assert_eq!(profile.segment_1_ttl, CacheControl::Ephemeral1h);
        assert_eq!(profile.segment_2_ttl, CacheControl::Ephemeral1h);
        assert_eq!(profile.segment_3_ttl, CacheControl::Ephemeral1h);
        assert!(profile.allow_extended_ttl);
        assert_eq!(profile.strategy, CacheStrategy::Default);
    }

    // --- Test 2: default_api_key returns identical defaults ---

    #[test]
    fn default_api_key_returns_same_defaults_as_subscriber() {
        let subscriber = CacheProfile::default_anthropic_subscriber();
        let api_key = CacheProfile::default_api_key();
        assert_eq!(subscriber.segment_1_ttl, api_key.segment_1_ttl);
        assert_eq!(subscriber.segment_2_ttl, api_key.segment_2_ttl);
        assert_eq!(subscriber.segment_3_ttl, api_key.segment_3_ttl);
        assert_eq!(subscriber.allow_extended_ttl, api_key.allow_extended_ttl);
        assert_eq!(subscriber.strategy, api_key.strategy);
    }

    // --- Test 3: allow_extended_ttl=false downgrades 1h to 5m with warn ---

    #[traced_test]
    #[test]
    fn allow_extended_false_downgrades_1h_to_5m_with_warn() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: false,
            strategy: CacheStrategy::Default,
        };

        let effective = profile.segment_1_control();
        assert_eq!(effective, CacheControl::Ephemeral5m);
        assert!(logs_contain("downgrading"));
    }

    // --- Test 4: allow_extended_ttl=false with 5m stored does NOT warn ---

    #[traced_test]
    #[test]
    fn allow_extended_false_with_5m_stored_does_not_warn() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral5m,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: false,
            strategy: CacheStrategy::Default,
        };

        let effective = profile.segment_1_control();
        assert_eq!(effective, CacheControl::Ephemeral5m);
        assert!(!logs_contain("downgrading"));
    }

    // --- Test 5: allow_extended_ttl=true respects stored segment_1_ttl ---

    #[test]
    fn allow_extended_true_preserves_stored_segment_1_ttl() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        };
        assert_eq!(profile.segment_1_control(), CacheControl::Ephemeral1h);
    }

    // --- Test 6a: requires_extended_ttl_beta true when seg1 is 1h and allow=true ---

    #[test]
    fn requires_extended_ttl_beta_true_when_seg1_is_1h_and_allowed() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        };
        assert!(profile.requires_extended_ttl_beta());
    }

    // --- Test 6b: requires_extended_ttl_beta false when all effective are 5m ---

    #[test]
    fn requires_extended_ttl_beta_false_when_all_effective_5m() {
        // Includes the downgrade case: stored 1h but allow=false → effective 5m.
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: false,
            strategy: CacheStrategy::Default,
        };
        assert!(!profile.requires_extended_ttl_beta());
    }

    // --- Test 6c: requires_extended_ttl_beta true when seg2 or seg3 is 1h ---

    #[test]
    fn requires_extended_ttl_beta_true_when_seg2_is_1h() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral5m,
            segment_2_ttl: CacheControl::Ephemeral1h,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        };
        assert!(profile.requires_extended_ttl_beta());
    }

    #[test]
    fn requires_extended_ttl_beta_true_when_seg3_is_24h() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral5m,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral24h,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        };
        assert!(profile.requires_extended_ttl_beta());
    }

    // --- Test 7: CacheStrategy variants are constructible ---

    #[test]
    fn cache_strategy_variants_are_constructible() {
        let _default = CacheStrategy::Default;
        let _mcp = CacheStrategy::McpAware;
        let _bedrock = CacheStrategy::BedrockExtraBody;
    }

    // --- Additional: 24h also downgrades when allow_extended=false ---

    #[traced_test]
    #[test]
    fn allow_extended_false_downgrades_24h_to_5m_with_warn() {
        let profile = CacheProfile {
            segment_1_ttl: CacheControl::Ephemeral24h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: false,
            strategy: CacheStrategy::Default,
        };
        let effective = profile.segment_1_control();
        assert_eq!(effective, CacheControl::Ephemeral5m);
        assert!(logs_contain("downgrading"));
    }
}
