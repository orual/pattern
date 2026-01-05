//! Database model types for ATProto OAuth storage.
//!
//! These types represent database rows and provide conversions to/from Jacquard types.
//! Using explicit model types allows for compile-time query verification with sqlx macros.

use jacquard::CowStr;
use jacquard::IntoStatic;
use jacquard::oauth::scopes::Scope;
use jacquard::oauth::session::{AuthRequestData, ClientSessionData, DpopClientData, DpopReqData};
use jacquard::oauth::types::{OAuthTokenType, TokenSet};
use jacquard::types::did::Did;
use jacquard::types::string::Datetime;
use jose_jwk::Key;

use crate::error::AuthError;

/// Database row for oauth_sessions table.
///
/// All fields are stored as primitive types suitable for SQLite.
/// JSON fields (scopes, dpop_key) are stored as TEXT.
#[derive(Debug, sqlx::FromRow)]
pub struct OAuthSessionRow {
    pub account_did: String,
    pub session_id: String,
    pub host_url: String,
    pub authserver_url: String,
    pub authserver_token_endpoint: String,
    pub authserver_revocation_endpoint: Option<String>,
    pub scopes: String,
    pub dpop_key: String,
    pub dpop_authserver_nonce: String,
    pub dpop_host_nonce: String,
    pub token_iss: String,
    pub token_sub: String,
    pub token_aud: String,
    pub token_scope: Option<String>,
    pub refresh_token: Option<String>,
    pub access_token: String,
    pub token_type: String,
    pub expires_at: Option<i64>,
}

impl OAuthSessionRow {
    /// Convert database row to Jacquard's ClientSessionData.
    ///
    /// This performs JSON deserialization of dpop_key and scopes,
    /// and parses DIDs and token types.
    pub fn to_client_session_data(&self) -> Result<ClientSessionData<'static>, AuthError> {
        // Parse the DPoP key from JSON
        let dpop_key: Key = serde_json::from_str(&self.dpop_key)?;

        // Parse scopes from JSON array
        let scope_strings: Vec<String> = serde_json::from_str(&self.scopes)?;
        let scopes: Vec<Scope<'static>> = scope_strings
            .iter()
            .filter_map(|s| Scope::parse(s).ok().map(|scope| scope.into_static()))
            .collect();

        // Parse token type - expects "DPoP" or "Bearer"
        // Default to DPoP for ATProto if parsing fails
        let token_type: OAuthTokenType = serde_json::from_str(&format!("\"{}\"", self.token_type))
            .unwrap_or(OAuthTokenType::DPoP);

        // Convert expires_at from unix timestamp to Datetime
        let expires_at = self.expires_at.and_then(|ts| {
            chrono::DateTime::from_timestamp(ts, 0).map(|dt| Datetime::new(dt.fixed_offset()))
        });

        // Parse DIDs
        let account_did = Did::new(&self.account_did)
            .map_err(|e| AuthError::InvalidDid(e.to_string()))?
            .into_static();
        let token_sub = Did::new(&self.token_sub)
            .map_err(|e| AuthError::InvalidDid(e.to_string()))?
            .into_static();

        Ok(ClientSessionData {
            account_did,
            session_id: CowStr::from(self.session_id.clone()),
            host_url: CowStr::from(self.host_url.clone()),
            authserver_url: CowStr::from(self.authserver_url.clone()),
            authserver_token_endpoint: CowStr::from(self.authserver_token_endpoint.clone()),
            authserver_revocation_endpoint: self
                .authserver_revocation_endpoint
                .clone()
                .map(CowStr::from),
            scopes,
            dpop_data: DpopClientData {
                dpop_key,
                dpop_authserver_nonce: CowStr::from(self.dpop_authserver_nonce.clone()),
                dpop_host_nonce: CowStr::from(self.dpop_host_nonce.clone()),
            },
            token_set: TokenSet {
                iss: CowStr::from(self.token_iss.clone()),
                sub: token_sub,
                aud: CowStr::from(self.token_aud.clone()),
                scope: self.token_scope.clone().map(CowStr::from),
                refresh_token: self.refresh_token.clone().map(CowStr::from),
                access_token: CowStr::from(self.access_token.clone()),
                token_type,
                expires_at,
            },
        })
    }
}

