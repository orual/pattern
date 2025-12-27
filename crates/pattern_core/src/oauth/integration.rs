//! OAuth integration that combines middleware with genai client
//!
//! This module provides the glue between Pattern's OAuth tokens,
//! the request transformation middleware, and genai's client.

use crate::error::CoreError;
use crate::oauth::auth_flow::DeviceAuthFlow;
use chrono::Utc;
use pattern_auth::{AuthDb, ProviderOAuthToken};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;

// Global refresh lock map to prevent concurrent refreshes for the same token
static REFRESH_LOCKS: LazyLock<Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// OAuth-enabled model provider that integrates with genai.
///
/// Tokens are stored at the constellation level (one per provider).
pub struct OAuthModelProvider {
    auth_db: AuthDb,
}

impl OAuthModelProvider {
    /// Create a new OAuth-enabled model provider
    pub fn new(auth_db: AuthDb) -> Self {
        Self { auth_db }
    }

    /// Get or refresh OAuth token for a provider
    pub async fn get_token(&self, provider: &str) -> Result<Option<ProviderOAuthToken>, CoreError> {
        // Try to get existing token
        let token = self
            .auth_db
            .get_provider_oauth_token(provider)
            .await
            .map_err(|e| CoreError::OAuthError {
                provider: provider.to_string(),
                operation: "get_token".to_string(),
                details: format!("Database error: {}", e),
            })?;

        if let Some(mut token) = token {
            let expires_display = token
                .expires_at
                .map(|e| e.to_string())
                .unwrap_or_else(|| "never".to_string());

            tracing::debug!(
                "Found OAuth token for provider '{}', expires at: {}, needs refresh: {}",
                provider,
                expires_display,
                token.needs_refresh()
            );

            // Check if token needs refresh
            if token.needs_refresh() && token.refresh_token.is_some() {
                // Get or create a lock for this specific token to prevent concurrent refreshes
                let lock_key = provider.to_string();
                let token_lock = {
                    let mut locks = REFRESH_LOCKS.lock().await;
                    locks
                        .entry(lock_key.clone())
                        .or_insert_with(|| Arc::new(Mutex::new(())))
                        .clone()
                };

                // Acquire the lock for this token refresh
                let _guard = token_lock.lock().await;

                // Re-check if token still needs refresh (another thread might have refreshed it)
                let token_check = self
                    .auth_db
                    .get_provider_oauth_token(provider)
                    .await
                    .map_err(|e| CoreError::OAuthError {
                        provider: provider.to_string(),
                        operation: "get_token".to_string(),
                        details: format!("Database error: {}", e),
                    })?;

                if let Some(fresh_token) = token_check {
                    if !fresh_token.needs_refresh() {
                        tracing::info!("Token was refreshed by another thread, using fresh token");
                        return Ok(Some(fresh_token));
                    }
                    // Update our local token in case it changed
                    token = fresh_token;
                }

                let expires_display = token
                    .expires_at
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "never".to_string());

                tracing::info!(
                    "OAuth token for {} needs refresh (expires: {}), attempting refresh...",
                    provider,
                    expires_display
                );

                // Refresh the token
                let config = match provider {
                    "anthropic" => crate::oauth::auth_flow::OAuthConfig::anthropic(),
                    _ => {
                        return Err(CoreError::OAuthError {
                            provider: provider.to_string(),
                            operation: "get_config".to_string(),
                            details: format!("Unknown OAuth provider: {}", provider),
                        });
                    }
                };

                let flow = DeviceAuthFlow::new(config);

                tracing::debug!(
                    "Attempting token refresh with refresh_token: {}",
                    if token.refresh_token.is_some() {
                        "[PRESENT]"
                    } else {
                        "[MISSING]"
                    }
                );

                match flow
                    .refresh_token(token.refresh_token.clone().unwrap())
                    .await
                {
                    Ok(token_response) => {
                        // Calculate new expiry
                        let new_expires_at = Utc::now()
                            + chrono::Duration::seconds(token_response.expires_in as i64);

                        tracing::info!(
                            "OAuth token refresh successful! New token expires at: {} ({} seconds from now)",
                            new_expires_at,
                            token_response.expires_in
                        );

                        // Update the token in database
                        // Only update refresh token if a new one was provided
                        let refresh_to_save = token_response
                            .refresh_token
                            .or_else(|| token.refresh_token.clone());

                        let updated_token = ProviderOAuthToken {
                            provider: provider.to_string(),
                            access_token: token_response.access_token,
                            refresh_token: refresh_to_save,
                            expires_at: Some(new_expires_at),
                            scope: token.scope.clone(),
                            session_id: token.session_id.clone(),
                            created_at: token.created_at,
                            updated_at: Utc::now(),
                        };

                        self.auth_db
                            .set_provider_oauth_token(&updated_token)
                            .await
                            .map_err(|e| CoreError::OAuthError {
                                provider: provider.to_string(),
                                operation: "update_oauth_token".to_string(),
                                details: format!("Failed to save refreshed token: {}", e),
                            })?;

                        token = updated_token;
                    }
                    Err(e) => {
                        tracing::error!("OAuth token refresh failed: {}", e);
                        return Err(e);
                    }
                }
            } else if token.needs_refresh() && token.refresh_token.is_none() {
                let expires_display = token
                    .expires_at
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "never".to_string());

                tracing::warn!(
                    "OAuth token for {} needs refresh but no refresh token available! Token expires: {}",
                    provider,
                    expires_display
                );
            }

