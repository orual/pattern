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
    /// Fresh PKCE callback flow completed this session — user completed the
    /// browser-based authorization and the token was just minted.
    #[cfg(feature = "subscription-oauth")]
    Pkce,
    /// Pattern's own PKCE-minted token loaded from keyring or JSON-file
    /// fallback. Distinct from [`AuthTier::Pkce`] (fresh PKCE flow) — this
    /// variant covers the case where the user ran `pattern auth` previously
    /// and the token is being reused from persistent storage.
    #[cfg(feature = "subscription-oauth")]
    StoredOauth,
}

impl AuthTier {
    /// Returns `true` if this tier authenticates via OAuth Bearer token (PKCE
    /// or session-pickup). Used by the shaper to decide whether to include
    /// `oauth-2025-04-20` in the `Anthropic-Beta` header — that marker must
    /// appear alongside the other beta markers in one header value, not in a
    /// separate header that would silently overwrite the shaper's output.
    ///
    /// When the `subscription-oauth` feature is disabled this always returns
    /// `false` (no OAuth tiers are compiled in).
    pub fn is_oauth(self) -> bool {
        #[cfg(feature = "subscription-oauth")]
        {
            matches!(
                self,
                AuthTier::SessionPickup | AuthTier::Pkce | AuthTier::StoredOauth
            )
        }
        #[cfg(not(feature = "subscription-oauth"))]
        {
            let _ = self;
            false
        }
    }
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

// ---- Tier-forcing helpers ----

/// A [`crate::creds_store::CredsStore`] that always reports "no stored
/// credential". Used by the `session_pickup_only` and `pkce_only` chains to
/// ensure the stored-OAuth tier never resolves, leaving only the intended
/// tier active.
#[cfg(feature = "subscription-oauth")]
struct MemOnlyCredsStore;

#[cfg(feature = "subscription-oauth")]
#[async_trait::async_trait]
impl crate::creds_store::CredsStore for MemOnlyCredsStore {
    async fn get(&self, _provider: &str) -> Result<Option<ProviderCredential>, ProviderError> {
        Ok(None)
    }

    async fn put(&self, _token: &ProviderCredential) -> Result<(), ProviderError> {
        // No-op: tier-forcing chains never store tokens.
        Ok(())
    }

