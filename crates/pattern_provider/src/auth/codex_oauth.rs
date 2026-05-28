// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Codex OAuth flow for OpenAI ChatGPT-subscription authentication.
//!
//! Ports the auth-flow surface from the official codex CLI
//! (`~/Git_Repos/codex/codex-rs/login/src/`, Apache-2.0) into Pattern's
//! `subscription-oauth` feature gate. Implements both the PKCE loopback
//! flow (default — opens a browser, listens on `localhost:1455` or 1457)
//! and the device-code fallback (for headless / `--headless` environments).
//!
//! ## Scope
//!
//! This module is *only* the OAuth state machine. It produces a
//! [`CodexTokenSet`] from a completed login or refresh; it does not persist
//! tokens (see `codex_storage`) or integrate with the credential chain
//! (see `resolver::OpenAiAuthChain`). Refresh is exposed here because the
//! token-exchange call mechanics are identical to the initial exchange.
//!
//! ## id_token verification
//!
//! Codex CLI does *not* verify the OAuth id_token signature — only the
//! `agent_identity` SSH-key JWT path uses `jsonwebtoken`/JWKS. We match
//! that posture: TLS to `auth.openai.com` is the authentication boundary;
//! verifying the JWT against a key fetched over the same TLS channel adds
//! defense in depth approximately zero. Claims are extracted by
//! base64-decoding the payload segment and reading the
//! `https://api.openai.com/auth` namespace exactly like codex's
//! [`parse_chatgpt_jwt_claims`].
//!
//! ## Tests
//!
//! Unit tests cover PKCE determinism, JWT claim extraction (synthetic
//! payload), and token-exchange round trip via wiremock. The loopback
//! listener test binds a real port (1455/1457 are skipped — the test uses
//! the OS-assigned port from a custom config) and POSTs to the callback.
//! Device-code polling is wiremock-driven.

#![cfg(feature = "subscription-oauth")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use jiff::Timestamp;
use miette::Diagnostic;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

// ---- Constants ----

/// OpenAI's public OAuth client ID for the Codex CLI. Reused for any
/// third-party client per OpenAI's documented policy (see codex CLI
/// repository for context).
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// OAuth scopes requested. Matches codex CLI exactly.
const SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "api.connectors.read",
    "api.connectors.invoke",
];

/// Default OAuth issuer. Codex CLI hardcodes this; we make it overridable
/// via [`CodexOAuthConfig`] for tests.
const DEFAULT_ISSUER: &str = "https://auth.openai.com";

/// Loopback redirect port (matches codex CLI).
const LOOPBACK_PORT_PRIMARY: u16 = 1455;
/// Fallback loopback port when primary is in use.
const LOOPBACK_PORT_FALLBACK: u16 = 1457;

/// Maximum time we wait for the browser-redirect callback. Codex uses the
/// same envelope; if the user takes longer they can retry.
const LOOPBACK_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Maximum time we wait for device-code verification. Codex's server uses
/// 15 minutes; matched here for parity.
const DEVICE_CODE_MAX_LIFETIME: Duration = Duration::from_secs(15 * 60);

// ---- Error type ----

/// Errors produced by the Codex OAuth flow.
///
/// Upstream typed errors (`std::io::Error`, `reqwest::Error`,
/// `base64::DecodeError`, `serde_json::Error`) are preserved as `#[source]`
/// so callers can match on cause kind (e.g. `io::ErrorKind::AddrInUse` for
/// retry decisions) instead of pattern-matching on stringified reasons.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum CodexOAuthError {
    /// Could not bind a TCP listener on either 1455 or 1457.
    #[error("could not bind loopback listener on ports {LOOPBACK_PORT_PRIMARY} or {LOOPBACK_PORT_FALLBACK}")]
    #[diagnostic(
        code(codex_oauth::loopback_bind),
        help("if both ports are held by other processes, re-run with --headless to force the device-code flow")
    )]
    LoopbackBindFailed(#[source] std::io::Error),

    /// Could not spawn the listener thread (rare; resource exhaustion).
    #[error("could not spawn loopback listener thread")]
    #[diagnostic(code(codex_oauth::loopback_thread_spawn))]
    LoopbackThreadSpawn(#[source] std::io::Error),

    /// Browser-open helper failed; caller falls back to printing the URL.
    #[error("could not open browser")]
    #[diagnostic(
        code(codex_oauth::browser_open),
        help("copy the URL printed to the terminal and open it manually")
    )]
    BrowserOpenFailed(#[source] std::io::Error),

    /// The 5-minute loopback timeout elapsed with no callback.
    #[error("OAuth callback did not arrive within {LOOPBACK_CALLBACK_TIMEOUT:?}")]
    #[diagnostic(
        code(codex_oauth::loopback_timeout),
        help("re-run the login command; the browser session can be closed")
    )]
    LoopbackTimeout,

    /// User denied authorization in the browser, or callback arrived with
    /// an OAuth error parameter set.
    #[error("authorization denied: {0}")]
    #[diagnostic(code(codex_oauth::oauth_denied))]
    OAuthDenied(String),

    /// CSRF guard tripped — the `state` echoed back didn't match what we
    /// sent.
    #[error("state parameter mismatch (CSRF guard)")]
    #[diagnostic(code(codex_oauth::state_invalid))]
    StateInvalid,

    /// Token-exchange POST returned a non-2xx response. Body kept verbatim
    /// since it's server-provided text without a typed schema we control.
    #[error("token exchange failed (HTTP {status})")]
    #[diagnostic(code(codex_oauth::token_exchange_failed))]
    TokenExchangeFailed { status: u16, body: String },

    /// Network / transport-level failure during a token-exchange POST.
    /// `reqwest::Error` preserves `is_timeout()` / `is_connect()` for
    /// caller retry decisions.
    #[error("token exchange transport error")]
    #[diagnostic(code(codex_oauth::token_exchange_transport))]
    TokenExchangeTransport(#[from] reqwest::Error),

    /// Token-exchange response was 2xx but the body didn't deserialize.
    /// Distinct from transport failures (which are retry-eligible).
    #[error("token exchange response could not be parsed as JSON")]
    #[diagnostic(code(codex_oauth::token_exchange_malformed))]
    TokenExchangeMalformed(#[source] serde_json::Error),

    /// id_token did not match `header.payload.signature` shape.
    #[error("id_token has wrong JWT shape (expected header.payload.signature)")]
    #[diagnostic(code(codex_oauth::id_token_shape))]
    IdTokenShape,

    /// id_token payload base64 decode failed.
    #[error("id_token payload base64 decode failed")]
    #[diagnostic(code(codex_oauth::id_token_base64))]
    IdTokenBase64(#[from] base64::DecodeError),

    /// id_token payload JSON parse failed.
    #[error("id_token payload JSON parse failed")]
    #[diagnostic(code(codex_oauth::id_token_json))]
    IdTokenJson(#[source] serde_json::Error),

    /// Token endpoint did not return an id_token (mandatory for our flow).
    #[error("token endpoint omitted id_token")]
    #[diagnostic(code(codex_oauth::id_token_missing))]
    IdTokenMissing,

    /// Token endpoint did not return a refresh_token (mandatory for our flow).
    #[error("token endpoint omitted refresh_token")]
    #[diagnostic(code(codex_oauth::refresh_token_missing))]
    RefreshTokenMissing,

    /// `expires_in` arithmetic overflowed the supported timestamp range.
    #[error("expires_in resulted in an out-of-range timestamp")]
    #[diagnostic(code(codex_oauth::expires_in_overflow))]
    ExpiresInOverflow(#[source] jiff::Error),

    /// Refresh-token call failed. `kind` classifies the cause for the chain.
    #[error("refresh failed ({kind:?}): {detail}")]
    #[diagnostic(
        code(codex_oauth::refresh_failed),
        help("run `pattern auth login openai` to re-authenticate")
    )]
    RefreshFailed {
        kind: RefreshFailureKind,
        detail: String,
    },

    /// Device-code lifetime elapsed without the user verifying.
    #[error("device-code authorization expired before user verified")]
    #[diagnostic(
        code(codex_oauth::device_code_expired),
        help("re-run the login command")
    )]
    DeviceCodeExpired,

    /// Server returned a denied state during device-code polling.
    #[error("device-code authorization denied: {0}")]
    #[diagnostic(code(codex_oauth::device_code_denied))]
    DeviceCodeDenied(String),
}

