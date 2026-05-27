// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Error types for the database layer.

use miette::Diagnostic;
use thiserror::Error;

/// Result type alias for database operations.
pub type DbResult<T> = Result<T, DbError>;

/// Database error types.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum DbError {
    /// rusqlite error from a query or connection operation.
    #[error("database error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    /// r2d2 pool error (timeout, exhaustion, init failure).
    #[error("connection pool error: {0}")]
    Pool(#[from] r2d2::Error),

    /// Schema migration error.
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),

    /// Loro document error.
    #[error("loro error: {0}")]
    Loro(String),

    /// Entity not found.
    #[error("{entity_type} not found: {id}")]
    NotFound {
        entity_type: &'static str,
        id: String,
    },

    /// Duplicate entity.
    #[error("{entity_type} already exists: {id}")]
    AlreadyExists {
        entity_type: &'static str,
        id: String,
    },

    /// Invalid data.
    #[error("invalid data: {message}")]
    InvalidData { message: String },

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// IO error (for filesystem operations if needed).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Constraint violation.
    #[error("constraint violation: {message}")]
    ConstraintViolation { message: String },

    /// SQLite extension load/init error.
    #[error("extension error: {0}")]
    #[diagnostic(help("ensure sqlite-vec is properly initialized before database operations"))]
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
