//! Configuration module for Pattern
//!
//! This module handles loading and validating configuration from TOML files
//! with support for environment variable overrides.

use miette::{Diagnostic, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Database configuration
    pub database: DatabaseConfig,

    /// Model configuration
    pub models: ModelConfig,

    /// Discord configuration
    #[cfg(feature = "discord")]
    #[serde(default)]
    pub discord: DiscordConfig,

    /// MCP configuration
    #[cfg(feature = "mcp")]
    #[serde(default)]
    pub mcp: McpConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Agent configuration
    #[serde(default)]
    pub agents: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// SurrealDB connection URL
    #[serde(default = "default_db_url")]
    pub url: String,

    /// Database namespace
    #[serde(default = "default_namespace")]
    pub namespace: String,

    /// Database name
    #[serde(default = "default_database")]
    pub database: String,

    /// Connection pool size
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,

    /// Connection timeout in seconds
    #[serde(default = "default_connection_timeout")]
    pub connection_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Default model provider
    #[serde(default = "default_provider")]
    pub default_provider: String,

    /// Default model name
    #[serde(default = "default_model")]
    pub default_model: String,

    /// Model capability mappings
    pub tiers: ModelTiers,

    /// Provider-specific configurations
    pub providers: ProvidersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTiers {
    #[serde(default = "default_routine_model")]
    pub routine: String,

    #[serde(default = "default_interactive_model")]
    pub interactive: String,

    #[serde(default = "default_investigative_model")]
    pub investigative: String,

    #[serde(default = "default_critical_model")]
    pub critical: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub anthropic: Option<AnthropicConfig>,

    #[serde(default)]
    pub openai: Option<OpenAIConfig>,

    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    /// API key or environment variable name
    pub api_key_env: String,

    /// Optional API base URL override
    pub base_url: Option<String>,

    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    /// API key or environment variable name
    pub api_key_env: String,

    /// Optional API base URL override
    pub base_url: Option<String>,

    /// Organization ID
    pub organization: Option<String>,

    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiConfig {
    /// API key or environment variable name
    pub api_key_env: String,

    /// Optional API base URL override
    pub base_url: Option<String>,

    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub timeout_secs: u64,
}

#[cfg(feature = "discord")]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscordConfig {
    /// Discord bot token or environment variable name
    pub token_env: String,

    /// Command prefix for text commands
    #[serde(default = "default_prefix")]
    pub prefix: String,

    /// Allowed channel IDs (if restricted)
    #[serde(default)]
    pub allowed_channels: Option<Vec<String>>,

    /// Admin user IDs
    #[serde(default)]
    pub admin_users: Option<Vec<String>>,

    /// Whether to register slash commands globally
    #[serde(default = "default_global_commands")]
    pub global_commands: bool,

    /// Rate limiting configuration
    #[serde(default)]
    pub rate_limits: RateLimitConfig,
}

#[cfg(feature = "discord")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Max messages per user per minute
    #[serde(default = "default_messages_per_minute")]
    pub messages_per_minute: usize,

    /// Max commands per user per minute
    #[serde(default = "default_commands_per_minute")]
    pub commands_per_minute: usize,
}

#[cfg(feature = "discord")]
impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            messages_per_minute: default_messages_per_minute(),
            commands_per_minute: default_commands_per_minute(),
        }
    }
}

