//! Session-pickup auth tier — read the Anthropic-ecosystem credentials file.
//!
//! Canonical path: `~/.claude/.credentials.json` (as per claude-code source
//! and our research in `docs/reference/oauth-and-detection.md`).
//! Legacy compat path: `~/.claude/session.json` (checked only if the primary
//! path is missing — older claude-code builds used this name).
//!
//! Pattern **never** writes either file — this is a read-only tier. The
//! refresh lifecycle belongs to claude-code itself; pattern just picks up
//! whatever's currently valid.
//!
//! # Acceptance criteria covered
//!
//! - **AC3.1** happy path → a valid credentials file yields
//!   `Ok(Some(token))`.
//! - **AC3.2** concurrent-write safety → `tokio::fs::read_to_string` issues
//!   a single `read()` syscall for small files (typical .credentials.json is
//!   a few KB). Claude-code writes the file atomically (rename-in-place
//!   pattern), so we see either the pre-write content or the post-write
//!   content — never a torn half. If the kernel hands us a partially-synced
//!   payload, JSON parse fails and we skip this tier (the caller retries
//!   on the next request, by which point the write will have completed).
//! - **AC3.3** missing file → `Ok(None)`, tier skip, no error.
//! - **AC3.4** malformed JSON → `Ok(None)` with a `tracing::warn!`, tier
//!   skip.
//! - **AC3.5** expired token in file → `Ok(None)`, tier skip.
//! - **AC3.6** absence of pattern's own keyring entry does NOT affect this
//!   tier. Session-pickup reads the claude-code file directly and never
//!   consults `creds_store` (which is pattern's keyring-or-JSON store for
//!   pattern-managed credentials).
//!
//! Gated behind the `subscription-oauth` feature. Without that feature
//! the whole module is absent and the Anthropic tier chain collapses to
//! API-key only.
#![cfg(feature = "subscription-oauth")]

use std::path::PathBuf;

use pattern_core::error::ProviderError;
use pattern_core::types::provider::ProviderCredential;
use secrecy::SecretString;
use serde::Deserialize;

/// Read-only auth tier that picks up claude-code's session credentials.
pub struct SessionPickupTier {
    /// Ordered list of candidate paths. Primary first, legacy fallbacks
    /// after. `pick_up` tries each in order and returns on the first valid
    /// unexpired token.
    paths: Vec<PathBuf>,
}

impl Default for SessionPickupTier {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            paths: vec![
                home.join(".claude").join(".credentials.json"),
                // legacy compat — older claude-code builds used this name
                home.join(".claude").join("session.json"),
            ],
        }
    }
}