    async fn delete(&self, _provider: &str) -> Result<(), ProviderError> {
        Ok(())
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

    /// Session-pickup-only chain. Forces tier 3 (ambient claude-code
    /// credentials at `~/.claude/.credentials.json`); API-key and stored
    /// OAuth tiers are not tried. Use when `--auth session-pickup` is
    /// explicitly requested so the chain resolves exactly one tier.
    ///
    /// Requires the `subscription-oauth` feature.
    #[cfg(feature = "subscription-oauth")]
    pub fn session_pickup_only() -> Self {
        use std::sync::Arc;
        Self {
            // Disabled API-key tier: ANTHROPIC_API_KEY is not consulted.
            api_key: ApiKeyTier::disabled("anthropic"),
            oauth: Some(OAuthChainState {
                session_pickup: super::session_pickup::SessionPickupTier::default(),
                // PkceTier is present but never reached — PKCE is not part of
                // the normal `resolve()` path; it's an interactive flow the
                // caller triggers explicitly when `NoAuthAvailable` is returned.
                pkce: Arc::new(super::pkce::PkceTier::anthropic()),
                // MemOnlyCredsStore: stored-OAuth tier (pattern's own PKCE
                // token) always misses, so only session-pickup is tried.
                creds_store: Arc::new(MemOnlyCredsStore),
                refresh_mutex: Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }

    /// PKCE-forcing chain: disables api-key, session-pickup, and stored-OAuth
    /// tiers so every resolve returns [`ProviderError::NoAuthAvailable`]. Use
    /// when `--auth pkce` is explicitly requested — the caller observes the
    /// `NoAuthAvailable` error and triggers the interactive PKCE flow
    /// externally. All three disabled tiers are sentinels:
    /// [`ApiKeyTier::disabled`], [`super::session_pickup::SessionPickupTier::noop`],
    /// and [`MemOnlyCredsStore`].
    ///
    /// Requires the `subscription-oauth` feature.
    #[cfg(feature = "subscription-oauth")]
    pub fn pkce_only() -> Self {
        use std::sync::Arc;
        Self {
            // Disabled API-key tier: forces the caller to the PKCE flow path.
            api_key: ApiKeyTier::disabled("anthropic"),
            oauth: Some(OAuthChainState {
                // Disabled session-pickup: pick_up always returns None.
                session_pickup: super::session_pickup::SessionPickupTier::noop(),
                pkce: Arc::new(super::pkce::PkceTier::anthropic()),
                // MemOnlyCredsStore: stored-OAuth tier always misses.
                creds_store: Arc::new(MemOnlyCredsStore),
                refresh_mutex: Arc::new(tokio::sync::Mutex::new(())),
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
        // Tier order: explicit-user-choice before ambient.
        //
        // 1. Stored OAuth — pattern's own PKCE token, persisted after a
        //    deliberate auth flow. Most-explicit user intent.
        // 2. API key — env-provided, also user-explicit. Takes precedence
        //    over session-pickup so setting ANTHROPIC_API_KEY actually
        //    overrides the ambient claude-code session.
        // 3. Session-pickup — ambient fallback, uses whatever claude-code
        //    happens to have in ~/.claude/.credentials.json. Last so
        //    explicit choices always win.

        // Tier 1: stored OAuth with refresh-on-near-expiry. Token was
        // previously minted via a PKCE flow and persisted to the keyring or
        // JSON-file fallback. Uses `StoredOauth` (not `Pkce`) to distinguish
        // from a fresh interactive PKCE callback completed in this session.
        #[cfg(feature = "subscription-oauth")]
        if let Some(oauth) = &self.oauth
            && let Some(stored) = oauth.creds_store.get("anthropic").await?
        {
            let token = self.refresh_if_needed(oauth, stored).await?;
            return Ok(ResolvedCredential {
                source: AuthTier::StoredOauth,
                token,
            });
        }

        // Tier 2: API key (always available; only tier without subscription-oauth).
        if let Some(token) = self.api_key.resolve() {
            return Ok(ResolvedCredential {
                source: AuthTier::ApiKey,
                token,
            });
        }

        // Tier 3: session-pickup (ambient claude-code credentials).
        #[cfg(feature = "subscription-oauth")]
        if let Some(oauth) = &self.oauth
            && let Some(token) = oauth.session_pickup.pick_up().await?
        {
            return Ok(ResolvedCredential {
                source: AuthTier::SessionPickup,
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

        let refresh_token =
            token
                .refresh_token
                .as_ref()
                .ok_or_else(|| ProviderError::RefreshFailed {
                    reason: "stored token has no refresh_token".into(),
                })?;

        let fresh = oauth.pkce.refresh(refresh_token).await?;
        oauth.creds_store.put(&fresh).await?;
        Ok(fresh)
    }
}

// ---- OpenAI: stored-OAuth (codex storage) → env API key → file-embedded API key ----

/// OpenAI credential chain mirroring [`AnthropicAuthChain`]'s tier-walking
/// shape. Tier order (explicit-over-ambient, same rationale as Anthropic):
///
/// 1. **Stored OAuth** — codex CLI's `.auth.json` with `auth_mode: chatgpt`
///    + valid `tokens` (or the equivalent keyring entry). Proactive refresh
///    fires when the access_token JWT's `exp` claim is within 8 seconds.
/// 2. **`OPENAI_API_KEY` env var** — explicit user intent, wins over any
///    ambient credential codex CLI might have lying around in a file.
/// 3. **File-embedded API key** — codex CLI's `.auth.json` with
///    `auth_mode: apikey` + non-null `OPENAI_API_KEY`. Last-resort ambient
///    fallback for users who configured codex with an API key but didn't
///    set the env var.
///
/// OAuth tiers gated on `subscription-oauth`; without that feature the
/// chain collapses to env + file-embedded api-key.
pub struct OpenAiAuthChain {
    api_key: ApiKeyTier,

    #[cfg(feature = "subscription-oauth")]
    oauth: Option<CodexOAuthChainState>,
}

#[cfg(feature = "subscription-oauth")]
struct CodexOAuthChainState {
    store: std::sync::Arc<super::codex_storage::CodexAuthStore>,
    config: super::codex_oauth::CodexOAuthConfig,
    http: reqwest::Client,
    /// In-process serialization of refresh attempts. Concurrent
    /// `resolve()` callers that find a near-expiry token queue here; the
    /// first does the network round trip + persists, subsequent callers
    /// re-read the store and observe the fresh token.
    refresh_mutex: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl OpenAiAuthChain {
    /// API-key-only chain. `OPENAI_API_KEY` env var is the only auth source;
    /// codex storage is not consulted.
    pub fn api_key_only() -> Self {
        Self {
            api_key: ApiKeyTier::new("openai", "OPENAI_API_KEY"),
            #[cfg(feature = "subscription-oauth")]
            oauth: None,
        }
    }

    /// Full chain with codex-compatible OAuth support.
    #[cfg(feature = "subscription-oauth")]
    pub fn with_oauth(
        store: std::sync::Arc<super::codex_storage::CodexAuthStore>,
        config: super::codex_oauth::CodexOAuthConfig,
        http: reqwest::Client,
    ) -> Self {
        Self {
            api_key: ApiKeyTier::new("openai", "OPENAI_API_KEY"),
            oauth: Some(CodexOAuthChainState {
                store,
                config,
                http,
                refresh_mutex: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }

    /// OAuth-only chain. Disables env + file-embedded api-key tiers so the
    /// chain resolves *only* the codex-OAuth tier. Used by tests that want
    /// to assert OAuth-tier behaviour without env-var interference.
    #[cfg(feature = "subscription-oauth")]
    pub fn oauth_only(
        store: std::sync::Arc<super::codex_storage::CodexAuthStore>,
        config: super::codex_oauth::CodexOAuthConfig,
        http: reqwest::Client,
    ) -> Self {
        Self {
            api_key: ApiKeyTier::disabled("openai"),
            oauth: Some(CodexOAuthChainState {
                store,
                config,
                http,
                refresh_mutex: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }
}

/// Proactive-refresh window: refresh if access_token expires within this
/// many seconds. Matches codex's `TOKEN_REFRESH_INTERVAL = 8`.
#[cfg(feature = "subscription-oauth")]
const REFRESH_BUFFER_SECS: i64 = 8;

#[async_trait::async_trait]
impl CredentialChain for OpenAiAuthChain {
    fn provider(&self) -> &str {
        "openai"
    }

    async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
        // Tier 1: stored OAuth (codex chatgpt-mode tokens with proactive refresh).
        #[cfg(feature = "subscription-oauth")]
        if let Some(oauth) = &self.oauth {
            let load = oauth
                .store
                .load()
                .await
                .map_err(storage_to_provider)?;
            if let Some(auth) = &load.auth
                && matches!(auth.auth_mode, Some(super::codex_storage::AuthMode::Chatgpt))
                && let Some(tokens) = &auth.tokens
            {
                let token = self
                    .resolve_oauth(oauth, auth, tokens, load.file_existed)
                    .await?;
                return Ok(ResolvedCredential {
                    source: AuthTier::StoredOauth,
                    token,
                });
            }
        }

        // Tier 2: OPENAI_API_KEY env var.
        if let Some(token) = self.api_key.resolve() {
            return Ok(ResolvedCredential {
                source: AuthTier::ApiKey,
                token,
            });
        }

        // Tier 3: file-embedded api key (codex's ApiKey mode). Only checked if
        // tier 1 didn't return AND env tier 2 was empty.
        #[cfg(feature = "subscription-oauth")]
        if let Some(oauth) = &self.oauth {
            let load = oauth
                .store
                .load()
                .await
                .map_err(storage_to_provider)?;
            if let Some(auth) = load.auth
                && matches!(auth.auth_mode, Some(super::codex_storage::AuthMode::ApiKey))
                && let Some(key) = auth.openai_api_key
            {
                return Ok(ResolvedCredential {
                    source: AuthTier::ApiKey,
                    token: super::api_key::token_from_literal_key(
                        "openai",
                        secrecy::SecretString::from(key),
                    ),
                });
            }
        }

        Err(ProviderError::NoAuthAvailable {
            provider: "openai".into(),
        })
    }
}

#[cfg(feature = "subscription-oauth")]
impl OpenAiAuthChain {
    /// Translate codex-shape `TokenData` + parent `AuthDotJson` into a
    /// Pattern `ProviderCredential`, refreshing first if the access_token
    /// JWT's `exp` claim is within the 8-second buffer.
    async fn resolve_oauth(
        &self,
        oauth: &CodexOAuthChainState,
        auth: &super::codex_storage::AuthDotJson,
        tokens: &super::codex_storage::TokenData,
        file_existed: bool,
    ) -> Result<pattern_core::types::provider::ProviderCredential, ProviderError> {
        let needs_refresh = match super::codex_oauth::parse_jwt_expiration(&tokens.access_token) {
            Ok(Some(exp)) => {
                let now = jiff::Timestamp::now();
                let secs_remaining = exp.as_second().saturating_sub(now.as_second());
                secs_remaining <= REFRESH_BUFFER_SECS
            }
            // Either we couldn't parse the access_token as a JWT or it has no
            // `exp` claim. Treat as "no proactive refresh"; reactive refresh
            // (Phase 4) will catch genuine expiry.
            Ok(None) | Err(_) => false,
        };

        let final_tokens = if needs_refresh {
            self.refresh_serialized(oauth, auth, tokens, file_existed)
                .await?
        } else {
            tokens.clone()
        };

        Ok(provider_credential_from_codex_tokens(&final_tokens))
    }

    /// Mutex-serialized refresh: first concurrent caller does the network
    /// round trip + persist; subsequent callers re-read post-lock and observe
    /// the fresh token without duplicating the call. Holds the cross-process
    /// flock for the full read-modify-write so Pattern + codex CLI can't race.
    async fn refresh_serialized(
        &self,
        oauth: &CodexOAuthChainState,
        prior_auth: &super::codex_storage::AuthDotJson,
        prior_tokens: &super::codex_storage::TokenData,
        file_existed_at_outer_load: bool,
    ) -> Result<super::codex_storage::TokenData, ProviderError> {
        // Cross-process lock spans the entire RMW; in-process mutex is held
        // INSIDE so we can re-read after winning the file lock too.
        let _file_guard = oauth.store.lock().await.map_err(storage_to_provider)?;
        let _mu_guard = oauth.refresh_mutex.lock().await;

        // Post-lock re-read: another task or another process may have
        // refreshed while we waited. Trust whatever's on disk.
        let post = oauth
            .store
            .load()
            .await
            .map_err(storage_to_provider)?;
        let (current_auth, current_tokens, file_existed) = match post.auth {
            Some(a) if matches!(a.auth_mode, Some(super::codex_storage::AuthMode::Chatgpt))
                && a.tokens.is_some() =>
            {
                let t = a.tokens.clone().expect("checked Some above");
                (a, t, post.file_existed)
            }
            // Storage was cleared while we waited — fall back to what we read
            // pre-lock and try refresh with that.
            _ => (
                prior_auth.clone(),
                prior_tokens.clone(),
                file_existed_at_outer_load,
            ),
        };

        // Re-check expiry under the lock — if the post-lock read shows a
        // fresh token, skip the network call.
        if let Ok(Some(exp)) = super::codex_oauth::parse_jwt_expiration(&current_tokens.access_token)
        {
            let secs_remaining = exp.as_second().saturating_sub(jiff::Timestamp::now().as_second());
            if secs_remaining > REFRESH_BUFFER_SECS {
                return Ok(current_tokens);
            }
        }

        // Network refresh.
        use secrecy::SecretString;
        let refresh_secret = SecretString::from(current_tokens.refresh_token.clone());
        let fresh = super::codex_oauth::refresh_token(&oauth.config, &oauth.http, &refresh_secret)
            .await
            .map_err(codex_oauth_to_provider)?;

        // Build the new AuthDotJson: keep mode + any embedded api_key,
        // replace tokens + last_refresh.
        let new_tokens = super::codex_storage::TokenData {
            id_token: fresh.id_token.clone(),
            access_token: fresh.access_token.expose_secret_to_string(),
            refresh_token: fresh.refresh_token.expose_secret_to_string(),
            account_id: fresh.account_id.clone(),
        };
        let new_auth = super::codex_storage::AuthDotJson {
            auth_mode: current_auth.auth_mode,
            openai_api_key: current_auth.openai_api_key,
            tokens: Some(new_tokens.clone()),
            last_refresh: Some(jiff::Timestamp::now()),
            agent_identity: current_auth.agent_identity,
        };
        oauth
            .store
            .save_under_lock(&new_auth, file_existed)
            .await
            .map_err(storage_to_provider)?;
        Ok(new_tokens)
    }
}

#[cfg(feature = "subscription-oauth")]
fn provider_credential_from_codex_tokens(
    tokens: &super::codex_storage::TokenData,
) -> pattern_core::types::provider::ProviderCredential {
    use pattern_core::types::provider::ProviderCredential;
    use secrecy::SecretString;
    // Derive expires_at from the access_token JWT's `exp` claim.
    let expires_at = super::codex_oauth::parse_jwt_expiration(&tokens.access_token)
        .ok()
        .flatten();
    let now = jiff::Timestamp::now();
    ProviderCredential {
        provider: "openai".into(),
        access_token: SecretString::from(tokens.access_token.clone()),
        refresh_token: Some(SecretString::from(tokens.refresh_token.clone())),
        expires_at,
        scope: None,
        session_id: tokens.account_id.clone(),
        created_at: now,
        updated_at: now,
    }
}

#[cfg(feature = "subscription-oauth")]
fn storage_to_provider(e: super::codex_storage::StorageError) -> ProviderError {
    use super::codex_storage::StorageError as S;
    match e {
        S::HomeDirNotFound => ProviderError::CredentialStoreUnavailable,
        S::Io { .. } => ProviderError::CredentialStoreUnavailable,
        S::Serialize(_) => ProviderError::CredentialStorage {
            reason: format!("codex_storage serialize: {e}"),
        },
        S::Lock(_) => ProviderError::CredentialStoreUnavailable,
        S::Keyring(p) => p,
    }
}

#[cfg(feature = "subscription-oauth")]
fn codex_oauth_to_provider(e: super::codex_oauth::CodexOAuthError) -> ProviderError {
    use super::codex_oauth::{CodexOAuthError as E, RefreshFailureKind};
    match e {
        E::RefreshFailed { kind, detail } => match kind {
            RefreshFailureKind::Expired | RefreshFailureKind::Exhausted | RefreshFailureKind::Revoked => {
                ProviderError::NoAuthAvailable {
                    provider: format!("openai (refresh: {kind:?} — {detail})"),
                }
            }
            RefreshFailureKind::Transient => ProviderError::RefreshFailed {
                reason: format!("transient: {detail}"),
            },
            RefreshFailureKind::Other => ProviderError::RefreshFailed {
                reason: format!("other: {detail}"),
            },
        },
        other => ProviderError::RefreshFailed {
            reason: other.to_string(),
        },
    }
}

/// Helper for getting a String out of a SecretString in the chain. The
/// type lives outside this crate; we add the extension to keep the call
/// sites tidy.
#[cfg(feature = "subscription-oauth")]
trait ExposeSecretString {
    fn expose_secret_to_string(&self) -> String;
}

#[cfg(feature = "subscription-oauth")]
impl ExposeSecretString for secrecy::SecretString {
    fn expose_secret_to_string(&self) -> String {
        use secrecy::ExposeSecret;
        self.expose_secret().to_string()
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
        assert!(
            matches!(err, ProviderError::NoAuthAvailable { provider } if provider == "anthropic")
        );
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
            // Stored OAuth (previously PKCE-minted, loaded from keyring/JSON)
            // must report StoredOauth, not Pkce. Fresh PKCE callback flow is
            // the only case that returns AuthTier::Pkce.
            assert_eq!(resolved.source, AuthTier::StoredOauth);
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
            let err = chain
                .resolve()
                .await
                .expect_err("no refresh_token → RefreshFailed");
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

    // OpenAI / codex-OAuth chain tests — tier walk, proactive refresh,
    // refresh-mutex serialization, rotation, error classification.
    #[cfg(feature = "subscription-oauth")]
    mod openai_oauth_chain {
        use super::*;
        use crate::auth::codex_oauth::CodexOAuthConfig;
        use crate::auth::codex_storage::{
            AuthDotJson, AuthMode, CodexAuthStore, TokenData,
        };
        use base64::Engine;
        use jiff::Timestamp;
        use secrecy::ExposeSecret;
        use std::sync::Arc;
        use tempfile::tempdir;
        use wiremock::matchers::{body_string_contains, method, path as wmpath};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        /// Build a synthetic JWT carrying the given `exp` claim. Used for
        /// access_token fixtures so the chain's proactive-refresh logic
        /// has something realistic to parse. No signature (codex doesn't
        /// verify; neither do we).
        fn jwt_with_exp(exp_secs: i64) -> String {
            let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
            let payload = serde_json::json!({ "exp": exp_secs });
            let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&payload).unwrap());
            let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"sig");
            format!("{header}.{payload_b64}.{sig}")
        }

        /// Synthetic id_token carrying the chatgpt_account_id claim.
        fn id_token_with_account(account_id: &str) -> String {
            let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
            let payload = serde_json::json!({
                "https://api.openai.com/auth": { "chatgpt_account_id": account_id }
            });
            let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&payload).unwrap());
            let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"sig");
            format!("{header}.{payload_b64}.{sig}")
        }

        fn config_pointing_at(server_uri: &str) -> CodexOAuthConfig {
            CodexOAuthConfig {
                client_id: "test-client".into(),
                issuer: server_uri.into(),
                scopes: vec!["openid".into(), "offline_access".into()],
            }
        }

        async fn seed_oauth_file(store: &CodexAuthStore, access_token: &str) {
            let auth = AuthDotJson {
                auth_mode: Some(AuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(TokenData {
                    id_token: id_token_with_account("acct_seed"),
                    access_token: access_token.into(),
                    refresh_token: "rt-seed".into(),
                    account_id: Some("acct_seed".into()),
                }),
                last_refresh: Some(Timestamp::now()),
                agent_identity: None,
            };
            // Direct file save (bypasses keyring + flock since the file
            // doesn't exist yet and we want save_file's exact semantics).
            // We need file_existed=true on later save() so the chain
            // mirrors back to the file; setting it up by writing the file
            // directly here.
            store.save(&auth, true).await.expect("seed save");
        }

        async fn seed_apikey_file(store: &CodexAuthStore, key: &str) {
            let auth = AuthDotJson {
                auth_mode: Some(AuthMode::ApiKey),
                openai_api_key: Some(key.into()),
                tokens: None,
                last_refresh: None,
                agent_identity: None,
            };
            store.save(&auth, true).await.expect("seed save");
        }

        /// Tier order: stored OAuth > env > file-embedded API key.
        /// (Test 1: OAuth wins over env when both present.)
        #[tokio::test]
        async fn stored_oauth_beats_env_api_key() {
            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            // Fresh access_token (far future expiry).
            let access = jwt_with_exp(Timestamp::now().as_second() + 3600);
            seed_oauth_file(&store, &access).await;

            let _env_guard = EnvGuard::set("OPENAI_API_KEY", "sk-env-should-lose");

            let chain = OpenAiAuthChain::with_oauth(
                store,
                CodexOAuthConfig::codex(),
                reqwest::Client::new(),
            );
            let resolved = chain.resolve().await.expect("resolves");
            assert_eq!(resolved.source, AuthTier::StoredOauth);
            assert_eq!(resolved.token.access_token.expose_secret(), access);
            assert_eq!(resolved.token.session_id.as_deref(), Some("acct_seed"));
        }

        /// Tier order: env API key wins when no OAuth stored.
        #[tokio::test]
        async fn env_api_key_used_when_no_stored_oauth() {
            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            let _env_guard = EnvGuard::set("OPENAI_API_KEY", "sk-env-test");

            let chain = OpenAiAuthChain::with_oauth(
                store,
                CodexOAuthConfig::codex(),
                reqwest::Client::new(),
            );
            let resolved = chain.resolve().await.expect("resolves");
            assert_eq!(resolved.source, AuthTier::ApiKey);
            assert_eq!(resolved.token.provider, "openai");
            assert_eq!(resolved.token.access_token.expose_secret(), "sk-env-test");
        }

        /// Tier order: file-embedded API key is last-resort when env is unset.
        #[tokio::test]
        async fn file_embedded_api_key_used_when_env_absent() {
            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            seed_apikey_file(&store, "sk-file-test").await;
            let _env_guard = EnvGuard::remove("OPENAI_API_KEY");

            let chain = OpenAiAuthChain::with_oauth(
                store,
                CodexOAuthConfig::codex(),
                reqwest::Client::new(),
            );
            let resolved = chain.resolve().await.expect("resolves");
            assert_eq!(resolved.source, AuthTier::ApiKey);
            assert_eq!(resolved.token.access_token.expose_secret(), "sk-file-test");
        }

        /// All tiers empty → NoAuthAvailable.
        #[tokio::test]
        async fn no_creds_anywhere_surfaces_no_auth_available() {
            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            let _env_guard = EnvGuard::remove("OPENAI_API_KEY");
            let chain = OpenAiAuthChain::with_oauth(
                store,
                CodexOAuthConfig::codex(),
                reqwest::Client::new(),
            );
            let err = chain.resolve().await.expect_err("no creds");
            assert!(
                matches!(err, ProviderError::NoAuthAvailable { provider } if provider == "openai")
            );
        }

        /// Proactive refresh: access_token within 8s of expiry triggers a
        /// network refresh; the new tokens are persisted; the chain
        /// returns the fresh credential. Verifies rotation: the server
        /// returns a NEW refresh_token, and we observe it stored.
        #[tokio::test]
        async fn proactive_refresh_within_buffer_rotates_tokens() {
            let server = MockServer::start().await;
            let new_access = jwt_with_exp(Timestamp::now().as_second() + 3600);
            let new_id = id_token_with_account("acct_after_refresh");
            Mock::given(method("POST"))
                .and(wmpath("/oauth/token"))
                .and(body_string_contains("grant_type=refresh_token"))
                .and(body_string_contains("refresh_token=rt-seed"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": new_access,
                    "refresh_token": "rt-NEW-rotated",
                    "id_token": new_id,
                    "expires_in": 3600
                })))
                .mount(&server)
                .await;

            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            // Stale access_token: 5 seconds remaining → inside 8s buffer.
            let stale = jwt_with_exp(Timestamp::now().as_second() + 5);
            seed_oauth_file(&store, &stale).await;

            let chain = OpenAiAuthChain::with_oauth(
                store.clone(),
                config_pointing_at(&server.uri()),
                reqwest::Client::new(),
            );
            let resolved = chain.resolve().await.expect("resolves with refresh");
            assert_eq!(resolved.source, AuthTier::StoredOauth);
            // Fresh access_token (not the seeded stale one).
            assert_eq!(resolved.token.access_token.expose_secret(), new_access);
            // Refresh token rotated.
            let stored = store.load().await.expect("post-refresh load").auth.unwrap();
            let stored_tokens = stored.tokens.expect("tokens present");
            assert_eq!(stored_tokens.refresh_token, "rt-NEW-rotated");
            assert_eq!(stored_tokens.access_token, new_access);
            assert_eq!(stored_tokens.account_id.as_deref(), Some("acct_after_refresh"));
            assert!(stored.last_refresh.is_some());
        }

        /// Concurrent resolves on a near-expiry token: only ONE network
        /// refresh fires; both callers see the fresh credential. The
        /// refresh_mutex serializes; the second caller re-reads under
        /// lock and observes the freshly-rotated token.
        #[tokio::test]
        async fn concurrent_refresh_attempts_dedupe_to_single_network_call() {
            use std::sync::atomic::{AtomicUsize, Ordering};
            let server = MockServer::start().await;
            let new_access = jwt_with_exp(Timestamp::now().as_second() + 3600);
            let new_id = id_token_with_account("acct_dedup");

            // The mock counts every hit so we can assert "exactly one".
            let hits = Arc::new(AtomicUsize::new(0));
            let hits_for_mock = hits.clone();
            let access_for_mock = new_access.clone();
            let id_for_mock = new_id.clone();
            Mock::given(method("POST"))
                .and(wmpath("/oauth/token"))
                .and(body_string_contains("grant_type=refresh_token"))
                .respond_with(move |_: &wiremock::Request| {
                    hits_for_mock.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "access_token": access_for_mock.clone(),
                        "refresh_token": "rt-rotated-dedup",
                        "id_token": id_for_mock.clone(),
                        "expires_in": 3600
                    }))
                })
                .mount(&server)
                .await;

            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            let stale = jwt_with_exp(Timestamp::now().as_second() + 3);
            seed_oauth_file(&store, &stale).await;

            let chain = Arc::new(OpenAiAuthChain::with_oauth(
                store.clone(),
                config_pointing_at(&server.uri()),
                reqwest::Client::new(),
            ));

            let c1 = chain.clone();
            let c2 = chain.clone();
            let (r1, r2) = tokio::join!(
                tokio::spawn(async move { c1.resolve().await }),
                tokio::spawn(async move { c2.resolve().await })
            );
            let r1 = r1.unwrap().expect("first resolve");
            let r2 = r2.unwrap().expect("second resolve");
            assert_eq!(r1.source, AuthTier::StoredOauth);
            assert_eq!(r2.source, AuthTier::StoredOauth);
            // Both see the fresh access_token.
            assert_eq!(r1.token.access_token.expose_secret(), new_access);
            assert_eq!(r2.token.access_token.expose_secret(), new_access);
            // Only ONE network refresh fired.
            assert_eq!(hits.load(Ordering::SeqCst), 1, "expected single network call");
        }

        /// Refresh server returns `refresh_token_expired` → chain surfaces
        /// `NoAuthAvailable` so the gateway can prompt re-login.
        #[tokio::test]
        async fn refresh_expired_classifies_as_no_auth_available() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wmpath("/oauth/token"))
                .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "error": "refresh_token_expired"
                })))
                .mount(&server)
                .await;

            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            let stale = jwt_with_exp(Timestamp::now().as_second() + 3);
            seed_oauth_file(&store, &stale).await;

            let chain = OpenAiAuthChain::with_oauth(
                store,
                config_pointing_at(&server.uri()),
                reqwest::Client::new(),
            );
            let err = chain.resolve().await.expect_err("refresh failure");
            // Per design: Expired/Exhausted/Revoked → NoAuthAvailable.
            // Transient/Other → RefreshFailed.
            assert!(
                matches!(&err, ProviderError::NoAuthAvailable { provider } if provider.starts_with("openai")),
                "got: {err:?}"
            );
        }

        /// Refresh server returns 5xx → chain surfaces `RefreshFailed`
        /// (retry-eligible classification).
        #[tokio::test]
        async fn refresh_5xx_classifies_as_refresh_failed() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wmpath("/oauth/token"))
                .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
                .mount(&server)
                .await;

            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            let stale = jwt_with_exp(Timestamp::now().as_second() + 3);
            seed_oauth_file(&store, &stale).await;

            let chain = OpenAiAuthChain::with_oauth(
                store,
                config_pointing_at(&server.uri()),
                reqwest::Client::new(),
            );
            let err = chain.resolve().await.expect_err("transient");
            assert!(matches!(err, ProviderError::RefreshFailed { .. }), "got: {err:?}");
        }