            Ok(Some(token))
        } else {
            tracing::debug!("No OAuth token found for provider '{}'", provider);
            Ok(None)
        }
    }

    /// Create a genai client with OAuth support
    pub fn create_client(&self) -> Result<genai::Client, CoreError> {
        // Use the OAuth client builder
        super::resolver::OAuthClientBuilder::new(self.auth_db.clone()).build()
    }

    /// Start OAuth flow for a provider
    pub fn start_oauth_flow(
        &self,
        provider: &str,
    ) -> Result<(String, crate::oauth::PkceChallenge), CoreError> {
        let config = match provider {
            "anthropic" => crate::oauth::auth_flow::OAuthConfig::anthropic(),
            _ => {
                return Err(CoreError::OAuthError {
                    provider: provider.to_string(),
                    operation: "start_flow".to_string(),
                    details: format!("Unknown OAuth provider: {}", provider),
                });
            }
        };

        let flow = DeviceAuthFlow::new(config);
        Ok(flow.start_auth())
    }

    /// Complete OAuth flow with authorization code
    pub async fn complete_oauth_flow(
        &self,
        provider: &str,
        code: String,
        pkce_challenge: &crate::oauth::PkceChallenge,
    ) -> Result<ProviderOAuthToken, CoreError> {
        let config = match provider {
            "anthropic" => crate::oauth::auth_flow::OAuthConfig::anthropic(),
            _ => {
                return Err(CoreError::OAuthError {
                    provider: provider.to_string(),
                    operation: "complete_flow".to_string(),
                    details: format!("Unknown OAuth provider: {}", provider),
                });
            }
        };

        let flow = DeviceAuthFlow::new(config);
        let token_response = flow.exchange_code(code, pkce_challenge).await?;

        // Calculate expiry
        let expires_at = Utc::now() + chrono::Duration::seconds(token_response.expires_in as i64);
        let now = Utc::now();

        // Store the token
        let token = ProviderOAuthToken {
            provider: provider.to_string(),
            access_token: token_response.access_token,
            refresh_token: token_response.refresh_token,
            expires_at: Some(expires_at),
            scope: token_response.scope,
            session_id: None,
            created_at: now,
            updated_at: now,
        };

        self.auth_db
            .set_provider_oauth_token(&token)
            .await
            .map_err(|e| CoreError::OAuthError {
                provider: provider.to_string(),
                operation: "create_oauth_token".to_string(),
                details: format!("Failed to save token: {}", e),
            })?;

        Ok(token)
    }

    /// Revoke OAuth tokens for a provider
    pub async fn revoke_oauth(&self, provider: &str) -> Result<(), CoreError> {
        self.auth_db
            .delete_provider_oauth_token(provider)
            .await
            .map_err(|e| CoreError::OAuthError {
                provider: provider.to_string(),
                operation: "delete_oauth_token".to_string(),
                details: format!("Failed to delete token: {}", e),
            })?;
        Ok(())
    }
}
