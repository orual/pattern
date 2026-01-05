//! Error types for database compatibility
//!
//! This module contains error types needed by the db and export modules.

use crate::db::{DatabaseError, entity::EntityError};
use compact_str::CompactString;
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Configuration-specific errors
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("TOML parse error: {0}")]
    TomlParse(String),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(String),

    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid value for field {field}: {reason}")]
    InvalidValue { field: String, reason: String },
}

#[derive(Error, Diagnostic, Debug)]
pub enum CoreError {
    #[error("Agent initialization failed")]
    #[diagnostic(
        code(pattern_core::agent_init_failed),
        help("Check the agent configuration and ensure all required fields are provided")
    )]
    AgentInitFailed { agent_type: String, cause: String },

    #[error("Agent {agent_id} processing failed: {details}")]
    #[diagnostic(
        code(pattern_core::agent_processing),
        help("Agent encountered an error during stream processing")
    )]
    AgentProcessing { agent_id: String, details: String },

    #[error("Memory block not found")]
    #[diagnostic(
        code(pattern_core::memory_not_found),
        help("The requested memory block doesn't exist for this agent")
    )]
    MemoryNotFound {
        agent_id: String,
        block_name: String,
        available_blocks: Vec<CompactString>,
    },

    #[error("Tool not found")]
    #[diagnostic(
        code(pattern_core::tool_not_found),
        help("Available tools: {}", available_tools.join(", "))
    )]
    ToolNotFound {
        tool_name: String,
        available_tools: Vec<String>,
        #[source_code]
        src: String,
        #[label("unknown tool")]
        span: (usize, usize),
    },

    #[error("Tool execution failed")]
    #[diagnostic(
        code(pattern_core::tool_execution_failed),
        help("Check tool parameters and ensure they match the expected schema")
    )]
    ToolExecutionFailed {
        tool_name: String,
        cause: String,
        parameters: serde_json::Value,
    },

    #[error("Invalid tool parameters for {tool_name}")]
    #[diagnostic(
        code(pattern_core::invalid_tool_params),
        help("Expected schema: {expected_schema}")
    )]
    InvalidToolParameters {
        tool_name: String,
        expected_schema: serde_json::Value,
        provided_params: serde_json::Value,
        validation_errors: Vec<String>,
    },

    #[error("Database connection failed")]
    #[diagnostic(
        code(pattern_core::database_connection_failed),
        help("Ensure SurrealDB is running at {connection_string}")
    )]
    DatabaseConnectionFailed {
        connection_string: String,
        #[source]
        cause: surrealdb::Error,
    },

    #[error("Database query failed")]
    #[diagnostic(code(pattern_core::database_query_failed), help("Query: {query}"))]
    DatabaseQueryFailed {
        query: String,
        table: String,
        #[source]
        cause: surrealdb::Error,
    },

    #[error("Serialization error")]
    #[diagnostic(
        code(pattern_core::serialization_error),
        help("Failed to serialize/deserialize {data_type}")
    )]
    SerializationError {
        data_type: String,
        #[source]
        cause: serde_json::Error,
    },

    #[error("Configuration error for field '{field}'")]
    #[diagnostic(
        code(pattern_core::configuration_error),
        help("Check configuration file at {config_path}\nExpected: {expected}")
    )]
    ConfigurationError {
        config_path: String,
        field: String,
        expected: String,
        #[source]
        cause: ConfigError,
    },

    #[error("Agent coordination failed")]
    #[diagnostic(
        code(pattern_core::coordination_failed),
        help("Coordination pattern '{pattern}' failed for group '{group}'")
    )]
    CoordinationFailed {
        group: String,
        pattern: String,
        participating_agents: Vec<String>,
        cause: String,
    },

    #[error("Vector search failed")]
    #[diagnostic(
        code(pattern_core::vector_search_failed),
        help("Failed to perform semantic search on {collection}")
    )]
    VectorSearchFailed {
        collection: String,
        dimension_mismatch: Option<(usize, usize)>,
        #[source]
        cause: EmbeddingError,
    },

    #[error("Agent group error")]
    #[diagnostic(
        code(pattern_core::agent_group_error),
        help("Operation failed for agent group '{group_name}'")
    )]
    AgentGroupError {
        group_name: String,
        operation: String,
        cause: String,
    },

    #[error("OAuth authentication error: {operation} failed for {provider}")]
    #[diagnostic(
        code(pattern_core::oauth_error),
        help("Check OAuth configuration and ensure tokens are valid")
    )]
    OAuthError {
        provider: String,
        operation: String,
        details: String,
    },

    #[error("Data source error in {source_name}: {operation} failed - {cause}")]
    #[diagnostic(
        code(pattern_core::data_source_error),
        help("Check data source configuration and connectivity")
    )]
    DataSourceError {
        source_name: String,
        operation: String,
        cause: String,
    },

    #[error("DAG-CBOR encoding error")]
    #[diagnostic(
        code(pattern_core::dagcbor_encoding_error),
        help("Failed to encode data as DAG-CBOR")
    )]
    DagCborEncodingError {
        data_type: String,
        #[source]
        cause: serde_ipld_dagcbor::error::EncodeError<std::collections::TryReserveError>,
    },

    #[error("Failed to decode DAG-CBOR data for {data_type}:\n {details}")]
    #[diagnostic(
        code(pattern_core::dagcbor_decoding_error),
        help("Failed to decode data from DAG-CBOR: {details}")
    )]
    DagCborDecodingError { data_type: String, details: String },

    #[error("CAR archive error: {operation} failed")]
    #[diagnostic(
        code(pattern_core::car_error),
        help("Check CAR file format and iroh-car compatibility")
    )]
    CarError {
        operation: String,
        #[source]
        cause: iroh_car::Error,
    },

    #[error("IO error: {operation} failed")]
    #[diagnostic(
        code(pattern_core::io_error),
        help("Check file permissions and disk space")
    )]
    IoError {
        operation: String,
        #[source]
        cause: std::io::Error,
    },

    #[error("Invalid data format: {data_type}")]
    #[diagnostic(
        code(pattern_core::invalid_format),
        help("Check the format of {data_type}: {details}")
    )]
    InvalidFormat { data_type: String, details: String },

    #[error("Agent not found: {identifier}")]
    #[diagnostic(
        code(pattern_core::agent_not_found),
        help("No agent exists with identifier: {identifier}")
    )]
    AgentNotFound { identifier: String },

    #[error("Group not found: {identifier}")]
    #[diagnostic(
        code(pattern_core::group_not_found),
        help("No group exists with identifier: {identifier}")
    )]
    GroupNotFound { identifier: String },

    #[error("No endpoint configured for: {target_type}")]
    #[diagnostic(
        code(pattern_core::no_endpoint_configured),
        help("Register an endpoint for {target_type} using MessageRouter::register_endpoint")
    )]
    NoEndpointConfigured { target_type: String },

    #[error("Rate limited: {target} (cooldown: {cooldown_secs}s)")]
    #[diagnostic(
        code(pattern_core::rate_limited),
        help("Wait {cooldown_secs} seconds before sending another message to {target}")
    )]
    RateLimited { target: String, cooldown_secs: u64 },

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

