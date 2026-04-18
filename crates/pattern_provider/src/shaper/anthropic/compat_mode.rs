//! [`ShaperCompatMode`] — controls request-shape similarity to the
//! subscription-routing reference client.
//!
//! Pattern never ships content-level impersonation (request-body signing,
//! TLS fingerprinting, etc.) without explicit future sign-off. The
//! `FullSurfaceImpersonation` variant is declared here for API stability
//! and panics if invoked.

/// Escalation ladder for shape-level similarity to Anthropic's reference
/// subscription client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShaperCompatMode {
    /// `system[0]` = honest pattern identification; no reference-client literal.
    ///
    /// Aspirational cleanest posture. Verified by Phase 4 Task 20 against
    /// a real subscription tier; if that verification succeeds, the default
    /// flips to this in a follow-up patch. Only mode available when
    /// `subscription-oauth` feature is off.
    HonestPattern,

    /// `system[0]` = the verbatim identifier string Anthropic's subscription
    /// routing expects (structural API requirement, NOT an identity claim).
    /// `system[1]` = identity-override prefix + `DEFAULT_BASE_INSTRUCTIONS`.
    /// `system[2]` = persona + long-lived blocks.
    ///
    /// Phase 4 default when `subscription-oauth` feature is on. Empirically
    /// known-working against subscription tier as of 2026-04-16.
    ///
    /// Gated behind `subscription-oauth` because the shape only serves
    /// subscription-tier routing; API-key-only builds don't need it.
    #[cfg(feature = "subscription-oauth")]
    SubscriptionRoutingShape,

    /// Full-surface impersonation (request-body signing, stainless-style
    /// headers, TLS fingerprinting, tool-name remapping).
    ///
    /// **Not implemented.** Declared for API stability; invoking `shape()`
    /// on a shaper configured for this mode panics with an explicit
    /// not-implemented message pointing at the sign-off policy.
    #[cfg(feature = "subscription-oauth")]
    FullSurfaceImpersonation,
}

impl Default for ShaperCompatMode {
    #[cfg(feature = "subscription-oauth")]
    fn default() -> Self {
        Self::SubscriptionRoutingShape
    }

    #[cfg(not(feature = "subscription-oauth"))]
    fn default() -> Self {
        Self::HonestPattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_feature_gate() {
        let d = ShaperCompatMode::default();
        #[cfg(feature = "subscription-oauth")]
        assert_eq!(d, ShaperCompatMode::SubscriptionRoutingShape);
        #[cfg(not(feature = "subscription-oauth"))]
        assert_eq!(d, ShaperCompatMode::HonestPattern);
    }
}
