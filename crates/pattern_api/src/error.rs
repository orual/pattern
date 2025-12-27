//! API error types

use miette::{Diagnostic, JSONReportHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// API error response
#[derive(Debug, thiserror::Error, Diagnostic, Serialize, Deserialize)]
pub enum ApiError {
    /// Request validation failed
    #[error("Validation failed: {message}")]
    #[diagnostic(
        code(api::validation_error),
        help("Check the field errors for specific validation issues")
    )]
    ValidationError {
        message: String,
        fields: Option<Vec<FieldError>>,
    },

    /// Authentication required
    #[error("Authentication required")]
    #[diagnostic(
        code(api::unauthorized),
        help("Please provide valid authentication credentials")
    )]
    Unauthorized { message: Option<String> },

    /// Insufficient permissions
    #[error("Insufficient permissions")]
    #[diagnostic(
        code(api::forbidden),
        help("You need the '{required_permission}' permission to perform this action")
    )]
    Forbidden { required_permission: String },

    /// Resource not found
    #[error("Resource not found: {resource_type}")]
    #[diagnostic(
        code(api::not_found),
        help("The {resource_type} with ID '{resource_id}' does not exist")
    )]
    NotFound {
        resource_type: String,
        resource_id: String,
    },

    /// Conflict with existing resource
    #[error("Resource conflict")]
    #[diagnostic(
        code(api::conflict),
        help("The resource already exists or is in a conflicting state")
    )]
    Conflict { message: String },

    /// Rate limit exceeded
    #[error("Rate limit exceeded")]
    #[diagnostic(
        code(api::rate_limit),
        help("Please wait {retry_after_seconds} seconds before retrying")
    )]
    RateLimitExceeded { retry_after_seconds: u64 },

    /// Database error from pattern-core
    #[error("{message}")]
    #[diagnostic(code(api::database_error), help("Database operation failed"))]
    Database { message: String, json: String },

    /// Core error from pattern-core
    #[error("{message}")]
    #[diagnostic(code(api::core_error), help("Core operation failed"))]
    Core { message: String, json: String },

    /// JSON error
    #[error("{message}")]
    #[diagnostic(
        code(api::json_error),
        help("Check that your JSON is valid and matches the expected schema")
    )]
    Json { message: String, json: String },

    /// Invalid UUID
    #[error("Invalid UUID: {0}")]
    #[diagnostic(code(api::invalid_uuid), help("The provided ID must be a valid UUID"))]
    Uuid(String),

    /// Invalid date/time
    #[error("Invalid date/time: {0}")]
    #[diagnostic(
        code(api::datetime_error),
        help("Check that your date/time format is valid (RFC3339 format expected)")
    )]
    DateTime(String),

    /// HTTP header error (for server feature)
    #[cfg(feature = "server")]
    #[error("Invalid header: {0}")]
    #[diagnostic(
        code(api::header_error),
        help("Check that your HTTP headers are valid")
    )]
    HeaderError(String),

    /// HTTP method error (for server feature)
    #[cfg(feature = "server")]
    #[error("Invalid method: {0}")]
    #[diagnostic(code(api::method_error), help("The HTTP method is not valid"))]
    MethodError(String),

    /// Service temporarily unavailable
    #[error("Service temporarily unavailable")]
    #[diagnostic(
        code(api::service_unavailable),
        help("The service is temporarily down for maintenance")
    )]
    ServiceUnavailable { retry_after_seconds: Option<u64> },
}

/// Field-level validation error
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl ApiError {
    /// Get HTTP status code for this error
    pub fn status_code(&self) -> u16 {
        match self {
            ApiError::ValidationError { .. } => 400,
            ApiError::Unauthorized { .. } => 401,
            ApiError::Forbidden { .. } => 403,
            ApiError::NotFound { .. } => 404,
            ApiError::Conflict { .. } => 409,
            ApiError::RateLimitExceeded { .. } => 429,
            ApiError::ServiceUnavailable { .. } => 503,

            // Pattern-core errors
            ApiError::Database { .. } => 500,
            ApiError::Core { .. } => 500,

            // External errors
            ApiError::Json { .. } => 400,
            ApiError::Uuid(_) => 400,
            ApiError::DateTime(_) => 400,
            #[cfg(feature = "server")]
            ApiError::HeaderError(_) => 400,
            #[cfg(feature = "server")]
            ApiError::MethodError(_) => 400,
        }
    }

    /// Create a validation error with field details
    pub fn validation(message: impl Into<String>) -> Self {
        Self::ValidationError {
            message: message.into(),
            fields: None,
        }
    }

    /// Create a validation error with field-specific errors
    pub fn validation_with_fields(message: impl Into<String>, fields: Vec<FieldError>) -> Self {
        Self::ValidationError {
            message: message.into(),
            fields: Some(fields),
        }
    }

    /// Create a not found error
    pub fn not_found(resource_type: impl Into<String>, resource_id: impl Into<String>) -> Self {
        Self::NotFound {
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
        }
    }
}

