//! Request shaper — per-provider (for now; model-group / cost-tier
//! dispatch can slot in later without reworking the trait).
//!
//! The gateway invokes a [`RequestShaper`] after credential resolution and
//! before rate-limit acquisition. Shapers:
//!
//! - Attach identification + beta headers appropriate for the outbound
//!   auth tier (subscription OAuth vs API key) and target model.
//! - Rewrite or restructure the system prompt if the provider requires it
//!   (Anthropic's `SubscriptionRoutingShape` injects the structural
//!   claude-code literal in slot \[0\]).
//!
//! # Module layout
//!
//! - [`anthropic`] — Anthropic-specific shaping. Holds
//!   [`anthropic::HonestPatternShaper`], the [`anthropic::ShaperCompatMode`]
//!   escalation ladder, and Anthropic's identification / beta-header
//!   construction.
//! - [`noop`] — [`noop::NoOpShaper`], the default for providers that don't
//!   need pattern-side request rewriting (Gemini, future OpenAI, etc.).
//!
//! Convenience re-exports at this module level ([`HonestPatternShaper`],
//! [`NoOpShaper`], [`ShaperCompatMode`], [`build_identification_headers`],
//! [`build_system_prompt`]) keep call sites short — `pattern_provider::shaper::HonestPatternShaper`
//! is equivalent to `pattern_provider::shaper::anthropic::HonestPatternShaper`.
//!
//! # Provider-agnostic vs provider-specific
//!
//! [`RequestShaper`], [`ShapeContext`], [`ShaperConfig`], and
//! [`wrap_system_reminder`] live at this top level because they're
//! cross-provider abstractions. Everything Anthropic-specific
//! (compat modes, beta-header allow/deny list, slot \[0\]/\[1\]/\[2\]
//! layout) lives under [`anthropic`]. When another provider grows
//! non-trivial shaping needs, add a sibling module (`gemini`,
//! `openai`, …) rather than piling provider-specific code at this level.
//!
//! Note: [`ShaperConfig`] is currently Anthropic-biased — `compat_mode`,
//! `target_is_first_party`, and the capability toggles only matter for
//! the Anthropic shaper. Pulling it into `anthropic::ShaperConfig` is a
//! future cleanup; leaving it cross-provider for now preserves the
//! existing public API shape.

pub mod anthropic;
pub mod noop;

// Convenience re-exports so `shaper::HonestPatternShaper` keeps working.
pub use anthropic::{
    HonestPatternShaper, ShaperCompatMode, build_content_blocks, build_identification_headers,
    build_system_prompt, prepend_routing_token,
};
pub use noop::NoOpShaper;

use pattern_core::error::ProviderError;

use crate::auth::AuthTier;
use crate::session_uuid::PatternSessionUuid;

// ---- Config ----

/// Static shaper configuration — validated at construction, read per
/// request. Instance-level.
#[derive(Debug, Clone)]
pub struct ShaperConfig {
    /// `X-App` header value. Default `"pattern"`. Task 20's live verification
    /// may determine this must be `"cli"` for subscription-routing compat;
    /// if so, the default flips in a follow-up patch.
    pub x_app: String,

    /// How closely to structurally match Anthropic's reference
    /// subscription client.
    pub compat_mode: ShaperCompatMode,

    /// Whether the target provider is Anthropic 1P (i.e. `claude.ai` /
    /// `api.anthropic.com`) vs a first-party-adjacent proxy. Only
    /// Anthropic's 1P endpoints expect the
    /// `prompt-caching-scope-2026-01-05` beta marker.
    pub target_is_first_party: bool,

    /// Emit `interleaved-thinking-2025-05-14` when the model is capable.
    pub enable_interleaved_thinking: bool,

    /// Emit `dev-full-thinking-2025-05-14` when the model is capable.
    pub enable_dev_full_thinking: bool,

    /// Emit `context-management-2025-06-27` when targeting claude-4+.
    pub enable_context_management: bool,