/// Parameters for inserting/updating an OAuth session.
///
/// This struct holds pre-serialized values ready for database insertion.
#[derive(Debug)]
pub struct OAuthSessionParams {
    pub account_did: String,
    pub session_id: String,
    pub host_url: String,
    pub authserver_url: String,
    pub authserver_token_endpoint: String,
    pub authserver_revocation_endpoint: Option<String>,
    pub scopes_json: String,
    pub dpop_key_json: String,
    pub dpop_authserver_nonce: String,
    pub dpop_host_nonce: String,
    pub token_iss: String,
    pub token_sub: String,
    pub token_aud: String,
    pub token_scope: Option<String>,
    pub refresh_token: Option<String>,
    pub access_token: String,
    pub token_type: String,
    pub expires_at: Option<i64>,
}

impl OAuthSessionParams {
    /// Create insertion parameters from a Jacquard ClientSessionData.
    pub fn from_session(session: &ClientSessionData<'_>) -> Result<Self, AuthError> {
        // Serialize scopes to JSON array
        let scopes: Vec<String> = session
            .scopes
            .iter()
            .map(|s| s.to_string_normalized())
            .collect();
        let scopes_json = serde_json::to_string(&scopes)?;

        // Serialize DPoP key to JSON
        let dpop_key_json = serde_json::to_string(&session.dpop_data.dpop_key)?;

        // Convert expires_at to unix timestamp
        let expires_at: Option<i64> = session.token_set.expires_at.as_ref().map(|dt| {
            let chrono_dt: &chrono::DateTime<chrono::FixedOffset> = dt.as_ref();
            chrono_dt.timestamp()
        });

        Ok(Self {
            account_did: session.account_did.as_str().to_string(),
            session_id: session.session_id.to_string(),
            host_url: session.host_url.to_string(),
            authserver_url: session.authserver_url.to_string(),
            authserver_token_endpoint: session.authserver_token_endpoint.to_string(),
            authserver_revocation_endpoint: session
                .authserver_revocation_endpoint
                .as_ref()
                .map(|s| s.to_string()),
            scopes_json,
            dpop_key_json,
            dpop_authserver_nonce: session.dpop_data.dpop_authserver_nonce.to_string(),
            dpop_host_nonce: session.dpop_data.dpop_host_nonce.to_string(),
            token_iss: session.token_set.iss.to_string(),
            token_sub: session.token_set.sub.as_str().to_string(),
            token_aud: session.token_set.aud.to_string(),
            token_scope: session.token_set.scope.as_ref().map(|s| s.to_string()),
            refresh_token: session
                .token_set
                .refresh_token
                .as_ref()
                .map(|s| s.to_string()),
            access_token: session.token_set.access_token.to_string(),
            token_type: session.token_set.token_type.as_str().to_string(),
            expires_at,
        })
    }
}

/// Database row for oauth_auth_requests table.
#[derive(Debug, sqlx::FromRow)]
pub struct OAuthAuthRequestRow {
    pub state: String,
    pub authserver_url: String,
    pub account_did: Option<String>,
    pub scopes: String,
    pub request_uri: String,
    pub authserver_token_endpoint: String,
    pub authserver_revocation_endpoint: Option<String>,
    pub pkce_verifier: String,
    pub dpop_key: String,
    pub dpop_nonce: String,
    pub expires_at: i64,
}

impl OAuthAuthRequestRow {
    /// Convert database row to Jacquard's AuthRequestData.
    pub fn to_auth_request_data(&self) -> Result<AuthRequestData<'static>, AuthError> {
        // Parse the DPoP key from JSON
        let dpop_key: Key = serde_json::from_str(&self.dpop_key)?;

