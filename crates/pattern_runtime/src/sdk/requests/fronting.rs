// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Fronting` (`haskell/Pattern/Fronting.hs`).
//!
//! The `FrontingReq` GADT variants cross the Haskell/Rust boundary as typed
//! Core values — each wire type derives [`tidepool_bridge_derive::FromCore`].
//!
//! # Constructor naming
//!
//! The wire-side `WireRoutingRule` and `WireMessagePattern` carry a `Wire`
//! prefix on the Rust side. On the Haskell side, `MessagePattern` constructors
//! are prefixed with `Pattern` (e.g. `PatternPrefix`, `PatternContains`) and
//! `RoutingRule` is a plain positional record — following the `Pattern.Spawn`
//! convention of prefixing potentially-colliding constructor names.
//!
//! # `Current` response encoding
//!
//! `Current` returns the fronting state serialized as a JSON string (same
//! pattern as `Pattern.Diagnostics.GetDiagnostics`). This avoids requiring a
//! `ToCore` derive on the snapshot type.

use tidepool_bridge_derive::FromCore;

// ── WireMessagePattern ────────────────────────────────────────────────────────

/// Wire mirror of [`pattern_core::fronting::MessagePattern`].
///
/// Constructor names are `Pattern`-prefixed on the Haskell side to avoid
/// collisions with any other `Prefix` / `Contains` constructors that may be
/// in scope.
#[derive(Debug, FromCore)]
pub enum WireMessagePattern {
    /// `PatternPrefix text`. Matches when the message body starts with the
    /// given string.
    #[core(module = "Pattern.Fronting", name = "PatternPrefix")]
    Prefix(String),
    /// `PatternContains text`. Matches when the message body contains the
    /// given string.
    #[core(module = "Pattern.Fronting", name = "PatternContains")]
    Contains(String),
    /// `PatternTopicTag text`. Matches when the body contains `#<tag>` at a
    /// word boundary.
    #[core(module = "Pattern.Fronting", name = "PatternTopicTag")]
    TopicTag(String),
    /// `PatternRegex text`. Matches when the compiled regex is found in the
    /// message body.
    #[core(module = "Pattern.Fronting", name = "PatternRegex")]
    Regex(String),
}

impl From<WireMessagePattern> for pattern_core::fronting::MessagePattern {
    fn from(w: WireMessagePattern) -> Self {
        match w {
            WireMessagePattern::Prefix(s) => Self::Prefix(s),
            WireMessagePattern::Contains(s) => Self::Contains(s),
            WireMessagePattern::TopicTag(s) => Self::TopicTag(s),
            WireMessagePattern::Regex(s) => Self::Regex(s),
        }
    }
}

// ── WireRoutingRule ───────────────────────────────────────────────────────────

/// Wire mirror of [`pattern_core::fronting::RoutingRule`].
///
/// Positional record: `id pattern target priority`. Tuple form avoids the
/// named-field-variant restriction imposed by the `FromCore` derive when this
/// type appears inside an enum variant.
#[derive(Debug, FromCore)]
#[core(module = "Pattern.Fronting", name = "RoutingRule")]
pub struct WireRoutingRule {
    pub id: String,
    pub pattern: WireMessagePattern,
    pub target: String,
    /// Priority. Wire as `i64` because `tidepool_bridge::FromCore` does
    /// not impl on `u32`. Coerced to `u32` (saturating, non-negative) at
    /// the conversion boundary; negative values clamp to 0.
    pub priority: i64,
}

impl From<WireRoutingRule> for pattern_core::fronting::RoutingRule {
    fn from(w: WireRoutingRule) -> Self {
        let priority = w.priority.max(0).min(u32::MAX as i64) as u32;
        pattern_core::fronting::RoutingRule::new(
            w.id,
            pattern_core::fronting::MessagePattern::from(w.pattern),
            w.target.as_str(),
            priority,
        )
    }
}

// ── FrontingReq ───────────────────────────────────────────────────────────────

/// Rust mirror of the Haskell `Fronting` GADT.
#[derive(Debug, FromCore)]
pub enum FrontingReq {
    /// Read the current fronting state. Returns a JSON-encoded snapshot string
    /// containing active personas, fallback, and routing rules.
    #[core(module = "Pattern.Fronting", name = "Current")]
    Current,
    /// Set the active fronting personas and optional fallback. Capability-gated
    /// on [`pattern_core::CapabilityFlag::FrontingControl`].
    #[core(module = "Pattern.Fronting", name = "Set")]
    Set(Vec<String>, Option<String>),
    /// Replace the routing rules. Capability-gated on `FrontingControl`.
    ///
    /// Rules are compiled at dispatch time; an invalid regex pattern returns
    /// an error and the existing rules are unchanged.
    #[core(module = "Pattern.Fronting", name = "Route")]
    Route(Vec<WireRoutingRule>),
    /// Clear the fronting set entirely (active personas, fallback, rules).
    /// Capability-gated on `FrontingControl`.
    #[core(module = "Pattern.Fronting", name = "Clear")]
    Clear,
}
