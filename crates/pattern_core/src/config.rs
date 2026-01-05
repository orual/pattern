//! Configuration system for Pattern
//!
//! This module provides configuration structures and utilities for persisting
//! Pattern settings across sessions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::context::DEFAULT_BASE_INSTRUCTIONS;
use crate::data_source::{BlueskyStream, DefaultCommandValidator, LocalPtyBackend, ProcessSource};
use crate::db::ConstellationDatabases;
use crate::memory::CONSTELLATION_OWNER;
use crate::runtime::ToolContext;
use crate::runtime::endpoints::{BlueskyAgent, BlueskyEndpoint};
use crate::{
    Result,
    agent::tool_rules::ToolRule,
    context::compression::CompressionStrategy,
    //data_source::bluesky::BlueskyFilter,
    id::{AgentId, GroupId, MemoryId},
    memory::{BlockSchema, MemoryPermission, MemoryType},
};

/// Controls how TOML config and DB config are merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigPriority {
    /// DB values win for content, TOML wins for config metadata.
    #[default]
    Merge,
    /// TOML overwrites everything except memory content.
    TomlWins,
    /// Ignore TOML entirely for existing agents.
    DbWins,
}

/// Database configuration for SQLite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Path to the database directory.
    pub path: PathBuf,
}

impl DatabaseConfig {
    /// Path to the constellation database file.
    pub fn constellation_db(&self) -> PathBuf {
        self.path.join("constellation.db")
    }

    /// Path to the auth database file.
    pub fn auth_db(&self) -> PathBuf {
        self.path.join("auth.db")
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("pattern"),
        }
    }
}

/// Resolve a path relative to a base directory
/// If the path is absolute, return it as-is
/// If the path is relative, resolve it relative to the base directory
fn resolve_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct ShellSourceConfig {
    /// Name of the data source
    pub name: String,
    #[serde(flatten)]
    pub validator: DefaultCommandValidator,
}

// =============================================================================
// Data Source Configuration
// =============================================================================

/// Configuration for a data source subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataSourceConfig {
    /// Bluesky firehose subscription
    Bluesky(BlueskySourceConfig),
    /// Discord event subscription
    Discord(DiscordSourceConfig),
    /// File watching
    File(FileSourceConfig),
    Shell(ShellSourceConfig),
    /// Custom/external data source
    Custom(CustomSourceConfig),
}

impl DataSourceConfig {
    /// Get the name/identifier of this data source
    pub fn name(&self) -> &str {
        match self {
            DataSourceConfig::Bluesky(c) => &c.name,
            DataSourceConfig::Discord(c) => &c.name,
            DataSourceConfig::File(c) => &c.name,
            DataSourceConfig::Shell(c) => &c.name,
            DataSourceConfig::Custom(c) => &c.name,
        }
    }

    /// Create DataBlock sources from this config.
    ///
    /// Returns a Vec because some configs (like File with multiple paths)
    /// create multiple source instances.
    ///
    /// Returns empty Vec for stream-only sources (Bluesky, Discord).
    pub async fn create_blocks(
        &self,
        dbs: Arc<ConstellationDatabases>,
    ) -> crate::error::Result<Vec<std::sync::Arc<dyn crate::DataBlock>>> {
        use crate::data_source::FileSource;
        use std::sync::Arc;
        let _ = dbs;

        match self {
            DataSourceConfig::File(cfg) => {
                let sources: Vec<Arc<dyn crate::DataBlock>> = cfg
                    .paths
                    .iter()
                    .map(|path| -> Arc<dyn crate::DataBlock> {
                        Arc::new(FileSource::from_config(path.clone(), cfg))
                    })
                    .collect();
                Ok(sources)
            }
            DataSourceConfig::Custom(cfg) => {
                // TODO: inventory lookup for custom block sources
                tracing::warn!(
                    source_type = %cfg.source_type,
                    "Custom block source type not yet supported via inventory"
                );
                Ok(vec![])
            }

            // Bluesky and Discord are stream sources, not block sources
            DataSourceConfig::Shell(_)
            | DataSourceConfig::Bluesky(_)
            | DataSourceConfig::Discord(_) => Ok(vec![]),
        }
    }

    /// Create DataStream sources from this config.
    ///
    /// Returns a Vec because some configs might create multiple stream instances.
    ///
    /// Returns empty Vec for block-only sources (File).
    pub async fn create_streams(
        &self,
        dbs: Arc<ConstellationDatabases>,
        tool_context: Arc<dyn ToolContext>,
    ) -> crate::error::Result<Vec<std::sync::Arc<dyn crate::DataStream>>> {
        match self {
            DataSourceConfig::Bluesky(cfg) => {
                let (agent, did) = BlueskyAgent::load(CONSTELLATION_OWNER, dbs.as_ref()).await?;
                let stream = BlueskyStream::new(cfg.name.clone(), tool_context.clone())
                    .with_agent_did(did.clone())
                    .with_authenticated_agent(agent.clone())
                    .with_config(cfg.clone());
                let agent_id = tool_context.agent_id().to_string();
                let endpoint = BlueskyEndpoint::from_agent(agent, agent_id, did);
                tool_context
                    .router()
                    .register_endpoint("bluesky".to_string(), Arc::new(endpoint))
                    .await;
                Ok(vec![Arc::new(stream)])
            }
            DataSourceConfig::Discord(_cfg) => {
                // TODO: DiscordSource::from_config when implemented
                tracing::debug!("Discord stream source not yet implemented");
                Ok(vec![])
            }
            DataSourceConfig::Shell(cfg) => {
                let shell = ProcessSource::new(
                    "process",
                    Arc::new(LocalPtyBackend::new("./".into())),
                    Arc::new(cfg.validator.clone()),
                );
                Ok(vec![Arc::new(shell)])
            }
            DataSourceConfig::Custom(cfg) => {
                // TODO: inventory lookup for custom stream sources
                tracing::warn!(
                    source_type = %cfg.source_type,
                    "Custom stream source type not yet supported via inventory"
                );
                Ok(vec![])
            }
            // File is a block source, not a stream source
            DataSourceConfig::File(_) => Ok(vec![]),
        }
    }
}

/// Helper for serde default
fn default_true() -> bool {
    true
}

fn default_target() -> String {
    CONSTELLATION_OWNER.to_string()
}

