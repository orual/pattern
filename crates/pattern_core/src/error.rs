use crate::{AgentId, embeddings::EmbeddingError};
use compact_str::CompactString;
use miette::{Diagnostic, IntoDiagnostic};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Configuration-specific errors
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
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

    #[error("Deprecated config: {field} - {message}")]
    Deprecated { field: String, message: String },
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

    #[error("Tool {tool_name} failed: {cause}\n{parameters}")]
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

    #[error("Model provider error")]
    #[diagnostic(
        code(pattern_core::model_provider_error),
        help("Check API credentials and rate limits for {provider}")
    )]
    ModelProviderError {
        provider: String,
        model: String,
        #[source]
        cause: genai::Error,
    },

    #[error("Upstream provider HTTP error: {provider} {status}")]
    #[diagnostic(
        code(pattern_core::provider_http_error),
        help(
            "Request to provider '{provider}' for model '{model}' failed with HTTP status {status}. Inspect headers/body for rate limits or retry guidance."
        )
    )]
    ProviderHttpError {
        provider: String,
        model: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
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

    #[error("SQLite database error: {0}")]
    #[diagnostic(
        code(pattern_core::sqlite_error),
        help("Check database connection and query")
    )]
    SqliteError(#[from] pattern_db::DbError),

    #[error("Authentication database error: {0}")]
    #[diagnostic(
        code(pattern_core::auth_error),
        help("Check auth database connection and credentials")
    )]
    AuthError(#[from] pattern_auth::AuthError),

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

    #[error("Already started: {component}")]
    #[diagnostic(code(pattern_core::already_started), help("{details}"))]
    AlreadyStarted { component: String, details: String },

    #[error("Export error during {operation}: {cause}")]
    #[diagnostic(
        code(pattern_core::export_error),
        help("Check export parameters and data format")
    )]
    ExportError { operation: String, cause: String },
}

pub type Result<T> = std::result::Result<T, CoreError>;

// Helper functions for creating common errors with context
impl CoreError {
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

    pub fn tool_not_found(name: impl Into<String>, available: Vec<String>) -> Self {
        let name = name.into();
        Self::ToolNotFound {
            tool_name: name.clone(),
            available_tools: available.to_vec(),
            src: format!("tool: {}", name),
            span: (6, 6 + name.len()),
        }
    }

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

    /// Prefer this over `model_error` to preserve HTTP status/headers when available.
    /// Falls back to `ModelProviderError` if the error does not carry HTTP details.
    pub fn from_genai_error(
        provider: impl Into<String>,
        model: impl Into<String>,
        cause: genai::Error,
    ) -> Self {
        let provider = provider.into();
        let model = model.into();
        // Try to extract HTTP status/body/headers from web client error
        if let genai::Error::WebModelCall { webc_error, .. } = &cause {
            if let genai::webc::Error::ResponseFailedStatus {
                status,
                body,
                headers,
            } = webc_error
            {
                // Clone headers into a simple Vec<(String,String)> for diagnostics/serialization
                let mut hdrs_vec: Vec<(String, String)> = Vec::new();
                for (k, v) in headers.as_ref().iter() {
                    let key = k.as_str().to_string();
                    let val = v.to_str().unwrap_or("").to_string();
                    hdrs_vec.push((key, val));
                }
                return Self::ProviderHttpError {
                    provider,
                    model,
                    status: status.as_u16(),
                    headers: hdrs_vec,
                    body: body.clone(),
                };
            }
        }
        Self::ModelProviderError {
            provider,
            model,
            cause,
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

    /// Construct a ToolExecutionFailed from a concrete error. The error is wrapped
    /// as a miette::Report and formatted with Debug ({:?}) to preserve rich context
    /// while keeping a single string field in the variant.
    pub fn tool_exec_error<E>(
        tool_name: impl Into<String>,
        parameters: serde_json::Value,
        err: E,
    ) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        // Use IntoDiagnostic to build a rich miette::Report from a non-Diagnostic error,
        // then format with {:?} for a readable, contextual message.
        let report = Err::<(), E>(err).into_diagnostic().unwrap_err();
        let cause = format!("{:?}", report);
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause,
            parameters,
        }
    }

    /// Variant of tool_exec_error that sets parameters to Null.
    pub fn tool_exec_error_simple(
        tool_name: impl Into<String>,
        err: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::tool_exec_error(tool_name, serde_json::Value::Null, err)
    }

    /// Construct a ToolExecutionFailed from a free-form message. Useful for
    /// deterministic user-facing causes (e.g., validation failures) while still
    /// attaching parameters for tool context.
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

    /// Construct ToolExecutionFailed from an existing miette::Report.
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