impl From<pattern_core::error::CoreError> for ApiError {
    fn from(err: pattern_core::error::CoreError) -> Self {
        let handler = JSONReportHandler::new();

        let message = format!("{}", err);
        let mut json = String::new();

        let err: Box<dyn Diagnostic> = Box::new(err);
        handler
            .render_report(&mut json, err.as_ref())
            .unwrap_or_default();

        Self::Core { message, json }
    }
}

#[cfg(feature = "server")]
impl From<axum::http::header::InvalidHeaderValue> for ApiError {
    fn from(err: axum::http::header::InvalidHeaderValue) -> Self {
        Self::HeaderError(err.to_string())
    }
}

#[cfg(feature = "server")]
impl From<axum::http::method::InvalidMethod> for ApiError {
    fn from(err: axum::http::method::InvalidMethod) -> Self {
        Self::MethodError(err.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        // Create a miette diagnostic for better error reporting
        let diagnostic = miette::miette!(
            code = "json::parse_error",
            help = "Check that your JSON is valid",
            "{}",
            err
        );

        let handler = JSONReportHandler::new();
        let message = err.to_string();
        let mut json = String::new();

        handler
            .render_report(&mut json, diagnostic.as_ref())
            .unwrap_or_default();

        Self::Json { message, json }
    }
}

impl From<uuid::Error> for ApiError {
    fn from(err: uuid::Error) -> Self {
        // For now just convert to string - could enhance later
        Self::Uuid(err.to_string())
    }
}

impl From<chrono::ParseError> for ApiError {
    fn from(err: chrono::ParseError) -> Self {
        // For now just convert to string - could enhance later
        Self::DateTime(err.to_string())
    }
}

// Server-side response conversion
#[cfg(feature = "server")]
impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        use axum::Json;
        use axum::http::StatusCode;

        let status =
            StatusCode::from_u16(self.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        // Convert error to a serializable format
        let error_message = self.to_string();
        let error_type = match &self {
            ApiError::ValidationError { .. } => "validation_error",
            ApiError::Unauthorized { .. } => "unauthorized",
            ApiError::Forbidden { .. } => "forbidden",
            ApiError::NotFound { .. } => "not_found",
            ApiError::Conflict { .. } => "conflict",
            ApiError::RateLimitExceeded { .. } => "rate_limit",
            ApiError::Database { .. } => "database_error",
            ApiError::Core { .. } => "core_error",
            ApiError::Json { .. } => "json_error",
            ApiError::Uuid(_) => "invalid_uuid",
            ApiError::DateTime(_) => "datetime_error",
            #[cfg(feature = "server")]
            ApiError::HeaderError(_) => "header_error",
            #[cfg(feature = "server")]
            ApiError::MethodError(_) => "method_error",
            ApiError::ServiceUnavailable { .. } => "service_unavailable",
        };

        // Extract detail if available
        let detail = match &self {
            ApiError::Database { json, .. } => Some(json),
            ApiError::Core { json, .. } => Some(json),
            ApiError::Json { json, .. } => Some(json),
            _ => None,
        };

        // Create error response body with optional detail
        let mut error_obj = serde_json::json!({
            "type": error_type,
            "message": error_message,
        });

        if let Some(d) = detail {
            error_obj["detail"] = serde_json::to_value(d).unwrap_or_default();
        }

        let body = serde_json::json!({
            "error": error_obj,
            "timestamp": chrono::Utc::now(),
        });

        (status, Json(body)).into_response()
    }
}