/// Bluesky firehose data source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskySourceConfig {
    /// Identifier for this source
    pub name: String,
    /// Jetstream endpoint URL (defaults to public endpoint)
    #[serde(default = "default_jetstream_endpoint")]
    pub jetstream_endpoint: String,
    /// target to route notifications to (should be set to the agent or group id or name)
    #[serde(default = "default_target")]
    pub target: String,
    /// NSIDs to filter for (e.g., "app.bsky.feed.post")
    #[serde(default)]
    pub nsids: Vec<String>,
    /// Specific DIDs to watch (empty = all)
    #[serde(default)]
    pub dids: Vec<String>,
    /// Keywords to filter posts by
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Languages to filter by (e.g., ["en", "es"])
    #[serde(default)]
    pub languages: Vec<String>,
    /// Only include posts that mention these DIDs (agent DID should be here)
    #[serde(default)]
    pub mentions: Vec<String>,
    /// Friends list - always see posts from these DIDs (bypasses mention requirement)
    #[serde(default)]
    pub friends: Vec<String>,
    /// Allow mentions from anyone, not just allowlisted DIDs
    #[serde(default)]
    pub allow_any_mentions: bool,
    /// Keywords to exclude - filter out posts containing these (takes precedence)
    #[serde(default)]
    pub exclude_keywords: Vec<String>,
    /// DIDs to exclude - never show posts from these (takes precedence over all inclusion filters)
    #[serde(default)]
    pub exclude_dids: Vec<String>,
    /// Only show threads where agent is actively participating (default: true)
    #[serde(default = "default_true")]
    pub require_agent_participation: bool,
}

impl Default for BlueskySourceConfig {
    fn default() -> Self {
        Self {
            name: "bluesky".to_string(),
            jetstream_endpoint: default_jetstream_endpoint(),
            target: default_target(),
            nsids: vec![],
            dids: vec![],
            keywords: vec![],
            languages: vec![],
            mentions: vec![],
            friends: vec![],
            allow_any_mentions: false,
            exclude_keywords: vec![],
            exclude_dids: vec![],
            require_agent_participation: true,
        }
    }
}

/// Discord event data source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordSourceConfig {
    /// Identifier for this source
    pub name: String,
    /// Guild ID to monitor (optional, monitors all if not specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    /// Channel IDs to monitor (empty = all)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_ids: Vec<String>,
}

/// File watching data source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSourceConfig {
    /// Identifier for this source
    pub name: String,
    /// Paths to watch (directories or files)
    pub paths: Vec<PathBuf>,
    /// Whether to watch directories recursively
    #[serde(default)]
    pub recursive: bool,
    /// Glob patterns for included files (empty = include all)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_patterns: Vec<String>,
    /// Glob patterns for excluded files
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_patterns: Vec<String>,
    /// Permission rules for file access (glob pattern -> permission)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_rules: Vec<FilePermissionRuleConfig>,
}

/// Permission rule for file access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePermissionRuleConfig {
    /// Glob pattern: "*.config.toml", "src/**/*.rs"
    pub pattern: String,
    /// Permission level: read_only, read_write, append
    #[serde(default)]
    pub permission: MemoryPermission,
}

/// Custom/external data source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomSourceConfig {
    /// Identifier for this source
    pub name: String,
    /// Type identifier for the custom source
    pub source_type: String,
    /// Arbitrary configuration data
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Top-level configuration for Pattern
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternConfig {
    /// Database configuration (path is directory containing both DBs).
    #[serde(default)]
    pub database: DatabaseConfig,

    /// Global model defaults.
    #[serde(default)]
    pub model: ModelConfig,

    /// Agent configurations (inline or file references).
    #[serde(default)]
    pub agents: Vec<AgentConfigRef>,

    /// Group configurations.
    #[serde(default)]
    pub groups: Vec<GroupConfig>,

    /// Bluesky configuration.
    #[serde(default)]
    pub bluesky: Option<BlueskyConfig>,

    /// Discord configuration.
    #[serde(default)]
    pub discord: Option<DiscordAppConfig>,
}

/// Discord options in pattern.toml (non-sensitive)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscordAppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_channels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_guilds: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_users: Option<Vec<String>>,
}

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent ID (persisted once created)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<AgentId>,

    /// Agent name
    pub name: String,

    /// System prompt/base instructions for the agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Path to file containing system prompt (alternative to inline)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_path: Option<PathBuf>,

    /// Agent persona (creates a core memory block)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,

    /// Path to file containing persona (alternative to inline)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona_path: Option<PathBuf>,

    /// Additional instructions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    /// Initial memory blocks
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub memory: HashMap<String, MemoryBlockConfig>,

    /// Optional Bluesky handle for this agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bluesky_handle: Option<String>,

    /// Data sources attached to this agent
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub data_sources: HashMap<String, DataSourceConfig>,

    /// Tool execution rules for this agent
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_rules: Vec<ToolRuleConfig>,

    /// Available tools for this agent
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,

    /// Optional model configuration (overrides global model config)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,

    /// Optional context configuration (overrides defaults)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigOptions>,
}

/// Configuration for tool execution rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRuleConfig {
    /// Name of the tool this rule applies to
    pub tool_name: String,

    /// Type of rule
    pub rule_type: ToolRuleTypeConfig,

    /// Conditions for this rule (tool names, parameters, etc.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,

    /// Priority of this rule (higher numbers = higher priority)
    #[serde(default = "default_rule_priority")]
    pub priority: u8,

    /// Optional metadata for this rule
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Configuration for tool rule types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ToolRuleTypeConfig {
    /// Continue the conversation loop after this tool is called (no heartbeat required)
    ContinueLoop,

    /// Exit conversation loop after this tool is called
    ExitLoop,

    /// This tool must be called after specified tools (ordering dependency)
    RequiresPrecedingTools,

    /// This tool must be called before specified tools
    RequiresFollowingTools,

    /// Multiple exclusive groups - only one tool from each group can be called per conversation
    ExclusiveGroups(Vec<Vec<String>>),

    /// Call this tool at conversation start
    StartConstraint,

    /// This tool must be called before conversation ends
    RequiredBeforeExit,

    /// Required for exit if condition is met
    RequiredBeforeExitIf,

    /// Maximum number of times this tool can be called
    MaxCalls(u32),

    /// Minimum cooldown period between calls (in seconds)
    Cooldown(u64),

    /// Call this tool periodically during long conversations (in seconds)
    Periodic(u64),

    /// Require user consent before executing the tool
    RequiresConsent {
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },

    /// Only allow these operations for multi-operation tools.
    AllowedOperations(std::collections::BTreeSet<String>),

    /// This tool is required for some other tool/data source
    Needed,
}

fn default_rule_priority() -> u8 {
    5
}

impl ToolRuleConfig {
    /// Convert configuration to runtime ToolRule
    pub fn to_tool_rule(&self) -> Result<ToolRule> {
        let rule_type = self.rule_type.to_runtime_type()?;
        let mut tool_rule = ToolRule::new(self.tool_name.clone(), rule_type);

        if !self.conditions.is_empty() {
            tool_rule = tool_rule.with_conditions(self.conditions.clone());
        }

        tool_rule = tool_rule.with_priority(self.priority);

        if let Some(metadata) = &self.metadata {
            tool_rule = tool_rule.with_metadata(metadata.clone());
        }

        Ok(tool_rule)
    }