/// Classification of refresh failures. Mirrors codex's
/// `RefreshTokenFailedReason` so the chain can decide whether to surface
/// "log in again" vs retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefreshFailureKind {
    /// Refresh token outright expired.
    Expired,
    /// Refresh token already consumed (rotation race or stolen-token replay).
    Exhausted,
    /// Refresh token revoked server-side.
    Revoked,
    /// Other 4xx — surface to user for re-login.
    Other,
    /// 5xx or network — retry-eligible.
    Transient,
}

// ---- Config ----

/// Configurable Codex OAuth client parameters. Defaults to OpenAI's
/// production endpoints; tests override the issuer to point at wiremock.
#[derive(Debug, Clone)]
pub struct CodexOAuthConfig {
    pub client_id: String,
    pub issuer: String,
    pub scopes: Vec<String>,
}

impl CodexOAuthConfig {
    /// Construct the production config: codex client_id, auth.openai.com
    /// issuer, official scope set.
    pub fn codex() -> Self {
        Self {
            client_id: CODEX_CLIENT_ID.to_string(),
            issuer: DEFAULT_ISSUER.to_string(),
            scopes: SCOPES.iter().map(|&s| s.to_string()).collect(),
        }
    }

    fn authorize_endpoint(&self) -> String {
        format!("{}/oauth/authorize", self.issuer)
    }
    fn token_endpoint(&self) -> String {
        // Same env-override codex CLI honours, for parity in test setups.
        std::env::var("CODEX_REFRESH_TOKEN_URL_OVERRIDE")
            .unwrap_or_else(|_| format!("{}/oauth/token", self.issuer))
    }
    /// Revocation endpoint, used by the future `pattern auth logout openai`
    /// subcommand (Phase 5).
    #[allow(dead_code)] // surfaced in Phase 5
    fn revoke_endpoint(&self) -> String {
        std::env::var("CODEX_REVOKE_TOKEN_URL_OVERRIDE")
            .unwrap_or_else(|_| format!("{}/oauth/revoke", self.issuer))
    }
    fn device_code_request_endpoint(&self) -> String {
        format!("{}/api/accounts/deviceauth/usercode", self.issuer)
    }
    fn device_code_poll_endpoint(&self) -> String {
        format!("{}/api/accounts/deviceauth/token", self.issuer)
    }
    /// User-facing verification URL printed during device-code flow.
    fn device_verification_uri(&self) -> String {
        format!("{}/codex/device", self.issuer)
    }
}

// ---- PKCE primitives ----

/// PKCE verifier byte length. Codex uses 64; spec allows 43–128.
const PKCE_VERIFIER_BYTES: usize = 64;
/// CSRF state byte length.
const STATE_BYTES: usize = 32;

/// PKCE material for one authorization round.
struct PkceMaterial {
    verifier: SecretString,
    challenge: String,
}

