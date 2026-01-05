//! Database backend abstraction for Pattern
//!
//! This module provides traits and implementations for:
//! - Database connectivity (embedded and remote)
//! - Vector storage and similarity search
//! - Schema management and migrations

use async_trait::async_trait;
use miette::Diagnostic;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::sync::Arc;
use thiserror::Error;

pub mod client;
pub mod entity;
pub mod migration;
pub mod ops;
pub mod schema;

// Re-export commonly used types
pub use entity::{BaseEvent, BaseTask, DbEntity};
pub use entity::{BaseTaskPriority, BaseTaskStatus};
// Note: BaseAgent is replaced by AgentRecord from the agent module
pub use schema::{EnergyLevel, ToolCall};

// Local imports
use crate::error::EmbeddingError;
use crate::id::IdError;

/// Core database error type
#[derive(Error, Debug, Diagnostic)]
pub enum DatabaseError {
    #[error("Connection failed {0}")]
    #[diagnostic(help("Check your database configuration and ensure the database is running"))]
    ConnectionFailed(#[source] surrealdb::Error),

    #[error("Query failed {0}")]
    #[diagnostic(help("Check the query syntax and table schema"))]
    QueryFailed(#[source] surrealdb::Error),

    #[error("Query failed: {query} on {table}")]
    #[diagnostic(help("Check the query syntax, parameters, and table schema"))]
    QueryFailedContext {
        query: String,
        table: String,
        #[source]
        cause: surrealdb::Error,
    },

    #[error("Serde problem: {0}")]
    #[diagnostic(help("Check the query syntax and table schema"))]
    SerdeProblem(#[from] serde_json::Error),

    #[error("Transaction failed {0}")]
    TransactionFailed(#[source] surrealdb::Error),

    #[error("Embedding model mismatch: database has {db_model}, config specifies {config_model}")]
    #[diagnostic(help(
        "To change embedding models, you must re-embed all data. Consider creating a new database or running a migration."
    ))]
    EmbeddingModelMismatch {
        db_model: String,
        config_model: String,
    },

    #[error("Error with embedding: {0}")]
    EmbeddingError(#[from] EmbeddingError),

    #[error("Schema version mismatch: database is at v{db_version}, code expects v{code_version}")]
    #[diagnostic(help("Run migrations to update the database schema"))]
    SchemaVersionMismatch { db_version: u32, code_version: u32 },

    #[error("Record not found: {entity_type} with id {id}")]
    NotFound { entity_type: String, id: String },

    #[error("Invalid vector dimensions: expected {expected}, got {actual}")]
    #[diagnostic(help("Ensure all embeddings use the same model and dimensions"))]
    InvalidVectorDimensions { expected: usize, actual: usize },

    #[error("SurrealDB JSON deserialization error")]
    #[diagnostic(code(pattern_core::surreal_json_value_error), help("{help}"))]
    SurrealJsonValueError {
        #[source]
        original: surrealdb::Error,
        help: String,
    },
    #[error("Error: {0}")]
    Other(String),
}

impl From<IdError> for DatabaseError {
    fn from(err: IdError) -> Self {
        DatabaseError::Other(err.to_string())
    }
}

impl From<entity::EntityError> for DatabaseError {
    fn from(err: entity::EntityError) -> Self {
        use entity::EntityError;
        match err {
            EntityError::InvalidId(e) => DatabaseError::Other(e.to_string()),
            EntityError::Serialization(e) => DatabaseError::SerdeProblem(e),
            EntityError::Database(e) => DatabaseError::QueryFailed(e),
            EntityError::Validation { message, .. } => DatabaseError::Other(message),
            EntityError::NotFound { entity_type, id } => {
                DatabaseError::NotFound { entity_type, id }
            }
            EntityError::RequiredFieldMissing { field, entity_type } => DatabaseError::Other(
                format!("Missing required field '{}' for {}", field, entity_type),
            ),
        }
    }
}

impl From<surrealdb::Error> for DatabaseError {
    fn from(err: surrealdb::Error) -> Self {
        // Check if it's the dreaded json::Value error
        let error_str = err.to_string();
        if error_str.contains("invalid type: enum")
            && error_str.contains("expected any valid JSON value")
        {
            DatabaseError::SurrealJsonValueError {
                original: err,
                help: "Cannot .take(0) from a SurrealDB response as a serde_json::Value. \
                                 Take the actual type (the DbModel type if this derives Entity) or print the raw Response for debugging."
                    .to_string(),
            }
        } else {
            DatabaseError::QueryFailed(err)
        }
    }
}

impl DatabaseError {
    /// Attach query/table context to a low-level query error.
    /// If this error is not a query error, returns self unchanged.
    pub fn with_context(self, query: impl Into<String>, table: impl Into<String>) -> Self {
        match self {
            DatabaseError::QueryFailed(e) => DatabaseError::QueryFailedContext {
                query: query.into(),
                table: table.into(),
                cause: e,
            },
            other => other,
        }
    }
}

pub type Result<T> = std::result::Result<T, DatabaseError>;

/// Configuration for database backends
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DatabaseConfig {
    /// Embedded database using SurrealKV
    Embedded {
        /// Path to the database file (defaults to "./pattern.db")
        #[serde(default = "default_db_path")]
        path: String,
        /// Whether to enforce strict schema validation
        #[serde(default)]
        strict_mode: bool,
    },
    /// Remote database connection (requires surreal-remote feature)
    #[cfg(feature = "surreal-remote")]
    Remote {
        /// Database server URL (e.g., "ws://localhost:8000")
        url: String,
        /// Optional username for authentication
        #[serde(default)]
        username: Option<String>,
        /// Optional password for authentication
        #[serde(default)]
        password: Option<String>,
        /// SurrealDB namespace to use
        namespace: String,
        /// SurrealDB database to use within the namespace
        database: String,
    },
}

fn default_db_path() -> String {
    "./pattern.db".to_string()
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseConfig::Embedded { path, strict_mode } => f
                .debug_struct("Embedded")
                .field("path", path)
                .field("strict_mode", strict_mode)
                .finish(),
            #[cfg(feature = "surreal-remote")]
            DatabaseConfig::Remote {
                url,
                username,
                password,
                namespace,
                database,
            } => f
                .debug_struct("Remote")
                .field("url", url)
                .field("username", username)
                .field("password", &password.as_ref().map(|_| "***REDACTED***"))
                .field("namespace", namespace)
                .field("database", database)
                .finish(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        DatabaseConfig::Embedded {
            path: default_db_path(),
            strict_mode: false,
        }
    }
}

/// A database query result
#[derive(Debug)]
pub struct QueryResponse {
    /// Number of rows affected by the query (for INSERT/UPDATE/DELETE)
    pub affected_rows: usize,
    /// The result data as JSON (for SELECT queries)
    pub data: serde_json::Value,
}

/// Search result from vector similarity search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
    /// ID of the matching record
    pub id: String,
    /// Similarity score (higher is more similar, typically 0-1 for cosine)
    pub score: f32,
    /// The full record data as JSON
    pub data: serde_json::Value,
}

/// Filter for vector searches
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Optional SQL WHERE clause to filter results (e.g., "status = 'active'")
    pub where_clause: Option<String>,
    /// Parameters for the WHERE clause to prevent SQL injection
    pub params: Vec<(String, serde_json::Value)>,
}