impl SessionPickupTier {
    /// Construct a tier that reads only the given candidate paths, in order.
    /// Primarily for tests; production uses [`Self::default`].
    pub fn with_paths(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    /// Attempt to read a valid ambient credentials session.
    ///
    /// - `Ok(Some(token))` — a valid unexpired credential was found at one
    ///   of the candidate paths.
    /// - `Ok(None)` — no valid credential in any candidate path. Covers
    ///   missing file, malformed JSON, and expired token cases (AC3.3/3.4/3.5).
    /// - `Err(ProviderError::CredentialStorage)` — a non-NotFound I/O
    ///   error (permission denied, etc.). The caller should not silently
    ///   fall through on these; something is actively wrong with the
    ///   filesystem.
    pub async fn pick_up(&self) -> Result<Option<ProviderCredential>, ProviderError> {
        for path in &self.paths {
            match tokio::fs::read_to_string(path).await {
                Ok(json) => match serde_json::from_str::<CredentialsFile>(&json) {
                    Ok(file) => match file.claude_credentials() {
                        Some(creds) => {
                            if let Some(token) = Self::to_pattern_token(creds) {
                                tracing::debug!(?path, "session-pickup: valid credential found");
                                return Ok(Some(token));
                            }
                            tracing::debug!(
                                ?path,
                                "session-pickup: file present but token expired or unusable; skipping"
                            );
                        }
                        None => {
                            tracing::debug!(
                                ?path,
                                "session-pickup: file parsed but no Anthropic credential block; skipping"
                            );
                        }
                    },
                    Err(e) => {
                        // AC3.4: malformed JSON warns and falls through.
                        tracing::warn!(?path, error = %e, "session-pickup: malformed JSON; skipping");
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // AC3.3: missing file is the normal skip case.
                    tracing::trace!(?path, "session-pickup: path missing; continuing");
                }
                Err(e) => {
                    tracing::warn!(?path, error = %e, "session-pickup: io error");
                    return Err(ProviderError::CredentialStorage {
                        reason: format!("session-pickup io error on {path:?}: {e}"),
                    });
                }
            }
        }
        Ok(None)
    }

    fn to_pattern_token(creds: ClaudeCredentials) -> Option<ProviderCredential> {
        // AC3.5: expired → skip.
        let now_ms = jiff::Timestamp::now().as_millisecond();
        if let Some(exp) = creds.expires_at
            && exp <= now_ms
        {
            return None;
        }

        // Access token is required; without it the entry is unusable.
        if creds.access_token.is_empty() {
            return None;
        }

        let now = jiff::Timestamp::now();
        Some(ProviderCredential {
            provider: "anthropic".into(),
            access_token: SecretString::from(creds.access_token),
            refresh_token: creds.refresh_token.map(SecretString::from),
            expires_at: creds
                .expires_at
                .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok()),
            scope: creds.scopes.map(|v| v.join(" ")),
            // claude-code's credentials file doesn't expose a session ID;
            // pattern synthesises its own per-persona UUID in session_uuid.rs.
            session_id: None,
            created_at: now,
            updated_at: now,
        })
    }
}

/// Wire shape of claude-code's `~/.claude/.credentials.json`.
///
/// The canonical layout (verified against a real file on 2026-04-17) is:
///
/// ```json
/// {
///   "claudeAiOauth": {
///     "accessToken": "sk-ant-oat01-...",
///     "refreshToken": "sk-ant-ort01-...",
///     "expiresAt": 1776485530581,
///     "scopes": ["user:inference", ...],
///     "subscriptionType": "max",
///     "rateLimitTier": "default_claude_max_20x"
///   },
///   "mcpOAuth": { /* per-server MCP OAuth state, ignored here */ }
/// }
/// ```
///
/// Some legacy `~/.claude/session.json` files may store the Anthropic
/// credential block at the top level without the `claudeAiOauth` wrapper;
/// `claude_credentials()` accepts both forms.
#[derive(Deserialize)]
struct CredentialsFile {
    /// Canonical shape: claude-code's current `.credentials.json`.
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeCredentials>,

    /// Legacy / fallback shape: the credential block at the top level
    /// (older claude-code installs, or proxies that write a flatter file).
    /// `#[serde(flatten)]` requires field-level accessors, so we instead
    /// capture the flat variant via `#[serde(default)]` + manual field
    /// listing on a sibling struct, unified by `claude_credentials()`.
    #[serde(flatten)]
    flat: Option<ClaudeCredentials>,
}

impl CredentialsFile {
    fn claude_credentials(self) -> Option<ClaudeCredentials> {
        // Prefer the canonical wrapped form; fall back to flat only if
        // the wrapped block is absent.
        self.claude_ai_oauth.or(self.flat)
    }
}

/// Credential fields shared by the wrapped and flat file layouts. Fields
/// irrelevant to pattern (subscriptionType, rateLimitTier, profile, etc.)
/// are simply ignored during deserialization.
#[derive(Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,

    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,

    /// Unix epoch milliseconds.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,

    #[serde(rename = "scopes")]
    scopes: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use tempfile::tempdir;

    fn write_creds(dir: &std::path::Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write credentials fixture");
        path
    }

