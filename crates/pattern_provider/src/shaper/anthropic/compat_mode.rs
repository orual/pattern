// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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

/// Provider-aware default shaper mode. Used by the runtime composer to
/// pick the right [`ShaperCompatMode`] without baking the Anthropic-
/// specific `SubscriptionRoutingShape` into non-Anthropic providers'
/// composed requests.
///
/// - `Anthropic` → [`ShaperCompatMode::default`] (feature-gated:
///   `SubscriptionRoutingShape` under `subscription-oauth`,
///   `HonestPattern` otherwise).
/// - All others (OpenAI, OpenAIResp, Gemini, Cohere, …) →
///   `HonestPattern`. These providers don't have Anthropic's
///   subscription-routing requirements, so the cleanest posture is to
///   produce content blocks without any routing wrappers and let the
///   per-provider shaper at the gateway adapt the wire shape (e.g.,
///   NoOpShaper flattens system_blocks → chat.system for genai's
///   OpenAI adapter).
pub fn default_shaper_mode_for(adapter: genai::adapter::AdapterKind) -> ShaperCompatMode {
    use genai::adapter::AdapterKind;
    match adapter {
        AdapterKind::Anthropic => ShaperCompatMode::default(),
        _ => ShaperCompatMode::HonestPattern,
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
