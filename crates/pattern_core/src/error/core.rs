// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Top-level `CoreError` wrapping all pattern-core error sub-systems.
//!
//! # Pre-v3 CoreError variants in this file
//!
//! All pre-v3 variants that do not belong to a sub-system (Runtime, Provider,
//! Memory) are kept here: `AgentInitFailed`, `AgentProcessing`, `ToolNotFound`,
//! `ToolExecutionFailed`, `InvalidToolParameters`, `SerializationError`,
//! `ConfigurationError`, `DataSourceError`, `DagCborEncodingError`,
//! `DagCborDecodingError`, `CarError`, `IoError`, `SqliteError`, `AuthError`,
//! `InvalidFormat`, `AgentNotFound`, `NoEndpointConfigured`, `RateLimited`,
//! `AlreadyStarted`, `ExportError`.
//!
//! Sub-system errors are wrapped via `#[from]` into `Runtime`, `Provider`,
//! and `Memory` variants below.
//!
//! Retired in v3-multi-agent Phase 6 (legacy coordination cleanup):
//! `CoordinationFailed`, `AgentGroupError`, `GroupNotFound`. The
//! coordination/agent-group framing was pre-v3 and replaced by the
//! constellation registry + persona relationships.

use compact_str::CompactString;
use miette::Diagnostic;
use thiserror::Error;

use super::{EmbeddingError, MemoryError, ProviderError, RuntimeError};
use crate::types::ids::AgentId;