        // Parse scopes from JSON array
        let scope_strings: Vec<String> = serde_json::from_str(&self.scopes)?;
        let scopes: Vec<Scope<'static>> = scope_strings
            .iter()
            .filter_map(|s| Scope::parse(s).ok().map(|scope| scope.into_static()))
            .collect();

        // Parse optional account_did
        let account_did = self
            .account_did
            .as_ref()
            .and_then(|s| Did::new(s).ok().map(|d| d.into_static()));

        // Parse dpop_nonce - empty string means None
        let dpop_authserver_nonce = if self.dpop_nonce.is_empty() {
            None
        } else {
            Some(CowStr::from(self.dpop_nonce.clone()))
        };

        Ok(AuthRequestData {
            state: CowStr::from(self.state.clone()),
            authserver_url: CowStr::from(self.authserver_url.clone()),
            account_did,
            scopes,
            request_uri: CowStr::from(self.request_uri.clone()),
            authserver_token_endpoint: CowStr::from(self.authserver_token_endpoint.clone()),
            authserver_revocation_endpoint: self
                .authserver_revocation_endpoint
                .clone()
                .map(CowStr::from),
            pkce_verifier: CowStr::from(self.pkce_verifier.clone()),
            dpop_data: DpopReqData {
                dpop_key,
                dpop_authserver_nonce,
            },
        })
    }
}

/// Parameters for inserting an OAuth auth request.
#[derive(Debug)]
pub struct OAuthAuthRequestParams {
    pub state: String,
    pub authserver_url: String,
    pub account_did: Option<String>,
    pub scopes_json: String,
    pub request_uri: String,
    pub authserver_token_endpoint: String,
    pub authserver_revocation_endpoint: Option<String>,
    pub pkce_verifier: String,
    pub dpop_key_json: String,
    pub dpop_nonce: String,
    pub expires_at: i64,
}

impl OAuthAuthRequestParams {
    /// Create insertion parameters from a Jacquard AuthRequestData.
    pub fn from_auth_request(auth_req: &AuthRequestData<'_>) -> Result<Self, AuthError> {
        // Serialize scopes to JSON array
        let scopes: Vec<String> = auth_req
            .scopes
            .iter()
            .map(|s| s.to_string_normalized())
            .collect();
        let scopes_json = serde_json::to_string(&scopes)?;

        // Serialize DPoP key to JSON
        let dpop_key_json = serde_json::to_string(&auth_req.dpop_data.dpop_key)?;

        // DPoP nonce - None becomes empty string
        let dpop_nonce = auth_req
            .dpop_data
            .dpop_authserver_nonce
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let now = chrono::Utc::now().timestamp();
        // Auth requests expire after 10 minutes
        let expires_at = now + 600;

        Ok(Self {
            state: auth_req.state.to_string(),
            authserver_url: auth_req.authserver_url.to_string(),
            account_did: auth_req
                .account_did
                .as_ref()
                .map(|d| d.as_str().to_string()),
            scopes_json,
            request_uri: auth_req.request_uri.to_string(),
            authserver_token_endpoint: auth_req.authserver_token_endpoint.to_string(),
            authserver_revocation_endpoint: auth_req
                .authserver_revocation_endpoint
                .as_ref()
                .map(|s| s.to_string()),
            pkce_verifier: auth_req.pkce_verifier.to_string(),
            dpop_key_json,
            dpop_nonce,
            expires_at,
        })
    }
}

/// Database row for app_password_sessions table.
///
/// This is a simpler session type compared to OAuth - just JWT tokens and identity info.
#[derive(Debug, sqlx::FromRow)]
pub struct AppPasswordSessionRow {
    pub did: String,
    pub session_id: String,
    pub access_jwt: String,
    pub refresh_jwt: String,
    pub handle: String,
}