/// Generate a fresh PKCE verifier and its S256 challenge. Verifier is 64
/// random bytes URL-safe-base64 no-pad encoded; challenge is
/// `base64url_nopad(sha256(verifier_str))`.
fn generate_pkce() -> PkceMaterial {
    let mut bytes = [0u8; PKCE_VERIFIER_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

    PkceMaterial {
        verifier: SecretString::from(verifier),
        challenge,
    }
}

fn generate_state() -> String {
    let mut bytes = [0u8; STATE_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---- id_token claim extraction ----

/// Subset of id_token claims we care about. Mirrors codex's `IdTokenInfo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdTokenClaims {
    pub email: Option<String>,
    pub chatgpt_plan_type: Option<String>,
    pub chatgpt_user_id: Option<String>,
    pub chatgpt_account_id: Option<String>,
    pub chatgpt_account_is_fedramp: bool,
    /// Raw JWT string. Stored verbatim in `.auth.json` for codex CLI parity.
    pub raw_jwt: String,
}

#[derive(Deserialize)]
struct OidcRoot {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<OidcProfile>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<OidcAuth>,
}

#[derive(Deserialize, Default)]
struct OidcProfile {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize, Default)]
struct OidcAuth {
    #[serde(default)]
    chatgpt_plan_type: Option<String>,
    #[serde(default)]
    chatgpt_user_id: Option<String>,
    /// Fallback when `chatgpt_user_id` isn't present (codex has this too).
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    chatgpt_account_is_fedramp: bool,
}

/// Decode the `exp` claim from any JWT (no signature verification). For
/// codex tokens, both `access_token` and `id_token` are JWTs; we use
/// this on `access_token` to determine when refresh is needed. Returns
/// `None` if the JWT has no `exp` claim. Matches codex's
/// `parse_jwt_expiration`.
pub fn parse_jwt_expiration(jwt: &str) -> Result<Option<Timestamp>, CodexOAuthError> {
    let mut parts = jwt.split('.');
    let payload_b64 = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => p,
        _ => return Err(CodexOAuthError::IdTokenShape),
    };
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)?;
    #[derive(Deserialize)]
    struct ExpClaim {
        #[serde(default)]
        exp: Option<i64>,
    }
    let claim: ExpClaim =
        serde_json::from_slice(&payload_bytes).map_err(CodexOAuthError::IdTokenJson)?;
    match claim.exp {
        None => Ok(None),
        Some(secs) => Timestamp::from_second(secs)
            .map(Some)
            .map_err(CodexOAuthError::ExpiresInOverflow),
    }
}

/// Decode and parse an id_token JWT. Signature is NOT verified — see
/// module-level docs. Returns the namespaced claims (plus the raw JWT,
/// stored verbatim so we can pass it through to codex's `.auth.json`).
pub fn parse_id_token(jwt: &str) -> Result<IdTokenClaims, CodexOAuthError> {
    let mut parts = jwt.split('.');
    let payload_b64 = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => p,
        _ => return Err(CodexOAuthError::IdTokenShape),
    };
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)?;
    let root: OidcRoot = serde_json::from_slice(&payload_bytes)
        .map_err(CodexOAuthError::IdTokenJson)?;

    let email = root
        .email
        .or_else(|| root.profile.and_then(|p| p.email));

    Ok(match root.auth {
        Some(a) => IdTokenClaims {
            email,
            chatgpt_plan_type: a.chatgpt_plan_type,
            chatgpt_user_id: a.chatgpt_user_id.or(a.user_id),
            chatgpt_account_id: a.chatgpt_account_id,
            chatgpt_account_is_fedramp: a.chatgpt_account_is_fedramp,
            raw_jwt: jwt.to_string(),
        },
        None => IdTokenClaims {
            email,
            raw_jwt: jwt.to_string(),
            ..Default::default()
        },
    })
}

// ---- Token-exchange wire shapes ----

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct OAuthErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // surfaced via raw body in the error message
    error_description: Option<String>,
}

// ---- Public result type ----

/// The artifact of a successful login or refresh. Used by the storage
/// layer to write `~/.codex/.auth.json` and by the chain to materialise
/// a `ProviderCredential`.
#[derive(Debug, Clone)]
pub struct CodexTokenSet {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    /// Raw id_token JWT (stored verbatim, matching codex's .auth.json shape).
    pub id_token: String,
    pub claims: IdTokenClaims,
    pub expires_at: Timestamp,
    /// Captured from the id_token's `chatgpt_account_id` claim; provided
    /// as a top-level field because the runtime header builder reaches
    /// for it directly.
    pub account_id: Option<String>,
}

// ---- Login flow selection ----

/// Which login flow to use. `Auto` tries loopback first, falling through
/// to device-code on bind failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoginFlow {
    Loopback,
    DeviceCode,
    Auto,
}

/// Handle produced by [`begin_login`]. Caller is expected to display the
/// URL or user code from the appropriate variant, then await
/// [`complete_login`].
pub enum CodexLoginHandle {
    Loopback(LoopbackHandle),
    DeviceCode(DeviceCodeHandle),
}

impl CodexLoginHandle {
    /// Authorize URL to open in the browser (loopback) — None for device-code.
    pub fn authorize_url(&self) -> Option<&str> {
        match self {
            Self::Loopback(h) => Some(&h.authorize_url),
            Self::DeviceCode(_) => None,
        }
    }

    /// User-code + verification URL pair (device-code) — None for loopback.
    pub fn user_code(&self) -> Option<(&str, &str)> {
        match self {
            Self::DeviceCode(h) => Some((&h.user_code, &h.verification_uri)),
            Self::Loopback(_) => None,
        }
    }
}

// ---- Loopback flow ----

/// Loopback PKCE state. Holds the live HTTP server (sync, on a dedicated
/// thread) plus the receiver the listener thread uses to deliver the
/// callback params.
pub struct LoopbackHandle {
    /// URL the user (or `open` crate) should visit.
    pub authorize_url: String,
    redirect_uri: String,
    state: String,
    verifier: SecretString,
    config: Arc<CodexOAuthConfig>,
    server: Arc<tiny_http::Server>,
    callback_rx: std::sync::mpsc::Receiver<LoopbackCallback>,
    listener_thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug)]
enum LoopbackCallback {
    Ok { code: String, state: String },
    Denied { reason: String },
}

impl Drop for LoopbackHandle {
    fn drop(&mut self) {
        // Ensure the listener thread shuts down even if complete_login was
        // never called or panicked.
        self.server.unblock();
        if let Some(jh) = self.listener_thread.take() {
            let _ = jh.join();
        }
    }
}

/// Bind on 1455, falling back to 1457. Returns the server + bound port.
///
/// On failure, surfaces the last underlying `io::Error` so callers can
/// classify (e.g. `AddrInUse` for fallback decisions). `tiny_http`'s
/// `Server::http` error type is `Box<dyn Error + Send + Sync>`, so we
/// downcast to `io::Error` where possible and synthesise one otherwise.
fn bind_loopback() -> Result<(tiny_http::Server, u16), CodexOAuthError> {
    let candidates = [LOOPBACK_PORT_PRIMARY, LOOPBACK_PORT_FALLBACK];
    let mut last_err: std::io::Error = std::io::Error::new(
        std::io::ErrorKind::Other,
        "no candidate ports tried",
    );
    for port in candidates {
        match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(server) => return Ok((server, port)),
            Err(boxed) => {
                last_err = match boxed.downcast::<std::io::Error>() {
                    Ok(io_err) => *io_err,
                    Err(other) => std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("port {port}: {other}"),
                    ),
                };
            }
        }
    }
    Err(CodexOAuthError::LoopbackBindFailed(last_err))
}

