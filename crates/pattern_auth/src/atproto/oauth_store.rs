//! Implementation of Jacquard's `ClientAuthStore` trait for SQLite storage.
//!
//! This provides persistent storage for OAuth sessions and auth requests,
//! enabling Pattern agents to maintain authenticated ATProto sessions across restarts.

use jacquard::oauth::authstore::ClientAuthStore;
use jacquard::oauth::session::{AuthRequestData, ClientSessionData};
use jacquard::session::SessionStoreError;
use jacquard::types::did::Did;

use crate::atproto::models::{
    OAuthAuthRequestParams, OAuthAuthRequestRow, OAuthSessionParams, OAuthSessionRow,
};
use crate::db::AuthDb;
use crate::error::AuthError;

impl ClientAuthStore for AuthDb {
    async fn get_session(
        &self,
        did: &Did<'_>,
        session_id: &str,
    ) -> Result<Option<ClientSessionData<'_>>, SessionStoreError> {
        let did_str = did.as_str();

        let row = sqlx::query_as!(
            OAuthSessionRow,
            r#"
            SELECT
                account_did as "account_did!",
                session_id as "session_id!",
                host_url as "host_url!",
                authserver_url as "authserver_url!",
                authserver_token_endpoint as "authserver_token_endpoint!",
                authserver_revocation_endpoint,
                scopes as "scopes!",
                dpop_key as "dpop_key!",
                dpop_authserver_nonce as "dpop_authserver_nonce!",
                dpop_host_nonce as "dpop_host_nonce!",
                token_iss as "token_iss!",
                token_sub as "token_sub!",
                token_aud as "token_aud!",
                token_scope,
                refresh_token,
                access_token as "access_token!",
                token_type as "token_type!",
                expires_at
            FROM oauth_sessions
            WHERE account_did = ? AND session_id = ?
            "#,
            did_str,
            session_id,
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let session = row
            .to_client_session_data()
            .map_err(SessionStoreError::from)?;

        Ok(Some(session))
    }