        /// `oauth_only()` chain ignores env API key.
        #[tokio::test]
        async fn oauth_only_chain_ignores_env_api_key() {
            let dir = tempdir().unwrap();
            let store = Arc::new(CodexAuthStore::file_only(dir.path().into()));
            let _env_guard = EnvGuard::set("OPENAI_API_KEY", "sk-env-should-be-ignored");

            let chain = OpenAiAuthChain::oauth_only(
                store,
                CodexOAuthConfig::codex(),
                reqwest::Client::new(),
            );
            // No stored oauth → no fallback → NoAuthAvailable.
            let err = chain.resolve().await.expect_err("oauth-only with no creds");
            assert!(matches!(err, ProviderError::NoAuthAvailable { .. }));
        }

        /// `api_key_only()` chain ignores stored OAuth.
        #[tokio::test]
        async fn api_key_only_chain_ignores_stored_oauth() {
            let _env_guard = EnvGuard::set("OPENAI_API_KEY", "sk-env-test");
            // No store passed in at all; api_key_only doesn't need one.
            let chain = OpenAiAuthChain::api_key_only();
            let resolved = chain.resolve().await.expect("resolves");
            assert_eq!(resolved.source, AuthTier::ApiKey);
            assert_eq!(resolved.token.access_token.expose_secret(), "sk-env-test");
        }
    }
}