/// Begin a loopback PKCE flow. Returns a handle holding the live listener
/// + the authorize URL. The caller is expected to open the URL (e.g. via
/// the `open` crate) and then await [`complete_login`].
pub fn begin_loopback(config: CodexOAuthConfig) -> Result<LoopbackHandle, CodexOAuthError> {
    let config = Arc::new(config);
    let pkce = generate_pkce();
    let state = generate_state();
    let (server, bound_port) = bind_loopback()?;
    let redirect_uri = format!("http://localhost:{bound_port}/auth/callback");

    let scope = config.scopes.join(" ");
    let params = [
        ("client_id", config.client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri.as_str()),
        ("scope", scope.as_str()),
        ("state", state.as_str()),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
    ];
    let authorize_url = format!(
        "{}?{}",
        config.authorize_endpoint(),
        serde_urlencoded::to_string(params)
            .expect("static params should always url-encode")
    );

    let (tx, rx) = std::sync::mpsc::channel();
    let server = Arc::new(server);
    let server_for_thread = server.clone();
    let expected_state = state.clone();
    let listener_thread = std::thread::Builder::new()
        .name("codex-oauth-loopback".into())
        .spawn(move || {
            serve_loopback(&server_for_thread, &expected_state, &tx);
        })
        .map_err(CodexOAuthError::LoopbackThreadSpawn)?;

    Ok(LoopbackHandle {
        authorize_url,
        redirect_uri,
        state,
        verifier: pkce.verifier,
        config,
        server,
        callback_rx: rx,
        listener_thread: Some(listener_thread),
    })
}

/// Listener loop. Serves `/auth/callback`, `/success`, `/cancel`, and
/// 404s everything else. Calls `server.unblock()` after delivering the
/// first valid callback so the iterator exits.
fn serve_loopback(
    server: &tiny_http::Server,
    expected_state: &str,
    tx: &std::sync::mpsc::Sender<LoopbackCallback>,
) {
    for req in server.incoming_requests() {
        let url = req.url().to_string();
        // tiny_http gives us the raw path + query — parse with url::Url
        // against a dummy base so we can use its query_pairs API.
        let parsed = url::Url::parse(&format!("http://localhost{url}"))
            .ok()
            .map(|u| {
                let path = u.path().to_string();
                let mut code = None;
                let mut state = None;
                let mut error = None;
                let mut error_description = None;
                for (k, v) in u.query_pairs() {
                    match k.as_ref() {
                        "code" => code = Some(v.to_string()),
                        "state" => state = Some(v.to_string()),
                        "error" => error = Some(v.to_string()),
                        "error_description" => error_description = Some(v.to_string()),
                        _ => {}
                    }
                }
                (path, code, state, error, error_description)
            });

        match parsed {
            Some((path, Some(code), Some(state), _, _)) if path == "/auth/callback" => {
                if state != expected_state {
                    let _ = tx.send(LoopbackCallback::Denied {
                        reason: "state mismatch".into(),
                    });
                    let _ = req.respond(
                        tiny_http::Response::from_string(
                            "Authorization failed: state mismatch.\n",
                        )
                        .with_status_code(400),
                    );
                    server.unblock();
                    return;
                }
                let _ = tx.send(LoopbackCallback::Ok { code, state });
                let _ = req.respond(loopback_success_response());
                server.unblock();
                return;
            }
            Some((path, _, _, Some(error), error_description))
                if path == "/auth/callback" =>
            {
                let reason = error_description.unwrap_or(error);
                let _ = tx.send(LoopbackCallback::Denied {
                    reason: reason.clone(),
                });
                let _ = req.respond(
                    tiny_http::Response::from_string(format!(
                        "Authorization denied: {reason}\n"
                    ))
                    .with_status_code(400),
                );
                server.unblock();
                return;
            }
            Some((path, _, _, _, _)) if path == "/cancel" => {
                let _ = tx.send(LoopbackCallback::Denied {
                    reason: "cancelled by user".into(),
                });
                let _ = req.respond(tiny_http::Response::from_string("Cancelled.\n"));
                server.unblock();
                return;
            }
            _ => {
                let _ = req
                    .respond(tiny_http::Response::from_string("404\n").with_status_code(404));
            }
        }
    }
}

fn loopback_success_response() -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let body = "<!doctype html><html><head><title>Pattern — auth complete</title>\
        <meta charset=\"utf-8\"></head><body style=\"font-family: system-ui; max-width: 32em; \
        margin: 4em auto; line-height: 1.5;\"><h1>Authorization complete</h1>\
        <p>You can close this tab and return to your terminal.</p></body></html>";
    tiny_http::Response::from_string(body.to_string()).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("static header"),
    )
}

/// Complete a loopback flow. Awaits the callback (5-minute timeout), then
/// exchanges the auth code for tokens.
async fn complete_loopback(
    mut handle: LoopbackHandle,
    http: &reqwest::Client,
) -> Result<CodexTokenSet, CodexOAuthError> {
    // The mpsc receiver is sync; wrap recv_timeout in spawn_blocking so we
    // don't block the runtime. A JoinError here means the worker thread
    // panicked — surface as LoopbackThreadSpawn (its failure-mode sibling).
    let rx = std::mem::replace(&mut handle.callback_rx, std::sync::mpsc::channel().1);
    let callback = tokio::task::spawn_blocking(move || rx.recv_timeout(LOOPBACK_CALLBACK_TIMEOUT))
        .await
        .map_err(|e| {
            CodexOAuthError::LoopbackThreadSpawn(std::io::Error::other(format!(
                "spawn_blocking join error: {e}"
            )))
        })?;

    let callback = callback.map_err(|_| CodexOAuthError::LoopbackTimeout)?;

    match callback {
        LoopbackCallback::Denied { reason } => Err(CodexOAuthError::OAuthDenied(reason)),
        LoopbackCallback::Ok { code, state } => {
            if state != handle.state {
                return Err(CodexOAuthError::StateInvalid);
            }
            let response = exchange_authorization_code(
                http,
                &handle.config,
                &code,
                handle.verifier.expose_secret(),
                &handle.redirect_uri,
            )
            .await?;
            token_response_into_set(response)
        }
    }
}

