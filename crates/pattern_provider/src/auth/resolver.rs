//! Per-provider credential resolution.
//!
//! Two concrete chains ship in Phase 4:
//!
//! - [`AnthropicAuthChain`] — session-pickup → stored OAuth (with refresh) →
//!   API key. The first two tiers are gated on the `subscription-oauth`
//!   feature. Without that feature the chain collapses to API key only.
//! - [`GeminiAuthChain`] — API key only.
//!
//! Each chain implements [`CredentialChain`] and the
//! [`crate::gateway::PatternGatewayClient`] will look up the right chain
//! per-call based on the inferred `AdapterKind`.
//!
//! # Refresh serialization
//!
//! Concurrent `resolve()` calls may both find a near-expiry stored token
//! and attempt to refresh it. [`AnthropicAuthChain`] serializes refreshes
//! behind a per-chain mutex: the first caller does the network round
//! trip, stores the fresh token, subsequent callers observe the fresh
//! token on their post-lock re-read. See AC4.7.

use pattern_core::error::ProviderError;
use pattern_core::types::provider::ProviderCredential;

use super::api_key::ApiKeyTier;

/// Which tier produced the credential. Useful for observability and for
/// telling "did we end up on the API-key fallback?" at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthTier {
    ApiKey,
    #[cfg(feature = "subscription-oauth")]
    SessionPickup,
    #[cfg(feature = "subscription-oauth")]
    Pkce,
}

/// A resolved credential together with the tier it came from.
#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    pub source: AuthTier,
    pub token: ProviderCredential,
}

/// Per-provider credential chain.
///
/// Implementations hold whatever tier-specific state they need (tokens,
/// HTTP clients, creds-store handles) and expose a single async
/// `resolve` entry point.
#[async_trait::async_trait]
pub trait CredentialChain: Send + Sync {
    /// Which provider this chain resolves for (matches AdapterKind-as-str).
    fn provider(&self) -> &str;

    /// Walk the tier chain, returning the first successful credential.
    /// Errors propagate — tier absence (e.g. missing env var) is not an
    /// error, just a fall-through.
    async fn resolve(&self) -> Result<ResolvedCredential, ProviderError>;
}

// ---- Gemini: API key only ----

/// Gemini credential chain. Currently API-key only; OAuth tiers aren't
/// applicable for Gemini's subscription model.
#[derive(Debug, Clone)]
pub struct GeminiAuthChain {
    api_key: ApiKeyTier,
}

impl Default for GeminiAuthChain {
    fn default() -> Self {
        Self {
            api_key: ApiKeyTier::gemini(),
        }
    }
}

impl GeminiAuthChain {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl CredentialChain for GeminiAuthChain {
    fn provider(&self) -> &str {
        "gemini"
    }

    async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
        if let Some(token) = self.api_key.resolve() {
            return Ok(ResolvedCredential {
                source: AuthTier::ApiKey,
                token,
            });
        }
        Err(ProviderError::NoAuthAvailable {
            provider: "gemini".into(),
        })
    }
}

// ---- Anthropic: full three-tier chain ----

/// Anthropic credential chain with session-pickup → stored OAuth → API key
/// fallback. The OAuth tiers are gated on `subscription-oauth`; without
/// that feature the chain is API-key only.
pub struct AnthropicAuthChain {
    api_key: ApiKeyTier,

    #[cfg(feature = "subscription-oauth")]
    oauth: Option<OAuthChainState>,
}

