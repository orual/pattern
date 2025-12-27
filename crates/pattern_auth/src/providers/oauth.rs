//! Provider OAuth token storage.
//!
//! This module provides `ProviderOAuthToken` for storing OAuth tokens
//! from AI model providers like Anthropic and OpenAI.

use chrono::{DateTime, Utc};

use crate::db::AuthDb;
use crate::error::AuthResult;

/// OAuth token for an AI model provider.
///
/// Stores OAuth credentials for providers like Anthropic, OpenAI, etc.
/// The provider name serves as the primary key (one token per provider).
#[derive(Debug, Clone)]
pub struct ProviderOAuthToken {
    /// Provider identifier (e.g., "anthropic", "openai").
    pub provider: String,
    /// OAuth access token.
    pub access_token: String,
    /// OAuth refresh token (if provided by the provider).
    pub refresh_token: Option<String>,
    /// Token expiration time (if provided).
    pub expires_at: Option<DateTime<Utc>>,
    /// OAuth scopes granted.
    pub scope: Option<String>,
    /// Session identifier (provider-specific).
    pub session_id: Option<String>,
    /// When this token was first stored.
    pub created_at: DateTime<Utc>,
    /// When this token was last updated.
    pub updated_at: DateTime<Utc>,
}

impl ProviderOAuthToken {
    /// Check if this token needs to be refreshed.
    ///
    /// Returns `true` if the token will expire within the next 5 minutes,
    /// or if it has already expired. Returns `false` if there is no
    /// expiration time set.
    pub fn needs_refresh(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => {
                let refresh_threshold = Utc::now() + chrono::Duration::minutes(5);
                expires_at <= refresh_threshold
            }
            None => false,
        }
    }

    /// Check if this token has expired.
    ///
    /// Returns `true` if the token's expiration time has passed.
    /// Returns `false` if there is no expiration time set.
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => expires_at <= Utc::now(),
            None => false,
        }
    }
}

