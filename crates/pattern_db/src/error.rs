//! Error types for the database layer.

use miette::Diagnostic;
use thiserror::Error;

/// Result type alias for database operations.
pub type DbResult<T> = Result<T, DbError>;

/// Database error types.
#[derive(Debug, Error, Diagnostic)]
pub enum DbError {
    /// SQLite/sqlx error
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// Migration error
    #[error("Migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// Loro document error
    #[error("Loro error: {0}")]
    Loro(String),

    /// Entity not found
    #[error("{entity_type} not found: {id}")]
    NotFound {
        entity_type: &'static str,
        id: String,
    },

    /// Duplicate entity
    #[error("{entity_type} already exists: {id}")]
    AlreadyExists {
        entity_type: &'static str,
        id: String,
    },

    /// Invalid data
    #[error("Invalid data: {message}")]
    InvalidData { message: String },

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// IO error (for filesystem operations if needed)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Constraint violation
    #[error("Constraint violation: {message}")]
    ConstraintViolation { message: String },

    /// SQLite extension error
    #[error("Extension error: {0}")]
    #[diagnostic(help("Ensure sqlite-vec is properly initialized before database operations"))]
    Extension(String),
}

impl DbError {
    /// Create a not found error.
    pub fn not_found(entity_type: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity_type,
            id: id.into(),
        }
    }

    /// Create an already exists error.
    pub fn already_exists(entity_type: &'static str, id: impl Into<String>) -> Self {
        Self::AlreadyExists {
            entity_type,
            id: id.into(),
        }
    }

    /// Create an invalid data error.
    pub fn invalid_data(message: impl Into<String>) -> Self {
        Self::InvalidData {
            message: message.into(),
        }
    }

    /// Create a loro error.
    pub fn loro(message: impl Into<String>) -> Self {
        Self::Loro(message.into())
    }
}