impl From<DatabaseError> for CoreError {
    fn from(err: DatabaseError) -> Self {
        match err {
            DatabaseError::ConnectionFailed(e) => Self::DatabaseConnectionFailed {
                connection_string: "embedded".to_string(),
                cause: e,
            },
            DatabaseError::QueryFailed(e) => Self::DatabaseQueryFailed {
                query: "unknown".to_string(),
                table: "unknown".to_string(),
                cause: e,
            },
            DatabaseError::QueryFailedContext {
                query,
                table,
                cause,
            } => Self::DatabaseQueryFailed {
                query,
                table,
                cause,
            },

            DatabaseError::SerdeProblem(e) => Self::SerializationError {
                data_type: "database record".to_string(),
                cause: e,
            },
            DatabaseError::NotFound { entity_type, id } => Self::DatabaseQueryFailed {
                query: format!("SELECT * FROM {} WHERE id = '{}'", entity_type, id),
                table: entity_type,
                cause: surrealdb::Error::Db(surrealdb::error::Db::Tx("not found".to_string())),
            },
            DatabaseError::EmbeddingError(e) => Self::VectorSearchFailed {
                collection: "unknown".to_string(),
                dimension_mismatch: None,
                cause: e,
            },
            DatabaseError::EmbeddingModelMismatch {
                db_model,
                config_model,
            } => Self::ConfigurationError {
                config_path: "database".to_string(),
                field: "embedding_model".to_string(),
                expected: db_model.clone(),
                cause: ConfigError::InvalidValue {
                    field: "embedding_model".to_string(),
                    reason: format!(
                        "Model mismatch: database has {}, config has {}",
                        db_model, config_model
                    ),
                },
            },
            DatabaseError::SchemaVersionMismatch {
                db_version,
                code_version,
            } => Self::DatabaseQueryFailed {
                query: "schema version check".to_string(),
                table: "system_metadata".to_string(),
                cause: surrealdb::Error::Db(surrealdb::error::Db::Tx(format!(
                    "Schema version mismatch: database v{}, code v{}",
                    db_version, code_version
                ))),
            },
            DatabaseError::InvalidVectorDimensions { expected, actual } => {
                Self::VectorSearchFailed {
                    collection: "unknown".to_string(),
                    dimension_mismatch: Some((expected, actual)),
                    cause: EmbeddingError::DimensionMismatch { expected, actual },
                }
            }
            DatabaseError::TransactionFailed(e) => Self::DatabaseQueryFailed {
                query: "transaction".to_string(),
                table: "unknown".to_string(),
                cause: e,
            },
            DatabaseError::SurrealJsonValueError { original, help } => Self::DatabaseQueryFailed {
                query: help,
                table: "".to_string(),
                cause: original,
            },
            DatabaseError::Other(msg) => Self::DatabaseQueryFailed {
                query: "unknown".to_string(),
                table: "unknown".to_string(),
                cause: surrealdb::Error::Db(surrealdb::error::Db::Tx(msg)),
            },
        }
    }
}