    /// Create configuration from runtime ToolRule
    pub fn from_tool_rule(rule: &ToolRule) -> Self {
        Self {
            tool_name: rule.tool_name.clone(),
            rule_type: ToolRuleTypeConfig::from_runtime_type(&rule.rule_type),
            conditions: rule.conditions.clone(),
            priority: rule.priority,
            metadata: rule.metadata.clone(),
        }
    }
}

impl ToolRuleTypeConfig {
    /// Convert configuration type to runtime type
    pub fn to_runtime_type(&self) -> Result<crate::agent::tool_rules::ToolRuleType> {
        use crate::agent::tool_rules::ToolRuleType;
        use std::time::Duration;

        let runtime_type = match self {
            ToolRuleTypeConfig::ContinueLoop => ToolRuleType::ContinueLoop,
            ToolRuleTypeConfig::ExitLoop => ToolRuleType::ExitLoop,
            ToolRuleTypeConfig::RequiresPrecedingTools => ToolRuleType::RequiresPrecedingTools,
            ToolRuleTypeConfig::RequiresFollowingTools => ToolRuleType::RequiresFollowingTools,
            ToolRuleTypeConfig::ExclusiveGroups(groups) => {
                ToolRuleType::ExclusiveGroups(groups.clone())
            }
            ToolRuleTypeConfig::StartConstraint => ToolRuleType::StartConstraint,
            ToolRuleTypeConfig::RequiredBeforeExit => ToolRuleType::RequiredBeforeExit,
            ToolRuleTypeConfig::RequiredBeforeExitIf => ToolRuleType::RequiredBeforeExitIf,
            ToolRuleTypeConfig::MaxCalls(max) => ToolRuleType::MaxCalls(*max),
            ToolRuleTypeConfig::Cooldown(seconds) => {
                ToolRuleType::Cooldown(Duration::from_secs(*seconds))
            }
            ToolRuleTypeConfig::Periodic(seconds) => {
                ToolRuleType::Periodic(Duration::from_secs(*seconds))
            }
            ToolRuleTypeConfig::RequiresConsent { scope } => ToolRuleType::RequiresConsent {
                scope: scope.clone(),
            },
            ToolRuleTypeConfig::AllowedOperations(ops) => {
                ToolRuleType::AllowedOperations(ops.clone())
            }
            ToolRuleTypeConfig::Needed => ToolRuleType::Needed,
        };

        Ok(runtime_type)
    }

    /// Create configuration type from runtime type
    pub fn from_runtime_type(runtime_type: &crate::agent::tool_rules::ToolRuleType) -> Self {
        use crate::agent::tool_rules::ToolRuleType;

        match runtime_type {
            ToolRuleType::ContinueLoop => ToolRuleTypeConfig::ContinueLoop,
            ToolRuleType::ExitLoop => ToolRuleTypeConfig::ExitLoop,
            ToolRuleType::RequiresPrecedingTools => ToolRuleTypeConfig::RequiresPrecedingTools,
            ToolRuleType::RequiresFollowingTools => ToolRuleTypeConfig::RequiresFollowingTools,
            ToolRuleType::ExclusiveGroups(groups) => {
                ToolRuleTypeConfig::ExclusiveGroups(groups.clone())
            }
            ToolRuleType::StartConstraint => ToolRuleTypeConfig::StartConstraint,
            ToolRuleType::RequiredBeforeExit => ToolRuleTypeConfig::RequiredBeforeExit,
            ToolRuleType::RequiredBeforeExitIf => ToolRuleTypeConfig::RequiredBeforeExitIf,
            ToolRuleType::MaxCalls(max) => ToolRuleTypeConfig::MaxCalls(*max),
            ToolRuleType::Cooldown(duration) => ToolRuleTypeConfig::Cooldown(duration.as_secs()),
            ToolRuleType::Periodic(duration) => ToolRuleTypeConfig::Periodic(duration.as_secs()),
            ToolRuleType::RequiresConsent { scope } => ToolRuleTypeConfig::RequiresConsent {
                scope: scope.clone(),
            },
            ToolRuleType::AllowedOperations(ops) => {
                ToolRuleTypeConfig::AllowedOperations(ops.clone())
            }
            ToolRuleType::Needed => ToolRuleTypeConfig::Needed,
        }
    }
}

impl AgentConfig {
    /// Convert tool rule configurations to runtime tool rules
    pub fn get_tool_rules(&self) -> Result<Vec<ToolRule>> {
        self.tool_rules
            .iter()
            .map(|config| config.to_tool_rule())
            .collect()
    }

    /// Set tool rules from runtime types
    pub fn set_tool_rules(&mut self, rules: &[ToolRule]) {
        self.tool_rules = rules.iter().map(ToolRuleConfig::from_tool_rule).collect();
    }