    /// Emit `extended-cache-ttl-2025-04-11` unconditionally.
    pub enable_extended_cache_ttl: bool,

    /// Emit `context-1m-2025-08-07` when the model is capable.
    pub enable_1m_context: bool,
}

impl Default for ShaperConfig {
    fn default() -> Self {
        Self {
            x_app: "pattern".into(),
            compat_mode: ShaperCompatMode::default(),
            target_is_first_party: true,
            enable_interleaved_thinking: false,
            enable_dev_full_thinking: false,
            enable_context_management: false,
            enable_extended_cache_ttl: false,
            enable_1m_context: false,
        }
    }
}

impl ShaperConfig {
    /// Validate the config. Run eagerly at shaper construction so misshaped
    /// configs fail at boot rather than at request time (AC5.5).
    ///
    /// Checks:
    /// - `x_app` must be non-empty.
    /// - No banned reference-client marker may smuggle into future config
    ///   fields (defence-in-depth; currently the banned list is internal
    ///   to the shaper, but if user-provided beta markers are added
    ///   later this check fires automatically).
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.x_app.trim().is_empty() {
            return Err(ProviderError::ShaperMisconfigured {
                reason: "x_app cannot be empty".into(),
            });
        }
        Ok(())
    }
}

// ---- Per-request context ----

/// Per-request inputs to a shaper. Borrowed; the shaper does not retain
/// these values.
pub struct ShapeContext<'a> {
    pub session_uuid: &'a PatternSessionUuid,
    pub model: &'a str,
    pub auth_tier: AuthTier,

    /// Persona identity / behaviour block. Rendered into slot \[2\] of
    /// `SubscriptionRoutingShape` or concatenated into the single block of
    /// `HonestPattern`.
    pub persona: &'a str,

    /// If `Some`, replaces `DEFAULT_BASE_INSTRUCTIONS` wholesale. Used when
    /// a persona wants to override the pattern-wide default; normal usage
    /// leaves this `None`.
    pub system_instructions_override: Option<&'a str>,

    /// Additional long-lived content (frequently-read memory blocks,
    /// etc.). Appended to slot \[2\] / the trailing single block.
    pub extra_long_lived_blocks: &'a [String],
}

// ---- Trait ----

/// Per-call shape transformation. Produces headers to inject and mutates
/// `ChatRequest` in place when system-prompt rewriting is needed.
///
/// Headers return as a [`std::collections::BTreeMap<String, String>`] —
/// names must be lowercased so that case-insensitive HTTP semantics work
/// correctly when the gateway merges shaper headers with per-tier auth
/// headers (which it does with `.extend()`, relying on BTreeMap's
/// last-insert-wins per key). BTreeMap over HashMap gives us
/// deterministic iteration order — useful for logging, tests, and any
/// future wire-formats that care about header ordering.
pub trait RequestShaper: Send + Sync {
    /// Apply shaping. Returns the identification + beta headers to inject.
    fn shape(
        &self,
        req: &mut genai::chat::ChatRequest,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError>;

    /// Headers-only path. Used by `count_tokens` and similar calls that
    /// don't carry a `ChatRequest` to shape. Must return the same set of
    /// identification headers `shape()` would emit for the same context.
    fn identification_headers(
        &self,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError>;
}

// ---- `<system-reminder>` helper ----

/// Wrap content in the `<system-reminder>...</system-reminder>` tag
/// convention Anthropic models are trained to recognise. Used for
/// memory-block metadata, mid-turn interrupts, and other system-surfaced
/// content injected into user-role messages.
pub fn wrap_system_reminder(content: &str) -> String {
    format!("<system-reminder>\n{content}\n</system-reminder>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_system_reminder_brackets_content() {
        let wrapped = wrap_system_reminder("memo");
        assert!(wrapped.starts_with("<system-reminder"));
        assert!(wrapped.ends_with("\n</system-reminder>"));
        assert!(wrapped.contains("memo"));
    }
}
