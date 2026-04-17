//! Identification + beta-header construction for outbound requests.
//!
//! Pattern identifies honestly. The `User-Agent` carries the pattern
//! version. `X-App` defaults to `"pattern"`. A per-request UUID and the
//! per-persona session UUID both appear in their own headers for
//! observability. Beta headers are curated from the config — reference-
//! client-specific markers are on a ban list that panics at construction
//! if somehow slipped past the banlist validator.

use pattern_core::error::ProviderError;

use crate::auth::AuthTier;
use crate::session_uuid::PatternSessionUuid;

use super::ShaperConfig;

/// Beta markers that identify Anthropic's internal CLI tooling — pattern
/// is a distinct client and NEVER sends these regardless of config.
///
/// If a config somehow smuggles one of these into its beta list, shaper
/// construction fails hard at `ShaperConfig::validate()` time.
pub(super) const BANNED_BETA_MARKERS: &[&str] = &[
    "claude-code-20250219",
    "cli-internal-2026-02-09",
    "summarize-connector-text-2026-03-13",
    "token-efficient-tools-2026-03-28",
];

/// Build the identification + beta headers for a single outbound request.
///
/// - `User-Agent`: `pattern/<cargo-version>`.
/// - `X-App`: `config.x_app` (defaults to `"pattern"`; Task 20
///   verification may force a change to `"cli"` for subscription-routing
///   compat).
/// - `X-Pattern-Session-Id`: the persona's rotating session UUID.
/// - `X-Client-Request-Id`: a fresh UUID-v4 per request.
/// - `Anthropic-Beta`: comma-joined beta markers per the auth tier +
///   model-capability flags. Empty if no markers apply.
pub fn build_identification_headers(
    config: &ShaperConfig,
    session_uuid: &PatternSessionUuid,
    auth_tier: AuthTier,
    model: &str,
) -> Result<Vec<(String, String)>, ProviderError> {
    let mut out = vec![
        (
            "User-Agent".into(),
            format!("pattern/{}", env!("CARGO_PKG_VERSION")),
        ),
        ("X-App".into(), config.x_app.clone()),
        ("X-Pattern-Session-Id".into(), session_uuid.to_string()),
        (
            "X-Client-Request-Id".into(),
            uuid::Uuid::new_v4().to_string(),
        ),
    ];

    let betas = build_beta_header_value(config, auth_tier, model);
    if !betas.is_empty() {
        out.push(("Anthropic-Beta".into(), betas));
    }

    Ok(out)
}