    /// Convert to database Agent model for persistence
    pub fn to_db_agent(&self, id: &str) -> pattern_db::models::Agent {
        use pattern_db::models::{Agent, AgentStatus};
        use sqlx::types::Json;

        let model = self.model.as_ref();

        Agent {
            id: id.to_string(),
            name: self.name.clone(),
            description: None,
            model_provider: model
                .map(|m| m.provider.clone())
                .unwrap_or_else(|| "anthropic".to_string()),
            model_name: model
                .and_then(|m| m.model.clone())
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
            system_prompt: self.system_prompt.clone().unwrap_or_default(),
            config: Json(serde_json::to_value(self).unwrap_or_default()),
            enabled_tools: Json(self.tools.clone()),
            tool_rules: if self.tool_rules.is_empty() {
                None
            } else {
                Some(Json(
                    serde_json::to_value(&self.tool_rules).unwrap_or_default(),
                ))
            },
            status: AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

impl AgentConfig {
    /// Load agent configuration from a file
    pub async fn load_from_file(path: &Path) -> Result<Self> {
        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            crate::CoreError::ConfigurationError {
                field: "agent config file".to_string(),
                config_path: path.display().to_string(),
                expected: "valid TOML file".to_string(),
                cause: crate::error::ConfigError::Io(e.to_string()),
            }
        })?;

        let mut config: AgentConfig =
            toml::from_str(&content).map_err(|e| crate::CoreError::ConfigurationError {
                field: "agent config".to_string(),
                config_path: path.display().to_string(),
                expected: "valid agent configuration".to_string(),
                cause: crate::error::ConfigError::TomlParse(e.to_string()),
            })?;

        // Resolve paths relative to the config file's directory
        let base_dir = path.parent().unwrap_or(Path::new("."));

        // Load system prompt from system_prompt_path if specified
        if let Some(ref system_prompt_path) = config.system_prompt_path {
            let resolved_path = resolve_path(base_dir, system_prompt_path);
            match tokio::fs::read_to_string(&resolved_path).await {
                Ok(system_prompt_content) => {
                    config.system_prompt = Some(system_prompt_content.trim().to_string());
                    // Clear system_prompt_path since we've loaded it inline
                    config.system_prompt_path = None;
                }
                Err(e) => {
                    return Err(crate::CoreError::ConfigurationError {
                        field: "system_prompt_path".to_string(),
                        config_path: path.display().to_string(),
                        expected: format!("readable file at {}", resolved_path.display()),
                        cause: crate::error::ConfigError::Io(e.to_string()),
                    });
                }
            }
        }

        // Load persona from persona_path if specified
        if let Some(ref persona_path) = config.persona_path {
            let resolved_path = resolve_path(base_dir, persona_path);
            tracing::info!("Loading persona from path: {}", resolved_path.display());
            match tokio::fs::read_to_string(&resolved_path).await {
                Ok(persona_content) => {
                    tracing::info!("Loaded persona content: {} chars", persona_content.len());
                    config.persona = Some(persona_content.trim().to_string());
                    // Clear persona_path since we've loaded it inline
                    config.persona_path = None;
                    tracing::info!("Persona loaded and persona_path cleared");
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to load persona from {}: {}",
                        resolved_path.display(),
                        e
                    );
                    return Err(crate::CoreError::ConfigurationError {
                        field: "persona_path".to_string(),
                        config_path: path.display().to_string(),
                        expected: format!("readable file at {}", resolved_path.display()),
                        cause: crate::error::ConfigError::Io(e.to_string()),
                    });
                }
            }
        }

        // Resolve memory block content_paths
        for (_, memory_block) in config.memory.iter_mut() {
            if let Some(ref content_path) = memory_block.content_path {
                memory_block.content_path = Some(resolve_path(base_dir, content_path));
            }
        }

        Ok(config)
    }
}

/// Reference to an agent config - either inline or from a file path.
///
/// When deserializing, this enum uses `#[serde(untagged)]` to automatically
/// determine the variant. The `Path` variant is tried first (single `config_path`
/// field), then `Inline` (full `AgentConfig` structure).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentConfigRef {
    /// Load config from an external file.
    Path {
        /// Path to the agent config TOML file.
        config_path: PathBuf,
    },
    /// Inline agent configuration.
    Inline(AgentConfig),
}

impl AgentConfigRef {
    /// Resolve to an AgentConfig, loading from file if needed.
    ///
    /// For `Path` variant, loads and parses the TOML file at the given path.
    /// For `Inline` variant, returns a clone of the embedded config.
    ///
    /// # Arguments
    /// * `base_dir` - Base directory for resolving relative paths in the config_path.
    pub async fn resolve(&self, base_dir: &Path) -> Result<AgentConfig> {
        match self {
            AgentConfigRef::Inline(config) => Ok(config.clone()),
            AgentConfigRef::Path { config_path } => {
                let path = resolve_path(base_dir, config_path);
                AgentConfig::load_from_file(&path).await
            }
        }
    }
}

/// Configuration for a memory block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockConfig {
    /// Content of the memory block (inline)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Path to file containing the content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_path: Option<PathBuf>,

    /// Permission level for this block
    #[serde(default)]
    pub permission: MemoryPermission,

    /// Type of memory (core, working, archival)
    #[serde(default)]
    pub memory_type: MemoryType,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional ID for shared memory blocks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<MemoryId>,

    /// Whether this memory should be shared with other agents
    #[serde(default)]
    pub shared: bool,

    /// Whether block is always loaded into context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,

    /// Maximum content size in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub char_limit: Option<usize>,

    /// Schema for structured content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<BlockSchema>,
}

impl MemoryBlockConfig {
    /// Load content from either inline or file path
    pub async fn load_content(&self) -> Result<String> {
        if let Some(content) = &self.content {
            Ok(content.clone())
        } else if let Some(path) = &self.content_path {
            tokio::fs::read_to_string(path).await.map_err(|e| {
                crate::CoreError::ConfigurationError {
                    field: "content_path".to_string(),
                    config_path: path.display().to_string(),
                    expected: "valid file path".to_string(),
                    cause: crate::error::ConfigError::Io(e.to_string()),
                }
            })
        } else {
            // Empty content is valid - allows declaring blocks with just permission/type
            Ok(String::new())
        }
    }
}

/// Configuration for an agent group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupConfig {
    /// Optional ID (generated if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<GroupId>,

    /// Name of the group
    pub name: String,

    /// Description of the group's purpose
    pub description: String,

    /// Coordination pattern to use
    pub pattern: GroupPatternConfig,

    /// Members of this group
    pub members: Vec<GroupMemberConfig>,

    /// Shared memory blocks accessible to all group members
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub shared_memory: HashMap<String, MemoryBlockConfig>,

    /// Data sources attached to this group
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub data_sources: HashMap<String, DataSourceConfig>,
}

/// Configuration for a group member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberConfig {
    /// Friendly name for this agent in the group
    pub name: String,

    /// Optional agent ID (if referencing existing agent)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,

    /// Optional path to agent configuration file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<PathBuf>,

    /// Optional inline agent configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_config: Option<AgentConfig>,

    /// Role in the group
    #[serde(default)]
    pub role: GroupMemberRoleConfig,

    /// Capabilities this agent brings
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Member role configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GroupMemberRoleConfig {
    #[default]
    Regular,
    Supervisor,
    Observer,
    Specialist {
        domain: String,
    },
}

/// Configuration for a sleeptime trigger
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleeptimeTriggerConfig {
    /// Name of the trigger
    pub name: String,
    /// Condition that activates this trigger
    pub condition: TriggerConditionConfig,
    /// Priority of this trigger
    pub priority: TriggerPriorityConfig,
}

/// Configuration for trigger conditions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConditionConfig {
    /// Time-based trigger
    TimeElapsed {
        /// Duration in seconds
        duration: u64,
    },
    /// Metric-based trigger
    MetricThreshold {
        /// Metric name
        metric: String,
        /// Threshold value
        threshold: f64,
    },
    /// Constellation activity trigger
    ConstellationActivity {
        /// Number of messages to trigger
        message_threshold: u32,
        /// Time window in seconds
        time_threshold: u64,
    },
    /// Custom evaluator
    Custom {
        /// Custom evaluator name
        evaluator: String,
    },
}

/// Configuration for trigger priority
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerPriorityConfig {
    Critical,
    High,
    Medium,
    Low,
}