    async fn upsert_session(
        &self,
        session: ClientSessionData<'_>,
    ) -> Result<(), SessionStoreError> {
        let params = OAuthSessionParams::from_session(&session).map_err(SessionStoreError::from)?;

        let now = chrono::Utc::now().timestamp();

        sqlx::query!(
            r#"
            INSERT INTO oauth_sessions (
                account_did, session_id, host_url, authserver_url,
                authserver_token_endpoint, authserver_revocation_endpoint,
                scopes, dpop_key, dpop_authserver_nonce, dpop_host_nonce,
                token_iss, token_sub, token_aud, token_scope,
                refresh_token, access_token, token_type, expires_at,
                created_at, updated_at
            ) VALUES (
                ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
            )
            ON CONFLICT (account_did, session_id) DO UPDATE SET
                host_url = excluded.host_url,
                authserver_url = excluded.authserver_url,
                authserver_token_endpoint = excluded.authserver_token_endpoint,
                authserver_revocation_endpoint = excluded.authserver_revocation_endpoint,
                scopes = excluded.scopes,
                dpop_key = excluded.dpop_key,
                dpop_authserver_nonce = excluded.dpop_authserver_nonce,
                dpop_host_nonce = excluded.dpop_host_nonce,
                token_iss = excluded.token_iss,
                token_sub = excluded.token_sub,
                token_aud = excluded.token_aud,
                token_scope = excluded.token_scope,
                refresh_token = excluded.refresh_token,
                access_token = excluded.access_token,
                token_type = excluded.token_type,
                expires_at = excluded.expires_at,
                updated_at = excluded.updated_at
            "#,
            params.account_did,
            params.session_id,
            params.host_url,
            params.authserver_url,
            params.authserver_token_endpoint,
            params.authserver_revocation_endpoint,
            params.scopes_json,
            params.dpop_key_json,
            params.dpop_authserver_nonce,
            params.dpop_host_nonce,
            params.token_iss,
            params.token_sub,
            params.token_aud,
            params.token_scope,
            params.refresh_token,
            params.access_token,
            params.token_type,
            params.expires_at,
            now,
            now,
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        Ok(())
    }

    async fn delete_session(
        &self,
        did: &Did<'_>,
        session_id: &str,
    ) -> Result<(), SessionStoreError> {
        let did_str = did.as_str();

        sqlx::query!(
            "DELETE FROM oauth_sessions WHERE account_did = ? AND session_id = ?",
            did_str,
            session_id,
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        Ok(())
    }

    async fn get_auth_req_info(
        &self,
        state: &str,
    ) -> Result<Option<AuthRequestData<'_>>, SessionStoreError> {
        let row = sqlx::query_as!(
            OAuthAuthRequestRow,
            r#"
            SELECT
                state as "state!",
                authserver_url as "authserver_url!",
                account_did,
                scopes as "scopes!",
                request_uri as "request_uri!",
                authserver_token_endpoint as "authserver_token_endpoint!",
                authserver_revocation_endpoint,
                pkce_verifier as "pkce_verifier!",
                dpop_key as "dpop_key!",
                dpop_nonce as "dpop_nonce!",
                expires_at as "expires_at!"
            FROM oauth_auth_requests
            WHERE state = ?
            "#,
            state,
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        let Some(row) = row else {
            return Ok(None);
        };

        // Check if request has expired
        let now = chrono::Utc::now().timestamp();
        if row.expires_at < now {
            // Delete expired request and return None
            let _ = self.delete_auth_req_info(state).await;
            return Ok(None);
        }

        let auth_req = row
            .to_auth_request_data()
            .map_err(SessionStoreError::from)?;

        Ok(Some(auth_req))
    }

    async fn save_auth_req_info(
        &self,
        auth_req_info: &AuthRequestData<'_>,
    ) -> Result<(), SessionStoreError> {
        let params = OAuthAuthRequestParams::from_auth_request(auth_req_info)
            .map_err(SessionStoreError::from)?;

        let now = chrono::Utc::now().timestamp();

        sqlx::query!(
            r#"
            INSERT INTO oauth_auth_requests (
                state, authserver_url, account_did, scopes, request_uri,
                authserver_token_endpoint, authserver_revocation_endpoint,
                pkce_verifier, dpop_key, dpop_nonce, created_at, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (state) DO UPDATE SET
                authserver_url = excluded.authserver_url,
                account_did = excluded.account_did,
                scopes = excluded.scopes,
                request_uri = excluded.request_uri,
                authserver_token_endpoint = excluded.authserver_token_endpoint,
                authserver_revocation_endpoint = excluded.authserver_revocation_endpoint,
                pkce_verifier = excluded.pkce_verifier,
                dpop_key = excluded.dpop_key,
                dpop_nonce = excluded.dpop_nonce,
                expires_at = excluded.expires_at
            "#,
            params.state,
            params.authserver_url,
            params.account_did,
            params.scopes_json,
            params.request_uri,
            params.authserver_token_endpoint,
            params.authserver_revocation_endpoint,
            params.pkce_verifier,
            params.dpop_key_json,
            params.dpop_nonce,
            now,
            params.expires_at,
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        Ok(())
    }

    async fn delete_auth_req_info(&self, state: &str) -> Result<(), SessionStoreError> {
        sqlx::query!("DELETE FROM oauth_auth_requests WHERE state = ?", state,)
            .execute(self.pool())
            .await
            .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        Ok(())
    }
}

/// Database row for listing OAuth sessions (simplified).
#[derive(Debug, sqlx::FromRow)]
pub struct OAuthSessionSummaryRow {
    pub account_did: String,
    pub session_id: String,
    pub host_url: String,
    pub expires_at: Option<i64>,
}

// Additional list/query methods for CLI commands (not part of ClientAuthStore trait)
impl AuthDb {
    /// List all stored OAuth sessions.
    ///
    /// Returns a list of summary rows for all stored OAuth sessions.
    pub async fn list_oauth_sessions(
        &self,
    ) -> crate::error::AuthResult<Vec<OAuthSessionSummaryRow>> {
        let rows = sqlx::query_as!(
            OAuthSessionSummaryRow,
            r#"
            SELECT
                account_did as "account_did!",
                session_id as "session_id!",
                host_url as "host_url!",
                expires_at
            FROM oauth_sessions
            ORDER BY account_did, session_id
            "#
        )
        .fetch_all(self.pool())
        .await?;

        Ok(rows)
    }

    /// Delete an OAuth session by DID (and optionally session_id).
    ///
    /// If `session_id` is None, deletes all sessions for the DID.
    pub async fn delete_oauth_session_by_did(
        &self,
        did: &str,
        session_id: Option<&str>,
    ) -> crate::error::AuthResult<u64> {
        let result = if let Some(sid) = session_id {
            sqlx::query!(
                "DELETE FROM oauth_sessions WHERE account_did = ? AND session_id = ?",
                did,
                sid,
            )
            .execute(self.pool())
            .await?
        } else {
            sqlx::query!("DELETE FROM oauth_sessions WHERE account_did = ?", did,)
                .execute(self.pool())
                .await?
        };

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jacquard::CowStr;
    use jacquard::IntoStatic;
    use jacquard::oauth::scopes::Scope;
    use jacquard::oauth::session::{DpopClientData, DpopReqData};
    use jacquard::oauth::types::OAuthTokenType;
    use jacquard::oauth::types::TokenSet;
    use jacquard::types::string::Datetime;
    use jose_jwk::Key;

    #[tokio::test]
    async fn test_oauth_session_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Create a test session
        let did = Did::new("did:plc:testuser123").unwrap();
        let session_id = "test-session-id";

        // Create DPoP key (minimal valid EC key for testing)
        let dpop_key: Key =
            serde_json::from_str(r#"{"kty":"EC","crv":"P-256","x":"test","y":"test","d":"test"}"#)
                .unwrap();

        let session = ClientSessionData {
            account_did: did.clone().into_static(),
            session_id: CowStr::from(session_id.to_string()),
            host_url: CowStr::from("https://bsky.social"),
            authserver_url: CowStr::from("https://bsky.social"),
            authserver_token_endpoint: CowStr::from("https://bsky.social/oauth/token"),
            authserver_revocation_endpoint: Some(CowStr::from("https://bsky.social/oauth/revoke")),
            scopes: vec![
                Scope::Atproto,
                Scope::parse("repo:*").unwrap().into_static(),
            ],
            dpop_data: DpopClientData {
                dpop_key: dpop_key.clone(),
                dpop_authserver_nonce: CowStr::from("auth-nonce"),
                dpop_host_nonce: CowStr::from("host-nonce"),
            },
            token_set: TokenSet {
                iss: CowStr::from("https://bsky.social"),
                sub: did.clone().into_static(),
                aud: CowStr::from("https://bsky.social"),
                scope: Some(CowStr::from("atproto repo:*")),
                refresh_token: Some(CowStr::from("refresh-token-value")),
                access_token: CowStr::from("access-token-value"),
                token_type: OAuthTokenType::DPoP,
                expires_at: Some(Datetime::now()),
            },
        };

        // Save the session
        db.upsert_session(session.clone()).await.unwrap();

        // Retrieve the session
        let retrieved = db
            .get_session(&did, session_id)
            .await
            .unwrap()
            .expect("session should exist");

        // Verify fields match
        assert_eq!(retrieved.account_did.as_str(), did.as_str());
        assert_eq!(retrieved.session_id.as_ref(), session_id);
        assert_eq!(retrieved.host_url.as_ref(), "https://bsky.social");
        assert_eq!(retrieved.scopes.len(), 2);
        assert_eq!(
            retrieved.token_set.access_token.as_ref(),
            "access-token-value"
        );

        // Delete the session
        db.delete_session(&did, session_id).await.unwrap();

        // Verify it's gone
        let deleted = db.get_session(&did, session_id).await.unwrap();
        assert!(deleted.is_none());
    }

    #[tokio::test]
    async fn test_auth_request_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let state = "test-state-abc123";

        // Create DPoP key
        let dpop_key: Key =
            serde_json::from_str(r#"{"kty":"EC","crv":"P-256","x":"test","y":"test","d":"test"}"#)
                .unwrap();

        let auth_req = AuthRequestData {
            state: CowStr::from(state.to_string()),
            authserver_url: CowStr::from("https://bsky.social"),
            account_did: Some(Did::new("did:plc:testuser").unwrap().into_static()),
            scopes: vec![Scope::Atproto],
            request_uri: CowStr::from("urn:ietf:params:oauth:request_uri:test"),
            authserver_token_endpoint: CowStr::from("https://bsky.social/oauth/token"),
            authserver_revocation_endpoint: None,
            pkce_verifier: CowStr::from("pkce-secret-verifier"),
            dpop_data: DpopReqData {
                dpop_key,
                dpop_authserver_nonce: Some(CowStr::from("initial-nonce")),
            },
        };

        // Save the auth request
        db.save_auth_req_info(&auth_req).await.unwrap();

        // Retrieve it
        let retrieved = db
            .get_auth_req_info(state)
            .await
            .unwrap()
            .expect("auth request should exist");

        assert_eq!(retrieved.state.as_ref(), state);
        assert_eq!(retrieved.pkce_verifier.as_ref(), "pkce-secret-verifier");
        assert!(retrieved.account_did.is_some());

        // Delete it
        db.delete_auth_req_info(state).await.unwrap();

        // Verify it's gone
        let deleted = db.get_auth_req_info(state).await.unwrap();
        assert!(deleted.is_none());
    }

    #[tokio::test]
    async fn test_session_update() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let did = Did::new("did:plc:testuser").unwrap();
        let session_id = "update-test";

        let dpop_key: Key =
            serde_json::from_str(r#"{"kty":"EC","crv":"P-256","x":"test","y":"test","d":"test"}"#)
                .unwrap();

        // Create initial session
        let session = ClientSessionData {
            account_did: did.clone().into_static(),
            session_id: CowStr::from(session_id.to_string()),
            host_url: CowStr::from("https://bsky.social"),
            authserver_url: CowStr::from("https://bsky.social"),
            authserver_token_endpoint: CowStr::from("https://bsky.social/oauth/token"),
            authserver_revocation_endpoint: None,
            scopes: vec![Scope::Atproto],
            dpop_data: DpopClientData {
                dpop_key: dpop_key.clone(),
                dpop_authserver_nonce: CowStr::from("nonce-1"),
                dpop_host_nonce: CowStr::from("host-1"),
            },
            token_set: TokenSet {
                iss: CowStr::from("https://bsky.social"),
                sub: did.clone().into_static(),
                aud: CowStr::from("https://bsky.social"),
                scope: None,
                refresh_token: None,
                access_token: CowStr::from("token-1"),
                token_type: OAuthTokenType::DPoP,
                expires_at: None,
            },
        };

        db.upsert_session(session).await.unwrap();

        // Update the session with new token
        let updated_session = ClientSessionData {
            account_did: did.clone().into_static(),
            session_id: CowStr::from(session_id.to_string()),
            host_url: CowStr::from("https://bsky.social"),
            authserver_url: CowStr::from("https://bsky.social"),
            authserver_token_endpoint: CowStr::from("https://bsky.social/oauth/token"),
            authserver_revocation_endpoint: None,
            scopes: vec![Scope::Atproto],
            dpop_data: DpopClientData {
                dpop_key,
                dpop_authserver_nonce: CowStr::from("nonce-2"),
                dpop_host_nonce: CowStr::from("host-2"),
            },
            token_set: TokenSet {
                iss: CowStr::from("https://bsky.social"),
                sub: did.clone().into_static(),
                aud: CowStr::from("https://bsky.social"),
                scope: None,
                refresh_token: None,
                access_token: CowStr::from("token-2"),
                token_type: OAuthTokenType::DPoP,
                expires_at: None,
            },
        };

        db.upsert_session(updated_session).await.unwrap();

        // Verify update
        let retrieved = db
            .get_session(&did, session_id)
            .await
            .unwrap()
            .expect("session should exist");

        assert_eq!(retrieved.token_set.access_token.as_ref(), "token-2");
        assert_eq!(
            retrieved.dpop_data.dpop_authserver_nonce.as_ref(),
            "nonce-2"
        );
    }
}
