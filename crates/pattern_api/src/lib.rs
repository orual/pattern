//! Pattern API types and definitions
//!
//! This crate defines the request/response types for the Pattern API,
//! shared between server and client implementations.

pub mod error;
pub mod events;
pub mod requests;
pub mod responses;

pub use error::ApiError;

// re-export for consumer crates, so that they don't have to import separately
pub use chrono;
pub use schemars;
pub use serde_json;
pub use uuid;

// Re-export common types from pattern-core
pub use pattern_core::agent::AgentState;
pub use pattern_core::id::{AgentId, GroupId, MessageId, UserId};
pub use pattern_core::messages::{ChatRole, Message, MessageContent};

/// API version constant
pub const API_VERSION: &str = "v1";

/// A hashed password that has been properly salted and hashed
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HashedPassword(String);

impl HashedPassword {
    /// Create a new hashed password from an already-hashed value
    /// This does NOT hash the input - it expects an already hashed value
    pub fn from_hash(hash: String) -> Self {
        Self(hash)
    }

    /// Get the hash string for storage
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A plaintext password that needs to be hashed
/// This type ensures we handle plaintext passwords carefully
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct PlaintextPassword(String);

impl PlaintextPassword {
    pub fn new(password: String) -> Self {
        Self(password)
    }

    /// Get the plaintext password for hashing
    /// This is intentionally not implementing Display or Deref to avoid accidental logging
    pub fn expose(&self) -> &str {
        &self.0
    }
}

// Explicitly no Serialize for PlaintextPassword - we never send passwords back
// Explicitly no Display/Debug with actual password content to avoid logging

/// JWT claims for access tokens
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccessTokenClaims {
    /// Subject (user ID)
    pub sub: UserId,
    /// Issued at
    pub iat: i64,
    /// Expiration time
    pub exp: i64,
    /// Token ID (for revocation)
    pub jti: uuid::Uuid,
    /// Token type
    pub token_type: String,
    /// Optional permissions (for API keys)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<requests::ApiPermission>>,
}

/// JWT claims for refresh tokens
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RefreshTokenClaims {
    /// Subject (user ID)
    pub sub: UserId,
    /// Issued at
    pub iat: i64,
    /// Expiration time
    pub exp: i64,
    /// Token ID (for revocation)
    pub jti: uuid::Uuid,
    /// Token type
    pub token_type: String,
    /// Token family (for refresh token rotation)
    pub family: uuid::Uuid,
}

/// HTTP method types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// API endpoint trait for request types
pub trait ApiEndpoint {
    /// The response type for this endpoint
    type Response;

    /// HTTP method for this endpoint
    const METHOD: Method;

    /// Path template for this endpoint (e.g., "/api/v1/users/{id}")
    const PATH: &'static str;
}

/// Parameter location for API requests
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamLocation {
    Path,
    Query,
    Body,
    Header,
}

/// Common metadata included in all responses
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResponseMetadata {
    /// API version
    pub version: String,
    /// Request ID for tracing
    pub request_id: uuid::Uuid,
    /// Timestamp of response
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl Default for ResponseMetadata {
    fn default() -> Self {
        Self {
            version: API_VERSION.to_string(),
            request_id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
        }
    }
}

/// Standard API response wrapper
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiResponse<T> {
    /// Response metadata
    pub meta: ResponseMetadata,
    /// Response data
    pub data: T,
}

impl<T> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self {
            meta: ResponseMetadata::default(),
            data,
        }
    }

    pub fn with_request_id(mut self, request_id: uuid::Uuid) -> Self {
        self.meta.request_id = request_id;
        self
    }
}

/// Pagination parameters
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PaginationParams {
    /// Page number (1-indexed)
    #[serde(default = "default_page")]
    pub page: u32,
    /// Items per page
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_page() -> u32 {
    1
}
fn default_limit() -> u32 {
    20
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            page: default_page(),
            limit: default_limit(),
        }
    }
}

/// Paginated response wrapper
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaginatedResponse<T> {
    /// Items in this page
    pub items: Vec<T>,
    /// Current page number
    pub page: u32,
    /// Items per page
    pub limit: u32,
    /// Total number of items
    pub total: u64,
    /// Total number of pages
    pub total_pages: u32,
}

impl<T> PaginatedResponse<T> {
    pub fn new(items: Vec<T>, page: u32, limit: u32, total: u64) -> Self {
        let total_pages = ((total as f64) / (limit as f64)).ceil() as u32;
        Self {
            items,
            page,
            limit,
            total,
            total_pages,
        }
    }
}