/// Top-level error type for pattern-core operations.
///
/// Use `Result<T>` (the crate-level type alias) in all public APIs rather than
/// naming this type directly where possible.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum CoreError {
    // ── Sub-system wrappers ──────────────────────────────────────────────────
    /// An error from the agent execution runtime (timeouts, crashes, etc.).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::{CoreError, RuntimeError};
    ///
    /// let err: CoreError = RuntimeError::RuntimeCrashed.into();
    /// assert!(err.to_string().contains("crashed"));
    /// ```
    #[error(transparent)]
    #[diagnostic(transparent)]
    Runtime(#[from] RuntimeError),

    /// An error from an external LLM provider or credential store.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use pattern_core::error::{CoreError, ProviderError};
    ///
    /// let err: CoreError = ProviderError::AuthFlowTimeout.into();
    /// assert!(err.to_string().contains("timed out"));
    /// ```
    #[error(transparent)]
    #[diagnostic(transparent)]
    Provider(#[from] ProviderError),

    /// An error from the memory block store.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::{CoreError, MemoryError};
    /// use pattern_core::types::block::BlockHandle;
    ///
    /// let err: CoreError = MemoryError::StoreCorrupted {
    ///     detail: "bad checksum".to_string(),
    /// }
    /// .into();
    /// assert!(err.to_string().contains("bad checksum"));
    /// ```
    #[error(transparent)]
    #[diagnostic(transparent)]
    Memory(#[from] MemoryError),

    /// An error from an embedding provider or vector comparison.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::{CoreError, EmbeddingError};
    ///
    /// let err: CoreError = EmbeddingError::EmptyInput.into();
    /// assert!(err.to_string().contains("empty"));
    /// ```
    #[error(transparent)]
    #[diagnostic(transparent)]
    Embedding(#[from] EmbeddingError),

    // ── Agent lifecycle ──────────────────────────────────────────────────────
    /// Agent initialisation failed before the first turn could run.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::AgentInitFailed {
    ///     agent_type: "SupportAgent".to_string(),
    ///     cause: "missing persona block".to_string(),
    /// };
    /// assert!(err.to_string().contains("initialization failed"));
    /// ```
    #[error("agent initialization failed")]
    #[diagnostic(
        code(pattern_core::agent_init_failed),
        help("check the agent configuration and ensure all required fields are provided")
    )]
    AgentInitFailed { agent_type: String, cause: String },

    /// An agent failed during stream processing.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::AgentProcessing {
    ///     agent_id: "orual-companion".to_string(),
    ///     details: "parse error".to_string(),
    /// };
    /// assert!(err.to_string().contains("orual-companion"));
    /// ```
    #[error("agent {agent_id} processing failed: {details}")]
    #[diagnostic(
        code(pattern_core::agent_processing),
        help("agent encountered an error during stream processing")
    )]
    AgentProcessing { agent_id: String, details: String },

    // ── Memory (legacy string-keyed variant) ─────────────────────────────────
    /// A memory block was not found by agent-ID + label (legacy string form).
    ///
    /// Prefer [`MemoryError::BlockNotFound`] in new code. This variant exists
    /// for call sites that still use string-keyed lookups.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::MemoryNotFound {
    ///     agent_id: "orual-companion".to_string(),
    ///     block_name: "persona".to_string(),
    ///     available_blocks: vec!["task_list".into()],
    /// };
    /// assert!(err.to_string().contains("Memory block not found"));
    /// ```
    #[error("Memory block not found")]
    #[diagnostic(
        code(pattern_core::memory_not_found),
        help("the requested memory block doesn't exist for this agent")
    )]
    MemoryNotFound {
        agent_id: String,
        block_name: String,
        available_blocks: Vec<CompactString>,
    },

    // ── Tool errors ───────────────────────────────────────────────────────────
    /// No tool matched the requested name.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::ToolNotFound {
    ///     tool_name: "fly".to_string(),
    ///     available_tools: vec!["search".to_string()],
    ///     src: "tool: fly".to_string(),
    ///     span: (6, 9),
    /// };
    /// assert!(err.to_string().contains("Tool not found"));
    /// ```
    #[error("Tool not found")]
    #[diagnostic(
        code(pattern_core::tool_not_found),
        help("available tools: {}", available_tools.join(", "))
    )]
    ToolNotFound {
        tool_name: String,
        available_tools: Vec<String>,
        #[source_code]
        src: String,
        #[label("unknown tool")]
        span: (usize, usize),
    },

    /// A tool call failed during execution.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::ToolExecutionFailed {
    ///     tool_name: "search".to_string(),
    ///     cause: "connection refused".to_string(),
    ///     parameters: serde_json::json!({}),
    /// };
    /// assert!(err.to_string().contains("search failed"));
    /// ```
    #[error("Tool {tool_name} failed: {cause}\n{parameters}")]
    #[diagnostic(
        code(pattern_core::tool_execution_failed),
        help("check tool parameters and ensure they match the expected schema")
    )]
    ToolExecutionFailed {
        tool_name: String,
        cause: String,
        parameters: serde_json::Value,
    },

    /// Tool parameters did not match the expected schema.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::InvalidToolParameters {
    ///     tool_name: "search".to_string(),
    ///     expected_schema: serde_json::json!({}),
    ///     provided_params: serde_json::json!({}),
    ///     validation_errors: vec!["missing field 'query'".to_string()],
    /// };
    /// assert!(err.to_string().contains("Invalid tool parameters"));
    /// ```
    #[error("Invalid tool parameters for {tool_name}")]
    #[diagnostic(
        code(pattern_core::invalid_tool_params),
        help("expected schema: {expected_schema}")
    )]
    InvalidToolParameters {
        tool_name: String,
        expected_schema: serde_json::Value,
        provided_params: serde_json::Value,
        validation_errors: Vec<String>,
    },

    // ── Provider (legacy rich variants) ──────────────────────────────────────
    /// An LLM provider returned an error (legacy: holds a `genai::Error`).
    ///
    /// New code should use [`ProviderError::RequestFailed`] instead.
    ///
    /// # Example
    ///
    /// Cannot construct genai::Error in doctest; see [`ProviderError::RequestFailed`].
    #[cfg(feature = "provider")]
    #[error("model provider error")]
    #[diagnostic(
        code(pattern_core::model_provider_error),
        help("check API credentials and rate limits for {provider}")
    )]
    ModelProviderError {
        provider: String,
        model: String,
        #[source]
        cause: genai::Error,
    },

    /// An LLM provider returned a structured HTTP error (legacy).
    ///
    /// New code should use [`ProviderError::RequestFailed`] instead.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::ProviderHttpError {
    ///     provider: "Anthropic".to_string(),
    ///     model: "claude-sonnet".to_string(),
    ///     status: 429,
    ///     headers: vec![],
    ///     body: "rate limited".to_string(),
    /// };
    /// assert!(err.to_string().contains("429"));
    /// ```
    #[error("upstream provider HTTP error: {provider} {status}")]
    #[diagnostic(
        code(pattern_core::provider_http_error),
        help(
            "request to provider '{provider}' for model '{model}' failed with HTTP status {status}"
        )
    )]
    ProviderHttpError {
        provider: String,
        model: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },

    // ── Serialization ─────────────────────────────────────────────────────────
    /// Serialization or deserialization of a value failed.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let raw_err: Result<i32, _> = serde_json::from_str("not_json");
    /// let cause = raw_err.unwrap_err();
    /// let err = CoreError::SerializationError {
    ///     data_type: "MyType".to_string(),
    ///     cause,
    /// };
    /// assert!(err.to_string().contains("Serialization error"));
    /// ```
    #[error("Serialization error")]
    #[diagnostic(
        code(pattern_core::serialization_error),
        help("failed to serialize/deserialize {data_type}")
    )]
    SerializationError {
        data_type: String,
        #[source]
        cause: serde_json::Error,
    },

    // ── Configuration ─────────────────────────────────────────────────────────
    /// A configuration file had an invalid or missing field.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::{ConfigError, CoreError};
    ///
    /// let err = CoreError::ConfigurationError {
    ///     config_path: "/etc/pattern.toml".to_string(),
    ///     field: "api_key".to_string(),
    ///     expected: "non-empty string".to_string(),
    ///     cause: ConfigError::MissingField("api_key".to_string()),
    /// };
    /// assert!(err.to_string().contains("api_key"));
    /// ```
    #[error("configuration error for field '{field}'")]
    #[diagnostic(
        code(pattern_core::configuration_error),
        help("check configuration file at {config_path}\nexpected: {expected}")
    )]
    ConfigurationError {
        config_path: String,
        field: String,
        expected: String,
        #[source]
        cause: ConfigError,
    },

    // ── Data source ───────────────────────────────────────────────────────────
    /// A data source operation failed.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::DataSourceError {
    ///     source_name: "bluesky-firehose".to_string(),
    ///     operation: "connect".to_string(),
    ///     cause: "DNS resolution failed".to_string(),
    /// };
    /// assert!(err.to_string().contains("bluesky-firehose"));
    /// ```
    #[error("data source error in {source_name}: {operation} failed - {cause}")]
    #[diagnostic(
        code(pattern_core::data_source_error),
        help("check data source configuration and connectivity")
    )]
    DataSourceError {
        source_name: String,
        operation: String,
        cause: String,
    },

    // ── Archive / export ──────────────────────────────────────────────────────
    /// DAG-CBOR encoding failed.
    ///
    /// # Example
    ///
    /// Cannot construct serde_ipld_dagcbor error in doctest directly.
    #[error("DAG-CBOR encoding error")]
    #[diagnostic(
        code(pattern_core::dagcbor_encoding_error),
        help("failed to encode data as DAG-CBOR")
    )]
    DagCborEncodingError { data_type: String, cause: String },

    /// DAG-CBOR decoding failed.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::DagCborDecodingError {
    ///     data_type: "Block".to_string(),
    ///     details: "unexpected EOF".to_string(),
    /// };
    /// assert!(err.to_string().contains("unexpected EOF"));
    /// ```
    #[error("failed to decode DAG-CBOR data for {data_type}:\n {details}")]
    #[diagnostic(
        code(pattern_core::dagcbor_decoding_error),
        help("failed to decode data from DAG-CBOR: {details}")
    )]
    DagCborDecodingError { data_type: String, details: String },

    /// A CAR archive operation failed.
    ///
    /// # Example
    ///
    /// Cannot construct iroh_car::Error in doctest directly.
    #[error("CAR archive error: {operation} failed")]
    #[diagnostic(
        code(pattern_core::car_error),
        help("check CAR file format and iroh-car compatibility")
    )]
    CarError { operation: String, cause: String },

    /// An export operation failed.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::ExportError {
    ///     operation: "serialize".to_string(),
    ///     cause: "unsupported format".to_string(),
    /// };
    /// assert!(err.to_string().contains("Export error"));
    /// ```
    #[error("Export error during {operation}: {cause}")]
    #[diagnostic(
        code(pattern_core::export_error),
        help("check export parameters and data format")
    )]
    ExportError { operation: String, cause: String },

    // ── I/O ───────────────────────────────────────────────────────────────────
    /// A filesystem or network I/O operation failed.
    ///
    /// # Example
    ///
    /// ```
    /// use std::io;
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::IoError {
    ///     operation: "read config".to_string(),
    ///     cause: io::Error::new(io::ErrorKind::NotFound, "file not found"),
    /// };
    /// assert!(err.to_string().contains("IO error"));
    /// ```
    #[error("IO error: {operation} failed")]
    #[diagnostic(
        code(pattern_core::io_error),
        help("check file permissions and disk space")
    )]
    IoError {
        operation: String,
        #[source]
        cause: std::io::Error,
    },

    // ── Database ──────────────────────────────────────────────────────────────
    /// A SQLite database operation failed.
    ///
    /// # Example
    ///
    /// Wraps a database error as a string — the typed `pattern_db::DbError`
    /// is mapped at the `pattern_memory` boundary.
    #[error("SQLite database error: {0}")]
    #[diagnostic(
        code(pattern_core::sqlite_error),
        help("check database connection and query")
    )]
    SqliteError(String),

    // ── Misc validation ───────────────────────────────────────────────────────
    /// A value was in an invalid or unrecognised format.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::InvalidFormat {
    ///     data_type: "AgentId".to_string(),
    ///     details: "must not be empty".to_string(),
    /// };
    /// assert!(err.to_string().contains("Invalid data format"));
    /// ```
    #[error("Invalid data format: {data_type}")]
    #[diagnostic(
        code(pattern_core::invalid_format),
        help("check the format of {data_type}: {details}")
    )]
    InvalidFormat { data_type: String, details: String },

    /// No agent was found for the given identifier.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::AgentNotFound { identifier: "ghost".to_string() };
    /// assert!(err.to_string().contains("ghost"));
    /// ```
    #[error("agent not found: {identifier}")]
    #[diagnostic(
        code(pattern_core::agent_not_found),
        help("no agent exists with identifier: {identifier}")
    )]
    AgentNotFound { identifier: String },

    /// No message endpoint is configured for the given target type.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::NoEndpointConfigured { target_type: "Discord".to_string() };
    /// assert!(err.to_string().contains("No endpoint configured"));
    /// ```
    #[error("No endpoint configured for: {target_type}")]
    #[diagnostic(
        code(pattern_core::no_endpoint_configured),
        help("register an endpoint for {target_type} using MessageRouter::register_endpoint")
    )]
    NoEndpointConfigured { target_type: String },

    /// A message or request was rate-limited at the routing layer.
    ///
    /// Distinct from [`ProviderError::RateLimited`] which applies to external
    /// provider calls. This variant applies to internal routing cooldowns.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::RateLimited { target: "discord-channel".to_string(), cooldown_secs: 30 };
    /// assert!(err.to_string().contains("Rate limited"));
    /// ```
    #[error("Rate limited: {target} (cooldown: {cooldown_secs}s)")]
    #[diagnostic(
        code(pattern_core::rate_limited),
        help("wait {cooldown_secs} seconds before sending another message to {target}")
    )]
    RateLimited { target: String, cooldown_secs: u64 },

    /// A component was started more than once.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::CoreError;
    ///
    /// let err = CoreError::AlreadyStarted {
    ///     component: "MemoryCache".to_string(),
    ///     details: "call stop() first".to_string(),
    /// };
    /// assert!(err.to_string().contains("Already started"));
    /// ```
    #[error("Already started: {component}")]
    #[diagnostic(code(pattern_core::already_started), help("{details}"))]
    AlreadyStarted { component: String, details: String },
}