/// Build the comma-joined `Anthropic-Beta` value per the config and
/// request context. Never emits a banned marker.
pub(super) fn build_beta_header_value(
    config: &ShaperConfig,
    auth_tier: AuthTier,
    model: &str,
) -> String {
    let mut betas: Vec<&str> = Vec::new();

    // OAuth routing marker — required when the outbound auth tier is
    // subscription OAuth.
    #[cfg(feature = "subscription-oauth")]
    if matches!(auth_tier, AuthTier::SessionPickup | AuthTier::Pkce) {
        betas.push("oauth-2025-04-20");
    }
    // Suppress "auth_tier unused when no oauth feature" by consuming it.
    let _ = auth_tier;

    // Prompt-caching scope marker — claude-code always sends this on 1P
    // traffic; the server tolerates the absence but it's expected.
    if config.target_is_first_party {
        betas.push("prompt-caching-scope-2026-01-05");
    }

    // Capability-gated markers.
    if config.enable_interleaved_thinking && model_supports_thinking(model) {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if config.enable_dev_full_thinking && model_supports_thinking(model) {
        betas.push("dev-full-thinking-2025-05-14");
    }
    if config.enable_context_management && model_is_claude_4_plus(model) {
        betas.push("context-management-2025-06-27");
    }
    if config.enable_extended_cache_ttl {
        betas.push("extended-cache-ttl-2025-04-11");
    }
    if config.enable_1m_context && model_supports_1m(model) {
        betas.push("context-1m-2025-08-07");
    }

    // Defence-in-depth: even if a future code path somehow adds a banned
    // marker to `betas`, strip it before emitting. This belt-and-suspenders
    // the policy against accidental regression.
    betas.retain(|m| !BANNED_BETA_MARKERS.contains(m));

    betas.join(",")
}

// ---- Model-capability helpers ----
//
// These are deliberately conservative substring matches, not a full
// model-feature matrix. They express "does this model name look like one
// that supports feature X". Upstream genai already handles the more
// nuanced model/adapter dispatch; these are shaper-side hints for the
// beta-header bundle.

fn model_supports_thinking(model: &str) -> bool {
    // Claude 4+ opus/sonnet lineages support extended thinking.
    model.contains("claude-opus-4") || model.contains("claude-sonnet-4")
}

fn model_is_claude_4_plus(model: &str) -> bool {
    // Any claude-4-* family. Conservative: specific major digit rather
    // than assuming alphanumeric sorting.
    model.contains("claude-opus-4")
        || model.contains("claude-sonnet-4")
        || model.contains("claude-haiku-4")
}

fn model_supports_1m(model: &str) -> bool {
    // Opus 4.6+ and Sonnet 4.6+ both advertise 1M-context betas.
    // Opus-4-7 (pattern's primary target) is covered by substring.
    model.contains("claude-opus-4-6")
        || model.contains("claude-opus-4-7")
        || model.contains("claude-sonnet-4-6")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn min_config() -> ShaperConfig {
        ShaperConfig {
            x_app: "pattern".into(),
            compat_mode: super::super::ShaperCompatMode::HonestPattern,
            target_is_first_party: false,
            enable_interleaved_thinking: false,
            enable_dev_full_thinking: false,
            enable_context_management: false,
            enable_extended_cache_ttl: false,
            enable_1m_context: false,
        }
    }

    #[test]
    fn identification_headers_contain_required_entries() {
        let config = min_config();
        let uuid = crate::session_uuid::SessionUuidRotator::new();
        let headers = build_identification_headers(
            &config,
            &uuid.current(),
            AuthTier::ApiKey,
            "claude-opus-4-7",
        )
        .expect("build ok");

        let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"User-Agent"));
        assert!(names.contains(&"X-App"));
        assert!(names.contains(&"X-Pattern-Session-Id"));
        assert!(names.contains(&"X-Client-Request-Id"));
    }

    #[test]
    fn beta_header_empty_with_minimal_config_on_api_key_auth() {
        let config = min_config();
        let value = build_beta_header_value(&config, AuthTier::ApiKey, "claude-opus-4-7");
        assert_eq!(value, "", "no flags + api-key auth → no beta markers");
    }

    #[cfg(feature = "subscription-oauth")]
    #[test]
    fn oauth_tier_adds_oauth_beta_marker() {
        let config = min_config();
        let value = build_beta_header_value(&config, AuthTier::SessionPickup, "claude-opus-4-7");
        assert!(value.contains("oauth-2025-04-20"));
    }

    #[test]
    fn first_party_target_adds_prompt_caching_scope() {
        let mut config = min_config();
        config.target_is_first_party = true;
        let value = build_beta_header_value(&config, AuthTier::ApiKey, "claude-opus-4-7");
        assert!(value.contains("prompt-caching-scope-2026-01-05"));
    }

    #[test]
    fn interleaved_thinking_requires_capable_model() {
        let mut config = min_config();
        config.enable_interleaved_thinking = true;
        // Opus 4 supports it.
        let v = build_beta_header_value(&config, AuthTier::ApiKey, "claude-opus-4-7");
        assert!(v.contains("interleaved-thinking-2025-05-14"));
        // Haiku 3 doesn't — no marker even if the flag is set.
        let v = build_beta_header_value(&config, AuthTier::ApiKey, "claude-haiku-3");
        assert!(!v.contains("interleaved-thinking"));
    }

    #[test]
    fn one_million_context_requires_capable_model() {
        let mut config = min_config();
        config.enable_1m_context = true;
        let v = build_beta_header_value(&config, AuthTier::ApiKey, "claude-opus-4-7");
        assert!(v.contains("context-1m-2025-08-07"));
        let v = build_beta_header_value(&config, AuthTier::ApiKey, "claude-haiku-4-5");
        assert!(!v.contains("context-1m"));
    }

    #[test]
    fn banned_markers_never_in_output() {
        let mut config = min_config();
        config.target_is_first_party = true;
        config.enable_interleaved_thinking = true;
        config.enable_dev_full_thinking = true;
        config.enable_context_management = true;
        config.enable_extended_cache_ttl = true;
        config.enable_1m_context = true;

        let v = build_beta_header_value(&config, AuthTier::ApiKey, "claude-opus-4-7");
        for banned in BANNED_BETA_MARKERS {
            assert!(
                !v.contains(banned),
                "banned marker {banned} slipped into beta value: {v}"
            );
        }
    }
}