// ---- Device-code flow ----

/// Device-code state. The poll target lives behind an `Arc<CodexOAuthConfig>`
/// so we can call `complete_device_code(&handle, &http)` without consuming.
pub struct DeviceCodeHandle {
    pub user_code: String,
    pub verification_uri: String,
    /// Some servers include a "click here, code pre-filled" URL.
    pub verification_uri_complete: Option<String>,
    /// Wall-clock deadline.
    pub expires_at: Instant,
    poll_interval: Duration,
    device_code: String,
    verifier: SecretString,
    config: Arc<CodexOAuthConfig>,
}

#[derive(Deserialize)]
struct DeviceCodeRequestResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    expires_in: Option<u64>,
}

/// Begin a device-code flow. Sends the initial usercode request and
/// returns a handle holding the user-facing code + URL.
pub async fn begin_device_code(
    config: CodexOAuthConfig,
    http: &reqwest::Client,
) -> Result<DeviceCodeHandle, CodexOAuthError> {
    let config = Arc::new(config);
    let pkce = generate_pkce();
    let scope = config.scopes.join(" ");
    let params = [
        ("client_id", config.client_id.as_str()),
        ("scope", scope.as_str()),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
    ];

    let response = http
        .post(config.device_code_request_endpoint())
        .form(&params)
        .send()
        .await?; // `?` auto-converts reqwest::Error via #[from].

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(CodexOAuthError::TokenExchangeFailed {
            status: status.as_u16(),
            body,
        });
    }
    let body = response.text().await?;
    let payload: DeviceCodeRequestResponse =
        serde_json::from_str(&body).map_err(CodexOAuthError::TokenExchangeMalformed)?;

    let interval = Duration::from_secs(payload.interval.unwrap_or(5));
    let lifetime = payload
        .expires_in
        .map(Duration::from_secs)
        .unwrap_or(DEVICE_CODE_MAX_LIFETIME);
    let expires_at = Instant::now() + lifetime;
    let verification_uri = if payload.verification_uri.is_empty() {
        config.device_verification_uri()
    } else {
        payload.verification_uri
    };

    Ok(DeviceCodeHandle {
        user_code: payload.user_code,
        verification_uri,
        verification_uri_complete: payload.verification_uri_complete,
        expires_at,
        poll_interval: interval,
        device_code: payload.device_code,
        verifier: pkce.verifier,
        config,
    })
}

/// Poll the device-code token endpoint until success, denial, or expiry.
/// Honours server-provided `slow_down` semantics by widening the interval.
async fn complete_device_code(
    handle: DeviceCodeHandle,
    http: &reqwest::Client,
) -> Result<CodexTokenSet, CodexOAuthError> {
    let mut interval = handle.poll_interval;

    loop {
        if Instant::now() >= handle.expires_at {
            return Err(CodexOAuthError::DeviceCodeExpired);
        }

        tokio::time::sleep(interval).await;

        let params = [
            ("client_id", handle.config.client_id.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", handle.device_code.as_str()),
            ("code_verifier", handle.verifier.expose_secret()),
        ];
        let response = http
            .post(handle.config.device_code_poll_endpoint())
            .form(&params)
            .send()
            .await?; // `?` auto-converts reqwest::Error.
        let status = response.status();
        let body = response.text().await?;

        if status.is_success() {
            let parsed: TokenResponse = serde_json::from_str(&body)
                .map_err(CodexOAuthError::TokenExchangeMalformed)?;
            return token_response_into_set(parsed);
        }

        // OAuth device-code error semantics live in the body.
        let err: OAuthErrorBody = serde_json::from_str(&body).unwrap_or(OAuthErrorBody {
            error: None,
            error_description: None,
        });
        match err.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval = interval.saturating_add(Duration::from_secs(5));
                continue;
            }
            Some("expired_token") => return Err(CodexOAuthError::DeviceCodeExpired),
            Some("access_denied") => {
                return Err(CodexOAuthError::DeviceCodeDenied(
                    "user denied authorization".into(),
                ));
            }
            _ => {
                return Err(CodexOAuthError::TokenExchangeFailed {
                    status: status.as_u16(),
                    body,
                });
            }
        }
    }
}

// ---- Token exchange ----

async fn exchange_authorization_code(
    http: &reqwest::Client,
    config: &CodexOAuthConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, CodexOAuthError> {
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", config.client_id.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let response = http
        .post(config.token_endpoint())
        .form(&params)
        .send()
        .await?; // reqwest::Error auto-converts via #[from].
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(CodexOAuthError::TokenExchangeFailed {
            status: status.as_u16(),
            body,
        });
    }
    serde_json::from_str(&body).map_err(CodexOAuthError::TokenExchangeMalformed)
}

/// Refresh an access token. Exposed here (rather than in the chain) so the
/// chain can use it without duplicating the request shape.
pub async fn refresh_token(
    config: &CodexOAuthConfig,
    http: &reqwest::Client,
    refresh_token: &SecretString,
) -> Result<CodexTokenSet, CodexOAuthError> {
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", config.client_id.as_str()),
        ("refresh_token", refresh_token.expose_secret()),
        ("scope", &config.scopes.join(" ")),
    ];
    let response = http
        .post(config.token_endpoint())
        .form(&params)
        .send()
        .await
        .map_err(|e| CodexOAuthError::RefreshFailed {
            kind: RefreshFailureKind::Transient,
            detail: e.to_string(),
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|e| CodexOAuthError::RefreshFailed {
        kind: RefreshFailureKind::Transient,
        detail: format!("response body read: {e}"),
    })?;
    if !status.is_success() {
        return Err(classify_refresh_failure(status.as_u16(), &body));
    }
    let parsed: TokenResponse =
        serde_json::from_str(&body).map_err(|e| CodexOAuthError::RefreshFailed {
            kind: RefreshFailureKind::Other,
            detail: format!("response parse: {e}"),
        })?;
    token_response_into_set(parsed)
}