/// Configuration for coordination patterns
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GroupPatternConfig {
    /// One agent leads, others follow
    Supervisor {
        /// The agent that leads (by member name)
        leader: String,
    },
    /// Agents take turns in order
    RoundRobin {
        /// Whether to skip unavailable agents
        #[serde(default = "default_skip_unavailable")]
        skip_unavailable: bool,
    },
    /// Sequential processing pipeline
    Pipeline {
        /// Ordered list of member names for each stage
        stages: Vec<String>,
    },
    /// Dynamic selection based on context
    Dynamic {
        /// Selector strategy name
        selector: String,
        /// Optional configuration for the selector
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        selector_config: HashMap<String, String>,
    },
    /// Background monitoring
    Sleeptime {
        /// Check interval in seconds
        check_interval: u64,
        /// Triggers that can activate intervention
        triggers: Vec<SleeptimeTriggerConfig>,
        /// Optional member name to activate on triggers (uses least recently active if not specified)
        #[serde(skip_serializing_if = "Option::is_none")]
        intervention_agent: Option<String>,
    },
}

fn default_skip_unavailable() -> bool {
    true
}

/// Bluesky/ATProto configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyConfig {
    /// Default filter for the firehose
    //#[serde(default, skip_serializing_if = "Option::is_none")]
    //pub default_filter: Option<BlueskyFilter>,

    /// Whether to automatically connect to firehose on startup
    #[serde(default)]
    pub auto_connect_firehose: bool,

    /// Jetstream endpoint URL (defaults to public endpoint)
    #[serde(default = "default_jetstream_endpoint")]
    pub jetstream_endpoint: String,
}

fn default_jetstream_endpoint() -> String {
    "wss://jetstream1.us-east.fire.hose.cam/subscribe".to_string()
}

/// Model provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider name (e.g., "anthropic", "openai")
    pub provider: String,

    /// Optional specific model to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Optional temperature setting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Additional provider-specific settings
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, toml::Value>,
}

// Default implementations
impl Default for PatternConfig {
    fn default() -> Self {
        Self {
            database: DatabaseConfig::default(),
            model: ModelConfig::default(),
            agents: Vec::new(),
            groups: Vec::new(),
            bluesky: None,
            discord: None,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            id: None,
            name: "Assistant".to_string(),
            system_prompt: None,
            system_prompt_path: None,
            persona: None,
            persona_path: None,
            instructions: None,
            memory: HashMap::new(),
            bluesky_handle: None,
            data_sources: HashMap::new(),
            tool_rules: Vec::new(),
            tools: Vec::new(),
            model: None,
            context: None,
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "Gemini".to_string(),
            model: None,
            temperature: None,
            settings: HashMap::new(),
        }
    }
}

// MemoryPermission already has Default derived

// Utility functions

/// Load configuration from a TOML file
pub async fn load_config(path: &Path) -> Result<PatternConfig> {
    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
        crate::CoreError::ConfigurationError {
            config_path: path.display().to_string(),
            field: "file".to_string(),
            expected: "readable TOML file".to_string(),
            cause: crate::error::ConfigError::Io(e.to_string()),
        }
    })?;

    let mut config: PatternConfig =
        toml::from_str(&content).map_err(|e| crate::CoreError::ConfigurationError {
            config_path: path.display().to_string(),
            field: "content".to_string(),
            expected: "valid TOML configuration".to_string(),
            cause: crate::error::ConfigError::TomlParse(e.to_string()),
        })?;

    // Resolve paths relative to the config file's directory
    let base_dir = path.parent().unwrap_or(Path::new("."));

    // Resolve config_path in AgentConfigRef::Path variants
    for agent_ref in config.agents.iter_mut() {
        if let AgentConfigRef::Path { config_path } = agent_ref {
            *config_path = resolve_path(base_dir, config_path);
        }
        // Note: For Inline agents, memory block paths are resolved when the
        // AgentConfig is used, not here. Path agents resolve paths in load_from_file.
    }

    // Resolve paths in group members
    for group in config.groups.iter_mut() {
        for member in group.members.iter_mut() {
            if let Some(ref config_path) = member.config_path {
                member.config_path = Some(resolve_path(base_dir, config_path));
            }
        }
    }

    Ok(config)
}

/// Save configuration to a TOML file
pub async fn save_config(config: &PatternConfig, path: &Path) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            crate::CoreError::ConfigurationError {
                config_path: parent.display().to_string(),
                field: "directory".to_string(),
                expected: "writable directory".to_string(),
                cause: crate::error::ConfigError::Io(e.to_string()),
            }
        })?;
    }

    let content =
        toml::to_string_pretty(config).map_err(|e| crate::CoreError::ConfigurationError {
            config_path: path.display().to_string(),
            field: "serialization".to_string(),
            expected: "serializable config structure".to_string(),
            cause: crate::error::ConfigError::TomlSerialize(e.to_string()),
        })?;

    tokio::fs::write(path, content)
        .await
        .map_err(|e| crate::CoreError::ConfigurationError {
            config_path: path.display().to_string(),
            field: "file".to_string(),
            expected: "writable file location".to_string(),
            cause: crate::error::ConfigError::Io(e.to_string()),
        })?;

    Ok(())
}

/// Merge two configurations, with the overlay taking precedence
pub fn merge_configs(base: PatternConfig, overlay: PartialConfig) -> PatternConfig {
    PatternConfig {
        database: overlay.database.unwrap_or(base.database),
        model: overlay.model.unwrap_or(base.model),
        agents: overlay.agents.unwrap_or(base.agents),
        groups: overlay.groups.unwrap_or(base.groups),
        bluesky: overlay.bluesky.or(base.bluesky),
        discord: base.discord,
    }
}

/// Partial configuration for overlaying
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentConfigRef>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<GroupConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub bluesky: Option<BlueskyConfig>,
}

/// Partial agent configuration for overlaying
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialAgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<AgentId>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<HashMap<String, MemoryBlockConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub bluesky_handle: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_sources: Option<HashMap<String, DataSourceConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_rules: Option<Vec<ToolRuleConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
}