/// Database row for provider_oauth_tokens table.
#[derive(Debug, sqlx::FromRow)]
struct ProviderOAuthTokenRow {
    provider: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
    scope: Option<String>,
    session_id: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl ProviderOAuthTokenRow {
    /// Convert database row to ProviderOAuthToken.
    fn to_token(&self) -> ProviderOAuthToken {
        ProviderOAuthToken {
            provider: self.provider.clone(),
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at.map(timestamp_to_datetime),
            scope: self.scope.clone(),
            session_id: self.session_id.clone(),
            created_at: timestamp_to_datetime(self.created_at),
            updated_at: timestamp_to_datetime(self.updated_at),
        }
    }
}

/// Convert a Unix timestamp (seconds) to a DateTime<Utc>.
fn timestamp_to_datetime(timestamp: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(timestamp, 0).unwrap_or_else(|| Utc::now())
}

impl AuthDb {
    /// Get an OAuth token for a specific provider.
    ///
    /// Returns `None` if no token has been stored for this provider.
    pub async fn get_provider_oauth_token(
        &self,
        provider: &str,
    ) -> AuthResult<Option<ProviderOAuthToken>> {
        let row = sqlx::query_as!(
            ProviderOAuthTokenRow,
            r#"
            SELECT
                provider as "provider!",
                access_token as "access_token!",
                refresh_token,
                expires_at,
                scope,
                session_id,
                created_at as "created_at!",
                updated_at as "updated_at!"
            FROM provider_oauth_tokens
            WHERE provider = ?
            "#,
            provider
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(row.map(|r| r.to_token()))
    }

    /// Store or update an OAuth token for a provider.
    ///
    /// This performs an upsert - creating a new token if one doesn't exist
    /// for this provider, or updating the existing token if it does.
    pub async fn set_provider_oauth_token(&self, token: &ProviderOAuthToken) -> AuthResult<()> {
        let expires_at = token.expires_at.map(|dt| dt.timestamp());
        let now = Utc::now().timestamp();

        sqlx::query!(
            r#"
            INSERT INTO provider_oauth_tokens (
                provider, access_token, refresh_token, expires_at, scope, session_id,
                created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (provider) DO UPDATE SET
                access_token = excluded.access_token,
                refresh_token = excluded.refresh_token,
                expires_at = excluded.expires_at,
                scope = excluded.scope,
                session_id = excluded.session_id,
                updated_at = excluded.updated_at
            "#,
            token.provider,
            token.access_token,
            token.refresh_token,
            expires_at,
            token.scope,
            token.session_id,
            now,
            now,
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Delete an OAuth token for a specific provider.
    pub async fn delete_provider_oauth_token(&self, provider: &str) -> AuthResult<()> {
        sqlx::query!(
            "DELETE FROM provider_oauth_tokens WHERE provider = ?",
            provider
        )
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// List all stored provider OAuth tokens.
    pub async fn list_provider_oauth_tokens(&self) -> AuthResult<Vec<ProviderOAuthToken>> {
        let rows = sqlx::query_as!(
            ProviderOAuthTokenRow,
            r#"
            SELECT
                provider as "provider!",
                access_token as "access_token!",
                refresh_token,
                expires_at,
                scope,
                session_id,
                created_at as "created_at!",
                updated_at as "updated_at!"
            FROM provider_oauth_tokens
            ORDER BY provider
            "#
        )
        .fetch_all(self.pool())
        .await?;

        Ok(rows.into_iter().map(|r| r.to_token()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_provider_oauth_token_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Initially no token
        let token = db.get_provider_oauth_token("anthropic").await.unwrap();
        assert!(token.is_none());

        // Create and store token
        let now = Utc::now();
        let expires = now + chrono::Duration::hours(1);
        let token = ProviderOAuthToken {
            provider: "anthropic".to_string(),
            access_token: "test-access-token".to_string(),
            refresh_token: Some("test-refresh-token".to_string()),
            expires_at: Some(expires),
            scope: Some("read write".to_string()),
            session_id: Some("session-123".to_string()),
            created_at: now,
            updated_at: now,
        };

        db.set_provider_oauth_token(&token).await.unwrap();

        // Retrieve and verify
        let retrieved = db
            .get_provider_oauth_token("anthropic")
            .await
            .unwrap()
            .expect("token should exist");

        assert_eq!(retrieved.provider, "anthropic");
        assert_eq!(retrieved.access_token, "test-access-token");
        assert_eq!(
            retrieved.refresh_token,
            Some("test-refresh-token".to_string())
        );
        assert!(retrieved.expires_at.is_some());
        assert_eq!(retrieved.scope, Some("read write".to_string()));
        assert_eq!(retrieved.session_id, Some("session-123".to_string()));

        // Delete and verify
        db.delete_provider_oauth_token("anthropic").await.unwrap();
        let deleted = db.get_provider_oauth_token("anthropic").await.unwrap();
        assert!(deleted.is_none());
    }

    #[tokio::test]
    async fn test_provider_oauth_token_update() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let now = Utc::now();

        // Create initial token
        let token = ProviderOAuthToken {
            provider: "openai".to_string(),
            access_token: "token-1".to_string(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        };

        db.set_provider_oauth_token(&token).await.unwrap();

        // Update token
        let updated_token = ProviderOAuthToken {
            provider: "openai".to_string(),
            access_token: "token-2".to_string(),
            refresh_token: Some("refresh-2".to_string()),
            expires_at: Some(now + chrono::Duration::hours(2)),
            scope: Some("full".to_string()),
            session_id: Some("new-session".to_string()),
            created_at: now,
            updated_at: now,
        };

        db.set_provider_oauth_token(&updated_token).await.unwrap();

        // Verify update
        let retrieved = db
            .get_provider_oauth_token("openai")
            .await
            .unwrap()
            .expect("token should exist");

        assert_eq!(retrieved.access_token, "token-2");
        assert_eq!(retrieved.refresh_token, Some("refresh-2".to_string()));
        assert!(retrieved.expires_at.is_some());
        assert_eq!(retrieved.scope, Some("full".to_string()));
        assert_eq!(retrieved.session_id, Some("new-session".to_string()));
    }

    #[tokio::test]
    async fn test_provider_oauth_token_minimal() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let now = Utc::now();

        // Token with only required fields
        let token = ProviderOAuthToken {
            provider: "minimal".to_string(),
            access_token: "minimal-token".to_string(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        };

        db.set_provider_oauth_token(&token).await.unwrap();

        let retrieved = db
            .get_provider_oauth_token("minimal")
            .await
            .unwrap()
            .expect("token should exist");

        assert_eq!(retrieved.provider, "minimal");
        assert_eq!(retrieved.access_token, "minimal-token");
        assert!(retrieved.refresh_token.is_none());
        assert!(retrieved.expires_at.is_none());
        assert!(retrieved.scope.is_none());
        assert!(retrieved.session_id.is_none());
    }

    #[tokio::test]
    async fn test_list_provider_oauth_tokens() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Initially empty
        let tokens = db.list_provider_oauth_tokens().await.unwrap();
        assert!(tokens.is_empty());

        let now = Utc::now();

        // Add multiple tokens
        for provider in ["anthropic", "openai", "google"] {
            let token = ProviderOAuthToken {
                provider: provider.to_string(),
                access_token: format!("{}-token", provider),
                refresh_token: None,
                expires_at: None,
                scope: None,
                session_id: None,
                created_at: now,
                updated_at: now,
            };
            db.set_provider_oauth_token(&token).await.unwrap();
        }

        // List all
        let tokens = db.list_provider_oauth_tokens().await.unwrap();
        assert_eq!(tokens.len(), 3);

        // Should be ordered by provider name
        assert_eq!(tokens[0].provider, "anthropic");
        assert_eq!(tokens[1].provider, "google");
        assert_eq!(tokens[2].provider, "openai");
    }

    #[test]
    fn test_token_expiry_checks() {
        let now = Utc::now();

        // Token expiring in 1 hour - not expired, doesn't need refresh
        let token = ProviderOAuthToken {
            provider: "test".to_string(),
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: Some(now + chrono::Duration::hours(1)),
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        };
        assert!(!token.is_expired());
        assert!(!token.needs_refresh());

        // Token expiring in 3 minutes - not expired, but needs refresh
        let token = ProviderOAuthToken {
            provider: "test".to_string(),
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: Some(now + chrono::Duration::minutes(3)),
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        };
        assert!(!token.is_expired());
        assert!(token.needs_refresh());

        // Token expired 1 hour ago - expired and needs refresh
        let token = ProviderOAuthToken {
            provider: "test".to_string(),
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: Some(now - chrono::Duration::hours(1)),
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        };
        assert!(token.is_expired());
        assert!(token.needs_refresh());

        // Token with no expiration - never expired, never needs refresh
        let token = ProviderOAuthToken {
            provider: "test".to_string(),
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        };
        assert!(!token.is_expired());
        assert!(!token.needs_refresh());
    }
}
