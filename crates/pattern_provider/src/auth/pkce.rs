//! PKCE (Proof Key for Code Exchange) auth tier for Anthropic subscription
//! OAuth.
//!
//! Ported from pattern's pre-v3 verified-working OAuth flow
//! (`rewrite-staging/provider/oauth/auth_flow.rs`) with the following changes:
//!
//! - Renamed `DeviceAuthFlow`/`OAuthConfig` → `PkceTier`/`PkceConfig`.
//! - Errors flow through [`pattern_core::error::ProviderError`] (not
//!   `CoreError`), splitting initial-exchange failures into
//!   `AuthExchangeFailed` and refresh failures into `RefreshFailed`.
//! - Tokens wrap in [`secrecy::SecretString`] throughout.
//! - PKCE verifier + state are 32 bytes (base64url-encoded), matching
//!   claude-code + cliproxy. Pre-v3 used 64.
//! - Manual-paste is the default `redirect_uri`. Empirical testing during
//!   planning confirmed this works; auto-loopback is deferred to a future
//!   polish task backed by `jacquard-oauth`'s `loopback` feature.
//!
//! Gated behind the `subscription-oauth` feature.
#![cfg(feature = "subscription-oauth")]

use std::time::Duration;

use base64::Engine;
use pattern_core::error::ProviderError;
use pattern_core::types::provider::ProviderCredential;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};

// ---- Config ----

/// PKCE client configuration. Defaults to Anthropic subscription OAuth.
#[derive(Debug, Clone)]
pub struct PkceConfig {
    /// OAuth client identifier.
    pub client_id: String,

    /// OAuth authorization endpoint (browser-visible URL).
    pub auth_endpoint: String,

    /// OAuth token-exchange endpoint (server-side POST target).
    pub token_endpoint: String,

    /// Redirect URI. Manual-paste flow uses `platform.claude.com/oauth/code/callback`.
    pub redirect_uri: String,

    /// Requested scope set, space-joined into the authorize URL.
    pub scopes: Vec<String>,

    /// Provider name used when minting [`ProviderCredential`] instances.
    /// Defaults to `"anthropic"` via [`PkceConfig::anthropic`].
    pub provider_name: String,
}

impl PkceConfig {
    /// Anthropic subscription OAuth config. Verified working against the
    /// live endpoints during v3 planning.
    ///
    /// Key configuration notes:
    /// - `auth_endpoint` lives on `claude.ai`, **not** `console.anthropic.com`
    ///   (that's the API-key/console flow we deliberately route away from).
    /// - `token_endpoint` is `console.anthropic.com/v1/oauth/token` —
    ///   verified working empirically. The platform has a sibling
    ///   `platform.claude.com/v1/oauth/token` URL documented in some docs;
    ///   Phase 4 pins the console.* URL because that's what round-tripped
    ///   a real token during planning.
    /// - `redirect_uri` is the manual-paste callback (user copies the code
    ///   from the browser into the CLI). Auto-loopback lives on a future
    ///   polish pass.
    /// - `scopes` exclude `org:create_api_key` — that scope routes Anthropic
    ///   into the API-key creation flow on the console, which is **not**
    ///   subscription auth. `user:sessions:claude_code` is Anthropic's
    ///   name for the subscription-session scope; requesting it consents
    ///   to the scope Anthropic defined, not a claim to be claude-code.
    pub fn anthropic() -> Self {
        Self {
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(),
            auth_endpoint: "https://claude.ai/oauth/authorize".into(),
            token_endpoint: "https://console.anthropic.com/v1/oauth/token".into(),
            redirect_uri: "https://platform.claude.com/oauth/code/callback".into(),
            scopes: vec![
                "user:profile".into(),
                "user:inference".into(),
                "user:sessions:claude_code".into(),
                "user:mcp_servers".into(),
                "user:file_upload".into(),
            ],
            provider_name: "anthropic".into(),
        }
    }
}

// ---- Pending auth state ----

/// Server-side state of an in-flight PKCE exchange. Produced by
/// [`PkceTier::begin_auth`]; consumed by [`PkceTier::complete_manual`].
///
/// Callers typically display `authorize_url()` to the user, wait for them
/// to paste the `code#state` string back, then call `complete_manual`.
pub struct PendingAuth {
    authorize_url: String,
    /// PKCE code verifier (32 random bytes, base64url-encoded). Held as
    /// `SecretString` since it proves possession of the challenge sent to
    /// the provider.
    verifier: SecretString,
    /// CSRF state token. Public because it echoes back in the callback URL
    /// verbatim.
    state: String,
}