fn classify_refresh_failure(status: u16, body: &str) -> CodexOAuthError {
    let parsed: OAuthErrorBody = serde_json::from_str(body).unwrap_or(OAuthErrorBody {
        error: None,
        error_description: None,
    });
    let kind = match parsed.error.as_deref() {
        Some("refresh_token_expired") => RefreshFailureKind::Expired,
        Some("refresh_token_reused") => RefreshFailureKind::Exhausted,
        Some("refresh_token_invalidated") => RefreshFailureKind::Revoked,
        _ if (500..600).contains(&status) => RefreshFailureKind::Transient,
        _ => RefreshFailureKind::Other,
    };
    CodexOAuthError::RefreshFailed {
        kind,
        detail: format!("HTTP {status}: {body}"),
    }
}

/// Common materialisation: TokenResponse → CodexTokenSet. Parses the
/// id_token claims, computes the absolute expiry timestamp.
fn token_response_into_set(resp: TokenResponse) -> Result<CodexTokenSet, CodexOAuthError> {
    let id_token = resp.id_token.ok_or(CodexOAuthError::IdTokenMissing)?;
    let claims = parse_id_token(&id_token)?;
    let refresh = resp
        .refresh_token
        .ok_or(CodexOAuthError::RefreshTokenMissing)?;
    let now = Timestamp::now();
    let expires_at = match resp.expires_in {
        Some(secs) => {
            let span = jiff::SignedDuration::from_secs(secs as i64);
            now.checked_add(span)
                .map_err(CodexOAuthError::ExpiresInOverflow)?
        }
        None => now,
    };
    Ok(CodexTokenSet {
        access_token: SecretString::from(resp.access_token),
        refresh_token: SecretString::from(refresh),
        account_id: claims.chatgpt_account_id.clone(),
        id_token,
        claims,
        expires_at,
    })
}

// ---- Top-level state machine ----

/// Start an OAuth login. Returns a handle whose specific variant depends
/// on which flow was selected (and which succeeded under `Auto`).
pub async fn begin_login(
    config: CodexOAuthConfig,
    flow: LoginFlow,
    http: &reqwest::Client,
) -> Result<CodexLoginHandle, CodexOAuthError> {
    match flow {
        LoginFlow::Loopback => Ok(CodexLoginHandle::Loopback(begin_loopback(config)?)),
        LoginFlow::DeviceCode => Ok(CodexLoginHandle::DeviceCode(
            begin_device_code(config, http).await?,
        )),
        LoginFlow::Auto => match begin_loopback(config.clone()) {
            Ok(h) => Ok(CodexLoginHandle::Loopback(h)),
            Err(CodexOAuthError::LoopbackBindFailed(source)) => {
                tracing::warn!(
                    error = %source,
                    kind = ?source.kind(),
                    "loopback bind failed; falling back to device-code"
                );
                Ok(CodexLoginHandle::DeviceCode(
                    begin_device_code(config, http).await?,
                ))
            }
            Err(e) => Err(e),
        },
    }
}