impl From<&pattern_db::models::Agent> for PartialAgentConfig {
    fn from(agent: &pattern_db::models::Agent) -> Self {
        // Start from JSON config if parseable, otherwise default
        let mut config: PartialAgentConfig =
            serde_json::from_value(agent.config.0.clone()).unwrap_or_default();

        // Always merge authoritative fields from DB columns (JSON may be stale/incomplete)
        config.id = Some(AgentId(agent.id.clone()));
        config.name = Some(agent.name.clone());

        // Use DB system_prompt if config's is missing/empty
        if config.system_prompt.is_none()
            || config.system_prompt.as_ref().is_some_and(|s| s.is_empty())
        {
            if !agent.system_prompt.is_empty() {
                config.system_prompt = Some(agent.system_prompt.clone());
            }
        }

        // Use DB model info if config's is missing
        if config.model.is_none() {
            config.model = Some(ModelConfig {
                provider: agent.model_provider.clone(),
                model: Some(agent.model_name.clone()),
                temperature: None,
                settings: HashMap::new(),
            });
        }

        // Use DB tools if config's is missing/empty
        if config.tools.is_none() || config.tools.as_ref().is_some_and(|t| t.is_empty()) {
            if !agent.enabled_tools.0.is_empty() {
                config.tools = Some(agent.enabled_tools.0.clone());
            }
        }

        // Use DB tool_rules if config's is missing
        if config.tool_rules.is_none() {
            if let Some(ref rules_json) = agent.tool_rules {
                config.tool_rules = serde_json::from_value(rules_json.0.clone()).ok();
            }
        }

        config
    }
}

/// Per-agent overrides - highest priority in config cascade
///
/// Used when loading an agent with runtime modifications that
/// shouldn't be persisted to the database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOverrides {
    /// Override model provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,

    /// Override model name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,

    /// Override system prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Override temperature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Override tool rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_rules: Option<Vec<ToolRuleConfig>>,

    /// Override enabled tools
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,

    /// Override context settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigOptions>,
}

impl AgentOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_model(mut self, provider: &str, name: &str) -> Self {
        self.model_provider = Some(provider.to_string());
        self.model_name = Some(name.to_string());
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }
}

/// Fully resolved agent configuration
///
/// All fields are concrete (no Options for required values).
/// Created by resolving the config cascade.
#[derive(Debug, Clone)]
pub struct ResolvedAgentConfig {
    pub id: AgentId,
    pub name: String,
    pub model_provider: String,
    pub model_name: String,
    pub system_prompt: String,
    pub persona: Option<String>,
    pub tool_rules: Vec<ToolRule>,
    pub enabled_tools: Vec<String>,
    pub memory_blocks: HashMap<String, MemoryBlockConfig>,
    pub data_sources: HashMap<String, DataSourceConfig>,
    pub context: ContextConfigOptions,
    pub temperature: Option<f32>,
}

impl ResolvedAgentConfig {
    /// Resolve from AgentConfig with defaults filled in
    pub fn from_agent_config(config: &AgentConfig, defaults: &AgentConfig) -> Self {
        let model = config.model.as_ref().or(defaults.model.as_ref());
        // TODO: revisit this, so it's easier to get the default base instructions plus whatever else
        let mut system_prompt = config
            .system_prompt
            .clone()
            .unwrap_or(DEFAULT_BASE_INSTRUCTIONS.to_string());
        system_prompt.push_str("\n");
        system_prompt.push_str(&config.instructions.clone().unwrap_or_default());
        Self {
            id: config.id.clone().unwrap_or_else(AgentId::generate),
            name: config.name.clone(),
            model_provider: model
                .map(|m| m.provider.clone())
                .unwrap_or_else(|| "anthropic".to_string()),
            model_name: model
                .and_then(|m| m.model.clone())
                .unwrap_or_else(|| "claude-sonnet-4-5-20250929".to_string()),
            system_prompt,
            persona: config.persona.clone(),
            tool_rules: config.get_tool_rules().unwrap_or_default(),
            enabled_tools: config.tools.clone(),
            memory_blocks: config.memory.clone(),
            data_sources: config.data_sources.clone(),
            context: config.context.clone().unwrap_or_default(),
            temperature: model.and_then(|m| m.temperature),
        }
    }

    /// Apply overrides to this resolved config
    pub fn apply_overrides(mut self, overrides: &AgentOverrides) -> Self {
        if let Some(ref provider) = overrides.model_provider {
            self.model_provider = provider.clone();
        }
        if let Some(ref name) = overrides.model_name {
            self.model_name = name.clone();
        }
        if let Some(ref prompt) = overrides.system_prompt {
            self.system_prompt = prompt.clone();
        }
        if let Some(temp) = overrides.temperature {
            self.temperature = Some(temp);
        }
        if let Some(ref rules) = overrides.tool_rules {
            self.tool_rules = rules.iter().filter_map(|r| r.to_tool_rule().ok()).collect();
        }
        if let Some(ref tools) = overrides.enabled_tools {
            self.enabled_tools = tools.clone();
        }
        if let Some(ref ctx) = overrides.context {
            self.context = ctx.clone();
        }
        self
    }
}

pub fn merge_agent_configs(base: AgentConfig, overlay: PartialAgentConfig) -> AgentConfig {
    AgentConfig {
        id: overlay.id.or(base.id),
        name: overlay.name.unwrap_or(base.name),
        system_prompt: overlay.system_prompt.or(base.system_prompt),
        system_prompt_path: None, // Not present in PartialAgentConfig, so always None in merge
        persona: overlay.persona.or(base.persona),
        persona_path: None, // Not present in PartialAgentConfig, so always None in merge
        instructions: overlay.instructions.or(base.instructions),
        memory: if let Some(overlay_memory) = overlay.memory {
            // Merge memory blocks, overlay takes precedence
            let mut merged = base.memory;
            merged.extend(overlay_memory);
            merged
        } else {
            base.memory
        },
        bluesky_handle: overlay.bluesky_handle.or(base.bluesky_handle),
        data_sources: if let Some(overlay_sources) = overlay.data_sources {
            // Merge data sources, overlay takes precedence
            let mut merged = base.data_sources;
            merged.extend(overlay_sources);
            merged
        } else {
            base.data_sources
        },
        tool_rules: overlay.tool_rules.unwrap_or(base.tool_rules),
        tools: overlay.tools.unwrap_or(base.tools),
        model: overlay.model.or(base.model),
        context: base.context, // Keep base context config for now (no overlay field yet)
    }
}

/// Optional context configuration for agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfigOptions {
    /// Maximum messages to keep before compression (hard cap)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<usize>,

    /// Compression strategy to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression_strategy: Option<CompressionStrategy>,

    /// Characters limit per memory block
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_char_limit: Option<usize>,

    /// Whether to enable thinking/reasoning
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,

    /// Whether to include tool descriptions in context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_descriptions: Option<bool>,

    /// Whether to include tool schemas in context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_schemas: Option<bool>,

    /// Limit for activity entries in context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_entries_limit: Option<usize>,
}

impl Default for ContextConfigOptions {
    fn default() -> Self {
        Self {
            max_messages: None,
            compression_strategy: None,
            memory_char_limit: None,
            enable_thinking: None,
            include_descriptions: None,
            include_schemas: None,
            activity_entries_limit: None,
        }
    }
}

