//! Authentication handlers

use axum::extract::{Json, State};
use pattern_api::{ApiError, requests::AuthRequest, responses::AuthResponse};
use serde_json::json;

use crate::{
    auth::{generate_access_token, generate_refresh_token, verify_password},
    middleware::extract_bearer_token,
    models::ServerUser,
    state::AppState,
};

/// Handle login requests
pub async fn login(
    State(state): State<AppState>,
    Json(request): Json<AuthRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match request {
        AuthRequest::Password { username, password } => {
            // // Look up user by username
            // let result: Option<ServerUser> = state
            //     .db
            //     .query("SELECT * FROM user WHERE username = $username LIMIT 1")
            //     .bind(("username", username.to_string()))
            //     .await
            //     .map_err(|e| ApiError::Database {
            //         message: format!("Database error: {}", e),
            //         json: "".to_string(),
            //     })?
            //     .take(0)
            //     .map_err(|e| ApiError::Database {
            //         message: format!("Failed to fetch user: {}", e),
            //         json: "".to_string(),
            //     })?;

            // let user = result.ok_or_else(|| ApiError::Unauthorized {
            //     message: Some("Invalid username or password".to_string()),
            // })?;

            // // Verify password
            // if !verify_password(&password, &user.password_hash).map_err(|e| ApiError::Core {
            //     message: format!("Password verification failed: {}", e),
            //     json: "".to_string(),
            // })? {
            //     return Err(ApiError::Unauthorized {
            //         message: Some("Invalid username or password".to_string()),
            //     });
            // }

            // // Generate tokens
            // let user_id = user.id;
            // let token_family = uuid::Uuid::new_v4();

            // let access_token = generate_access_token(
            //     user_id.clone(),
            //     &state.jwt_encoding_key,
            //     state.config.access_token_ttl,
            // )
            // .map_err(|e| ApiError::Core {
            //     message: format!("Failed to generate access token: {}", e),
            //     json: "".to_string(),
            // })?;

            // let refresh_token = generate_refresh_token(
            //     user_id.clone(),
            //     token_family,
            //     &state.jwt_encoding_key,
            //     state.config.refresh_token_ttl,
            // )
            // .map_err(|e| ApiError::Core {
            //     message: format!("Failed to generate refresh token: {}", e),
            //     json: "".to_string(),
            // })?;

            Ok(Json(json!({})))
        }
        AuthRequest::ApiKey { api_key: _ } => {
            // TODO: Implement API key authentication
            Err(ApiError::ServiceUnavailable {
                retry_after_seconds: None,
            })
        }
    }
}

/// Handle token refresh requests
pub async fn refresh_token(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<AuthResponse>, ApiError> {
    // Extract refresh token from Authorization header
    let token = extract_bearer_token(&headers).ok_or_else(|| ApiError::Unauthorized {
        message: Some("Missing refresh token".to_string()),
    })?;

    // Validate refresh token
    let claims =
        crate::auth::validate_refresh_token(token, &state.jwt_decoding_key).map_err(|_| {
            ApiError::Unauthorized {
                message: Some("Invalid or expired refresh token".to_string()),
            }
        })?;

    // TODO: Check if token family is still valid (not revoked)
    // For now, we'll just generate new tokens

    // Generate new tokens with same family
    let access_token = generate_access_token(
        claims.sub.clone(),
        &state.jwt_encoding_key,
        state.config.access_token_ttl,
    )
    .map_err(|e| ApiError::Core {
        message: format!("Failed to generate access token: {}", e),
        json: "".to_string(),
    })?;

    let refresh_token = generate_refresh_token(
        claims.sub.clone(),
        claims.family, // Reuse same family
        &state.jwt_encoding_key,
        state.config.refresh_token_ttl,
    )
    .map_err(|e| ApiError::Core {
        message: format!("Failed to generate refresh token: {}", e),
        json: "".to_string(),
    })?;

    Ok(Json(AuthResponse {
        access_token,
        refresh_token,
        token_type: "Bearer".to_string(),
        expires_in: state.config.access_token_ttl,
        user: pattern_api::responses::UserResponse {
            // TODO: Load user from database
            id: claims.sub.clone(),
            username: "TODO".to_string(),
            display_name: None,
            email: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_active: true,
        },
    }))
}
