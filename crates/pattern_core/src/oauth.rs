//! OAuth authentication support for external services
//!
//! This module provides OAuth token storage and management for integrating
//! with external services that require OAuth authentication (e.g., Anthropic).

pub mod auth_flow;

#[cfg(feature = "oauth")]
pub mod middleware;

#[cfg(feature = "oauth")]
pub mod resolver;

#[cfg(feature = "oauth")]
pub mod integration;

use crate::CoreError;
use crate::id::{OAuthTokenId, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// OAuth token entity for database persistence
///
/// Stores OAuth tokens for external service authentication.
/// Tokens are associated with Pattern users and include refresh capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub id: OAuthTokenId,

    /// The external service provider (e.g., "anthropic", "github")
    pub provider: String,

    /// The access token for API requests
    pub access_token: String,

    /// Optional refresh token for renewing access
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,

    /// When the access token expires
    pub expires_at: DateTime<Utc>,

    /// Optional session ID from the provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Optional scope granted by the token
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,

    /// When this token was created
    pub created_at: DateTime<Utc>,

    /// When this token was last used
    pub last_used_at: DateTime<Utc>,

    /// The user who owns this token
    pub owner_id: UserId,
}

impl OAuthToken {
    /// Create a new OAuth token
    pub fn new(
        provider: String,
        access_token: String,
        refresh_token: Option<String>,
        expires_at: DateTime<Utc>,
        owner_id: UserId,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: OAuthTokenId::generate(),
            provider,
            access_token,
            refresh_token,
            expires_at,
            session_id: None,
            scope: None,
            created_at: now,
            last_used_at: now,
            owner_id,
        }
    }

    /// Check if the token needs refresh (within 5 minutes of expiry)
    pub fn needs_refresh(&self) -> bool {
        let now = Utc::now();
        let time_until_expiry = self.expires_at.signed_duration_since(now);
        time_until_expiry.num_seconds() < 300 // Less than 5 minutes
    }

    /// Check if the token is expired
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Update the token after refresh
    pub fn update_from_refresh(
        &mut self,
        new_access_token: String,
        new_refresh_token: Option<String>,
        new_expires_at: DateTime<Utc>,
    ) {
        self.access_token = new_access_token;
        if new_refresh_token.is_some() {
            self.refresh_token = new_refresh_token;
        }
        self.expires_at = new_expires_at;
        self.last_used_at = Utc::now();
    }

    /// Mark the token as used
    pub fn mark_used(&mut self) {
        self.last_used_at = Utc::now();
    }
}

/// OAuth device flow state for PKCE challenges
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
    pub created_at: DateTime<Utc>,
}

impl PkceChallenge {
    pub fn new(verifier: String, challenge: String, state: String) -> Self {
        Self {
            verifier,
            challenge,
            state,
            created_at: Utc::now(),
        }
    }

    /// Check if this challenge is still valid (15 minute timeout)
    pub fn is_valid(&self) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.created_at);
        elapsed.num_minutes() < 15
    }
}

/// Token request types for OAuth flows
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "grant_type")]
pub enum TokenRequest {
    #[serde(rename = "authorization_code")]
    AuthorizationCode {
        client_id: String,
        code: String,
        redirect_uri: String,
        code_verifier: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
    #[serde(rename = "refresh_token")]
    RefreshToken {
        client_id: String,
        refresh_token: String,
    },
}

/// Token response from OAuth provider
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub expires_in: u64, // seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub token_type: String,

    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// OAuth provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OAuthProvider {
    Anthropic,
}

impl OAuthProvider {
    /// Get the provider name as a string
    pub fn as_str(&self) -> &'static str {
        match self {
            OAuthProvider::Anthropic => "anthropic",
        }
    }
}

impl std::fmt::Display for OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// OAuth client for device flow authentication
pub struct OAuthClient {
    provider: OAuthProvider,
    flow: auth_flow::DeviceAuthFlow,
}

impl OAuthClient {
    /// Create a new OAuth client for a provider
    pub fn new(provider: OAuthProvider) -> Self {
        let config = match provider {
            OAuthProvider::Anthropic => auth_flow::OAuthConfig::anthropic(),
        };
        Self {
            provider,
            flow: auth_flow::DeviceAuthFlow::new(config),
        }
    }

    /// Start the device authorization flow
    pub fn start_device_flow(&self) -> Result<DeviceCodeResponse, CoreError> {
        let (auth_url, pkce_challenge) = self.flow.start_auth();

        // For device flow, we simulate the device code response
        // In a real implementation, this would come from the OAuth provider
        Ok(DeviceCodeResponse {
            verification_uri: auth_url,
            user_code: "PATTERN-OAUTH".to_string(), // Placeholder for device flow
            device_code: pkce_challenge.state.clone(),
            expires_in: 900, // 15 minutes
            interval: 5,
            pkce_challenge: Some(pkce_challenge),
        })
    }

    /// Exchange authorization code for tokens
    pub async fn exchange_code(
        &self,
        code: String,
        pkce_challenge: &PkceChallenge,
    ) -> Result<TokenResponse, CoreError> {
        self.flow.exchange_code(code, pkce_challenge).await
    }

    /// Get the provider type
    pub fn provider(&self) -> OAuthProvider {
        self.provider
    }
}

/// Device code response for OAuth device flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub verification_uri: String,
    pub user_code: String,
    pub device_code: String,
    pub expires_in: u64,
    pub interval: u64,
    #[serde(skip)]
    pub pkce_challenge: Option<PkceChallenge>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_token_expiry_checking() {
        let user_id = UserId::generate();
        let mut token = OAuthToken::new(
            "anthropic".to_string(),
            "test_access_token".to_string(),
            Some("test_refresh_token".to_string()),
            Utc::now() + Duration::minutes(10),
            user_id,
        );

        // Token expires in 10 minutes, should not need refresh yet
        assert!(!token.needs_refresh());
        assert!(!token.is_expired());

        // Set expiry to 4 minutes from now
        token.expires_at = Utc::now() + Duration::minutes(4);
        assert!(token.needs_refresh());
        assert!(!token.is_expired());

        // Set to expired
        token.expires_at = Utc::now() - Duration::minutes(1);
        assert!(token.needs_refresh());
        assert!(token.is_expired());
    }

    #[test]
    fn test_pkce_challenge_validity() {
        let challenge = PkceChallenge::new(
            "verifier123".to_string(),
            "challenge456".to_string(),
            "state789".to_string(),
        );

        assert!(challenge.is_valid());

        // Create an old challenge
        let mut old_challenge = challenge.clone();
        old_challenge.created_at = Utc::now() - Duration::minutes(20);
        assert!(!old_challenge.is_valid());
    }
}