/// Core database operations
#[async_trait]
pub trait DatabaseBackend: Send + Sync {
    /// Connect to the database with the given configuration
    async fn connect(config: DatabaseConfig) -> Result<Arc<Self>>
    where
        Self: Sized;

    /// Execute a raw query
    async fn execute(
        &self,
        query: &str,
        params: Vec<(String, serde_json::Value)>,
    ) -> Result<QueryResponse>;

    /// Execute a query expecting a single result
    async fn query_one<T: DeserializeOwned>(
        &self,
        query: &str,
        params: Vec<(String, serde_json::Value)>,
    ) -> Result<Option<T>>;

    /// Execute a query expecting multiple results
    async fn query_many<T: DeserializeOwned>(
        &self,
        query: &str,
        params: Vec<(String, serde_json::Value)>,
    ) -> Result<Vec<T>>;

    /// Check if the database is healthy
    async fn health_check(&self) -> Result<()>;

    /// Get the current schema version
    async fn schema_version(&self) -> Result<u32>;
}

/// Database operations that require generics (not object-safe)
#[async_trait]
pub trait DatabaseOperations: DatabaseBackend {
    /// Begin a transaction
    async fn transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(Arc<dyn Transaction>) -> Result<R> + Send,
        R: Send;
}

/// Query builder for type-safe queries
pub struct Query<'a> {
    query: String,
    params: Vec<(&'a str, serde_json::Value)>,
}

impl<'a> Query<'a> {
    /// Create a new query builder
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            params: Vec::new(),
        }
    }

    /// Bind a parameter to the query
    pub fn bind<T: Serialize>(mut self, name: &'a str, value: T) -> Result<Self> {
        let json_value = serde_json::to_value(value)?;
        self.params.push((name, json_value));
        Ok(self)
    }

    /// Execute the query expecting a single result
    pub async fn query_one<T: DeserializeOwned, DB: DatabaseBackend + ?Sized>(
        self,
        db: &DB,
    ) -> Result<Option<T>> {
        let params = self
            .params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        db.query_one(&self.query, params).await
    }

    /// Execute the query expecting multiple results
    pub async fn query_many<T: DeserializeOwned, DB: DatabaseBackend + ?Sized>(
        self,
        db: &DB,
    ) -> Result<Vec<T>> {
        let params = self
            .params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        db.query_many(&self.query, params).await
    }

    /// Execute the query without expecting typed results
    pub async fn execute<DB: DatabaseBackend + ?Sized>(self, db: &DB) -> Result<QueryResponse> {
        let params = self
            .params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        db.execute(&self.query, params).await
    }
}