/// Drive the selected handle to completion.
pub async fn complete_login(
    handle: CodexLoginHandle,
    http: &reqwest::Client,
) -> Result<CodexTokenSet, CodexOAuthError> {
    match handle {
        CodexLoginHandle::Loopback(h) => complete_loopback(h, http).await,
        CodexLoginHandle::DeviceCode(h) => complete_device_code(h, http).await,
    }
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // PKCE primitives --------------------------------------------------------

    #[test]
    fn pkce_verifier_is_64_random_bytes_base64url_no_pad() {
        let pkce = generate_pkce();
        // 64 raw bytes → ceil(64*8/6) = 86 base64 chars without padding.
        // This catches accidental changes to PKCE_VERIFIER_BYTES and to the
        // base64 alphabet (e.g. switching from URL_SAFE_NO_PAD to STANDARD).
        assert_eq!(pkce.verifier.expose_secret().len(), 86);
        for c in pkce.verifier.expose_secret().chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "verifier contains non-base64url char {c}"
            );
        }
    }

    /// Pinned test vector from RFC 7636 §4.6 (the canonical PKCE example).
    /// Tests our SHA-256 + URL-safe-base64-no-pad pipeline against the
    /// spec, not against our own implementation.
    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected_challenge);
    }

    #[test]
    fn authorize_url_carries_required_params() {
        // begin_loopback binds a port; if 1455 and 1457 are both held we
        // skip rather than fail flakily. This exercises the URL builder
        // against the production config (codex client_id, default issuer).
        let handle = match begin_loopback(CodexOAuthConfig::codex()) {
            Ok(h) => h,
            Err(CodexOAuthError::LoopbackBindFailed(_)) => return,
            Err(e) => panic!("unexpected error: {e:?}"),
        };
        let url = handle.authorize_url.clone();
        // Drop the handle so the listener thread shuts down.
        drop(handle);

        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains(&format!("client_id={CODEX_CLIENT_ID}")));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("state="));
        assert!(url.contains("id_token_add_organizations=true"));
        // Scopes are space-joined then URL-encoded; check for the
        // load-bearing ones rather than the entire string.
        assert!(url.contains("openid"));
        assert!(url.contains("offline_access"));
        assert!(url.contains("api.connectors.invoke"));
    }

    // id_token parsing -------------------------------------------------------

    /// Build a synthetic JWT with the given payload. Header and signature
    /// are dummy values — we don't verify, codex doesn't verify, the test
    /// only exercises payload decoding.
    fn synth_jwt(payload: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload_bytes);
        let sig = URL_SAFE_NO_PAD.encode(b"sig");
        format!("{header}.{payload_b64}.{sig}")
    }

    #[test]
    fn parse_id_token_extracts_namespaced_claims() {
        let jwt = synth_jwt(json!({
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_user_id": "user_abc",
                "chatgpt_account_id": "acct_xyz",
                "chatgpt_account_is_fedramp": false
            }
        }));
        let claims = parse_id_token(&jwt).expect("parse ok");
        assert_eq!(claims.email.as_deref(), Some("user@example.com"));
        assert_eq!(claims.chatgpt_plan_type.as_deref(), Some("pro"));
        assert_eq!(claims.chatgpt_user_id.as_deref(), Some("user_abc"));
        assert_eq!(claims.chatgpt_account_id.as_deref(), Some("acct_xyz"));
        assert!(!claims.chatgpt_account_is_fedramp);
        assert_eq!(claims.raw_jwt, jwt);
    }

    #[test]
    fn parse_id_token_falls_back_user_id_to_chatgpt_user_id() {
        let jwt = synth_jwt(json!({
            "https://api.openai.com/auth": { "user_id": "legacy_id" }
        }));
        let claims = parse_id_token(&jwt).expect("parse ok");
        assert_eq!(claims.chatgpt_user_id.as_deref(), Some("legacy_id"));
    }

    #[test]
    fn parse_id_token_handles_missing_auth_namespace() {
        let jwt = synth_jwt(json!({"email": "u@example.com"}));
        let claims = parse_id_token(&jwt).expect("parse ok");
        assert_eq!(claims.email.as_deref(), Some("u@example.com"));
        assert!(claims.chatgpt_account_id.is_none());
    }

    #[test]
    fn parse_id_token_rejects_malformed_jwt() {
        assert!(matches!(
            parse_id_token("not-a-jwt"),
            Err(CodexOAuthError::IdTokenShape)
        ));
        assert!(matches!(
            parse_id_token("only.two"),
            Err(CodexOAuthError::IdTokenShape)
        ));
        // base64 decode failure → IdTokenBase64.
        assert!(matches!(
            parse_id_token("header.!!!not-base64!!!.sig"),
            Err(CodexOAuthError::IdTokenBase64(_))
        ));
        // valid base64 but invalid JSON → IdTokenJson.
        let bad_payload = URL_SAFE_NO_PAD.encode(b"not json");
        let jwt = format!("aGVhZGVy.{bad_payload}.c2ln");
        assert!(matches!(
            parse_id_token(&jwt),
            Err(CodexOAuthError::IdTokenJson(_))
        ));
    }

    #[test]
    fn parse_id_token_profile_email_fallback() {
        let jwt = synth_jwt(json!({
            "https://api.openai.com/profile": { "email": "p@example.com" }
        }));
        let claims = parse_id_token(&jwt).expect("parse ok");
        assert_eq!(claims.email.as_deref(), Some("p@example.com"));
    }

    /// Pinned against the actual id_token shape returned by
    /// `auth.openai.com` (verified 2026-05-26 against a real codex login).
    /// Values are synthesized but the structure — including the
    /// additional claims we deliberately don't model (subscription
    /// timestamps, groups, organizations, localhost flag) — matches
    /// production. Tolerance of unknown fields is load-bearing here:
    /// if OpenAI adds new claims, our parser keeps working as long as
    /// the ones we care about stay put.
    #[test]
    fn parse_id_token_matches_real_codex_shape() {
        let jwt = synth_jwt(json!({
            "at_hash": "fake_at_hash_v",
            "aud": ["app_EMoamEEZ73f0CkXaXp7hrann"],
            "auth_provider": "passwordless",
            "auth_time": 1_700_000_000_u64,
            "email": "user@example.test",
            "email_verified": true,
            "exp": 1_700_003_600_u64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "00000000-aaaa-bbbb-cccc-000000000000",
                "chatgpt_plan_type": "plus",
                "chatgpt_subscription_active_start": "2026-01-01T00:00:00+00:00",
                "chatgpt_subscription_active_until": "2026-12-31T00:00:00+00:00",
                "chatgpt_subscription_last_checked": "2026-05-26T00:00:00+00:00",
                "chatgpt_user_id": "user-FAKEUSERID",
                "groups": [],
                "localhost": true,
                "organizations": [
                    {
                        "id": "org-FAKEORG",
                        "is_default": true,
                        "role": "owner",
                        "title": "Personal",
                    }
                ],
                "user_id": "user-FAKEUSERID",
            },
            "iat": 1_700_000_000_u64,
            "iss": "https://auth.openai.com",
            "jti": "fake-jti-uuid",
            "name": "Fake User",
            "rat": 1_700_000_000_u64,
            "sid": "fake-sid",
            "sub": "auth0|FAKESUB",
        }));
        let claims = parse_id_token(&jwt).expect("parse ok");
        assert_eq!(claims.email.as_deref(), Some("user@example.test"));
        assert_eq!(
            claims.chatgpt_account_id.as_deref(),
            Some("00000000-aaaa-bbbb-cccc-000000000000")
        );
        assert_eq!(claims.chatgpt_plan_type.as_deref(), Some("plus"));
        assert_eq!(claims.chatgpt_user_id.as_deref(), Some("user-FAKEUSERID"));
        assert!(!claims.chatgpt_account_is_fedramp);
        assert_eq!(claims.raw_jwt, jwt);
    }

    // Token exchange ---------------------------------------------------------

    fn test_config(issuer: String) -> CodexOAuthConfig {
        CodexOAuthConfig {
            client_id: "test-client".into(),
            issuer,
            scopes: vec!["openid".into(), "offline_access".into()],
        }
    }

    #[tokio::test]
    async fn exchange_authorization_code_success_round_trip() {
        let server = MockServer::start().await;
        let id_token = synth_jwt(json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_test" }
        }));
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=test-code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at",
                "refresh_token": "rt",
                "id_token": id_token,
                "expires_in": 1800
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let resp = exchange_authorization_code(
            &http,
            &config,
            "test-code",
            "verifier-xx",
            "http://localhost:1455/auth/callback",
        )
        .await
        .expect("exchange ok");
        let set = token_response_into_set(resp).expect("materialise ok");

        assert_eq!(set.access_token.expose_secret(), "at");
        assert_eq!(set.refresh_token.expose_secret(), "rt");
        assert_eq!(set.account_id.as_deref(), Some("acct_test"));
        assert!(set.expires_at > Timestamp::now());
    }

    #[tokio::test]
    async fn exchange_authorization_code_surfaces_400_as_token_exchange_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant"
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let err = exchange_authorization_code(
            &http,
            &config,
            "bad",
            "verifier",
            "http://localhost:1455/auth/callback",
        )
        .await
        .expect_err("400 should surface");
        assert!(matches!(
            err,
            CodexOAuthError::TokenExchangeFailed { status: 400, .. }
        ));
    }

    #[tokio::test]
    async fn refresh_request_includes_scope_and_classifies_expired() {
        let server = MockServer::start().await;
        // Body match verifies our refresh request sends the scope param
        // (the chain depends on this for the OpenID `offline_access`
        // scope to keep working across refreshes).
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=rt-old"))
            .and(body_string_contains("scope=openid"))
            .and(body_string_contains("offline_access"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": "refresh_token_expired"
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let err = refresh_token(&config, &http, &SecretString::from("rt-old".to_string()))
            .await
            .expect_err("expired should surface");
        match err {
            CodexOAuthError::RefreshFailed {
                kind: RefreshFailureKind::Expired,
                ..
            } => {}
            other => panic!("expected Expired refresh failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_classifies_refresh_token_reused_as_exhausted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": "refresh_token_reused"
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let err = refresh_token(&config, &http, &SecretString::from("rt-old".to_string()))
            .await
            .expect_err("reused should surface");
        assert!(matches!(
            err,
            CodexOAuthError::RefreshFailed {
                kind: RefreshFailureKind::Exhausted,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn refresh_5xx_classifies_as_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let err = refresh_token(&config, &http, &SecretString::from("rt".to_string()))
            .await
            .expect_err("5xx should surface");
        assert!(matches!(
            err,
            CodexOAuthError::RefreshFailed {
                kind: RefreshFailureKind::Transient,
                ..
            }
        ));
    }

    // Device-code ------------------------------------------------------------

    #[tokio::test]
    async fn device_code_polls_until_authorized() {
        let server = MockServer::start().await;
        let id_token = synth_jwt(json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_dc" }
        }));

        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc-1",
                "user_code": "ABCD-EFGH",
                "verification_uri": format!("{}/codex/device", server.uri()),
                "interval": 1_u64,
                "expires_in": 120_u64
            })))
            .mount(&server)
            .await;

        // First poll: still pending. Second poll: success.
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "authorization_pending"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-dc",
                "refresh_token": "rt-dc",
                "id_token": id_token,
                "expires_in": 1800
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let handle = begin_device_code(config, &http).await.expect("begin dc ok");
        assert_eq!(handle.user_code, "ABCD-EFGH");

        let set = complete_device_code(handle, &http).await.expect("complete dc ok");
        assert_eq!(set.access_token.expose_secret(), "at-dc");
        assert_eq!(set.account_id.as_deref(), Some("acct_dc"));
    }

    #[tokio::test]
    async fn refresh_token_response_rotation_persists_new_refresh() {
        // Server returns a new refresh_token; the materialised set must
        // contain the new value, not the original.
        let server = MockServer::start().await;
        let id_token = synth_jwt(json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_rot" }
        }));
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-new",
                "refresh_token": "rt-NEW-rotated",
                "id_token": id_token,
                "expires_in": 1800
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let set = refresh_token(&config, &http, &SecretString::from("rt-old".to_string()))
            .await
            .expect("refresh ok");
        assert_eq!(set.refresh_token.expose_secret(), "rt-NEW-rotated");
        assert_eq!(set.access_token.expose_secret(), "at-new");
    }

    #[tokio::test]
    async fn device_code_surfaces_expired_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc-1",
                "user_code": "ABCD",
                "verification_uri": "https://example.invalid/codex/device",
                "interval": 1_u64,
                "expires_in": 120_u64
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "expired_token"
            })))
            .mount(&server)
            .await;

        let config = test_config(server.uri());
        let http = reqwest::Client::new();
        let handle = begin_device_code(config, &http).await.expect("begin ok");
        let err = complete_device_code(handle, &http)
            .await
            .expect_err("expired");
        assert!(matches!(err, CodexOAuthError::DeviceCodeExpired));
    }

    // Loopback ---------------------------------------------------------------
    //
    // Tests bind an OS-assigned port (`:0`) and drive the listener via a
    // real HTTP GET — same shape a browser would issue after the OAuth
    // server redirects.

    #[tokio::test]
    async fn loopback_listener_accepts_valid_callback() {
        let server =
            Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind os-assigned"));
        let bound = server.server_addr().to_ip().expect("ip addr").port();
        let expected_state = "the-state".to_string();
        let (tx, rx) = std::sync::mpsc::channel();

        let server_clone = server.clone();
        let expected_for_thread = expected_state.clone();
        let listener = std::thread::spawn(move || {
            serve_loopback(&server_clone, &expected_for_thread, &tx);
        });

        let url = format!(
            "http://127.0.0.1:{bound}/auth/callback?code=good-code&state={expected_state}"
        );
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("callback request");
        assert!(resp.status().is_success());

        let callback = rx.recv().expect("callback delivered");
        match callback {
            LoopbackCallback::Ok { code, state } => {
                assert_eq!(code, "good-code");
                assert_eq!(state, expected_state);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        listener.join().expect("listener cleanly joined");
    }

    #[tokio::test]
    async fn loopback_listener_rejects_state_mismatch() {
        let server =
            Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind os-assigned"));
        let bound = server.server_addr().to_ip().expect("ip addr").port();
        let (tx, rx) = std::sync::mpsc::channel();
        let server_clone = server.clone();
        let listener = std::thread::spawn(move || {
            serve_loopback(&server_clone, "expected-state", &tx);
        });

        let url = format!("http://127.0.0.1:{bound}/auth/callback?code=c&state=WRONG");
        let _ = reqwest::Client::new().get(&url).send().await.expect("hit");

        let callback = rx.recv().expect("delivered");
        assert!(matches!(callback, LoopbackCallback::Denied { .. }));
        listener.join().expect("joined");
    }

    #[tokio::test]
    async fn loopback_listener_handles_oauth_error_callback() {
        let server =
            Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind os-assigned"));
        let bound = server.server_addr().to_ip().expect("ip addr").port();
        let (tx, rx) = std::sync::mpsc::channel();
        let server_clone = server.clone();
        let listener = std::thread::spawn(move || {
            serve_loopback(&server_clone, "any", &tx);
        });

        let url = format!(
            "http://127.0.0.1:{bound}/auth/callback?error=access_denied\
             &error_description=user%20canceled"
        );
        let _ = reqwest::Client::new().get(&url).send().await.expect("hit");

        let callback = rx.recv().expect("delivered");
        match callback {
            LoopbackCallback::Denied { reason } => assert!(reason.contains("canceled")),
            _ => panic!("expected Denied"),
        }
        listener.join().expect("joined");
    }
}
