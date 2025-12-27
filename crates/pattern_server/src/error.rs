//! Server error types

use axum::response::{IntoResponse, Response};
use pattern_api::ApiError;

pub type ServerResult<T> = Result<T, ServerError>;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    // #[error("Database error: {0}")]
    // Database(#[from] pattern_core::db_v1::DatabaseError),
    #[error("Core error: {0}")]
    Core(#[from] pattern_core::error::CoreError),

    #[error("API error: {0}")]
    Api(#[from] ApiError),

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("Password hashing error: {0}")]
    Argon2(#[from] argon2::password_hash::Error),

    #[error("Invalid address: {0}")]
    AddrParse(#[from] std::net::AddrParseError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Internal server error")]
    Internal,
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        // Convert to ApiError for consistent error responses
        let api_error = match self {
            // ServerError::Database(e) => ApiError::from(e),
            ServerError::Core(e) => ApiError::from(e),
            ServerError::Api(e) => e,
            ServerError::Jwt(_) => ApiError::Unauthorized {
                message: Some("Invalid or expired token".to_string()),
            },
            ServerError::Argon2(_) => ApiError::validation("Invalid password"),
            ServerError::Config(_msg) => ApiError::ServiceUnavailable {
                retry_after_seconds: None,
            },
            _ => ApiError::ServiceUnavailable {
                retry_after_seconds: Some(30),
            },
        };

        api_error.into_response()
    }
}