#[cfg(feature = "subscription-oauth")]
struct OAuthChainState {
    session_pickup: super::session_pickup::SessionPickupTier,
    pkce: std::sync::Arc<super::pkce::PkceTier>,
    creds_store: std::sync::Arc<dyn crate::creds_store::CredsStore>,
    /// Serializes concurrent refresh attempts (AC4.7). All callers that
    /// find a near-expiry token queue here; the first does the network
    /// refresh, subsequent callers read the fresh token from the store.
    refresh_mutex: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl AnthropicAuthChain {
    /// API-key-only chain — suitable for environments without a keyring
    /// or any OAuth plumbing.
    pub fn api_key_only() -> Self {
        Self {
            api_key: ApiKeyTier::anthropic(),
            #[cfg(feature = "subscription-oauth")]
            oauth: None,
        }
    }

    /// Full three-tier chain with OAuth support. Requires the
    /// `subscription-oauth` feature.
    #[cfg(feature = "subscription-oauth")]
    pub fn with_oauth(
        session_pickup: super::session_pickup::SessionPickupTier,
        pkce: std::sync::Arc<super::pkce::PkceTier>,
        creds_store: std::sync::Arc<dyn crate::creds_store::CredsStore>,
    ) -> Self {
        Self {
            api_key: ApiKeyTier::anthropic(),
            oauth: Some(OAuthChainState {
                session_pickup,
                pkce,
                creds_store,
                refresh_mutex: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }
}

#[async_trait::async_trait]
impl CredentialChain for AnthropicAuthChain {
    fn provider(&self) -> &str {
        "anthropic"
    }

    async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
        // Tier 1: session-pickup (ambient claude-code credentials).
        #[cfg(feature = "subscription-oauth")]
        if let Some(oauth) = &self.oauth {
            if let Some(token) = oauth.session_pickup.pick_up().await? {
                return Ok(ResolvedCredential {
                    source: AuthTier::SessionPickup,
                    token,
                });
            }

            // Tier 2: stored OAuth with refresh-on-near-expiry.
            if let Some(stored) = oauth.creds_store.get("anthropic").await? {
                let token = self.refresh_if_needed(oauth, stored).await?;
                return Ok(ResolvedCredential {
                    source: AuthTier::Pkce,
                    token,
                });
            }
        }

        // Tier 3: API key (always available; only tier without subscription-oauth).
        if let Some(token) = self.api_key.resolve() {
            return Ok(ResolvedCredential {
                source: AuthTier::ApiKey,
                token,
            });
        }

        Err(ProviderError::NoAuthAvailable {
            provider: "anthropic".into(),
        })
    }
}

#[cfg(feature = "subscription-oauth")]
impl AnthropicAuthChain {
    async fn refresh_if_needed(
        &self,
        oauth: &OAuthChainState,
        token: ProviderCredential,
    ) -> Result<ProviderCredential, ProviderError> {
        if !token.needs_refresh() {
            return Ok(token);
        }

        // Serialize refreshes (AC4.7). Acquire the mutex BEFORE re-reading —
        // that way concurrent refresh attempts don't each do a network round
        // trip. The first task to hit the mutex refreshes; subsequent tasks
        // re-read the store and see the fresh token.
        let _guard = oauth.refresh_mutex.lock().await;

        // Post-lock re-read: another task may have refreshed while we
        // waited for the mutex.
        if let Some(post_lock) = oauth.creds_store.get("anthropic").await?
            && !post_lock.needs_refresh()
        {
            return Ok(post_lock);
        }

        let refresh_token = token.refresh_token.as_ref().ok_or_else(|| {
            ProviderError::RefreshFailed {
                reason: "stored token has no refresh_token".into(),
            }
        })?;

        let fresh = oauth.pkce.refresh(refresh_token).await?;
        oauth.creds_store.put(&fresh).await?;
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::api_key::EnvGuard;

    #[tokio::test]
    async fn gemini_chain_uses_api_key() {
        let _g = EnvGuard::set("GEMINI_API_KEY", "gem-test");
        let chain = GeminiAuthChain::new();
        let resolved = chain.resolve().await.expect("resolves");
        assert_eq!(resolved.source, AuthTier::ApiKey);
        assert_eq!(resolved.token.provider, "gemini");
    }

    #[tokio::test]
    async fn gemini_chain_errors_when_no_key() {
        let _g1 = EnvGuard::remove("GEMINI_API_KEY");
        let _g2 = EnvGuard::remove("GOOGLE_API_KEY");
        let chain = GeminiAuthChain::new();
        let err = chain.resolve().await.expect_err("no key → NoAuthAvailable");
        assert!(matches!(err, ProviderError::NoAuthAvailable { provider } if provider == "gemini"));
    }

    #[tokio::test]
    async fn anthropic_api_key_only_chain_uses_env() {
        let _g = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-chain-test");
        let chain = AnthropicAuthChain::api_key_only();
        let resolved = chain.resolve().await.expect("resolves");
        assert_eq!(resolved.source, AuthTier::ApiKey);
        assert_eq!(resolved.token.provider, "anthropic");
    }

    #[tokio::test]
    async fn anthropic_chain_without_key_surfaces_no_auth_available() {
        let _g = EnvGuard::remove("ANTHROPIC_API_KEY");
        let chain = AnthropicAuthChain::api_key_only();
        let err = chain.resolve().await.expect_err("no key → NoAuthAvailable");
        assert!(matches!(err, ProviderError::NoAuthAvailable { provider } if provider == "anthropic"));
    }

    // subscription-oauth tier-chain tests — session-pickup, stored-token,
    // refresh-on-near-expiry, refresh mutex serialization.
    #[cfg(feature = "subscription-oauth")]
    mod oauth_chain {
        use super::*;
        use crate::auth::pkce::{PkceConfig, PkceTier};
        use crate::auth::session_pickup::SessionPickupTier;
        use crate::creds_store::{CredsStore, JsonFallbackStore};
        use jiff::{Timestamp, ToSpan};
        use secrecy::SecretString;
        use std::sync::Arc;
        use tempfile::tempdir;
        use wiremock::matchers::{body_string_contains, method, path as wmpath};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn make_pkce_tier(server_uri: String) -> Arc<PkceTier> {
            let mut config = PkceConfig::anthropic();
            config.token_endpoint = format!("{server_uri}/v1/oauth/token");
            Arc::new(PkceTier::new(config))
        }

        fn make_session_pickup_noop() -> SessionPickupTier {
            // Pointed at a definitely-missing path so pick_up returns Ok(None).
            SessionPickupTier::with_paths(vec!["/this/path/does/not/exist.json".into()])
        }

        #[tokio::test]
        async fn stored_token_used_when_session_pickup_empty() {
            let dir = tempdir().unwrap();
            let store: Arc<dyn CredsStore> =
                Arc::new(JsonFallbackStore::with_root(dir.path().join("creds")).unwrap());

            // Pre-seed a non-near-expiry stored token.
            let now = Timestamp::now();
            let stored = ProviderCredential {
                provider: "anthropic".into(),
                access_token: SecretString::from("at-stored".to_string()),
                refresh_token: Some(SecretString::from("rt-stored".to_string())),
                expires_at: now.checked_add(2.hours()).ok(),
                scope: None,
                session_id: None,
                created_at: now,
                updated_at: now,
            };
            store.put(&stored).await.unwrap();

            let chain = AnthropicAuthChain::with_oauth(
                make_session_pickup_noop(),
                Arc::new(PkceTier::anthropic()), // unused; no refresh expected
                store,
            );

            let _g = EnvGuard::remove("ANTHROPIC_API_KEY");
            let resolved = chain.resolve().await.expect("resolves via stored");
            assert_eq!(resolved.source, AuthTier::Pkce);
            use secrecy::ExposeSecret;
            assert_eq!(resolved.token.access_token.expose_secret(), "at-stored");
        }

        #[tokio::test]
        async fn stored_near_expiry_triggers_single_refresh() {
            // AC4.7: ten concurrent resolves on a near-expiry token must
            // produce exactly ONE refresh network call.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wmpath("/v1/oauth/token"))
                .and(body_string_contains("grant_type=refresh_token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "at-refreshed",
                    "refresh_token": "rt-refreshed",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                })))
                .expect(1) // AC4.7: exactly one refresh, not ten.
                .mount(&server)
                .await;

            let dir = tempdir().unwrap();
            let store: Arc<dyn CredsStore> =
                Arc::new(JsonFallbackStore::with_root(dir.path().join("creds")).unwrap());

            let now = Timestamp::now();
            // Near-expiry: 30 seconds out, well inside the 5-minute refresh window.
            let stored = ProviderCredential {
                provider: "anthropic".into(),
                access_token: SecretString::from("at-old".to_string()),
                refresh_token: Some(SecretString::from("rt-old".to_string())),
                expires_at: now.checked_add(30.seconds()).ok(),
                scope: None,
                session_id: None,
                created_at: now,
                updated_at: now,
            };
            store.put(&stored).await.unwrap();

            let chain = Arc::new(AnthropicAuthChain::with_oauth(
                make_session_pickup_noop(),
                make_pkce_tier(server.uri()),
                store,
            ));

            let _g = EnvGuard::remove("ANTHROPIC_API_KEY");

            // Fire 10 concurrent resolves.
            let mut handles = Vec::new();
            for _ in 0..10 {
                let chain = chain.clone();
                handles.push(tokio::spawn(async move { chain.resolve().await }));
            }
            for h in handles {
                let resolved = h.await.unwrap().expect("each resolve must succeed");
                use secrecy::ExposeSecret;
                assert_eq!(resolved.token.access_token.expose_secret(), "at-refreshed");
            }
            // wiremock's `.expect(1)` asserts on MockServer drop — the refresh
            // endpoint was hit exactly once across all 10 callers.
        }

        #[tokio::test]
        async fn stored_near_expiry_with_no_refresh_token_errors() {
            let dir = tempdir().unwrap();
            let store: Arc<dyn CredsStore> =
                Arc::new(JsonFallbackStore::with_root(dir.path().join("creds")).unwrap());

            let now = Timestamp::now();
            let stored = ProviderCredential {
                provider: "anthropic".into(),
                access_token: SecretString::from("at-orphan".to_string()),
                refresh_token: None, // no refresh token → cannot refresh
                expires_at: now.checked_add(30.seconds()).ok(),
                scope: None,
                session_id: None,
                created_at: now,
                updated_at: now,
            };
            store.put(&stored).await.unwrap();

            let chain = AnthropicAuthChain::with_oauth(
                make_session_pickup_noop(),
                Arc::new(PkceTier::anthropic()),
                store,
            );

            let _g = EnvGuard::remove("ANTHROPIC_API_KEY");
            let err = chain.resolve().await.expect_err("no refresh_token → RefreshFailed");
            assert!(
                matches!(
                    &err,
                    ProviderError::RefreshFailed { reason } if reason.contains("refresh_token")
                ),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn refresh_http_error_surfaces_as_refresh_failed() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wmpath("/v1/oauth/token"))
                .respond_with(ResponseTemplate::new(401).set_body_string("invalid_grant"))
                .mount(&server)
                .await;

            let dir = tempdir().unwrap();
            let store: Arc<dyn CredsStore> =
                Arc::new(JsonFallbackStore::with_root(dir.path().join("creds")).unwrap());

            let now = Timestamp::now();
            let stored = ProviderCredential {
                provider: "anthropic".into(),
                access_token: SecretString::from("at-bad".to_string()),
                refresh_token: Some(SecretString::from("rt-bad".to_string())),
                expires_at: now.checked_add(30.seconds()).ok(),
                scope: None,
                session_id: None,
                created_at: now,
                updated_at: now,
            };
            store.put(&stored).await.unwrap();

            let chain = AnthropicAuthChain::with_oauth(
                make_session_pickup_noop(),
                make_pkce_tier(server.uri()),
                store,
            );

            let _g = EnvGuard::remove("ANTHROPIC_API_KEY");
            let err = chain.resolve().await.expect_err("401 → RefreshFailed");
            assert!(
                matches!(&err, ProviderError::RefreshFailed { .. }),
                "got: {err:?}"
            );
        }

        #[tokio::test]
        async fn chain_falls_through_to_api_key_when_all_oauth_tiers_empty() {
            let dir = tempdir().unwrap();
            let store: Arc<dyn CredsStore> =
                Arc::new(JsonFallbackStore::with_root(dir.path().join("creds")).unwrap());
            // store is empty, no session-pickup, API key set.

            let chain = AnthropicAuthChain::with_oauth(
                make_session_pickup_noop(),
                Arc::new(PkceTier::anthropic()),
                store,
            );

            let _g = EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-fallback-test");
            let resolved = chain.resolve().await.expect("resolves via api key");
            assert_eq!(resolved.source, AuthTier::ApiKey);
        }
    }
}