    fn valid_creds_json(expires_at_ms: Option<i64>) -> String {
        // Legacy flat shape — some older `session.json` files wrote the
        // credential block at the top level without the `claudeAiOauth`
        // wrapper. The pickup tier accepts this via the `flat` fallback.
        let expiry = expires_at_ms
            .map(|ms| format!("\"expiresAt\": {ms},"))
            .unwrap_or_default();
        format!(
            r#"{{
                "accessToken": "at-subscription-test",
                "refreshToken": "rt-subscription-test",
                {expiry}
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "max",
                "rateLimitTier": "high"
            }}"#
        )
    }

    /// Canonical `~/.claude/.credentials.json` shape — the `claudeAiOauth`
    /// wrapper plus a sibling `mcpOAuth` object that the pickup tier
    /// must ignore. Verified against a real on-disk file on 2026-04-17.
    fn canonical_creds_json(expires_at_ms: Option<i64>) -> String {
        let expiry = expires_at_ms
            .map(|ms| format!("\"expiresAt\": {ms},"))
            .unwrap_or_default();
        format!(
            r#"{{
                "claudeAiOauth": {{
                    "accessToken": "sk-ant-oat01-real-shape",
                    "refreshToken": "sk-ant-ort01-real-shape",
                    {expiry}
                    "scopes": ["user:file_upload", "user:inference", "user:mcp_servers", "user:profile", "user:sessions:claude_code"],
                    "subscriptionType": "max",
                    "rateLimitTier": "default_claude_max_20x"
                }},
                "mcpOAuth": {{
                    "plugin:some:server|abc123": {{
                        "serverName": "plugin:some:server",
                        "serverUrl": "https://example.invalid/mcp",
                        "accessToken": "",
                        "expiresAt": 0
                    }}
                }}
            }}"#
        )
    }

    #[tokio::test]
    async fn valid_unexpired_credential_is_picked_up() {
        // AC3.1: happy path.
        let dir = tempdir().expect("tempdir");
        let future_ms = jiff::Timestamp::now().as_millisecond() + 3_600_000; // +1h
        let path = write_creds(
            dir.path(),
            ".credentials.json",
            &valid_creds_json(Some(future_ms)),
        );

        let tier = SessionPickupTier::with_paths(vec![path]);
        let token = tier
            .pick_up()
            .await
            .expect("pick_up ok")
            .expect("token present");

        assert_eq!(token.provider, "anthropic");
        assert_eq!(token.access_token.expose_secret(), "at-subscription-test");
        assert_eq!(
            token.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("rt-subscription-test")
        );
        assert_eq!(token.scope.as_deref(), Some("user:inference user:profile"));
        assert!(token.expires_at.is_some());
    }

    #[tokio::test]
    async fn missing_file_skips_tier_without_error() {
        // AC3.3.
        let tier = SessionPickupTier::with_paths(vec!["/this/path/does/not/exist.json".into()]);
        let result = tier.pick_up().await.expect("missing file is not an error");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn malformed_json_skips_tier_without_error() {
        // AC3.4.
        let dir = tempdir().expect("tempdir");
        let path = write_creds(dir.path(), ".credentials.json", "{not valid json");

        let tier = SessionPickupTier::with_paths(vec![path]);
        let result = tier
            .pick_up()
            .await
            .expect("malformed json is skipped, not errored");
        assert!(result.is_none(), "malformed → None");
    }

    #[tokio::test]
    async fn expired_token_is_skipped() {
        // AC3.5.
        let dir = tempdir().expect("tempdir");
        let past_ms = jiff::Timestamp::now().as_millisecond() - 3_600_000; // 1h ago
        let path = write_creds(
            dir.path(),
            ".credentials.json",
            &valid_creds_json(Some(past_ms)),
        );

        let tier = SessionPickupTier::with_paths(vec![path]);
        let result = tier.pick_up().await.expect("expired → None");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn empty_access_token_is_skipped() {
        // Defence-in-depth: file technically present and parseable but
        // access_token is the empty string → skip rather than return a
        // bogus Bearer.
        let dir = tempdir().expect("tempdir");
        let path = write_creds(
            dir.path(),
            ".credentials.json",
            r#"{"accessToken": "", "scopes": []}"#,
        );

        let tier = SessionPickupTier::with_paths(vec![path]);
        let result = tier.pick_up().await.expect("empty token → None");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn primary_path_takes_precedence_over_legacy() {
        let dir = tempdir().expect("tempdir");
        let future_ms = jiff::Timestamp::now().as_millisecond() + 3_600_000;

        let primary = write_creds(
            dir.path(),
            ".credentials.json",
            &valid_creds_json(Some(future_ms)).replace("at-subscription-test", "primary-wins"),
        );
        let legacy = write_creds(
            dir.path(),
            "session.json",
            &valid_creds_json(Some(future_ms)).replace("at-subscription-test", "legacy-loses"),
        );

        let tier = SessionPickupTier::with_paths(vec![primary, legacy]);
        let token = tier
            .pick_up()
            .await
            .expect("pick_up ok")
            .expect("token present");
        assert_eq!(token.access_token.expose_secret(), "primary-wins");
    }

    #[tokio::test]
    async fn legacy_path_used_when_primary_missing() {
        let dir = tempdir().expect("tempdir");
        let future_ms = jiff::Timestamp::now().as_millisecond() + 3_600_000;

        let primary = dir.path().join(".credentials.json");
        // Deliberately do NOT create primary.
        let legacy = write_creds(
            dir.path(),
            "session.json",
            &valid_creds_json(Some(future_ms)).replace("at-subscription-test", "legacy-found"),
        );

        let tier = SessionPickupTier::with_paths(vec![primary, legacy]);
        let token = tier
            .pick_up()
            .await
            .expect("pick_up ok")
            .expect("token present");
        assert_eq!(token.access_token.expose_secret(), "legacy-found");
    }

    #[tokio::test]
    async fn no_expiry_field_is_treated_as_valid() {
        // Some credential formats omit expiresAt (static long-lived tokens).
        // Absent expiry = treat as never-expired from this tier's POV; the
        // gateway's refresh layer handles actual expiry detection on 401.
        let dir = tempdir().expect("tempdir");
        let path = write_creds(dir.path(), ".credentials.json", &valid_creds_json(None));

        let tier = SessionPickupTier::with_paths(vec![path]);
        let token = tier
            .pick_up()
            .await
            .expect("pick_up ok")
            .expect("no-expiry = valid");
        assert!(token.expires_at.is_none());
    }

    /// Canonical `.credentials.json` shape: `claudeAiOauth` wrapper +
    /// sibling `mcpOAuth` object. The pickup tier must reach into the
    /// wrapper and ignore the MCP sibling. Schema verified against a
    /// real on-disk file on 2026-04-17.
    #[tokio::test]
    async fn canonical_wrapped_shape_is_picked_up() {
        let dir = tempdir().expect("tempdir");
        let future_ms = jiff::Timestamp::now().as_millisecond() + 3_600_000;
        let path = write_creds(
            dir.path(),
            ".credentials.json",
            &canonical_creds_json(Some(future_ms)),
        );

        let tier = SessionPickupTier::with_paths(vec![path]);
        let token = tier
            .pick_up()
            .await
            .expect("pick_up ok")
            .expect("canonical shape → token present");

        assert_eq!(token.provider, "anthropic");
        assert_eq!(token.access_token.expose_secret(), "sk-ant-oat01-real-shape");
        assert_eq!(
            token.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("sk-ant-ort01-real-shape")
        );
        assert!(token.expires_at.is_some());
        let scope = token.scope.expect("scope populated");
        assert!(scope.contains("user:inference"));
        assert!(scope.contains("user:sessions:claude_code"));
    }

    /// A file with ONLY the `mcpOAuth` sibling (no `claudeAiOauth`,
    /// no flat fallback fields) must not mistakenly resolve anything.
    #[tokio::test]
    async fn mcp_only_file_yields_no_credential() {
        let dir = tempdir().expect("tempdir");
        let path = write_creds(
            dir.path(),
            ".credentials.json",
            r#"{"mcpOAuth": {"plugin:foo|abc": {"serverName":"plugin:foo","serverUrl":"https://example.invalid","accessToken":"","expiresAt":0}}}"#,
        );

        let tier = SessionPickupTier::with_paths(vec![path]);
        let result = tier.pick_up().await.expect("pick_up ok");
        assert!(
            result.is_none(),
            "file with only mcpOAuth must not yield a credential"
        );
    }
}