impl PendingAuth {
    /// URL the user must visit in their browser to authorize.
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    /// CSRF state value; useful for diagnostic UIs ("expected state X, got Y").
    pub fn state(&self) -> &str {
        &self.state
    }
}

// ---- Tier ----

/// PKCE OAuth tier. Build with [`PkceTier::anthropic`] for the preset, or
/// [`PkceTier::new`] with a custom [`PkceConfig`].
pub struct PkceTier {
    config: PkceConfig,
    http: reqwest::Client,
}

impl PkceTier {
    /// Construct with an explicit config.
    pub fn new(config: PkceConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Construct with Anthropic subscription-OAuth defaults.
    pub fn anthropic() -> Self {
        Self::new(PkceConfig::anthropic())
    }

    /// Internal constructor for tests — lets a wiremock'd `base_url` override
    /// the token endpoint without otherwise touching the config. Auth endpoint
    /// isn't hit in the token-exchange tests so we don't override it.
    #[cfg(test)]
    fn with_token_endpoint(mut config: PkceConfig, token_endpoint: String) -> Self {
        config.token_endpoint = token_endpoint;
        Self::new(config)
    }

    /// Begin a PKCE flow. Returns a [`PendingAuth`] holding the authorize URL
    /// the user should visit and the verifier/state the CLI must remember
    /// until the user pastes back.
    pub fn begin_auth(&self) -> PendingAuth {
        let (verifier, challenge) = generate_pkce();
        let state = generate_state();

        let scope = self.config.scopes.join(" ");
        let params = [
            // `code=true` signals Anthropic's OAuth server that this is a
            // subscription (Max) auth; without it the browser falls through
            // to the API-key creation flow.
            ("code", "true"),
            ("client_id", self.config.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", self.config.redirect_uri.as_str()),
            ("scope", scope.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ];

        let authorize_url = format!(
            "{}?{}",
            self.config.auth_endpoint,
            serde_urlencoded::to_string(params).expect("urlencode failure is impossible for &str params")
        );

        PendingAuth {
            authorize_url,
            verifier: SecretString::from(verifier),
            state,
        }
    }

    /// Complete a manual-paste PKCE flow.
    ///
    /// `code_and_state` is the exact string the user pastes back from the
    /// browser redirect — `<code>#<state>`. We split on `#`, validate the
    /// state against the pending auth (CSRF guard), and exchange the code
    /// for tokens.
    pub async fn complete_manual(
        &self,
        pending: PendingAuth,
        code_and_state: &str,
    ) -> Result<ProviderCredential, ProviderError> {
        let (code, state) = split_code_and_state(code_and_state)?;

        if state != pending.state {
            return Err(ProviderError::AuthExchangeFailed {
                reason: "state parameter mismatch (CSRF guard)".into(),
            });
        }

        let response = self
            .exchange(TokenRequestBody::AuthorizationCode {
                client_id: &self.config.client_id,
                code: &code,
                redirect_uri: &self.config.redirect_uri,
                code_verifier: pending.verifier.expose_secret(),
                state: Some(&state),
            })
            .await?;

        Ok(self.token_from_response(response))
    }

    /// Refresh the access token using a stored refresh token. Returns a
    /// fresh [`ProviderCredential`] with new access + refresh values.
    pub async fn refresh(
        &self,
        refresh_token: &SecretString,
    ) -> Result<ProviderCredential, ProviderError> {
        let response = self
            .exchange(TokenRequestBody::Refresh {
                client_id: &self.config.client_id,
                refresh_token: refresh_token.expose_secret(),
            })
            .await
            .map_err(|e| match e {
                ProviderError::AuthExchangeFailed { reason } => ProviderError::RefreshFailed { reason },
                other => other,
            })?;

        Ok(self.token_from_response(response))
    }

    async fn exchange(
        &self,
        body: TokenRequestBody<'_>,
    ) -> Result<TokenResponse, ProviderError> {
        let form = body.into_form();
        let response = self
            .http
            .post(&self.config.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&form)
            .send()
            .await
            .map_err(|e| ProviderError::AuthExchangeFailed {
                reason: format!("HTTP request failed: {e}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::AuthExchangeFailed {
                reason: format!("provider returned HTTP {status}: {body_text}"),
            });
        }

        response
            .json::<TokenResponse>()
            .await
            .map_err(|e| ProviderError::AuthExchangeFailed {
                reason: format!("token response parse failed: {e}"),
            })
    }

    fn token_from_response(&self, resp: TokenResponse) -> ProviderCredential {
        let now = jiff::Timestamp::now();
        // `expires_in` is seconds from now; compute absolute expiry.
        let expires_at = resp
            .expires_in
            .and_then(|secs| {
                let dur = Duration::from_secs(secs);
                let span = jiff::SignedDuration::try_from(dur).ok()?;
                now.checked_add(span).ok()
            });

        ProviderCredential {
            provider: self.config.provider_name.clone(),
            access_token: SecretString::from(resp.access_token),
            refresh_token: resp.refresh_token.map(SecretString::from),
            expires_at,
            scope: resp.scope,
            session_id: None,
            created_at: now,
            updated_at: now,
        }
    }
}

// ---- PKCE primitives ----

/// Generate a PKCE verifier (32 random bytes, base64url-encoded) and the
/// SHA-256-based code challenge. Matches claude-code / cliproxy conventions.
pub(crate) fn generate_pkce() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

    (verifier, challenge)
}

/// Generate a CSRF state parameter (32 random bytes, base64url-encoded).
pub(crate) fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Split a `<code>#<state>` pasted callback string. Accepts the full URL
/// too — we pull the fragment and treat it as state, the path as code.
pub(crate) fn split_code_and_state(
    code_and_state: &str,
) -> Result<(String, String), ProviderError> {
    // Try URL parsing first — callers sometimes paste the full callback URL.
    if let Ok(parsed) = url::Url::parse(code_and_state) {
        let mut code = None;
        let mut state = None;
        for (k, v) in parsed.query_pairs() {
            match k.as_ref() {
                "code" => code = Some(v.to_string()),
                "state" => state = Some(v.to_string()),
                _ => {}
            }
        }
        if let (Some(c), Some(s)) = (code, state) {
            return Ok((c, s));
        }
    }

    // Otherwise, treat as plain `code#state`.
    let mut split = code_and_state.split('#');
    let code = split.next().ok_or_else(|| ProviderError::AuthExchangeFailed {
        reason: "paste string was empty".into(),
    })?;
    let state = split.next().ok_or_else(|| ProviderError::AuthExchangeFailed {
        reason: "paste missing '#state' suffix; did you copy the whole string?".into(),
    })?;
    if code.is_empty() || state.is_empty() {
        return Err(ProviderError::AuthExchangeFailed {
            reason: "empty code or state in paste".into(),
        });
    }
    Ok((code.to_string(), state.to_string()))
}

// ---- Request / response shapes ----

enum TokenRequestBody<'a> {
    AuthorizationCode {
        client_id: &'a str,
        code: &'a str,
        redirect_uri: &'a str,
        code_verifier: &'a str,
        state: Option<&'a str>,
    },
    Refresh {
        client_id: &'a str,
        refresh_token: &'a str,
    },
}

impl<'a> TokenRequestBody<'a> {
    fn into_form(self) -> Vec<(&'a str, &'a str)> {
        match self {
            Self::AuthorizationCode {
                client_id,
                code,
                redirect_uri,
                code_verifier,
                state,
            } => {
                let mut v = vec![
                    ("grant_type", "authorization_code"),
                    ("client_id", client_id),
                    ("code", code),
                    ("redirect_uri", redirect_uri),
                    ("code_verifier", code_verifier),
                ];
                if let Some(s) = state {
                    v.push(("state", s));
                }
                v
            }
            Self::Refresh {
                client_id,
                refresh_token,
            } => vec![
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_token),
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// Seconds until expiry, per RFC 6749. Some providers omit; we treat
    /// absence as "no known expiry" and rely on 401-based refresh.
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn pkce_verifier_and_challenge_are_base64url_safe() {
        let (verifier, challenge) = generate_pkce();
        for c in verifier.chars().chain(challenge.chars()) {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "base64url-no-pad should not contain '{c}'"
            );
        }
        // Determinism: same verifier → same challenge.
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);
    }