/// Configuration-specific errors.
///
/// Used as the `cause` field in [`CoreError::ConfigurationError`].
#[derive(thiserror::Error, Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ConfigError {
    /// An I/O error occurred while reading or writing the config file.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ConfigError;
    ///
    /// let err = ConfigError::Io("permission denied".to_string());
    /// assert!(err.to_string().contains("permission denied"));
    /// ```
    #[error("IO error: {0}")]
    Io(String),

    /// The TOML config file could not be parsed.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ConfigError;
    ///
    /// let err = ConfigError::TomlParse("unexpected key".to_string());
    /// assert!(err.to_string().contains("unexpected key"));
    /// ```
    #[error("TOML parse error: {0}")]
    TomlParse(String),

    /// The TOML config could not be serialized.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ConfigError;
    ///
    /// let err = ConfigError::TomlSerialize("type mismatch".to_string());
    /// assert!(err.to_string().contains("type mismatch"));
    /// ```
    #[error("TOML serialize error: {0}")]
    TomlSerialize(String),

    /// A required configuration field was absent.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ConfigError;
    ///
    /// let err = ConfigError::MissingField("api_key".to_string());
    /// assert!(err.to_string().contains("api_key"));
    /// ```
    #[error("missing required field: {0}")]
    MissingField(String),

    /// A configuration field had an invalid value.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ConfigError;
    ///
    /// let err = ConfigError::InvalidValue {
    ///     field: "timeout".to_string(),
    ///     reason: "must be positive".to_string(),
    /// };
    /// assert!(err.to_string().contains("timeout"));
    /// ```
    #[error("invalid value for field {field}: {reason}")]
    InvalidValue { field: String, reason: String },

    /// A deprecated configuration field was present.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ConfigError;
    ///
    /// let err = ConfigError::Deprecated {
    ///     field: "max_tokens".to_string(),
    ///     message: "use context_window instead".to_string(),
    /// };
    /// assert!(err.to_string().contains("max_tokens"));
    /// ```
    #[error("deprecated config: {field} - {message}")]
    Deprecated { field: String, message: String },
}

