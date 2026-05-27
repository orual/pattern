//! Anthropic-specific request shaping.
//!
//! Houses [`HonestPatternShaper`] plus the submodules it composes:
//!
//! - [`compat_mode`] — the [`ShaperCompatMode`] escalation ladder
//!   (`HonestPattern` / `SubscriptionRoutingShape` /
//!   `FullSurfaceImpersonation`).
//! - [`headers`] — identification + `anthropic-beta` header construction,
//!   including the permanent `BANNED_BETA_MARKERS` deny list.
//! - [`system_prompt`] — system-prompt array layout per compat mode,
//!   including slot \[0\]/\[1\]/\[2\] dispatch for `SubscriptionRoutingShape`.
//!
//! Everything here is Anthropic-specific. Other providers (Gemini,
//! OpenAI, etc.) either use [`super::noop::NoOpShaper`] or grow their
//! own sibling module (`shaper/gemini.rs` etc.) when they need
//! pattern-side shaping.

pub mod compat_mode;
pub mod headers;
pub mod system_prompt;

pub use compat_mode::{ShaperCompatMode, default_shaper_mode_for};
pub use headers::build_identification_headers;
pub use system_prompt::{build_content_blocks, build_system_prompt, prepend_routing_token};

use pattern_core::DEFAULT_BASE_INSTRUCTIONS;
use pattern_core::error::ProviderError;

use super::{RequestShaper, ShapeContext, ShaperConfig};

/// Anthropic-target shaper. Applies `SubscriptionRoutingShape` by default
/// (when `subscription-oauth` feature is on) or `HonestPattern` otherwise.
///
/// The shaper is the single source of truth for the outbound
/// `anthropic-beta` header — auth-tier markers (e.g. `oauth-2025-04-20`)
/// and capability markers (e.g. `prompt-caching-scope-2026-01-05`) are
/// comma-joined into one header value here. See [`headers`] for the
/// allow/deny list.
#[derive(Debug, Clone)]
pub struct HonestPatternShaper {
    config: ShaperConfig,
}

impl HonestPatternShaper {
    /// Construct, validating the config. Returns
    /// `ProviderError::ShaperMisconfigured` on any validation failure
    /// (AC5.5 — config errors surface at construction, not at request time).
    pub fn new(config: ShaperConfig) -> Result<Self, ProviderError> {
        config.validate()?;
        Ok(Self { config })
    }
}

impl RequestShaper for HonestPatternShaper {
    fn shape(
        &self,
        req: &mut genai::chat::ChatRequest,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        // Two paths:
        //
        // 1. Caller pre-populated `system_blocks` (production runtime via
        //    the compose pipeline). Preserve their content — including
        //    cache-control markers placed by Segment1Pass — and only
        //    prepend the mode-specific routing token (idempotent).
        //
        // 2. Caller did not pre-populate (tests, ad-hoc callers). Build
        //    the full sequence from `ctx.persona` +
        //    `ctx.system_instructions_override` so the request has a
        //    sensible system prompt without requiring callers to know
        //    the layout.
        //
        // The previous behaviour rebuilt unconditionally, clobbering
        // any pre-populated blocks (and cache markers, and persona
        // content threaded via `request.persona`). That's the bug this
        // is fixing.
        match req.system_blocks.as_mut() {
            Some(blocks) if !blocks.is_empty() => {
                prepend_routing_token(blocks, self.config.compat_mode);
            }
            _ => {
                let instructions = ctx
                    .system_instructions_override
                    .unwrap_or(DEFAULT_BASE_INSTRUCTIONS);
                let blocks = build_system_prompt(
                    self.config.compat_mode,
                    instructions,
                    ctx.persona,
                    ctx.extra_long_lived_blocks,
                );
                req.system_blocks = Some(blocks);
            }
        }

        self.identification_headers(ctx)
    }