/// Summary of an ATProto identity for listing.
///
/// This provides a simplified view of stored ATProto sessions for CLI display.
#[derive(Debug, Clone)]
pub struct AtprotoIdentitySummary {
    /// The DID (decentralized identifier) of the account.
    pub did: String,
    /// The handle (e.g., user.bsky.social).
    pub handle: String,
    /// The session ID used for this identity.
    pub session_id: String,
    /// Whether this is an OAuth session or app-password session.
    pub auth_type: AtprotoAuthType,
    /// When the token expires (for OAuth), if known.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Type of ATProto authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtprotoAuthType {
    /// OAuth with DPoP tokens.
    OAuth,
    /// Simple app-password with JWT tokens.
    AppPassword,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_params_roundtrip() {
        let did = Did::new("did:plc:testuser123").unwrap();
        let dpop_key: Key =
            serde_json::from_str(r#"{"kty":"EC","crv":"P-256","x":"test","y":"test","d":"test"}"#)
                .unwrap();

        let session = ClientSessionData {
            account_did: did.clone().into_static(),
            session_id: CowStr::from("test-session"),
            host_url: CowStr::from("https://bsky.social"),
            authserver_url: CowStr::from("https://bsky.social"),
            authserver_token_endpoint: CowStr::from("https://bsky.social/oauth/token"),
            authserver_revocation_endpoint: Some(CowStr::from("https://bsky.social/oauth/revoke")),
            scopes: vec![Scope::Atproto],
            dpop_data: DpopClientData {
                dpop_key,
                dpop_authserver_nonce: CowStr::from("auth-nonce"),
                dpop_host_nonce: CowStr::from("host-nonce"),
            },
            token_set: TokenSet {
                iss: CowStr::from("https://bsky.social"),
                sub: did.clone().into_static(),
                aud: CowStr::from("https://bsky.social"),
                scope: Some(CowStr::from("atproto")),
                refresh_token: Some(CowStr::from("refresh-token")),
                access_token: CowStr::from("access-token"),
                token_type: OAuthTokenType::DPoP,
                expires_at: None,
            },
        };

        // Convert to params
        let params = OAuthSessionParams::from_session(&session).unwrap();

        // Verify key fields
        assert_eq!(params.account_did, "did:plc:testuser123");
        assert_eq!(params.session_id, "test-session");
        assert_eq!(params.token_type, "DPoP");

        // Verify JSON serialization
        let scopes: Vec<String> = serde_json::from_str(&params.scopes_json).unwrap();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0], "atproto");
    }

    #[test]
    fn test_auth_request_params_roundtrip() {
        let dpop_key: Key =
            serde_json::from_str(r#"{"kty":"EC","crv":"P-256","x":"test","y":"test","d":"test"}"#)
                .unwrap();

        let auth_req = AuthRequestData {
            state: CowStr::from("test-state"),
            authserver_url: CowStr::from("https://bsky.social"),
            account_did: Some(Did::new("did:plc:testuser").unwrap().into_static()),
            scopes: vec![Scope::Atproto],
            request_uri: CowStr::from("urn:ietf:params:oauth:request_uri:test"),
            authserver_token_endpoint: CowStr::from("https://bsky.social/oauth/token"),
            authserver_revocation_endpoint: None,
            pkce_verifier: CowStr::from("pkce-secret"),
            dpop_data: DpopReqData {
                dpop_key,
                dpop_authserver_nonce: Some(CowStr::from("initial-nonce")),
            },
        };

        let params = OAuthAuthRequestParams::from_auth_request(&auth_req).unwrap();

        assert_eq!(params.state, "test-state");
        assert_eq!(params.account_did, Some("did:plc:testuser".to_string()));
        assert_eq!(params.dpop_nonce, "initial-nonce");
        assert!(params.expires_at > chrono::Utc::now().timestamp());
    }
}