/// Standard config file locations
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Project-specific config
    paths.push(PathBuf::from("pattern.toml"));

    // User config directory
    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("pattern").join("config.toml"));
    }

    // Home directory fallback
    if let Some(home_dir) = dirs::home_dir() {
        paths.push(home_dir.join(".pattern").join("config.toml"));
    }

    paths
}

/// Load configuration from standard locations
pub async fn load_config_from_standard_locations() -> Result<PatternConfig> {
    for path in config_paths() {
        if path.exists() {
            return load_config(&path).await;
        }
    }

    // No config found, return default
    Ok(PatternConfig::default())
}

impl PatternConfig {
    /// Load configuration from standard locations
    pub async fn load() -> Result<Self> {
        load_config_from_standard_locations().await
    }

    /// Load configuration from a specific file
    pub async fn load_from(path: &Path) -> Result<Self> {
        load_config(path).await
    }

    /// Save configuration to a specific file
    pub async fn save_to(&self, path: &Path) -> Result<()> {
        save_config(self, path).await
    }

    /// Save configuration to standard location
    pub async fn save(&self) -> Result<()> {
        let config_path = config_paths()
            .into_iter()
            .find(|p| p.parent().map_or(false, |parent| parent.exists()))
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("pattern")
                    .join("config.toml")
            });

        self.save_to(&config_path).await
    }

    /// Load config with deprecation checks.
    ///
    /// Returns error for hard-deprecated patterns (singular [agent]).
    /// Warns for soft-deprecated patterns ([user]).
    pub async fn load_with_deprecation_check(
        path: &Path,
    ) -> std::result::Result<Self, crate::error::ConfigError> {
        use crate::error::ConfigError;

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ConfigError::Io(e.to_string()))?;
        let raw: toml::Value =
            toml::from_str(&content).map_err(|e| ConfigError::TomlParse(e.to_string()))?;

        // Check for deprecated patterns.
        if raw.get("agent").is_some() && raw.get("agents").is_none() {
            return Err(ConfigError::Deprecated {
                field: "agent".into(),
                message: "Singular [agent] is deprecated. Use [[agents]].\n\
                         Run: pattern config migrate"
                    .into(),
            });
        }

        if raw.get("user").is_some() {
            tracing::warn!("[user] block is deprecated and ignored. Remove it from config.");
        }

        // Convert Value to PatternConfig instead of re-parsing.
        raw.try_into()
            .map_err(|e: toml::de::Error| ConfigError::TomlParse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PatternConfig::default();
        assert!(config.agents.is_empty());
        assert_eq!(config.model.provider, "Gemini");
        assert!(config.groups.is_empty());
    }

    #[test]
    fn test_config_serialization() {
        let config = PatternConfig::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        assert!(toml.contains("[model]"));
        assert!(toml.contains("[database]"));
    }

    #[test]
    fn test_tool_rules_configuration() {
        use crate::agent::tool_rules::{ToolRule, ToolRuleType};
        use std::time::Duration;

        // Create tool rules
        let rules = vec![
            ToolRule::start_constraint("setup".to_string()),
            ToolRule::continue_loop("fast_search".to_string()),
            ToolRule::max_calls("api_call".to_string(), 3),
            ToolRule::cooldown("slow_tool".to_string(), Duration::from_secs(5)),
        ];

        // Create agent config with tool rules
        let mut agent_config = AgentConfig::default();
        agent_config.set_tool_rules(&rules);

        // Test conversion
        let loaded_rules = agent_config.get_tool_rules().unwrap();
        assert_eq!(loaded_rules.len(), 4);

        // Test individual rule types
        assert_eq!(loaded_rules[0].tool_name, "setup");
        assert!(matches!(
            loaded_rules[0].rule_type,
            ToolRuleType::StartConstraint
        ));

        assert_eq!(loaded_rules[1].tool_name, "fast_search");
        assert!(matches!(
            loaded_rules[1].rule_type,
            ToolRuleType::ContinueLoop
        ));

        assert_eq!(loaded_rules[2].tool_name, "api_call");
        assert!(matches!(
            loaded_rules[2].rule_type,
            ToolRuleType::MaxCalls(3)
        ));

        assert_eq!(loaded_rules[3].tool_name, "slow_tool");
        assert!(matches!(
            loaded_rules[3].rule_type,
            ToolRuleType::Cooldown(_)
        ));
    }

    #[test]
    fn test_tool_rule_config_serialization() {
        use crate::agent::tool_rules::ToolRule;
        use std::time::Duration;

        let rule = ToolRule::cooldown("test_tool".to_string(), Duration::from_secs(30));
        let config_rule = ToolRuleConfig::from_tool_rule(&rule);

        // Test serialization
        let serialized = toml::to_string(&config_rule).unwrap();
        assert!(serialized.contains("tool_name"));
        assert!(serialized.contains("rule_type"));

        // Test deserialization
        let deserialized: ToolRuleConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.tool_name, "test_tool");

        // Convert back to runtime type
        let runtime_rule = deserialized.to_tool_rule().unwrap();
        assert_eq!(runtime_rule.tool_name, "test_tool");
        assert!(matches!(
            runtime_rule.rule_type,
            crate::agent::tool_rules::ToolRuleType::Cooldown(_)
        ));
    }

    #[test]
    fn test_agent_config_with_tool_rules() {
        use crate::agent::tool_rules::ToolRule;

        // Create an agent config with tool rules
        let mut agent_config = AgentConfig::default();
        let rules = vec![
            ToolRule::start_constraint("init".to_string()),
            ToolRule::continue_loop("search".to_string()),
        ];
        agent_config.set_tool_rules(&rules);

        // Test getting rules back
        let loaded_rules = agent_config.get_tool_rules().unwrap();
        assert_eq!(loaded_rules.len(), 2);
        assert_eq!(loaded_rules[0].tool_name, "init");
        assert_eq!(loaded_rules[1].tool_name, "search");

        // Test serialization roundtrip via PatternConfig with inline agent
        let config = PatternConfig {
            agents: vec![AgentConfigRef::Inline(agent_config.clone())],
            ..Default::default()
        };
        let toml_content = toml::to_string_pretty(&config).unwrap();
        let deserialized_config: PatternConfig = toml::from_str(&toml_content).unwrap();

        // Extract the inline agent and verify rules
        assert_eq!(deserialized_config.agents.len(), 1);
        if let AgentConfigRef::Inline(ref agent) = deserialized_config.agents[0] {
            let reloaded_rules = agent.get_tool_rules().unwrap();
            assert_eq!(reloaded_rules.len(), 2);
            assert_eq!(reloaded_rules[0].tool_name, "init");
            assert_eq!(reloaded_rules[1].tool_name, "search");
        } else {
            panic!("Expected Inline agent");
        }
    }

    #[test]
    fn test_database_config_directory_helpers() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = DatabaseConfig {
            path: temp_dir.path().to_path_buf(),
        };
        assert_eq!(
            config.constellation_db(),
            temp_dir.path().join("constellation.db")
        );
        assert_eq!(config.auth_db(), temp_dir.path().join("auth.db"));
    }

    #[test]
    fn test_config_priority_default() {
        assert_eq!(ConfigPriority::default(), ConfigPriority::Merge);
    }

    #[test]
    fn test_merge_configs() {
        let base = PatternConfig {
            agents: vec![AgentConfigRef::Inline(AgentConfig {
                name: "BaseAgent".to_string(),
                ..Default::default()
            })],
            ..Default::default()
        };
        let overlay = PartialConfig {
            agents: Some(vec![AgentConfigRef::Inline(AgentConfig {
                name: "OverlayAgent".to_string(),
                ..Default::default()
            })]),
            ..Default::default()
        };

        let merged = merge_configs(base, overlay);
        assert_eq!(merged.agents.len(), 1);
        if let AgentConfigRef::Inline(ref agent) = merged.agents[0] {
            assert_eq!(agent.name, "OverlayAgent");
        } else {
            panic!("Expected Inline agent");
        }
    }

    #[test]
    fn test_agent_config_ref_inline_deserialize() {
        let toml = r#"
            name = "TestAgent"
            system_prompt = "Hello"
        "#;
        let parsed: AgentConfigRef = toml::from_str(toml).unwrap();
        match parsed {
            AgentConfigRef::Inline(config) => {
                assert_eq!(config.name, "TestAgent");
            }
            _ => panic!("Expected Inline variant"),
        }
    }

    #[test]
    fn test_agent_config_ref_path_deserialize() {
        let toml = r#"
            config_path = "agents/pattern.toml"
        "#;
        let parsed: AgentConfigRef = toml::from_str(toml).unwrap();
        match parsed {
            AgentConfigRef::Path { config_path } => {
                assert_eq!(config_path, PathBuf::from("agents/pattern.toml"));
            }
            _ => panic!("Expected Path variant"),
        }
    }

    #[tokio::test]
    async fn test_agent_config_ref_resolve_inline() {
        let config = AgentConfig {
            name: "TestAgent".to_string(),
            system_prompt: Some("Test prompt".to_string()),
            ..Default::default()
        };
        let config_ref = AgentConfigRef::Inline(config.clone());

        let resolved = config_ref.resolve(Path::new("/tmp")).await.unwrap();
        assert_eq!(resolved.name, "TestAgent");
        assert_eq!(resolved.system_prompt, Some("Test prompt".to_string()));
    }

    #[tokio::test]
    async fn test_agent_config_ref_resolve_path_not_found() {
        let config_ref = AgentConfigRef::Path {
            config_path: PathBuf::from("nonexistent/agent.toml"),
        };

        let result = config_ref.resolve(Path::new("/tmp")).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_pattern_config_plural_agents() {
        let temp_dir = tempfile::tempdir().unwrap();
        let toml = format!(
            r#"
            [database]
            path = "{}"

            [[agents]]
            name = "Agent1"

            [[agents]]
            name = "Agent2"
        "#,
            temp_dir.path().display()
        );
        let config: PatternConfig = toml::from_str(&toml).unwrap();
        assert_eq!(config.agents.len(), 2);
    }

    #[test]
    fn test_pattern_config_agent_config_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let toml = format!(
            r#"
            [database]
            path = "{}"

            [[agents]]
            config_path = "agents/pattern.toml"
        "#,
            temp_dir.path().display()
        );
        let config: PatternConfig = toml::from_str(&toml).unwrap();
        assert_eq!(config.agents.len(), 1);
    }

    #[test]
    fn test_group_config_serialization() {
        let group = GroupConfig {
            id: None,
            name: "Main Group".to_string(),
            description: "Primary ADHD support group".to_string(),
            pattern: GroupPatternConfig::RoundRobin {
                skip_unavailable: true,
            },
            members: vec![
                GroupMemberConfig {
                    name: "Executive".to_string(),
                    agent_id: None,
                    config_path: None,
                    agent_config: None,
                    role: GroupMemberRoleConfig::Regular,
                    capabilities: vec!["planning".to_string(), "organization".to_string()],
                },
                GroupMemberConfig {
                    name: "Memory".to_string(),
                    agent_id: Some(AgentId::generate()),
                    config_path: None,
                    agent_config: None,
                    role: GroupMemberRoleConfig::Specialist {
                        domain: "memory_management".to_string(),
                    },
                    capabilities: vec!["recall".to_string()],
                },
            ],
            data_sources: HashMap::new(),
            shared_memory: HashMap::new(),
        };

        let toml = toml::to_string_pretty(&group).unwrap();
        assert!(toml.contains("name = \"Main Group\""));
        assert!(toml.contains("type = \"round_robin\""));
        assert!(toml.contains("[[members]]"));
        assert!(toml.contains("name = \"Executive\""));
    }

    #[test]
    fn test_memory_block_config_new_fields() {
        let toml = r#"
            content = "Test content"
            permission = "read_write"
            memory_type = "core"
            pinned = true
            char_limit = 4096
        "#;
        let config: MemoryBlockConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.pinned, Some(true));
        assert_eq!(config.char_limit, Some(4096));
    }

    #[test]
    fn test_memory_block_config_defaults() {
        let toml = r#"
            permission = "read_write"
            memory_type = "working"
        "#;
        let config: MemoryBlockConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.pinned, None);
        assert_eq!(config.char_limit, None);
        assert!(config.schema.is_none());
    }

    #[test]
    fn test_memory_block_config_with_schema() {
        let toml = r#"
            permission = "read_write"
            memory_type = "core"
            [schema]
            Text = {}
        "#;
        let config: MemoryBlockConfig = toml::from_str(toml).unwrap();
        assert!(config.schema.is_some());
    }

    #[tokio::test]
    async fn test_deprecation_check_singular_agent_errors() {
        use crate::error::ConfigError;

        // Create a temp file with singular [agent].
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[agent]
name = "Test"
"#,
        )
        .unwrap();

        let result = PatternConfig::load_with_deprecation_check(&config_path).await;
        assert!(matches!(result, Err(ConfigError::Deprecated { field, .. }) if field == "agent"));
    }

    #[tokio::test]
    async fn test_deprecation_check_plural_agents_ok() {
        // Create a temp file with plural [[agents]].
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[[agents]]
name = "Test"
"#,
        )
        .unwrap();

        let result = PatternConfig::load_with_deprecation_check(&config_path).await;
        assert!(result.is_ok());
    }
}