    fn identification_headers(
        &self,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        build_identification_headers(&self.config, ctx.session_uuid, ctx.auth_tier, ctx.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthTier;
    use crate::session_uuid::SessionUuidRotator;

    fn min_config() -> ShaperConfig {
        ShaperConfig {
            x_app: "pattern".into(),
            compat_mode: ShaperCompatMode::HonestPattern,
            target_is_first_party: false,
            enable_interleaved_thinking: false,
            enable_dev_full_thinking: false,
            enable_context_management: false,
            enable_extended_cache_ttl: false,
            enable_1m_context: false,
        }
    }

    fn make_chat_request() -> genai::chat::ChatRequest {
        genai::chat::ChatRequest::from_user("hi")
    }

    #[test]
    fn validate_rejects_empty_x_app() {
        let mut c = min_config();
        c.x_app = "".into();
        let err = HonestPatternShaper::new(c).expect_err("empty x_app must fail");
        assert!(matches!(err, ProviderError::ShaperMisconfigured { .. }));
    }

    #[test]
    fn validate_rejects_whitespace_x_app() {
        let mut c = min_config();
        c.x_app = "   ".into();
        let err = HonestPatternShaper::new(c).expect_err("whitespace x_app must fail");
        assert!(matches!(err, ProviderError::ShaperMisconfigured { .. }));
    }

    #[test]
    fn honest_pattern_injects_single_system_block() {
        let shaper = HonestPatternShaper::new(min_config()).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = make_chat_request();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::ApiKey,
            persona: "I am Pattern.",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        let _headers = shaper.shape(&mut req, &ctx).expect("shape ok");

        let blocks = req.system_blocks.as_ref().expect("blocks injected");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("I am Pattern."));
        assert!(
            !blocks[0].text.contains("Claude Code"),
            "HonestPattern must not contain the claude-code literal"
        );
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn subscription_routing_shape_injects_three_system_blocks() {
        let mut config = min_config();
        config.compat_mode = ShaperCompatMode::SubscriptionRoutingShape;
        let shaper = HonestPatternShaper::new(config).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = make_chat_request();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::SessionPickup,
            persona: "I am Pattern.",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        let headers = shaper.shape(&mut req, &ctx).expect("shape ok");

        let blocks = req.system_blocks.as_ref().expect("blocks injected");
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].text.contains("Claude Code"), "slot[0] literal");
        assert!(
            blocks[1].text.contains("NOT Claude Code"),
            "slot[1] negation"
        );
        assert!(blocks[2].text.contains("I am Pattern."), "slot[2] persona");

        // The shaper IS responsible for the `oauth-2025-04-20` beta marker —
        // it must appear in the same `Anthropic-Beta` header value as any
        // capability markers (e.g. `prompt-caching-scope`). Emitting it from
        // `gateway::auth_headers_for_tier` instead would silently overwrite
        // the shaper's value via BTreeMap::extend (last-insert-wins).
        // See Phase 4 code-review fix: the shaper is the single source of truth.
        let anthropic_beta = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, v)| v.as_str())
            .unwrap_or_default();
        assert!(
            anthropic_beta.contains("oauth-2025-04-20"),
            "shaper must include oauth-2025-04-20 in anthropic-beta for OAuth tiers; \
             got: {anthropic_beta:?}"
        );
    }

    #[test]
    fn system_instructions_override_replaces_default() {
        let shaper = HonestPatternShaper::new(min_config()).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = make_chat_request();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::ApiKey,
            persona: "persona",
            system_instructions_override: Some("CUSTOM BASE INSTRUCTIONS MARKER"),
            extra_long_lived_blocks: &[],
        };
        let _ = shaper.shape(&mut req, &ctx).expect("shape ok");

        let blocks = req.system_blocks.as_ref().expect("blocks");
        assert!(
            blocks
                .iter()
                .any(|b| b.text.contains("CUSTOM BASE INSTRUCTIONS MARKER")),
            "override must appear in rendered blocks"
        );
    }

    // -- Non-clobbering shape() (the fix) -------------------------------------

    /// When the caller has pre-populated `system_blocks` (production path:
    /// runtime's compose pipeline emits content blocks via Segment1Pass),
    /// `shape()` must NOT rebuild them. It only prepends the
    /// mode-specific routing token. Pre-fix the shaper rebuilt
    /// unconditionally, clobbering both content and any cache markers.
    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn shape_preserves_pre_populated_blocks_and_only_prepends_routing_token() {
        use genai::chat::{CacheControl, SystemBlock};

        let mut config = min_config();
        config.compat_mode = ShaperCompatMode::SubscriptionRoutingShape;
        let shaper = HonestPatternShaper::new(config).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        // Caller-supplied content blocks: [base, persona]. The persona
        // block carries a cache_control marker — must survive shape().
        let pre_populated = vec![
            SystemBlock::new("You are NOT Claude Code.\n\nbase content"),
            {
                let mut b = SystemBlock::new("agent's persona content");
                b.cache_control = Some(CacheControl::Ephemeral1h);
                b
            },
        ];

        let mut req = make_chat_request();
        req.system_blocks = Some(pre_populated.clone());

        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::SessionPickup,
            persona: "this should be ignored — runtime owns persona via pre_populated",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        shaper.shape(&mut req, &ctx).expect("shape ok");

        let blocks = req.system_blocks.as_ref().expect("blocks remain");
        assert_eq!(
            blocks.len(),
            3,
            "routing token + 2 pre-populated content blocks"
        );
        assert!(
            blocks[0].text.contains("Claude Code"),
            "slot[0] routing literal prepended"
        );
        assert_eq!(
            blocks[1].text, pre_populated[0].text,
            "base content preserved verbatim"
        );
        assert_eq!(
            blocks[2].text, pre_populated[1].text,
            "persona content preserved verbatim"
        );
        assert_eq!(
            blocks[2].cache_control,
            Some(CacheControl::Ephemeral1h),
            "cache_control on the persona block must survive shape()"
        );
    }

    /// Idempotency: shape() must not stack routing tokens across calls.
    /// Pre-fix this couldn't happen because the shaper rebuilt every
    /// time. Post-fix the prepend is gated on the existing first block
    /// not already being the routing literal.
    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn shape_is_idempotent_on_repeated_calls() {
        let mut config = min_config();
        config.compat_mode = ShaperCompatMode::SubscriptionRoutingShape;
        let shaper = HonestPatternShaper::new(config).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = make_chat_request();
        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::SessionPickup,
            persona: "I am Pattern.",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        shaper.shape(&mut req, &ctx).expect("shape ok #1");
        let after_first: Vec<String> = req
            .system_blocks
            .as_ref()
            .expect("blocks")
            .iter()
            .map(|b| b.text.clone())
            .collect();
        shaper.shape(&mut req, &ctx).expect("shape ok #2");
        let after_second: Vec<String> = req
            .system_blocks
            .as_ref()
            .expect("blocks")
            .iter()
            .map(|b| b.text.clone())
            .collect();
        assert_eq!(
            after_first, after_second,
            "second shape() call must be a no-op; routing token must not stack"
        );
    }

    /// HonestPattern must NOT prepend a routing token even when called
    /// against pre-populated blocks. The mode is for non-Anthropic-route
    /// providers and audit configurations where the literal is wrong.
    #[test]
    fn honest_pattern_shape_leaves_pre_populated_blocks_untouched() {
        use genai::chat::SystemBlock;

        let shaper = HonestPatternShaper::new(min_config()).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let pre_populated = vec![SystemBlock::new("base + persona content combined")];
        let mut req = make_chat_request();
        req.system_blocks = Some(pre_populated.clone());

        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::ApiKey,
            persona: "ignored",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        shaper.shape(&mut req, &ctx).expect("shape ok");

        let blocks = req.system_blocks.as_ref().expect("blocks remain");
        assert_eq!(blocks.len(), 1, "HonestPattern adds nothing");
        assert_eq!(blocks[0].text, pre_populated[0].text);
    }

    /// Tests still call shape() with no pre-populated blocks. The
    /// fallback build-from-scratch path must continue to work so test
    /// fixtures that exercise the shaper alone get a sensible system
    /// prompt. This is the same behaviour as before the fix.
    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn shape_falls_back_to_full_build_when_blocks_empty() {
        let mut config = min_config();
        config.compat_mode = ShaperCompatMode::SubscriptionRoutingShape;
        let shaper = HonestPatternShaper::new(config).expect("valid");
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = make_chat_request();
        // Explicitly empty (not None) — the empty-blocks path also falls back.
        req.system_blocks = Some(Vec::new());

        let ctx = ShapeContext {
            session_uuid: &session,
            model: "claude-opus-4-7",
            auth_tier: AuthTier::SessionPickup,
            persona: "I am Pattern.",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        shaper.shape(&mut req, &ctx).expect("shape ok");

        let blocks = req.system_blocks.as_ref().expect("blocks");
        assert_eq!(
            blocks.len(),
            3,
            "fallback build_system_prompt produces all 3 slots"
        );
        assert!(blocks[0].text.contains("Claude Code"));
        assert!(blocks[1].text.contains("NOT Claude Code"));
        assert!(blocks[2].text.contains("I am Pattern."));
    }
}