impl From<EntityError> for CoreError {
    fn from(err: EntityError) -> Self {
        // Convert EntityError to DatabaseError, then to CoreError
        let db_err: DatabaseError = err.into();
        db_err.into()
    }
}

// Helper functions for creating common errors with context
impl CoreError {
    pub fn memory_not_found(
        agent_id: &crate::AgentId,
        block_name: impl Into<String>,
        available_blocks: Vec<CompactString>,
    ) -> Self {
        Self::MemoryNotFound {
            agent_id: agent_id.to_string(),
            block_name: block_name.into(),
            available_blocks,
        }
    }

    pub fn tool_not_found(name: impl Into<String>, available: Vec<String>) -> Self {
        let name = name.into();
        Self::ToolNotFound {
            tool_name: name.clone(),
            available_tools: available.to_vec(),
            src: format!("tool: {}", name),
            span: (6, 6 + name.len()),
        }
    }

    pub fn database_connection_failed(
        connection_string: impl Into<String>,
        cause: surrealdb::Error,
    ) -> Self {
        Self::DatabaseConnectionFailed {
            connection_string: connection_string.into(),
            cause,
        }
    }

    /// Create a DatabaseQueryFailed with explicit context.
    pub fn database_query_error(
        operation_or_query: impl Into<String>,
        table: impl Into<String>,
        cause: surrealdb::Error,
    ) -> Self {
        Self::DatabaseQueryFailed {
            query: operation_or_query.into(),
            table: table.into(),
            cause,
        }
    }

    /// Builder-style: attach query/table context to an existing DatabaseQueryFailed.
    /// Returns self unchanged for other variants.
    pub fn with_db_context(mut self, query: impl Into<String>, table: impl Into<String>) -> Self {
        match &mut self {
            CoreError::DatabaseQueryFailed {
                query: q, table: t, ..
            } => {
                *q = query.into();
                *t = table.into();
                self
            }
            _ => self,
        }
    }

    pub fn tool_validation_error(tool_name: impl Into<String>, error: impl Into<String>) -> Self {
        let tool_name = tool_name.into();
        Self::InvalidToolParameters {
            tool_name,
            expected_schema: serde_json::Value::Null,
            provided_params: serde_json::Value::Null,
            validation_errors: vec![error.into()],
        }
    }

    pub fn tool_execution_error(tool_name: impl Into<String>, error: impl Into<String>) -> Self {
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause: error.into(),
            parameters: serde_json::Value::Null,
        }
    }
}

impl From<surrealdb::Error> for CoreError {
    fn from(e: surrealdb::Error) -> Self {
        CoreError::DatabaseQueryFailed {
            query: "unknown".to_string(),
            table: "unknown".to_string(),
            cause: e,
        }
    }
}

/// Errors related to embedding operations
#[derive(Error, Debug, Diagnostic)]
pub enum EmbeddingError {
    #[error("Embedding generation failed")]
    #[diagnostic(help("Check your embedding model configuration and input text"))]
    GenerationFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("Model not found: {0}")]
    #[diagnostic(help("Ensure the model is downloaded or accessible"))]
    ModelNotFound(String),

    #[error("Invalid dimensions: expected {expected}, got {actual}")]
    #[diagnostic(help("All embeddings must use the same model to ensure consistent dimensions"))]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("API error: {0}")]
    #[diagnostic(help("Check your API key and network connection"))]
    ApiError(String),

    #[error("Batch size too large: {size} (max: {max})")]
    BatchSizeTooLarge { size: usize, max: usize },

    #[error("Empty input provided")]
    #[diagnostic(help("Provide at least one non-empty text to embed"))]
    EmptyInput,
}