// ── Helper constructors ──────────────────────────────────────────────────────

impl CoreError {
    /// Construct a [`CoreError::MemoryNotFound`] with typed inputs.
    pub fn memory_not_found(
        agent_id: &AgentId,
        block_name: impl Into<String>,
        available_blocks: Vec<CompactString>,
    ) -> Self {
        Self::MemoryNotFound {
            agent_id: agent_id.to_string(),
            block_name: block_name.into(),
            available_blocks,
        }
    }

    /// Construct a [`CoreError::ToolNotFound`] with source-span labels.
    pub fn tool_not_found(name: impl Into<String>, available: Vec<String>) -> Self {
        let name = name.into();
        Self::ToolNotFound {
            tool_name: name.clone(),
            available_tools: available,
            src: format!("tool: {}", name),
            span: (6, 6 + name.len()),
        }
    }

    /// Construct a [`CoreError::ModelProviderError`] from a `genai::Error`.
    #[cfg(feature = "provider")]
    pub fn model_error(
        provider: impl Into<String>,
        model: impl Into<String>,
        cause: genai::Error,
    ) -> Self {
        Self::ModelProviderError {
            provider: provider.into(),
            model: model.into(),
            cause,
        }
    }

    /// Prefer this over `model_error` to preserve HTTP status/headers when
    /// available. Falls back to `ModelProviderError` if the error does not
    /// carry HTTP details.
    #[cfg(feature = "provider")]
    pub fn from_genai_error(
        provider: impl Into<String>,
        model: impl Into<String>,
        cause: genai::Error,
    ) -> Self {
        let provider = provider.into();
        let model = model.into();
        if let genai::Error::WebModelCall { webc_error, .. } = &cause
            && let genai::webc::Error::ResponseFailedStatus {
                status,
                body,
                headers,
            } = webc_error
        {
            let hdrs: Vec<(String, String)> = headers
                .as_ref()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            return Self::ProviderHttpError {
                provider,
                model,
                status: status.as_u16(),
                headers: hdrs,
                body: body.clone(),
            };
        }
        Self::ModelProviderError {
            provider,
            model,
            cause,
        }
    }

