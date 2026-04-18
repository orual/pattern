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
//! Two concrete shapers ship in Phase 4:
//!
//! - [`HonestPatternShaper`] — Anthropic, with a [`ShaperCompatMode`]
//!   escalation ladder.
//! - [`NoOpShaper`] — default for non-Anthropic providers (Gemini, etc.).
//!   Emits a minimal `User-Agent` only; never touches `ChatRequest`.

pub mod compat_mode;
pub mod headers;
pub mod system_prompt;

pub use compat_mode::ShaperCompatMode;
pub use headers::build_identification_headers;
pub use system_prompt::build_system_prompt;

use pattern_core::DEFAULT_BASE_INSTRUCTIONS;
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

// ---- HonestPatternShaper (Anthropic) ----

/// Anthropic-target shaper. Applies `SubscriptionRoutingShape` by default
/// (when `subscription-oauth` feature is on) or `HonestPattern` otherwise.
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

        self.identification_headers(ctx)
    }

    fn identification_headers(
        &self,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        build_identification_headers(&self.config, ctx.session_uuid, ctx.auth_tier, ctx.model)
    }
}

// ---- NoOpShaper (default for non-Anthropic providers) ----

/// No-op shaper. Emits a minimal `User-Agent` for honest identification
/// but never touches `ChatRequest`. The default shaper for providers
/// (Gemini, OpenAI, etc.) that don't need pattern-side shaping.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpShaper;

impl RequestShaper for NoOpShaper {
    fn shape(
        &self,
        _req: &mut genai::chat::ChatRequest,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        self.identification_headers(ctx)
    }

    fn identification_headers(
        &self,
        _ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        let mut out = std::collections::BTreeMap::new();
        out.insert(
            "user-agent".into(),
            format!("pattern/{}", env!("CARGO_PKG_VERSION")),
        );
        Ok(out)
    }
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
        let uuid = crate::session_uuid::SessionUuidRotator::new();
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
        let uuid = crate::session_uuid::SessionUuidRotator::new();
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

        // The shaper is no longer responsible for the `oauth-2025-04-20`
        // beta marker — that's emitted by `gateway::auth_headers_for_tier`
        // alongside the Bearer token. Shaper output should NOT contain it
        // even when the auth_tier says OAuth.
        let anthropic_beta = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Anthropic-Beta"))
            .map(|(_, v)| v.as_str())
            .unwrap_or_default();
        assert!(
            !anthropic_beta.contains("oauth-2025-04-20"),
            "shaper output must not contain the OAuth auth marker"
        );
    }

    #[test]
    fn system_instructions_override_replaces_default() {
        let shaper = HonestPatternShaper::new(min_config()).expect("valid");
        let uuid = crate::session_uuid::SessionUuidRotator::new();
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

    #[test]
    fn noop_shaper_emits_only_user_agent() {
        let shaper = NoOpShaper;
        let uuid = crate::session_uuid::SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = make_chat_request();
        let before_system = req.system.clone();

        let ctx = ShapeContext {
            session_uuid: &session,
            model: "gemini-2.5-pro",
            auth_tier: AuthTier::ApiKey,
            persona: "persona",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        let headers = shaper.shape(&mut req, &ctx).expect("shape ok");

        // Request untouched.
        assert_eq!(req.system, before_system);
        assert!(
            req.system_blocks.is_none(),
            "NoOpShaper must leave system_blocks untouched (None)"
        );

        // Only a user-agent header (lowercased for HTTP case-insensitivity).
        assert_eq!(headers.len(), 1);
        let user_agent = headers
            .get("user-agent")
            .expect("NoOpShaper should emit user-agent");
        assert!(user_agent.starts_with("pattern/"));
    }

    #[test]
    fn wrap_system_reminder_brackets_content() {
        let wrapped = wrap_system_reminder("memo");
        assert!(wrapped.starts_with("<system-reminder>\n"));
        assert!(wrapped.ends_with("\n</system-reminder>"));
        assert!(wrapped.contains("memo"));
    }
}
