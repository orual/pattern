//! Error types for pattern_auth.

use miette::Diagnostic;
use thiserror::Error;

/// Result type for auth operations.
pub type AuthResult<T> = Result<T, AuthError>;

/// Errors that can occur in auth operations.
#[derive(Debug, Error, Diagnostic)]
pub enum AuthError {
    /// Database error from sqlx.
    #[error("Database error: {0}")]
    #[diagnostic(code(pattern_auth::database))]
    Database(#[from] sqlx::Error),

    /// Migration error.
    #[error("Migration error: {0}")]
    #[diagnostic(code(pattern_auth::migration))]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// IO error.
    #[error("IO error: {0}")]
    #[diagnostic(code(pattern_auth::io))]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("Serialization error: {0}")]
    #[diagnostic(code(pattern_auth::serde))]
    Serde(#[from] serde_json::Error),

    /// Session not found.
    #[error("Session not found: {did} / {session_id}")]
    #[diagnostic(code(pattern_auth::session_not_found))]
    SessionNotFound { did: String, session_id: String },

    /// Auth request not found (PKCE state).
    #[error("Auth request not found for state: {state}")]
    #[diagnostic(code(pattern_auth::auth_request_not_found))]
    AuthRequestNotFound { state: String },

    /// Discord config not found.
    #[error("Discord bot configuration not found")]
    #[diagnostic(code(pattern_auth::discord_config_not_found))]
    DiscordConfigNotFound,

    /// Provider OAuth token not found.
    #[error("OAuth token not found for provider: {provider}")]
    #[diagnostic(code(pattern_auth::provider_token_not_found))]
    ProviderTokenNotFound { provider: String },

    /// Invalid DID format.
    #[error("Invalid DID: {0}")]
    #[diagnostic(code(pattern_auth::invalid_did))]
    InvalidDid(String),
}

// Convert to Jacquard's SessionStoreError.
// Map to specific variants where possible, only use Other for truly other errors.
impl From<AuthError> for jacquard::session::SessionStoreError {
    fn from(err: AuthError) -> Self {
        use jacquard::session::SessionStoreError;
        match err {
            // Direct mappings to SessionStoreError variants
            AuthError::Io(e) => SessionStoreError::Io(e),
            AuthError::Serde(e) => SessionStoreError::Serde(e),
            // All other errors go to Other
            other => SessionStoreError::Other(Box::new(other)),
        }
    }
}