    /// Construct a [`CoreError::InvalidToolParameters`] from a validation message.
    pub fn tool_validation_error(tool_name: impl Into<String>, error: impl Into<String>) -> Self {
        Self::InvalidToolParameters {
            tool_name: tool_name.into(),
            expected_schema: serde_json::Value::Null,
            provided_params: serde_json::Value::Null,
            validation_errors: vec![error.into()],
        }
    }

    /// Construct a [`CoreError::ToolExecutionFailed`] from a message string.
    pub fn tool_execution_error(tool_name: impl Into<String>, error: impl Into<String>) -> Self {
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause: error.into(),
            parameters: serde_json::Value::Null,
        }
    }

    /// Construct [`CoreError::ToolExecutionFailed`] from a concrete error.
    pub fn tool_exec_error<E>(
        tool_name: impl Into<String>,
        parameters: serde_json::Value,
        err: E,
    ) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        use miette::IntoDiagnostic;
        let report = Err::<(), E>(err).into_diagnostic().unwrap_err();
        let cause = format!("{:?}", report);
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause,
            parameters,
        }
    }

    /// Variant of [`Self::tool_exec_error`] with `Null` parameters.
    pub fn tool_exec_error_simple(
        tool_name: impl Into<String>,
        err: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::tool_exec_error(tool_name, serde_json::Value::Null, err)
    }

    /// Construct [`CoreError::ToolExecutionFailed`] from a free-form message.
    pub fn tool_exec_msg(
        tool_name: impl Into<String>,
        parameters: serde_json::Value,
        message: impl Into<String>,
    ) -> Self {
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause: message.into(),
            parameters,
        }
    }

    /// Construct [`CoreError::ToolExecutionFailed`] from a `miette::Report`.
    pub fn tool_exec_report(
        tool_name: impl Into<String>,
        parameters: serde_json::Value,
        report: miette::Report,
    ) -> Self {
        let cause = format!("{:?}", report);
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause,
            parameters,
        }
    }

    /// Construct [`CoreError::ToolExecutionFailed`] from a `Diagnostic`.
    pub fn tool_exec_diagnostic(
        tool_name: impl Into<String>,
        parameters: serde_json::Value,
        diag: impl miette::Diagnostic + Send + Sync + 'static,
    ) -> Self {
        let report = miette::Report::new(diag);
        let cause = format!("{:?}", report);
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause,
            parameters,
        }
    }

    /// If this error came from an upstream provider HTTP failure, return
    /// borrowed parts: `(status, headers, body)`.
    pub fn provider_http_parts(&self) -> Option<(u16, &[(String, String)], &str)> {
        match self {
            CoreError::ProviderHttpError {
                status,
                headers,
                body,
                ..
            } => Some((*status, headers.as_slice(), body.as_str())),
            _ => None,
        }
    }

    /// Suggest a wait duration for rate limits or service-busy errors based on
    /// known response headers. Returns `None` if not applicable.
    pub fn rate_limit_hint(&self) -> Option<std::time::Duration> {
        let (_, headers, _) = self.provider_http_parts()?;
        let map: std::collections::HashMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();

        // Retry-After (seconds or HTTP-date)
        if let Some(raw) = map.get("retry-after").map(|s| s.as_str()) {
            let s = raw.trim();
            if let Ok(secs) = s.parse::<u64>() {
                return Some(std::time::Duration::from_millis(secs * 1000));
            }
        }

        // Anthropic reset epoch
        if let Some(raw) = map
            .get("anthropic-ratelimit-unified-5h-reset")
            .or_else(|| map.get("anthropic-ratelimit-unified-reset"))
            .map(|s| s.as_str())
            && let Ok(epoch) = raw.trim().parse::<u64>()
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            if epoch > now {
                return Some(std::time::Duration::from_millis((epoch - now) * 1000));
            }
        }

        // Provider-specific reset headers (OpenAI/Groq-like)
        let keys = [
            "x-ratelimit-reset-requests",
            "x-ratelimit-reset-tokens",
            "x-ratelimit-reset",
            "ratelimit-reset",
        ];
        for k in keys {
            if let Some(raw) = map.get(k).map(|s| s.as_str()) {
                let s = raw.trim();
                if let Some(stripped) = s.strip_suffix("ms")
                    && let Ok(v) = stripped.trim().parse::<u64>()
                {
                    return Some(std::time::Duration::from_millis(v));
                }
                if let Some(stripped) = s.strip_suffix('s')
                    && let Ok(v) = stripped.trim().parse::<u64>()
                {
                    return Some(std::time::Duration::from_millis(v * 1000));
                }
                if let Some(stripped) = s.strip_suffix('m')
                    && let Ok(v) = stripped.trim().parse::<u64>()
                {
                    return Some(std::time::Duration::from_millis(v * 60_000));
                }
                if let Some(stripped) = s.strip_suffix('h')
                    && let Ok(v) = stripped.trim().parse::<u64>()
                {
                    return Some(std::time::Duration::from_millis(v * 3_600_000));
                }
                if let Ok(secs) = s.parse::<u64>() {
                    return Some(std::time::Duration::from_millis(secs * 1000));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Report;

    #[test]
    fn test_tool_not_found_with_suggestions() {
        let error = CoreError::tool_not_found(
            "unknown_tool",
            vec![
                "tool1".to_string(),
                "tool2".to_string(),
                "tool3".to_string(),
            ],
        );
        let report = Report::new(error);
        let output = format!("{:?}", report);
        // Error messages use lowercase sentence fragments (per CLAUDE.md).
        assert!(output.contains("available tools: tool1, tool2, tool3"));
    }
}