#[cfg(feature = "mcp")]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    /// Transport type: "stdio", "http", or "sse"
    #[serde(default = "default_transport")]
    pub transport: String,

    /// Port for HTTP/SSE transports
    #[serde(default = "default_mcp_port")]
    pub port: u16,

    /// Host to bind to
    #[serde(default = "default_host")]
    pub host: String,

    /// Maximum concurrent requests
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_requests: usize,

    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level: trace, debug, info, warn, error
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Log format: json, pretty, compact
    #[serde(default = "default_log_format")]
    pub format: String,

    /// Optional log file path
    pub file: Option<PathBuf>,

    /// Whether to log to stdout
    #[serde(default = "default_log_stdout")]
    pub stdout: bool,

    /// Log rotation settings
    #[serde(default)]
    pub rotation: Option<LogRotationConfig>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
            file: None,
            stdout: default_log_stdout(),
            rotation: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRotationConfig {
    /// Maximum log file size in MB
    pub max_size_mb: u64,

    /// Maximum number of log files to keep
    pub max_files: usize,

    /// Whether to compress old log files
    #[serde(default)]
    pub compress: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// Path to external agent configuration file
    pub config_path: Option<PathBuf>,

    /// Agent group configurations
    #[serde(default)]
    pub groups: Vec<AgentGroupConfig>,

    /// Default agent timeout in seconds
    #[serde(default = "default_agent_timeout")]
    pub default_timeout_secs: u64,

    /// Whether to enable agent health checks
    #[serde(default = "default_health_checks")]
    pub enable_health_checks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentGroupConfig {
    /// Group name
    pub name: String,

    /// Coordination pattern
    pub coordination: String,

    /// Agents in this group
    pub agents: Vec<String>,

    /// Group-specific configuration
    #[serde(default)]
    pub config: serde_json::Value,
}

// Configuration error types
#[derive(Error, Diagnostic, Debug)]
pub enum ConfigError {
    #[error("Configuration file not found")]
    #[diagnostic(
        code(pattern::config::file_not_found),
        help("Create a configuration file at one of the expected locations")
    )]
    FileNotFound {
        searched_paths: Vec<PathBuf>,
        current_dir: PathBuf,
    },

    #[error("Failed to read configuration file")]
    #[diagnostic(code(pattern::config::read_failed))]
    ReadFailed {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    #[error("Failed to parse configuration")]
    #[diagnostic(code(pattern::config::parse_failed))]
    ParseFailed {
        path: PathBuf,
        #[source]
        cause: toml::de::Error,
    },

    #[error("Configuration validation failed")]
    #[diagnostic(code(pattern::config::validation_failed))]
    ValidationFailed { errors: Vec<ValidationError> },

    #[error("Environment variable not found")]
    #[diagnostic(
        code(pattern::config::env_var_not_found),
        help("Set the environment variable: export {var_name}=<value>")
    )]
    EnvVarNotFound { var_name: String, used_for: String },
}

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl Config {
    /// Load configuration from file with environment variable overrides
    pub async fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Read file
        let content =
            tokio::fs::read_to_string(path)
                .await
                .map_err(|e| ConfigError::ReadFailed {
                    path: path.to_path_buf(),
                    cause: e,
                })?;

        // Parse TOML
        let mut config: Config =
            toml::from_str(&content).map_err(|e| ConfigError::ParseFailed {
                path: path.to_path_buf(),
                cause: e,
            })?;

        // Apply environment variable overrides
        config.apply_env_overrides()?;

        // Validate configuration
        config.validate()?;

        Ok(config)
    }

    /// Load configuration from default locations
    pub async fn load_default() -> Result<Self> {
        let search_paths = vec![
            PathBuf::from("pattern.toml"),
            PathBuf::from("config/pattern.toml"),
            PathBuf::from(".config/pattern/pattern.toml"),
            dirs::config_dir()
                .map(|d| d.join("pattern/pattern.toml"))
                .unwrap_or_default(),
            PathBuf::from("/etc/pattern/pattern.toml"),
        ];

        for path in &search_paths {
            if path.exists() {
                return Self::load(path).await;
            }
        }

        Err(ConfigError::FileNotFound {
            searched_paths: search_paths,
            current_dir: std::env::current_dir().unwrap_or_default(),
        })?
    }

    /// Apply environment variable overrides
    fn apply_env_overrides(&mut self) -> Result<()> {
        // Database URL
        if let Ok(url) = std::env::var("PATTERN_DATABASE_URL") {
            self.database.url = url;
        }

        // Discord token
        #[cfg(feature = "discord")]
        if self.discord.token_env.starts_with("$") || self.discord.token_env.is_empty() {
            let var_name = self.discord.token_env.trim_start_matches('$');
            let var_name = if var_name.is_empty() {
                "DISCORD_TOKEN"
            } else {
                var_name
            };

            self.discord.token_env =
                std::env::var(var_name).map_err(|_| ConfigError::EnvVarNotFound {
                    var_name: var_name.to_string(),
                    used_for: "Discord bot authentication".to_string(),
                })?;
        }

        // Model API keys
        if let Some(anthropic) = &self.models.providers.anthropic {
            if anthropic.api_key_env.starts_with("$") {
                let var_name = anthropic.api_key_env.trim_start_matches('$');
                std::env::var(var_name).map_err(|_| ConfigError::EnvVarNotFound {
                    var_name: var_name.to_string(),
                    used_for: "Anthropic API authentication".to_string(),
                })?;
            }
        }

        Ok(())
    }

    /// Validate configuration
    fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        // Validate database URL
        if self.database.url.is_empty() {
            errors.push(ValidationError {
                field: "database.url".to_string(),
                message: "Database URL cannot be empty".to_string(),
            });
        }

        // Validate model configuration
        if self.models.default_provider.is_empty() {
            errors.push(ValidationError {
                field: "models.default_provider".to_string(),
                message: "Default provider must be specified".to_string(),
            });
        }

        // Validate Discord config if enabled
        #[cfg(feature = "discord")]
        {
            if self.discord.token_env.is_empty() {
                errors.push(ValidationError {
                    field: "discord.token_env".to_string(),
                    message: "Discord token must be provided".to_string(),
                });
            }
        }

        if !errors.is_empty() {
            return Err(ConfigError::ValidationFailed { errors })?;
        }

        Ok(())
    }
}

