//! API-key auth tier — always-present final fallback.
//!
//! Each provider has a canonical env var (e.g. `ANTHROPIC_API_KEY`,
//! `GEMINI_API_KEY`, `GOOGLE_API_KEY`). The tier reads the env var lazily
//! on each resolve — this lets tests override via
//! `std::env::set_var` / `remove_var` without rebuilding the tier.
//!
//! API keys don't expire, so the produced [`ProviderOAuthToken`] has
//! `expires_at = None`. The `ProviderOAuthToken` shape is shared across
//! all auth tiers (session-pickup, PKCE, API key) even though "OAuth" is
//! in the name — it's just "the thing the gateway needs to authenticate a
//! request", not necessarily the product of an OAuth exchange.

use pattern_core::types::provider::ProviderOAuthToken;
use secrecy::SecretString;

/// API-key tier for a single provider.
///
/// Cheap to construct; holds only the provider name and the env var name
/// to consult. Env-var reads happen on every `resolve()` call.
#[derive(Debug, Clone)]
pub struct ApiKeyTier {
    provider: String,
    env_var: String,
}

impl ApiKeyTier {
    /// Construct a tier that reads `env_var` for `provider`'s API key.
    pub fn new(provider: impl Into<String>, env_var: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            env_var: env_var.into(),
        }
    }

    /// Preset: Anthropic reads `ANTHROPIC_API_KEY`.
    pub fn anthropic() -> Self {
        Self::new("anthropic", "ANTHROPIC_API_KEY")
    }

    /// Preset: Gemini reads `GEMINI_API_KEY` (with `GOOGLE_API_KEY` as a
    /// widely-used alternative — checked at resolve-time).
    ///
    /// When the primary var is absent we fall through to the alternative
    /// inside [`Self::resolve`] rather than constructing two tier instances.
    pub fn gemini() -> Self {
        Self::new("gemini", "GEMINI_API_KEY")
    }

    /// The provider this tier resolves for.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Resolve the API key. Returns:
    /// - `Some(token)` when the env var is set to a non-empty string.
    /// - `None` when absent or empty (tier fall-through).
    pub fn resolve(&self) -> Option<ProviderOAuthToken> {
        let key = read_api_key(&self.env_var).or_else(|| {
            // Gemini-specific compat: fall back to GOOGLE_API_KEY.
            if self.provider == "gemini" {
                read_api_key("GOOGLE_API_KEY")
            } else {
                None
            }
        })?;

        let now = jiff::Timestamp::now();
        Some(ProviderOAuthToken {
            provider: self.provider.clone(),
            access_token: SecretString::from(key),
            refresh_token: None,
            expires_at: None,
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        })
    }
}

fn read_api_key(env_var: &str) -> Option<String> {
    let raw = std::env::var(env_var).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Build a token from a literal API key. Used by non-env auth paths (e.g.
/// a key loaded from config file) that don't want to pollute the env.
pub fn token_from_literal_key(provider: impl Into<String>, key: SecretString) -> ProviderOAuthToken {
    let now = jiff::Timestamp::now();
    ProviderOAuthToken {
        provider: provider.into(),
        access_token: key,
        refresh_token: None,
        expires_at: None,
        scope: None,
        session_id: None,
        created_at: now,
        updated_at: now,
    }
}

/// Guard helper: sets the target env var to `value`, restores previous
/// state on drop. The tests in this module run serially under nextest by
/// default; if multi-threaded test harness is ever introduced, this is
/// the point to convert to `#[serial_test]`.
///
/// Keeps the test module cleaner than manual `set_var` / `remove_var`.
#[cfg(test)]
pub(crate) struct EnvGuard {
    name: String,
    prior: Option<String>,
}

#[cfg(test)]
impl EnvGuard {
    pub(crate) fn set(name: &str, value: &str) -> Self {
        let prior = std::env::var(name).ok();
        // SAFETY: tests are single-threaded via nextest's per-test
        // isolation. See module comment above.
        unsafe {
            std::env::set_var(name, value);
        }
        Self {
            name: name.into(),
            prior,
        }
    }

    pub(crate) fn remove(name: &str) -> Self {
        let prior = std::env::var(name).ok();
        unsafe {
            std::env::remove_var(name);
        }
        Self {
            name: name.into(),
            prior,
        }
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(&self.name, v),
                None => std::env::remove_var(&self.name),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn anthropic_env_var_resolves() {
        let _g = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-test-123");
        let tier = ApiKeyTier::anthropic();
        let token = tier.resolve().expect("env set → Some");
        assert_eq!(token.provider, "anthropic");
        assert_eq!(token.access_token.expose_secret(), "sk-ant-test-123");
        assert!(token.expires_at.is_none(), "api keys never expire");
    }

    #[test]
    fn empty_env_var_falls_through() {
        let _g = EnvGuard::set("ANTHROPIC_API_KEY", "   ");
        let tier = ApiKeyTier::anthropic();
        assert!(tier.resolve().is_none(), "whitespace-only key = tier skip");
    }

    #[test]
    fn absent_env_var_falls_through() {
        let _g = EnvGuard::remove("ANTHROPIC_API_KEY");
        let tier = ApiKeyTier::anthropic();
        assert!(tier.resolve().is_none());
    }

    #[test]
    fn gemini_falls_back_to_google_api_key() {
        let _g1 = EnvGuard::remove("GEMINI_API_KEY");
        let _g2 = EnvGuard::set("GOOGLE_API_KEY", "goog-test");
        let tier = ApiKeyTier::gemini();
        let token = tier.resolve().expect("fallback resolves");
        assert_eq!(token.provider, "gemini");
        assert_eq!(token.access_token.expose_secret(), "goog-test");
    }

    #[test]
    fn gemini_primary_env_takes_precedence() {
        let _g1 = EnvGuard::set("GEMINI_API_KEY", "primary");
        let _g2 = EnvGuard::set("GOOGLE_API_KEY", "secondary");
        let tier = ApiKeyTier::gemini();
        let token = tier.resolve().expect("resolves");
        assert_eq!(token.access_token.expose_secret(), "primary");
    }

    #[test]
    fn literal_key_bypass_works() {
        let tok = token_from_literal_key("custom", SecretString::from("literal-key".to_string()));
        assert_eq!(tok.provider, "custom");
        assert_eq!(tok.access_token.expose_secret(), "literal-key");
        assert!(tok.expires_at.is_none());
    }

    #[test]
    fn no_provider_means_no_gemini_fallback() {
        let _g1 = EnvGuard::remove("ANTHROPIC_API_KEY");
        let _g2 = EnvGuard::set("GOOGLE_API_KEY", "irrelevant");
        let tier = ApiKeyTier::anthropic();
        // Anthropic tier doesn't consult GOOGLE_API_KEY even if it's set.
        assert!(tier.resolve().is_none());
    }
}