/// Transaction handle
#[async_trait]
pub trait Transaction: Send + Sync {
    /// Execute a query within the transaction
    async fn execute(
        &self,
        query: &str,
        params: Vec<(String, serde_json::Value)>,
    ) -> Result<QueryResponse>;

    /// Commit the transaction
    async fn commit(self: Box<Self>) -> Result<()>;

    /// Rollback the transaction
    async fn rollback(self: Box<Self>) -> Result<()>;
}

/// Vector storage and search operations
#[async_trait]
pub trait VectorStore: DatabaseBackend + DatabaseOperations {
    /// Search for similar vectors
    async fn vector_search(
        &self,
        table: &str,
        embedding_field: &str,
        query_vector: &[f32],
        limit: usize,
        filter: Option<SearchFilter>,
    ) -> Result<Vec<VectorSearchResult>>;

    /// Create a vector index
    async fn create_vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: usize,
        distance_metric: DistanceMetric,
    ) -> Result<()>;

    /// Check if a vector index exists
    async fn vector_index_exists(&self, table: &str, field: &str) -> Result<bool>;
}

/// Distance metrics for vector similarity
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    Cosine,
    Euclidean,
    Manhattan,
}

impl DistanceMetric {
    pub fn as_surreal_string(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "COSINE",
            DistanceMetric::Euclidean => "EUCLIDEAN",
            DistanceMetric::Manhattan => "MANHATTAN",
        }
    }
}

/// System metadata stored in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetadata {
    /// The embedding model used for vector storage
    pub embedding_model: String,
    /// Number of dimensions in the embedding vectors
    pub embedding_dimensions: usize,
    /// Current database schema version
    pub schema_version: u32,
    /// When the database was created
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the database was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Load system metadata from the database
pub async fn load_metadata<DB: DatabaseBackend>(db: &DB) -> Result<Option<SystemMetadata>> {
    let response = db
        .execute("SELECT * FROM system_metadata LIMIT 1", vec![])
        .await?;

    if let Some(data) = response.data.as_array().and_then(|arr| arr.first()) {
        Ok(Some(serde_json::from_value(data.clone())?))
    } else {
        Ok(None)
    }
}

/// Initialize or validate the database schema
pub async fn initialize_schema<DB: DatabaseBackend>(
    db: &DB,
    embedding_model: &str,
    embedding_dimensions: usize,
) -> Result<()> {
    let metadata = load_metadata(db).await?;

    if let Some(metadata) = metadata {
        if metadata.embedding_model != embedding_model {
            return Err(DatabaseError::EmbeddingModelMismatch {
                db_model: metadata.embedding_model,
                config_model: embedding_model.to_string(),
            });
        }
        if metadata.embedding_dimensions != embedding_dimensions {
            return Err(DatabaseError::InvalidVectorDimensions {
                expected: metadata.embedding_dimensions,
                actual: embedding_dimensions,
            });
        }
    } else {
        // First time setup
        create_metadata(db, embedding_model, embedding_dimensions).await?;
    }

    Ok(())
}

/// Create initial system metadata
async fn create_metadata<DB: DatabaseBackend>(
    db: &DB,
    embedding_model: &str,
    embedding_dimensions: usize,
) -> Result<()> {
    let now = chrono::Utc::now();
    let metadata = SystemMetadata {
        embedding_model: embedding_model.to_string(),
        embedding_dimensions,
        schema_version: 1,
        created_at: now,
        updated_at: now,
    };

    db.execute(
        "CREATE system_metadata CONTENT $metadata",
        vec![(
            "metadata".to_string(),
            serde_json::to_value(&metadata).unwrap(),
        )],
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distance_metric_serialization() {
        let metric = DistanceMetric::Cosine;
        let json = serde_json::to_string(&metric).unwrap();
        assert_eq!(json, "\"cosine\"");

        let parsed: DistanceMetric = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DistanceMetric::Cosine));
    }

    #[test]
    fn test_database_config_default() {
        let config = DatabaseConfig::default();
        match config {
            DatabaseConfig::Embedded { path, strict_mode } => {
                assert_eq!(path, "./pattern.db");
                assert!(!strict_mode);
            }
            #[cfg(feature = "surreal-remote")]
            _ => panic!("Expected embedded config"),
        }
    }
}

/// Strip SurrealDB's angle brackets from record IDs (⟨id⟩ -> id)
pub fn strip_brackets(s: &str) -> &str {
    s.strip_prefix('⟨')
        .and_then(|s| s.strip_suffix('⟩'))
        .unwrap_or(s)
}

/// Strip SurrealDB's datetime prefix/suffix (d'2024-01-01T00:00:00Z' -> 2024-01-01T00:00:00Z)
pub fn strip_dt(s: &str) -> &str {
    s.strip_prefix("d'")
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s)
}

/// Convert SurrealDB's Datetime type to a chrono DateTime
pub fn from_surreal_datetime(dt: surrealdb::Datetime) -> chrono::DateTime<chrono::Utc> {
    let datetime = chrono::NaiveDateTime::parse_from_str(&dt.to_string(), "d'%FT%T%.fZ'")
        .expect("should be valid ISO-8601");

    datetime.and_utc()
}