// Default value functions
fn default_db_url() -> String {
    "ws://localhost:8000".to_string()
}

fn default_namespace() -> String {
    "pattern".to_string()
}

fn default_database() -> String {
    "pattern".to_string()
}

fn default_pool_size() -> usize {
    10
}

fn default_connection_timeout() -> u64 {
    30
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "claude-3-haiku-20240307".to_string()
}

fn default_routine_model() -> String {
    "claude-3-haiku-20240307".to_string()
}

fn default_interactive_model() -> String {
    "claude-3-7-sonnet-latest".to_string()
}

fn default_investigative_model() -> String {
    "gpt-4".to_string()
}

fn default_critical_model() -> String {
    "claude-3-opus-20240229".to_string()
}

fn default_request_timeout() -> u64 {
    60
}

fn default_prefix() -> String {
    "!".to_string()
}

fn default_global_commands() -> bool {
    false
}

fn default_messages_per_minute() -> usize {
    20
}

fn default_commands_per_minute() -> usize {
    10
}

fn default_transport() -> String {
    "sse".to_string()
}

fn default_mcp_port() -> u16 {
    8080
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_max_concurrent() -> usize {
    100
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "pretty".to_string()
}

fn default_log_stdout() -> bool {
    true
}

fn default_agent_timeout() -> u64 {
    300
}

fn default_health_checks() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config {
            database: DatabaseConfig {
                url: default_db_url(),
                namespace: default_namespace(),
                database: default_database(),
                pool_size: default_pool_size(),
                connection_timeout_secs: default_connection_timeout(),
            },
            models: ModelConfig {
                default_provider: default_provider(),
                default_model: default_model(),
                tiers: ModelTiers {
                    routine: default_routine_model(),
                    interactive: default_interactive_model(),
                    investigative: default_investigative_model(),
                    critical: default_critical_model(),
                },
                providers: ProvidersConfig::default(),
            },
            #[cfg(feature = "discord")]
            discord: DiscordConfig::default(),
            #[cfg(feature = "mcp")]
            mcp: McpConfig::default(),
            logging: LoggingConfig::default(),
            agents: AgentConfig::default(),
        };

        // Should serialize and deserialize correctly
        let serialized = toml::to_string(&config).unwrap();
        let _deserialized: Config = toml::from_str(&serialized).unwrap();
    }
}