    /// Construct ToolExecutionFailed from a Diagnostic, preserving its context.
    /// Prefer this when the error type already implements `Diagnostic`.
    pub fn tool_exec_diagnostic(
        tool_name: impl Into<String>,
        parameters: serde_json::Value,
        diag: impl Diagnostic + Send + Sync + 'static,
    ) -> Self {
        // Build a miette report directly to preserve Diagnostic details, then format
        // with {:?} for a readable multi-line message with spans/help.
        let report = miette::Report::new(diag);
        let cause = format!("{:?}", report);
        Self::ToolExecutionFailed {
            tool_name: tool_name.into(),
            cause,
            parameters,
        }
    }

    /// If this error came from an upstream provider HTTP failure, return
    /// borrowed parts: (status, headers, body).
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

    /// Suggest a wait duration for rate limits or service busy errors based on
    /// known headers. Returns None if not applicable.
    pub fn rate_limit_hint(&self) -> Option<std::time::Duration> {
        let (_, headers, _) = self.provider_http_parts()?;
        // Create a lowercase lookup map
        let mut map = std::collections::HashMap::<String, String>::new();
        for (k, v) in headers.iter() {
            map.insert(k.to_ascii_lowercase(), v.clone());
        }

        // Retry-After seconds or HTTP-date
        if let Some(raw) = map.get("retry-after").map(|s| s.as_str()) {
            let s = raw.trim();
            if let Ok(secs) = s.parse::<u64>() {
                return Some(std::time::Duration::from_millis(secs * 1000));
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
                let now = chrono::Utc::now();
                let ms = dt
                    .with_timezone(&chrono::Utc)
                    .signed_duration_since(now)
                    .num_milliseconds();
                if ms > 0 {
                    return Some(std::time::Duration::from_millis(ms as u64));
                }
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                let now = chrono::Utc::now();
                let ms = dt
                    .with_timezone(&chrono::Utc)
                    .signed_duration_since(now)
                    .num_milliseconds();
                if ms > 0 {
                    return Some(std::time::Duration::from_millis(ms as u64));
                }
            }
        }

        // Anthropic reset epoch
        if let Some(raw) = map
            .get("anthropic-ratelimit-unified-5h-reset")
            .or_else(|| map.get("anthropic-ratelimit-unified-reset"))
            .map(|s| s.as_str())
        {
            if let Ok(epoch) = raw.trim().parse::<u64>() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                if epoch > now {
                    return Some(std::time::Duration::from_millis((epoch - now) * 1000));
                }
            }
        }

        // Provider-specific reset durations (OpenAI/Groq-like)
        let keys = [
            "x-ratelimit-reset-requests",
            "x-ratelimit-reset-tokens",
            "x-ratelimit-reset-input-tokens",
            "x-ratelimit-reset-output-tokens",
            "x-ratelimit-reset-images-requests",
            "x-ratelimit-reset",
            "ratelimit-reset",
        ];
        for k in keys.iter() {
            if let Some(raw) = map.get(*k).map(|s| s.as_str()) {
                let s = raw.trim();
                if let Some(stripped) = s.strip_suffix("ms") {
                    if let Ok(v) = stripped.trim().parse::<u64>() {
                        return Some(std::time::Duration::from_millis(v));
                    }
                }
                if let Some(stripped) = s.strip_suffix('s') {
                    if let Ok(v) = stripped.trim().parse::<u64>() {
                        return Some(std::time::Duration::from_millis(v * 1000));
                    }
                }
                if let Some(stripped) = s.strip_suffix('m') {
                    if let Ok(v) = stripped.trim().parse::<u64>() {
                        return Some(std::time::Duration::from_millis(v * 60_000));
                    }
                }
                if let Some(stripped) = s.strip_suffix('h') {
                    if let Ok(v) = stripped.trim().parse::<u64>() {
                        return Some(std::time::Duration::from_millis(v * 3_600_000));
                    }
                }
                if let Ok(secs) = s.parse::<u64>() {
                    return Some(std::time::Duration::from_millis(secs * 1000));
                }
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                    let now = chrono::Utc::now();
                    let ms = dt
                        .with_timezone(&chrono::Utc)
                        .signed_duration_since(now)
                        .num_milliseconds();
                    if ms > 0 {
                        return Some(std::time::Duration::from_millis(ms as u64));
                    }
                }
                if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
                    let now = chrono::Utc::now();
                    let ms = dt
                        .with_timezone(&chrono::Utc)
                        .signed_duration_since(now)
                        .num_milliseconds();
                    if ms > 0 {
                        return Some(std::time::Duration::from_millis(ms as u64));
                    }
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
        assert!(output.contains("Available tools: tool1, tool2, tool3"));
    }
}