    #[test]
    fn state_is_unique_across_calls() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b, "32 random bytes should effectively never collide");
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let tier = PkceTier::anthropic();
        let pending = tier.begin_auth();
        let url = pending.authorize_url();

        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state="));
        assert!(url.contains("code=true"), "subscription-flow marker missing");
        assert!(url.contains("client_id=9d1c250a"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fplatform.claude.com"));
    }

    #[test]
    fn split_code_state_parses_plain_hash_form() {
        let (c, s) = split_code_and_state("my-code#my-state").expect("parse ok");
        assert_eq!(c, "my-code");
        assert_eq!(s, "my-state");
    }

    #[test]
    fn split_code_state_parses_full_callback_url() {
        let (c, s) = split_code_and_state(
            "https://platform.claude.com/oauth/code/callback?code=my-code&state=my-state",
        )
        .expect("parse ok");
        assert_eq!(c, "my-code");
        assert_eq!(s, "my-state");
    }

    #[test]
    fn split_code_state_rejects_missing_state() {
        let err = split_code_and_state("just-code").expect_err("should fail");
        assert!(matches!(err, ProviderError::AuthExchangeFailed { .. }));
    }

    #[tokio::test]
    async fn complete_manual_rejects_state_mismatch() {
        let tier = PkceTier::anthropic();
        let pending = tier.begin_auth();

        // Use the right code but a different state.
        let paste = format!("dummy-code#{}-tampered", pending.state());
        let err = tier
            .complete_manual(pending, &paste)
            .await
            .expect_err("state mismatch should surface as AuthExchangeFailed");
        assert!(
            matches!(&err, ProviderError::AuthExchangeFailed { reason } if reason.contains("state"))
        );
    }

    #[tokio::test]
    async fn complete_manual_exchanges_token_on_success() {
        // Stand up a wiremock'd token endpoint and verify the exchange
        // round-trips end-to-end.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=good-code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-fresh",
                "refresh_token": "rt-fresh",
                "expires_in": 3600,
                "scope": "user:inference",
                "token_type": "Bearer"
            })))
            .mount(&server)
            .await;

        let mut config = PkceConfig::anthropic();
        config.token_endpoint = format!("{}/v1/oauth/token", server.uri());
        let tier = PkceTier::new(config);

        let pending = tier.begin_auth();
        let paste = format!("good-code#{}", pending.state());
        let token = tier.complete_manual(pending, &paste).await.expect("exchange ok");

        assert_eq!(token.provider, "anthropic");
        assert_eq!(token.access_token.expose_secret(), "at-fresh");
        assert_eq!(
            token.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("rt-fresh")
        );
        assert!(token.expires_at.is_some());
        assert_eq!(token.scope.as_deref(), Some("user:inference"));
    }

    #[tokio::test]
    async fn complete_manual_surfaces_http_error_as_auth_exchange_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "code expired"
            })))
            .mount(&server)
            .await;

        let tier = PkceTier::with_token_endpoint(
            PkceConfig::anthropic(),
            format!("{}/v1/oauth/token", server.uri()),
        );

        let pending = tier.begin_auth();
        let paste = format!("bad-code#{}", pending.state());
        let err = tier
            .complete_manual(pending, &paste)
            .await
            .expect_err("400 → AuthExchangeFailed");
        assert!(
            matches!(&err, ProviderError::AuthExchangeFailed { reason } if reason.contains("400"))
        );
    }

    #[tokio::test]
    async fn refresh_swaps_auth_exchange_error_for_refresh_failed() {
        // refresh() call re-maps AuthExchangeFailed → RefreshFailed so
        // callers get the right miette diagnostic code.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid_grant"))
            .mount(&server)
            .await;

        let tier = PkceTier::with_token_endpoint(
            PkceConfig::anthropic(),
            format!("{}/v1/oauth/token", server.uri()),
        );

        let refresh = SecretString::from("rt-stale".to_string());
        let err = tier
            .refresh(&refresh)
            .await
            .expect_err("401 on refresh → RefreshFailed");
        assert!(
            matches!(&err, ProviderError::RefreshFailed { .. }),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn refresh_round_trips_new_tokens_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=rt-old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-refreshed",
                "refresh_token": "rt-new",
                "expires_in": 1800,
                "token_type": "Bearer"
            })))
            .mount(&server)
            .await;

        let tier = PkceTier::with_token_endpoint(
            PkceConfig::anthropic(),
            format!("{}/v1/oauth/token", server.uri()),
        );

        let refresh = SecretString::from("rt-old".to_string());
        let token = tier.refresh(&refresh).await.expect("refresh ok");

        assert_eq!(token.access_token.expose_secret(), "at-refreshed");
        assert_eq!(
            token.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("rt-new")
        );
    }
}
