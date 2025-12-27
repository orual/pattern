//! Entity trait system for extensible database models
//!
//! This module provides a trait-based system that allows other crates to define
//! their own database entities without modifying pattern-core. The core crate
//! provides base implementations for common entities (User, Agent, Task, etc.)
//! while domain-specific crates (like pattern-nd) can extend these with their
//! own fields and behavior.

use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use super::schema::TableDefinition;
// Local import
use crate::id::IdType;

/// Error type for entity operations
#[derive(Debug, thiserror::Error)]
pub enum EntityError {
    #[error("Failed to parse ID: {0}")]
    InvalidId(#[from] crate::id::IdError),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    Database(#[from] surrealdb::Error),

    #[error("Validation error: {message}")]
    Validation {
        message: String,
        field: Option<String>,
        expected: Option<String>,
        actual: Option<String>,
    },

    #[error("Entity not found: {entity_type} with id {id}")]
    NotFound { entity_type: String, id: String },

    #[error("Field missing: {field} is required for {entity_type}")]
    RequiredFieldMissing { field: String, entity_type: String },
}

impl EntityError {
    /// Create a validation error with just a message
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            field: None,
            expected: None,
            actual: None,
        }
    }

    /// Create a validation error for a specific field
    pub fn field_validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            field: Some(field.into()),
            expected: None,
            actual: None,
        }
    }

    /// Create a validation error with expected and actual values
    pub fn validation_mismatch(
        field: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        let field_str = field.into();
        Self::Validation {
            message: format!("Field validation failed for {}", &field_str),
            field: Some(field_str),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
        }
    }

    /// Create a not found error
    pub fn not_found(entity_type: impl Into<String>, id: impl ToString) -> Self {
        Self::NotFound {
            entity_type: entity_type.into(),
            id: id.to_string(),
        }
    }

    /// Create a required field missing error
    pub fn required_field(field: impl Into<String>, entity_type: impl Into<String>) -> Self {
        Self::RequiredFieldMissing {
            field: field.into(),
            entity_type: entity_type.into(),
        }
    }
}

/// Result type for entity operations
pub type Result<T> = std::result::Result<T, EntityError>;

/// Core trait for all database entities
///
/// This trait defines the contract between domain models (what the application uses)
/// and database models (what gets stored). It enables:
/// - Type-safe database operations
/// - Schema generation
/// - Clean separation between storage and domain logic
pub trait DbEntity: Send + Sync {
    /// The database model type (what gets stored)
    type DbModel: for<'de> Deserialize<'de> + Serialize + Send + Sync + Debug + 'static;

    /// The domain type (what the app uses)
    type Domain: Send + Sync + Debug + 'static;

    /// The ID type for this entity
    type Id: IdType + Debug;

    /// Convert from domain to database model
    fn to_db_model(&self) -> Self::DbModel;

    /// Convert from database model to domain
    fn from_db_model(db_model: Self::DbModel) -> std::result::Result<Self::Domain, EntityError>;

    /// Get the table name for this entity
    fn table_name() -> &'static str;

    fn id(&self) -> &Self::Id;

    /// Get the record key for storing in the database
    /// Default implementation uses the ID's key representation
    fn record_key(&self) -> String {
        self.id().to_key()
    }

    /// Get the schema definition for this entity
    fn schema() -> TableDefinition;

    fn field_keys() -> Vec<String>;
}

pub trait HasRecordId {
    fn id(&self);
    fn record_id(&self);
}

// Re-export base entity implementations
mod base;
pub use base::*;
