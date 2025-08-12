use super::{AgentId, MemoryBlockId, ModelCapability, UserId};
use crate::{
    config::ModelConfig,
    db::Database,
    error::{AgentError, Result},
};
use letta::{
    Block, CreateAgentRequest, CreateMessagesRequest, LettaClient, MessageCreate,
    types::LettaMessageUnion,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Agent configuration from TOML file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigToml {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub is_sleeptime: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleeptime_interval: Option<u64>,
    /// Persona is the evolvable part of the agent's personality
    #[serde(default)]
    pub persona: String,
    /// Optional custom system prompt (if not using default template)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Optional model capability overrides for this agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<crate::config::CapabilityModels>,
}

/// Memory block configuration from TOML file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockConfigToml {
    pub name: String,
    pub max_length: usize,
    pub default_value: String,
    pub description: String,
}

/// Configuration for an agent in the system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier for this agent type
    pub id: AgentId,
    pub name: String,
    pub description: String,
    /// System prompt for the agent
    pub system_prompt: String,
    /// Whether this agent runs as a sleeptime agent
    pub is_sleeptime: bool,
    /// Interval for sleeptime checks (in seconds)
    pub sleeptime_interval: Option<u64>,
}

/// Configuration for a shared memory block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockConfig {
    /// Unique identifier for this memory block
    pub name: MemoryBlockId,
    /// Maximum length for the block value
    pub max_length: usize,
    /// Default value when initializing
    pub default_value: String,
    /// Description of what this block stores
    pub description: String,
}

/// Multi-agent system that can be configured with different agent types
#[derive(Debug, Clone)]
pub struct MultiAgentSystem {
    letta: Arc<LettaClient>,
    db: Arc<Database>,
    agent_configs: HashMap<AgentId, AgentConfig>,
    memory_configs: HashMap<MemoryBlockId, MemoryBlockConfig>,
    pub(crate) system_prompt_template: Option<String>,
    pub(crate) pattern_specific_prompt: Option<String>,
    pub(crate) model_config: Arc<ModelConfig>,
    cache: Option<Arc<crate::cache::PatternCache>>,
}

impl MultiAgentSystem {
    /// Get a reference to the database
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Initialize system agents (create all agents upfront)
    pub async fn initialize_system_agents(&self) -> Result<()> {
        info!("Initializing system agents...");

        // Skip creating system agents - only create for real users
        // self.initialize_user(UserId(0)).await?;

        info!("System agents initialized successfully");
        Ok(())
    }

    /// Initialize a partner's constellation by Discord ID
    pub async fn initialize_partner(&self, discord_id: &str, name: &str) -> Result<UserId> {
        use crate::error::ValidationError;

        // Parse Discord ID to u64
        let discord_id_u64 = discord_id.parse::<u64>().map_err(|_| {
            crate::error::PatternError::Validation(ValidationError::InvalidAgentId(format!(
                "Invalid Discord ID format: {}",
                discord_id
            )))
        })?;

        // Get or create database user
        let user = self
            .db
            .get_or_create_user_by_discord_id(discord_id_u64, name)
            .await?;

        let user_id = UserId(user.id);

        // Check if already initialized
        if self.is_user_initialized(user_id).await? {
            info!(
                "Partner {} (user_id: {}) already initialized",
                name, user_id.0
            );
            return Ok(user_id);
        }

        // Initialize the constellation
        info!(
            "Initializing constellation for partner {} (user_id: {})",
            name, user_id.0
        );
        self.initialize_user(user_id).await?;

        Ok(user_id)
    }

    /// Check if a user's constellation is already initialized
    pub async fn is_user_initialized(&self, user_id: UserId) -> Result<bool> {
        // Check if user has any agents in the database
        let agents = self.db.get_agents_for_user(user_id.0).await?;
        Ok(!agents.is_empty())
    }

    /// Configure Letta to use our MCP server with SSE transport
    pub async fn configure_mcp_server_sse(&self, mcp_port: u16) -> Result<()> {
        use letta::types::tool::{McpServerConfig, McpServerType, SseServerConfig};

        info!("Configuring Pattern MCP server in Letta (SSE transport)");

        // Create MCP server config for SSE transport
        // Use public URL if provided (for cloud deployments), otherwise localhost
        let mcp_url = if let Ok(public_url) = std::env::var("MCP_PUBLIC_URL") {
            format!("{}/sse", public_url.trim_end_matches('/'))
        } else {
            format!("http://localhost:{}/sse", mcp_port)
        };

        let server_config = McpServerConfig::Sse(SseServerConfig {
            server_name: "pattern_mcp".to_string(),
            server_type: Some(McpServerType::Sse),
            server_url: mcp_url,
            auth_header: None,
            auth_token: None,
            custom_headers: None,
        });

        // Add the MCP server to Letta
        debug!(
            "Sending SSE MCP server config to Letta: {:?}",
            server_config
        );
        match self.letta.tools().add_mcp_server(server_config).await {
            Ok(_) => {
                info!("Successfully added SSE MCP server to Letta");
                // Register all MCP tools once after server is configured
                self.register_all_mcp_tools().await?;
            }
            Err(e) => {
                // Check if it's already exists error
                match &e {
                    letta::LettaError::Api {
                        status, message, ..
                    } => {
                        if *status == 409 || message.contains("already exists") {
                            info!("SSE MCP server already exists in Letta, continuing...");
                            // Still register tools in case they're missing
                            self.register_all_mcp_tools().await?;
                        } else {
                            return Err(crate::error::PatternError::Agent(
                                AgentError::CreationFailed {
                                    name: "SSE MCP server config".to_string(),
                                    source: e,
                                },
                            ));
                        }
                    }
                    _ => {
                        return Err(crate::error::PatternError::Agent(
                            AgentError::CreationFailed {
                                name: "SSE MCP server config".to_string(),
                                source: e,
                            },
                        ));
                    }
                }
            }
        }

        // Wait a moment for the server to be ready
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // List available tools from the MCP server
        info!("Listing tools from SSE MCP server 'pattern_mcp'...");
        let tools = match self
            .letta
            .tools()
            .list_mcp_tools_by_server("pattern_mcp")
            .await
        {
            Ok(tools) => tools,
            Err(e) => {
                error!("Failed to list SSE MCP tools: {:?}", e);
                return Err(crate::error::PatternError::Agent(
                    AgentError::CreationFailed {
                        name: "SSE MCP tools list".to_string(),
                        source: e,
                    },
                ));
            }
        };

        info!("SSE MCP server configured with {} tools", tools.len());
        for tool in &tools {
            debug!(
                "  - {}: {}",
                tool.name,
                tool.description.as_deref().unwrap_or("No description")
            );
        }

        Ok(())
    }

    /// Configure Letta to use our MCP server for Discord tools
    pub async fn configure_mcp_server(&self, mcp_port: u16) -> Result<()> {
        use letta::types::tool::{McpServerConfig, McpServerType, StreamableHttpServerConfig};

        info!("Configuring Pattern MCP server in Letta");

        // First, try to list existing tools to see if server is already configured
        match self
            .letta
            .tools()
            .list_mcp_tools_by_server("pattern_mcp")
            .await
        {
            Ok(tools) => {
                info!(
                    "MCP server 'pattern_mcp' already configured with {} tools",
                    tools.len()
                );
                // Still ensure all tools are registered
                self.register_all_mcp_tools().await?;
                return Ok(());
            }
            Err(_) => {
                // Server doesn't exist yet, continue with configuration
                debug!("MCP server not found, creating new configuration");
            }
        }

        // Create MCP server config for streamable HTTP transport
        let server_config = McpServerConfig::StreamableHttp(StreamableHttpServerConfig {
            server_name: "pattern_mcp".to_string(),
            server_type: Some(McpServerType::StreamableHttp),
            server_url: format!("http://localhost:{}", mcp_port), // Streamable HTTP serves at root
            auth_header: None,                                    // Could add auth later if needed
            auth_token: None,
            custom_headers: None,
        });

        // Add the MCP server to Letta
        debug!("Sending MCP server config to Letta: {:?}", server_config);
        match self.letta.tools().add_mcp_server(server_config).await {
            Ok(_) => {
                info!("Successfully added MCP server to Letta");
                // Register all MCP tools once after server is configured
                self.register_all_mcp_tools().await?;
            }
            Err(e) => {
                // Check if it's a "already exists" error
                match &e {
                    letta::LettaError::Api {
                        status, message, ..
                    } => {
                        if *status == 409 || message.contains("already exists") {
                            info!("MCP server already exists in Letta, continuing...");
                            // Still register tools in case they're missing
                            self.register_all_mcp_tools().await?;
                        } else {
                            return Err(crate::error::PatternError::Agent(
                                AgentError::CreationFailed {
                                    name: "MCP server config".to_string(),
                                    source: e,
                                },
                            ));
                        }
                    }
                    _ => {
                        return Err(crate::error::PatternError::Agent(
                            AgentError::CreationFailed {
                                name: "MCP server config".to_string(),
                                source: e,
                            },
                        ));
                    }
                }
            }
        }

        // Wait a moment for the server to be fully ready
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Skip tool listing verification - it fails due to a Letta API bug
        // but the tools still work correctly
        info!("MCP server configured successfully");
        debug!("Note: Tool listing verification skipped - known Letta API issue");

        // The tools are available despite the API error
        // This has been confirmed through the Letta UI

        Ok(())
    }

    /// Map model capability to actual model string for a specific agent
    pub fn capability_to_model(
        &self,
        agent_id: &AgentId,
        capability: ModelCapability,
    ) -> (String, Option<f32>) {
        // Check for agent-specific override first
        if let Some(agent_models) = self.model_config.agents.get(agent_id.as_str()) {
            let model = match capability {
                ModelCapability::Routine => &agent_models.routine,
                ModelCapability::Interactive => &agent_models.interactive,
                ModelCapability::Investigative => &agent_models.investigative,
                ModelCapability::Critical => &agent_models.critical,
            };
            let temperature = agent_models.temperature;
            if let Some(model) = model {
                return (model.clone(), temperature);
            }
        }

        // Fall back to default mappings
        let default_model = match capability {
            ModelCapability::Routine => &self.model_config.default.routine,
            ModelCapability::Interactive => &self.model_config.default.interactive,
            ModelCapability::Investigative => &self.model_config.default.investigative,
            ModelCapability::Critical => &self.model_config.default.critical,
        };

        (
            default_model.clone().unwrap_or_else(|| {
                // Final fallback to hardcoded defaults
                match capability {
                    ModelCapability::Routine => "groq/llama-3.1-8b-instant",
                    ModelCapability::Interactive => "groq/llama-3.3-70b-versatile",
                    ModelCapability::Investigative => "openai/gpt-4o",
                    ModelCapability::Critical => "anthropic/claude-3-opus-20240229",
                }
                .to_string()
            }),
            Some(0.7),
        )
    }

    /// Update an agent's model using capability level
    pub async fn update_agent_model_capability(
        &self,
        user_id: UserId,
        agent_id: &AgentId,
        capability: ModelCapability,
    ) -> Result<()> {
        let (model, temperature) = self.capability_to_model(agent_id, capability);
        info!(
            "Updating agent {} to {} capability ({})",
            agent_id.as_str(),
            capability,
            model
        );
        self.update_agent_model(user_id, agent_id, &model, temperature)
            .await
    }

    /// Update an agent's model configuration
    pub async fn update_agent_model(
        &self,
        user_id: UserId,
        agent_id: &AgentId,
        model: &str,
        temperature: Option<f32>,
    ) -> Result<()> {
        // Define allowed models
        let allowed_models = [
            // Groq models (fast and cheap)
            "groq/llama-3.3-70b-versatile",
            "groq/llama-3.1-70b-versatile",
            "groq/llama-3.1-8b-instant",
            "groq/mixtral-8x7b-32768",
            // OpenAI models
            "openai/gpt-4o",
            "openai/gpt-4o-mini",
            "openai/gpt-4-turbo",
            "openai/gpt-3.5-turbo",
            // Anthropic models
            "anthropic/claude-3-opus-20240229",
            "anthropic/claude-3-sonnet-20240229",
            "anthropic/claude-3-haiku-20240307",
            "anthropic/claude-3-7-sonnet-latest",
            // Google models
            "google_ai/gemini-2.0-flash-exp",
            "google_ai/gemini-1.5-pro",
            "google_ai/gemini-1.5-flash",
            // Local/free options
            "letta/letta-free",
            "ollama/llama3",
            "ollama/mistral",
        ];

        // Validate model is allowed
        if !allowed_models.contains(&model) {
            return Err(crate::error::PatternError::Agent(
                AgentError::InvalidModel {
                    model: model.to_string(),
                    allowed: allowed_models.iter().map(|s| s.to_string()).collect(),
                },
            ));
        }

        // Get the agent config
        let config = self
            .agent_configs
            .get(agent_id)
            .ok_or_else(|| AgentError::UnknownAgent {
                agent: agent_id.clone(),
            })?;

        // Get or create the agent to get its ID
        let letta_agent_id = self.create_or_get_agent(user_id, agent_id, config).await?;

        // Build the LLM config based on provider
        let llm_config = if model.starts_with("openai/") {
            letta::types::LLMConfig::openai(model).with_temperature(temperature.unwrap_or(0.7))
        } else if model.starts_with("anthropic/") {
            letta::types::LLMConfig::anthropic(model).with_temperature(temperature.unwrap_or(0.7))
        } else if model.starts_with("ollama/") {
            // Local models via Ollama
            let endpoint = std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string());
            letta::types::LLMConfig::local(model, endpoint)
                .with_temperature(temperature.unwrap_or(0.7))
        } else if model.starts_with("groq/") {
            // Groq - OpenAI-compatible but with its own endpoint type
            letta::types::LLMConfig {
                model: model.to_string(),
                model_endpoint_type: letta::types::ModelEndpointType::Groq,
                model_endpoint: None,        // Uses default Groq endpoint
                context_window: Some(32768), // Groq default
                provider_name: Some("groq".to_string()),
                temperature: Some(temperature.unwrap_or(0.7)),
                ..Default::default()
            }
        } else if model.starts_with("google_ai/") {
            // Google AI (Gemini)
            letta::types::LLMConfig {
                model: model.to_string(),
                model_endpoint_type: letta::types::ModelEndpointType::GoogleAi,
                model_endpoint: None,          // Uses default Google AI endpoint
                context_window: Some(1048576), // Gemini 1.5 has 1M context
                provider_name: Some("google_ai".to_string()),
                temperature: Some(temperature.unwrap_or(0.7)),
                ..Default::default()
            }
        } else if model.starts_with("together/") {
            // Together AI
            letta::types::LLMConfig {
                model: model.to_string(),
                model_endpoint_type: letta::types::ModelEndpointType::Together,
                model_endpoint: None,
                context_window: Some(32768), // Default for most Together models
                provider_name: Some("together".to_string()),
                temperature: Some(temperature.unwrap_or(0.7)),
                ..Default::default()
            }
        } else if model.starts_with("mistral/") {
            // Mistral AI
            letta::types::LLMConfig {
                model: model.to_string(),
                model_endpoint_type: letta::types::ModelEndpointType::Mistral,
                model_endpoint: None,
                context_window: Some(32768), // Mistral default
                provider_name: Some("mistral".to_string()),
                temperature: Some(temperature.unwrap_or(0.7)),
                ..Default::default()
            }
        } else {
            // Default fallback (includes letta/letta-free)
            letta::types::LLMConfig::openai(model).with_temperature(temperature.unwrap_or(0.7))
        };

        // Update the agent
        let update_request = letta::types::UpdateAgentRequest {
            llm_config: Some(llm_config),
            ..Default::default()
        };

        self.letta
            .agents()
            .update(&letta_agent_id, update_request)
            .await
            .map_err(|e| AgentError::UpdateFailed {
                agent: agent_id.clone(),
                source: e,
            })?;

        info!(
            "Updated agent {} for user {} to use model {}",
            agent_id.as_str(),
            user_id.0,
            model
        );

        Ok(())
    }

    /// Create a new multi-agent system
    pub fn new(letta: Arc<LettaClient>, db: Arc<Database>) -> Self {
        Self {
            letta,
            db,
            agent_configs: HashMap::new(),
            memory_configs: HashMap::new(),
            system_prompt_template: None,
            pattern_specific_prompt: None,
            model_config: Arc::new(crate::config::ModelConfig::default()),
            cache: None,
        }
    }

    /// Set the model configuration
    pub fn with_model_config(mut self, config: crate::config::ModelConfig) -> Self {
        self.model_config = Arc::new(config);
        self
    }

    /// Set the cache instance
    pub fn with_cache(mut self, cache: Arc<crate::cache::PatternCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Configure an agent type
    pub fn add_agent_config(&mut self, config: AgentConfig) {
        self.agent_configs.insert(config.id.clone(), config);
    }

    /// Configure a shared memory block
    pub fn add_memory_config(&mut self, config: MemoryBlockConfig) {
        self.memory_configs.insert(config.name.clone(), config);
    }

    /// Initialize all configured agents for a user
    pub async fn initialize_user(&self, user_id: UserId) -> Result<()> {
        info!("Initializing multi-agent system for user {}", user_id.0);

        // Initialize shared memory blocks
        self.initialize_shared_memory(user_id).await?;

        // Fetch all agents once to avoid repeated API calls
        let existing_agents = match self.letta.agents().list(None).await {
            Ok(agents) => agents,
            Err(e) => {
                warn!("Failed to list agents, will create all: {:?}", e);
                vec![]
            }
        };

        // Create agents
        let mut agent_ids = Vec::new();
        for (agent_id, config) in &self.agent_configs {
            self.create_or_get_agent_with_list(user_id, agent_id, config, &existing_agents)
                .await?;
            agent_ids.push(agent_id.clone());
        }

        // Create all designed groups from our architecture
        self.create_default_groups(user_id, &agent_ids).await?;

        info!("Multi-agent system initialized for user {}", user_id.0);
        Ok(())
    }

    /// Create the default groups from our architecture design
    async fn create_default_groups(
        &self,
        user_id: UserId,
        all_agent_ids: &[AgentId],
    ) -> Result<()> {
        info!("Creating default groups for user {}", user_id.0);

        // 1. Main Group - Dynamic routing for normal conversation
        match self
            .get_or_create_group(
                user_id,
                "main",
                all_agent_ids.to_vec(),
                crate::db::GroupManagerType::Dynamic,
                Some(serde_json::json!({
                    "manager_agent_id": "pattern",
                    "termination_token": "DONE",
                    "max_turns": 10
                })),
            )
            .await
        {
            Ok(_) => info!("Created 'main' group with dynamic routing"),
            Err(e) => warn!("Failed to create 'main' group: {}", e),
        }

        // 2. Crisis Group - Round robin for quick systematic checks
        let crisis_agents = vec![
            AgentId::new("pattern").unwrap(),
            AgentId::new("momentum").unwrap(),
            AgentId::new("anchor").unwrap(),
        ];

        match self
            .get_or_create_group(
                user_id,
                "crisis",
                crisis_agents,
                crate::db::GroupManagerType::RoundRobin,
                None,
            )
            .await
        {
            Ok(_) => info!("Created 'crisis' group with round-robin routing"),
            Err(e) => warn!("Failed to create 'crisis' group: {}", e),
        }

        // 3. Planning Group - Supervisor mode with Entropy leading
        let planning_agents = vec![
            AgentId::new("entropy").unwrap(),
            AgentId::new("flux").unwrap(),
            AgentId::new("pattern").unwrap(),
        ];

        match self
            .get_or_create_group(
                user_id,
                "planning",
                planning_agents,
                crate::db::GroupManagerType::Supervisor,
                Some(serde_json::json!({
                    "manager_agent_id": "entropy"
                })),
            )
            .await
        {
            Ok(_) => info!("Created 'planning' group with Entropy as supervisor"),
            Err(e) => warn!("Failed to create 'planning' group: {}", e),
        }

        // 4. Memory/Sleeptime Group - Pattern + Archive for memory processing
        let memory_agents = vec![
            AgentId::new("pattern").unwrap(),
            AgentId::new("archive").unwrap(),
        ];

        match self
            .get_or_create_group(
                user_id,
                "memory",
                memory_agents,
                crate::db::GroupManagerType::Sleeptime,
                Some(serde_json::json!({
                    "manager_agent_id": "archive",
                    "frequency": 20  // Every 20 messages
                })),
            )
            .await
        {
            Ok(_) => info!("Created 'memory' sleeptime group with Archive managing"),
            Err(e) => warn!("Failed to create 'memory' group: {}", e),
        }

        Ok(())
    }

    /// Initialize shared memory blocks for a user
    async fn initialize_shared_memory(&self, user_id: UserId) -> Result<()> {
        let user = self
            .db
            .get_or_create_user(&format!("user_{}", user_id.0))
            .await?;

        for (block_name, config) in &self.memory_configs {
            self.db
                .upsert_shared_memory(
                    user.id,
                    block_name.as_str(),
                    &config.default_value,
                    config.max_length as i32,
                )
                .await?;
        }
        debug!(
            "Initialized {} shared memory blocks for user {}",
            self.memory_configs.len(),
            user_id.0
        );
        Ok(())
    }

    /// Update an existing agent's configuration
    async fn update_agent(
        &self,
        letta_agent_id: &letta::LettaId,
        agent_id: &AgentId,
        config: &AgentConfig,
    ) -> Result<()> {
        info!("Updating agent {} with new configuration", letta_agent_id);

        // Build new system prompt
        let system_prompt = Self::build_system_prompt(
            config,
            self.system_prompt_template
                .as_ref()
                .expect("missing base template prompt"),
            self.pattern_specific_prompt
                .as_ref()
                .expect("missing primary pattern prompt"),
        );

        // Create update request
        let update_request = letta::types::UpdateAgentRequest {
            name: None, // Keep existing name
            system: Some(system_prompt),
            agent_type: None,       // Keep existing type
            llm_config: None,       // Keep existing LLM config
            embedding_config: None, // Keep existing embedding config
            tags: None,             // Keep existing tags for now
            description: Some(config.description.clone()),
            metadata: None, // Keep existing metadata
        };

        // Update the agent
        self.letta
            .agents()
            .update(letta_agent_id, update_request)
            .await
            .map_err(|e| {
                error!("Failed to update agent '{}': {:?}", letta_agent_id, e);
                crate::error::AgentError::UpdateFailed {
                    agent: agent_id.clone(),
                    source: e,
                }
            })?;

        info!("Successfully updated agent {}", letta_agent_id);

        // Invalidate cache if available
        if let Some(_cache) = &self.cache {
            // We need to get the user_id for this agent
            // For now, we'll just log a warning - in a real implementation
            // we'd need to pass user_id to this method or look it up
            warn!("Cache invalidation for agent updates not fully implemented - need user_id");
            // TODO: cache.remove_agent(user_id, agent_id.as_str()).await?;
        }

        Ok(())
    }

    /// Create or get a specific agent for a user with pre-fetched agent list
    async fn create_or_get_agent_with_list(
        &self,
        user_id: UserId,
        agent_id: &AgentId,
        config: &AgentConfig,
        existing_agents: &[letta::types::AgentState],
    ) -> Result<letta::LettaId> {
        let user = self
            .db
            .get_or_create_user(&format!("user_{}", user_id.0))
            .await?;

        // Build current system prompt for comparison
        let current_system_prompt = Self::build_system_prompt(
            config,
            self.system_prompt_template
                .as_ref()
                .expect("missing base template prompt"),
            self.pattern_specific_prompt
                .as_ref()
                .expect("missing primary pattern prompt"),
        );
        let current_system_prompt_hash = crate::cache::hash_string(&current_system_prompt);

        // Check cache first if available
        if let Some(cache) = &self.cache {
            if let Some(cached_agent) = cache.get_agent(user_id.0, agent_id.as_str()).await? {
                // Verify system prompt hasn't changed
                if cached_agent.system_prompt_hash == current_system_prompt_hash {
                    debug!(
                        "Agent {} found in cache with ID {}",
                        agent_id.as_str(),
                        cached_agent.letta_id
                    );
                    // Parse the string ID back to LettaId
                    return Ok(cached_agent.letta_id.parse().map_err(|_| {
                        crate::error::PatternError::Validation(
                            crate::error::ValidationError::InvalidAgentId(format!(
                                "Failed to parse cached agent ID: {}",
                                cached_agent.letta_id
                            )),
                        )
                    })?);
                } else {
                    debug!(
                        "Agent {} found in cache but system prompt changed",
                        agent_id.as_str()
                    );
                    // Will update the agent below
                }
            }
        }

        // Check if agent already exists
        let user_hash = format!("{:x}", user_id.0 % 1000000);
        let agent_name = format!("{}_{}", agent_id.as_str(), user_hash);

        // Check in the pre-fetched list
        for agent in existing_agents {
            if agent.name == agent_name {
                debug!("Agent {} already exists with ID {}", agent_name, agent.id);

                // Check if we need to update the agent
                let current_system_prompt = Self::build_system_prompt(
                    config,
                    self.system_prompt_template
                        .as_ref()
                        .expect("missing base template prompt"),
                    self.pattern_specific_prompt
                        .as_ref()
                        .expect("missing primary pattern prompt"),
                );
                if agent.system.as_deref() != Some(&current_system_prompt) {
                    info!("Agent {} system prompt changed, updating...", agent_name);
                    self.update_agent(&agent.id, agent_id, config).await?;
                }

                return Ok(agent.id.clone());
            }
        }

        // If not found, create new agent
        info!("Creating new agent {} for user {}", agent_name, user_id.0);

        // Create new agent with shared memory
        let shared_memory = self.db.get_shared_memory(user.id).await?;

        // Build memory blocks
        let mut memory_blocks = vec![
            // Persona block contains evolvable personality
            Block::persona(&Self::build_persona(config)),
            Block::human(&format!("User {} from Discord", user_id.0)),
            // Core memories for conversation context
            letta::types::Block::new(
                "core_memory",
                format!(
                    "Agent: {}\nDescription: {}\nShared Memory Access: current_state, active_context, bond_evolution",
                    config.name, config.description
                ),
            ),
        ];

        // Add shared memory blocks as context
        for block in shared_memory {
            let content = format!("{}: {}", block.block_name, block.block_value);
            memory_blocks.push(Block::human(&content));
        }

        // Use environment variables or config for model selection
        let model = std::env::var("LETTA_MODEL").unwrap_or_else(|_| "letta/letta-free".to_string());
        let embedding_model =
            std::env::var("LETTA_EMBEDDING_MODEL").unwrap_or_else(|_| model.clone());

        debug!(
            "Creating agent with model: {} and embedding: {}",
            model, embedding_model
        );

        // Build tags for this agent
        let mut tags = vec![
            format!("user_{}", user_id.0), // User-specific tag
            agent_id.as_str().to_string(), // Agent type tag
            config.name.to_lowercase(),    // Agent name tag
        ];

        // Add role-based tags
        if config.is_sleeptime {
            tags.push("sleeptime".to_string());
            tags.push("orchestrator".to_string());
        } else {
            tags.push("specialist".to_string());
        }

        // Add function-based tags
        match agent_id.as_str() {
            "pattern" => tags.push("coordinator".to_string()),
            "entropy" => tags.push("tasks".to_string()),
            "flux" => tags.push("time".to_string()),
            "archive" => tags.push("memory".to_string()),
            "momentum" => tags.push("energy".to_string()),
            "anchor" => tags.push("habits".to_string()),
            _ => {}
        }

        // Separate system prompt from persona
        let system_prompt = Self::build_system_prompt(
            config,
            self.system_prompt_template
                .as_ref()
                .expect("missing base template prompt"),
            self.pattern_specific_prompt
                .as_ref()
                .expect("missing primary pattern prompt"),
        );

        let request = CreateAgentRequest::builder()
            .name(&agent_name)
            .model(&model)
            .embedding(&embedding_model)
            .system(&system_prompt)
            .agent_type(letta::types::AgentType::MemGPTv2)
            .enable_sleeptime(config.is_sleeptime)
            .enable_reasoner(true)
            .tags(tags)
            .tools(vec![
                // Don't include send_message - use Discord tools instead
                "core_memory_append".to_string(),
                "core_memory_replace".to_string(),
                "conversation_search".to_string(),
                "archival_memory_insert".to_string(),
                "archival_memory_search".to_string(),
            ])
            .memory_blocks(memory_blocks)
            .build();

        info!(
            "Creating agent '{}' for user {} with agent_id '{}'",
            agent_name,
            user_id.0,
            agent_id.as_str()
        );

        let agent = self.letta.agents().create(request).await.map_err(|e| {
            crate::error::PatternError::Agent(AgentError::CreationFailed {
                name: agent_name.clone(),
                source: e,
            })
        })?;

        info!(
            "Successfully created agent '{}' with ID: {}",
            agent_name, agent.id
        );

        Ok(agent.id)
    }

    /// Create or get a specific agent for a user (original method for compatibility)
    async fn create_or_get_agent(
        &self,
        user_id: UserId,
        agent_id: &AgentId,
        config: &AgentConfig,
    ) -> Result<letta::LettaId> {
        let user = self
            .db
            .get_or_create_user(&format!("user_{}", user_id.0))
            .await?;

        // Check if agent already exists
        // Use a simpler naming scheme to avoid validation issues
        // Hash the user_id to keep it shorter while still unique
        let user_hash = format!("{:x}", user_id.0 % 1000000); // Keep it to 6 hex chars max
        let agent_name = format!("{}_{}", agent_id.as_str(), user_hash);

        // Try to get existing agent from Letta directly
        match self.letta.agents().list(None).await {
            Ok(agents) => {
                for agent in agents {
                    if agent.name == agent_name {
                        debug!("Agent {} already exists with ID {}", agent_name, agent.id);

                        // Check if we need to update the agent
                        // For now, we'll update if the system prompt has changed
                        let current_system_prompt = Self::build_system_prompt(
                            config,
                            self.system_prompt_template
                                .as_ref()
                                .expect("missing base template prompt"),
                            self.pattern_specific_prompt
                                .as_ref()
                                .expect("missing primary pattern prompt"),
                        );
                        if agent.system.as_deref() != Some(&current_system_prompt) {
                            info!("Agent {} system prompt changed, updating...", agent_name);
                            self.update_agent(&agent.id, agent_id, config).await?;
                        }

                        // Check if MCP tools need to be attached - but check ALL tool names
                        // to avoid false positives that trigger unnecessary attachments
                        let needs_tools = match self.letta.agents().list_tools(&agent.id).await {
                            Ok(tools) => {
                                // Get expected tools from central location
                                let expected_tools = Self::expected_mcp_tools();

                                // Check if any expected tools are missing
                                let tool_names: std::collections::HashSet<_> =
                                    tools.iter().map(|t| t.name.as_str()).collect();

                                let missing_any = expected_tools
                                    .iter()
                                    .any(|expected| !tool_names.contains(expected));

                                if missing_any {
                                    debug!(
                                        "Some MCP tools missing for agent {}, will attach",
                                        agent_name
                                    );
                                }
                                missing_any
                            }
                            Err(_) => true, // If we can't check, assume we need tools
                        };

                        if needs_tools {
                            debug!("Attaching MCP tools to existing agent {}", agent_name);
                            if let Err(e) = self.attach_mcp_tools_to_agent(&agent.id).await {
                                warn!(
                                    "Failed to attach MCP tools to existing agent {}: {:?}",
                                    agent_name, e
                                );
                            }
                        } else {
                            debug!("Agent {} already has all MCP tools attached", agent_name);
                        }

                        return Ok(agent.id);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to list agents, will try to create: {:?}", e);
            }
        }

        // Create new agent with shared memory
        let shared_memory = self.db.get_shared_memory(user.id).await?;

        // Build memory blocks
        let mut memory_blocks = vec![
            // Persona block contains evolvable personality
            Block::persona(&Self::build_persona(config)),
            Block::human(&format!("User {} from Discord", user_id.0)),
            // Core memories for conversation context
            letta::types::Block::new(
                "core_memory",
                format!(
                    "Agent: {}\nDescription: {}\nShared Memory Access: current_state, active_context, bond_evolution",
                    config.name, config.description
                ),
            ),
        ];

        // Add shared memory blocks as context
        for block in shared_memory {
            let content = format!("{}: {}", block.block_name, block.block_value);
            memory_blocks.push(Block::human(&content));
        }

        // Use environment variables or config for model selection
        // Fallback to letta-free if not specified
        let model = std::env::var("LETTA_MODEL").unwrap_or_else(|_| "letta/letta-free".to_string());
        let embedding_model =
            std::env::var("LETTA_EMBEDDING_MODEL").unwrap_or_else(|_| model.clone());

        debug!(
            "Creating agent with model: {} and embedding: {}",
            model, embedding_model
        );

        // Build tags for this agent
        let mut tags = vec![
            format!("user_{}", user_id.0), // User-specific tag
            agent_id.as_str().to_string(), // Agent type tag
            config.name.to_lowercase(),    // Agent name tag
        ];

        // Add role-based tags
        if config.is_sleeptime {
            tags.push("sleeptime".to_string());
            tags.push("orchestrator".to_string());
        } else {
            tags.push("specialist".to_string());
        }

        // Add function-based tags
        match agent_id.as_str() {
            "pattern" => tags.push("coordinator".to_string()),
            "entropy" => tags.push("tasks".to_string()),
            "flux" => tags.push("time".to_string()),
            "archive" => tags.push("memory".to_string()),
            "momentum" => tags.push("energy".to_string()),
            "anchor" => tags.push("habits".to_string()),
            _ => {}
        }

        // Separate system prompt from persona
        // System prompt contains unchangeable base behavior
        let system_prompt = Self::build_system_prompt(
            config,
            self.system_prompt_template
                .as_ref()
                .expect("missing base template prompt"),
            self.pattern_specific_prompt
                .as_ref()
                .expect("missing primary pattern prompt"),
        );

        let request = CreateAgentRequest::builder()
            .name(&agent_name)
            .model(&model)
            .embedding(&embedding_model)
            .system(&system_prompt) // Set the actual system prompt
            .agent_type(letta::types::AgentType::MemGPTv2)
            .enable_sleeptime(config.is_sleeptime)
            .tags(tags) // Add tags for filtering
            .include_base_tools(false)
            .include_multi_agent_tools(false)
            .tools(vec![
                // Don't include send_message - use Discord tools instead
                "core_memory_append".to_string(),
                "core_memory_replace".to_string(),
                "conversation_search".to_string(),
                "archival_memory_insert".to_string(), // Store important info
                "archival_memory_search".to_string(), // Retrieve context
                                                      // Discord messaging tools from MCP will be available
            ])
            .memory_blocks(memory_blocks) // Add the memory blocks
            .build();

        info!(
            "Creating agent '{}' for user {} with agent_id '{}'",
            agent_name,
            user_id.0,
            agent_id.as_str()
        );

        let agent = self.letta.agents().create(request).await.map_err(|e| {
            error!("Failed to create agent '{}': {:?}", agent_name, e);
            crate::error::AgentError::CreationFailed {
                name: agent_name.clone(),
                source: e,
            }
        })?;

        info!(
            "Successfully created agent '{}' with ID: {}",
            agent_name, agent.id
        );

        // Store agent in database cache
        self.db
            .upsert_agent(
                user.id,
                agent_id.as_str(),
                &agent.id.to_string(),
                &agent_name,
            )
            .await?;

        // Store in foyer cache if available
        if let Some(cache) = &self.cache {
            let system_prompt_hash = crate::cache::hash_string(&system_prompt);
            if let Err(e) = cache
                .insert_agent(
                    user.id,
                    agent_id.as_str(),
                    agent.id.to_string(),
                    system_prompt_hash,
                )
                .await
            {
                warn!("Failed to cache agent {}: {:?}", agent_name, e);
                // Don't fail agent creation if caching fails
            }
        }

        // Attach MCP tools to the newly created agent
        if let Err(e) = self.attach_mcp_tools_to_agent(&agent.id).await {
            warn!(
                "Failed to attach MCP tools to agent {}: {:?}",
                agent_name, e
            );
            // Don't fail agent creation if tool attachment fails
        }

        Ok(agent.id)
    }

    /// Get the list of expected MCP tools
    fn expected_mcp_tools() -> &'static [&'static str] {
        &[
            "get_agent_memory",
            "update_agent_memory",
            "update_agent_model",
            "schedule_event",
            "check_activity_state",
            "record_energy_state",
            "send_message",
            "write_agent_knowledge",
            "read_agent_knowledge",
            "list_knowledge_files",
            "sync_knowledge_to_letta",
        ]
    }

    /// Register all MCP tools with Letta (call once after server starts)
    async fn register_all_mcp_tools(&self) -> Result<()> {
        info!("Registering all MCP tools with Letta...");

        let mcp_tools = Self::expected_mcp_tools();

        let server_name = "pattern_mcp";
        let mut registered_count = 0;

        for tool_name in mcp_tools {
            match self
                .letta
                .tools()
                .add_mcp_tool(server_name, tool_name)
                .await
            {
                Ok(tool) => {
                    debug!(
                        "Registered MCP tool '{}' with Letta: {:?}",
                        tool_name, tool.id
                    );
                    registered_count += 1;
                }
                Err(e) => {
                    // Tool might already be registered, which is fine
                    debug!(
                        "Failed to register MCP tool '{}' (may already exist): {:?}",
                        tool_name, e
                    );
                }
            }
        }

        info!("Registered {} MCP tools with Letta", registered_count);
        Ok(())
    }

    /// Attach MCP tools to an agent after creation
    pub async fn attach_mcp_tools_to_agent(&self, agent_id: &letta::LettaId) -> Result<()> {
        debug!("Attaching MCP tools to agent {}", agent_id);

        // Get expected tools from central location
        let mcp_tools = Self::expected_mcp_tools();

        // Get all available tools from Letta
        let available_tools = self.letta.tools().list(None).await.map_err(|e| {
            error!("Failed to list available tools: {:?}", e);
            AgentError::Other(e)
        })?;

        // Find and attach each MCP tool
        let mut attached_count = 0;
        for tool_name in mcp_tools {
            // Find the tool ID by name
            let tool = available_tools.iter().find(|t| t.name == *tool_name);

            if let Some(tool) = tool {
                if let Some(tool_id) = &tool.id {
                    match self.letta.agents().attach_tool(agent_id, tool_id).await {
                        Ok(_) => {
                            attached_count += 1;
                        }
                        Err(e) => {
                            // Tool might already be attached, which is fine
                            debug!(
                                "Failed to attach tool '{}' to agent {} (may already be attached): {:?}",
                                tool_name, agent_id, e
                            );
                        }
                    }
                } else {
                    warn!("Tool '{}' found but has no ID", tool_name);
                }
            } else {
                debug!("MCP tool '{}' not found in available tools", tool_name);
            }
        }

        if attached_count > 0 {
            info!(
                "Attached {} MCP tools to agent {}",
                attached_count, agent_id
            );
        }

        // Optionally verify tools in debug mode
        #[cfg(debug_assertions)]
        {
            match self.letta.agents().list_tools(agent_id).await {
                Ok(attached_tools) => {
                    debug!(
                        "Agent {} has {} total tools attached",
                        agent_id,
                        attached_tools.len()
                    );
                }
                Err(e) => {
                    debug!(
                        "Failed to list attached tools for agent {}: {:?}",
                        agent_id, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Update shared memory and sync to agents
    pub async fn update_shared_memory(
        &self,
        user_id: UserId,
        block_name: &str,
        block_value: &str,
    ) -> Result<()> {
        let user = self
            .db
            .get_or_create_user(&format!("user_{}", user_id.0))
            .await?;

        // Validate block exists in config
        let config = self
            .memory_configs
            .get(&MemoryBlockId::new(block_name)?)
            .ok_or_else(|| crate::error::DatabaseError::MemoryBlockNotFound {
                user_id: user.id,
                block: block_name.to_string(),
            })?;

        // Validate length
        if block_value.len() > config.max_length {
            return Err(AgentError::MemoryTooLong {
                block: MemoryBlockId::new(block_name)?,
                max: config.max_length,
                actual: block_value.len(),
            }
            .into());
        }

        // Update in database
        self.db
            .upsert_shared_memory(user.id, block_name, block_value, config.max_length as i32)
            .await?;

        // Update Foyer cache if available (write-through cache)
        if let Some(cache) = &self.cache {
            if let Err(e) = cache
                .update_memory_block_value(user.id, block_name, block_value.to_string())
                .await
            {
                warn!("Failed to update memory block in Foyer cache: {:?}", e);
                // Don't fail the update if cache update fails
            }
        }

        // TODO: Sync to agents via Letta blocks API
        // For now, agents will get updated memory on next interaction
        // In the future, we could update agent memory blocks directly

        Ok(())
    }

    /// Get current state from shared memory
    pub async fn get_shared_memory(&self, user_id: UserId, block_name: &str) -> Result<String> {
        // Check Foyer cache first if available
        if let Some(cache) = &self.cache {
            if let Some(cached_value) = cache.get_memory_block(user_id.0, block_name).await? {
                return Ok((*cached_value).clone());
            }
        }

        let user = self
            .db
            .get_or_create_user(&format!("user_{}", user_id.0))
            .await?;
        let block = self
            .db
            .get_shared_memory_block(user.id, block_name)
            .await?
            .ok_or_else(|| crate::error::DatabaseError::MemoryBlockNotFound {
                user_id: user.id,
                block: block_name.to_string(),
            })?;

        // Store in Foyer cache for next time
        if let Some(cache) = &self.cache {
            if let Err(e) = cache
                .insert_memory_block(user_id.0, block_name, block.block_value.clone())
                .await
            {
                warn!("Failed to insert memory block into Foyer cache: {:?}", e);
            }
        }

        Ok(block.block_value)
    }

    /// Get all shared memory for a user
    pub async fn get_all_shared_memory(&self, user_id: UserId) -> Result<HashMap<String, String>> {
        let user = self
            .db
            .get_or_create_user(&format!("user_{}", user_id.0))
            .await?;

        let blocks = self.db.get_shared_memory(user.id).await?;

        Ok(blocks
            .into_iter()
            .map(|b| (b.block_name, b.block_value))
            .collect())
    }

    /// Get configured agent types
    pub fn agent_configs(&self) -> &HashMap<AgentId, AgentConfig> {
        &self.agent_configs
    }

    /// Get configured memory blocks
    pub fn memory_configs(&self) -> &HashMap<MemoryBlockId, MemoryBlockConfig> {
        &self.memory_configs
    }

    /// Build the system prompt with base rules and agent-specific instructions
    fn build_system_prompt(
        config: &AgentConfig,
        base_template: &str,
        pattern_specific: &str,
    ) -> String {
        base_template
            .replace("{agent_name}", &config.name)
            .replace("{agent_description}", &config.description)
            .replace("{pattern_specific}", pattern_specific)
    }

    /// Build the evolvable persona for the agent
    fn build_persona(config: &AgentConfig) -> String {
        config.system_prompt.clone()
    }

    /// Create a new group of agents
    pub async fn create_group(
        &self,
        user_id: UserId,
        group_name: &str,
        agent_ids: Vec<AgentId>,
        manager_type: crate::db::GroupManagerType,
        manager_config: Option<serde_json::Value>,
    ) -> Result<letta::types::Group> {
        // Ensure all agents exist first
        let mut letta_agent_ids = Vec::new();
        for agent_id in &agent_ids {
            let config =
                self.agent_configs
                    .get(agent_id)
                    .ok_or_else(|| AgentError::UnknownAgent {
                        agent: agent_id.clone(),
                    })?;
            let agent_letta_id = self.create_or_get_agent(user_id, agent_id, config).await?;
            letta_agent_ids.push(agent_letta_id);
        }

        // Prepare manager config based on type
        use letta::types::groups::*;
        let manager_config_typed = match manager_type {
            crate::db::GroupManagerType::RoundRobin => {
                GroupCreateManagerConfig::RoundRobin(RoundRobinManager {
                    max_turns: manager_config
                        .as_ref()
                        .and_then(|c| c.get("max_turns"))
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32),
                })
            }
            crate::db::GroupManagerType::Supervisor => {
                let manager_agent_id = manager_config
                    .as_ref()
                    .and_then(|c| c.get("manager_agent_id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentError::CreationFailed {
                        name: "supervisor manager".to_string(),
                        source: letta::LettaError::api(
                            400,
                            "Supervisor manager requires manager_agent_id",
                        ),
                    })?;

                // Get the actual Letta agent ID for the manager
                let manager_agent_id_parsed =
                    AgentId::new(manager_agent_id).map_err(|e| AgentError::CreationFailed {
                        name: format!("supervisor agent {}", manager_agent_id),
                        source: letta::LettaError::api(400, e.to_string()),
                    })?;

                let config = self
                    .agent_configs
                    .get(&manager_agent_id_parsed)
                    .ok_or_else(|| AgentError::UnknownAgent {
                        agent: manager_agent_id_parsed.clone(),
                    })?;

                let manager_agent = self
                    .create_or_get_agent(user_id, &manager_agent_id_parsed, config)
                    .await?;

                GroupCreateManagerConfig::Supervisor(SupervisorManager {
                    manager_agent_id: manager_agent,
                })
            }
            crate::db::GroupManagerType::Dynamic => {
                let manager_agent_id = manager_config
                    .as_ref()
                    .and_then(|c| c.get("manager_agent_id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentError::CreationFailed {
                        name: "dynamic manager".to_string(),
                        source: letta::LettaError::api(
                            400,
                            "Dynamic manager requires manager_agent_id",
                        ),
                    })?;

                let manager_agent_id_parsed =
                    AgentId::new(manager_agent_id).map_err(|e| AgentError::CreationFailed {
                        name: format!("dynamic manager agent {}", manager_agent_id),
                        source: letta::LettaError::api(400, e.to_string()),
                    })?;

                let config = self
                    .agent_configs
                    .get(&manager_agent_id_parsed)
                    .ok_or_else(|| AgentError::UnknownAgent {
                        agent: manager_agent_id_parsed.clone(),
                    })?;

                let manager_agent = self
                    .create_or_get_agent(user_id, &manager_agent_id_parsed, config)
                    .await?;

                GroupCreateManagerConfig::Dynamic(DynamicManager {
                    manager_agent_id: manager_agent,
                    termination_token: manager_config
                        .as_ref()
                        .and_then(|c| c.get("termination_token"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    max_turns: manager_config
                        .as_ref()
                        .and_then(|c| c.get("max_turns"))
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32),
                })
            }
            crate::db::GroupManagerType::Sleeptime => {
                let manager_agent_id = manager_config
                    .as_ref()
                    .and_then(|c| c.get("manager_agent_id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentError::CreationFailed {
                        name: "sleeptime manager".to_string(),
                        source: letta::LettaError::api(
                            400,
                            "Sleeptime manager requires manager_agent_id",
                        ),
                    })?;

                let manager_agent_id_parsed =
                    AgentId::new(manager_agent_id).map_err(|e| AgentError::CreationFailed {
                        name: format!("sleeptime manager agent {}", manager_agent_id),
                        source: letta::LettaError::api(400, e.to_string()),
                    })?;

                let config = self
                    .agent_configs
                    .get(&manager_agent_id_parsed)
                    .ok_or_else(|| AgentError::UnknownAgent {
                        agent: manager_agent_id_parsed.clone(),
                    })?;

                let manager_agent = self
                    .create_or_get_agent(user_id, &manager_agent_id_parsed, config)
                    .await?;

                GroupCreateManagerConfig::Sleeptime(SleeptimeManager {
                    manager_agent_id: manager_agent,
                    sleeptime_agent_frequency: manager_config
                        .as_ref()
                        .and_then(|c| c.get("frequency"))
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32),
                })
            }
            _ => {
                return Err(AgentError::CreationFailed {
                    name: "group manager".to_string(),
                    source: letta::LettaError::api(
                        400,
                        format!("Unsupported manager type: {:?}", manager_type),
                    ),
                }
                .into());
            }
        };

        // Create the group in Letta
        let group_request = GroupCreate {
            agent_ids: letta_agent_ids.clone(),
            description: format!("{} (user {})", group_name, user_id.0),
            manager_config: Some(manager_config_typed),
            shared_block_ids: None, // TODO: Add shared memory blocks
        };

        let group = self
            .letta
            .groups()
            .create(group_request)
            .await
            .map_err(|e| AgentError::Other(e))?;

        // Cache in database with serialized group data
        let manager_config_json = manager_config.map(|v| v.to_string());
        let group_data_json =
            serde_json::to_string(&group).map_err(|e| AgentError::CreationFailed {
                name: format!("group {}", group_name),
                source: letta::LettaError::api(500, format!("Failed to serialize group: {}", e)),
            })?;

        self.db
            .create_group_with_cache(
                user_id.0,
                &group.id.to_string(),
                group_name,
                manager_type,
                manager_config_json.as_deref(),
                None, // TODO: shared block IDs
                Some(&group_data_json),
            )
            .await?;

        // Insert into Foyer cache if available
        if let Some(cache) = &self.cache {
            if let Err(e) = cache
                .insert_group(user_id.0, group_name, group.clone())
                .await
            {
                warn!("Failed to insert new group into Foyer cache: {:?}", e);
                // Don't fail group creation if caching fails
            }
        }

        Ok(group)
    }

    /// Get or create a group
    pub async fn get_or_create_group(
        &self,
        user_id: UserId,
        group_name: &str,
        agent_ids: Vec<AgentId>,
        manager_type: crate::db::GroupManagerType,
        manager_config: Option<serde_json::Value>,
    ) -> Result<letta::types::Group> {
        // Check Foyer cache first if available
        if let Some(cache) = &self.cache {
            if let Some(cached_group) = cache.get_group(user_id.0, group_name).await? {
                info!("Retrieved group '{}' from Foyer cache", group_name);
                return Ok((*cached_group).clone());
            }
        }

        // Check database second
        if let Some(cached_group) = self.db.get_group_by_name(user_id.0, group_name).await? {
            // Try to use cached group data first
            if let Some(group_data) = cached_group.group_data {
                match serde_json::from_str::<letta::types::Group>(&group_data) {
                    Ok(group) => {
                        info!("Using cached group '{}' with ID: {}", group_name, group.id);

                        // Store in Foyer cache for next time
                        if let Some(cache) = &self.cache {
                            if let Err(e) = cache
                                .insert_group(user_id.0, group_name, group.clone())
                                .await
                            {
                                warn!("Failed to insert group into Foyer cache: {:?}", e);
                            }
                        }

                        return Ok(group);
                    }
                    Err(e) => {
                        warn!("Failed to deserialize cached group data: {}", e);
                        // Fall through to API fetch
                    }
                }
            }

            // If no cached data or deserialization failed, try to fetch from Letta
            let group_id = cached_group
                .group_id
                .parse()
                .map_err(|e: letta::LettaIdError| AgentError::InvalidLettaId(e.to_string()))?;
            match self.letta.groups().get(&group_id).await {
                Ok(group) => {
                    info!("Retrieved group '{}' from Letta API", group_name);

                    // Store in Foyer cache for next time
                    if let Some(cache) = &self.cache {
                        if let Err(e) = cache
                            .insert_group(user_id.0, group_name, group.clone())
                            .await
                        {
                            warn!("Failed to insert group into Foyer cache: {:?}", e);
                        }
                    }

                    return Ok(group);
                }
                Err(e) => {
                    warn!(
                        "Cached group '{}' not found in Letta (will recreate): {}",
                        group_name, e
                    );
                    // Continue to create new
                }
            }
        }

        // Create new group
        self.create_group(user_id, group_name, agent_ids, manager_type, manager_config)
            .await
    }

    /// Send a message to a group
    pub async fn send_message_to_group(
        &self,
        user_id: UserId,
        group_name: &str,
        message: &str,
    ) -> Result<letta::types::LettaResponse> {
        // Get the group from database
        let group = self
            .db
            .get_group(user_id.0, group_name)
            .await?
            .ok_or_else(|| AgentError::UnknownAgent {
                agent: AgentId::new(format!("group_{}", group_name)).unwrap(),
            })?;

        // Send message to the group
        let response =
            self.letta
                .groups()
                .send_message(
                    &group.group_id.parse().map_err(|e: letta::LettaIdError| {
                        AgentError::InvalidLettaId(e.to_string())
                    })?,
                    vec![MessageCreate::user(message)],
                )
                .await
                .map_err(|e| AgentError::Other(e))?;

        Ok(response)
    }

    /// List all groups for a user
    pub async fn list_user_groups(&self, user_id: UserId) -> Result<Vec<crate::db::Group>> {
        self.db.list_user_groups(user_id.0).await
    }

    /// Delete a group
    pub async fn delete_group(&self, user_id: UserId, group_name: &str) -> Result<()> {
        // Get group from database
        if let Some(group) = self.db.get_group(user_id.0, group_name).await? {
            // Delete from Letta
            let group_id = group
                .group_id
                .parse()
                .map_err(|e: letta::LettaIdError| AgentError::InvalidLettaId(e.to_string()))?;
            if let Err(e) = self.letta.groups().delete(&group_id).await {
                warn!("Failed to delete group from Letta: {}", e);
            }

            // Delete from database
            self.db.delete_group(user_id.0, group_name).await?;

            // Remove from Foyer cache if available
            if let Some(cache) = &self.cache {
                if let Err(e) = cache.remove_group(user_id.0, group_name).await {
                    warn!("Failed to remove group from Foyer cache: {:?}", e);
                    // Don't fail group deletion if cache removal fails
                }
            }
        }

        Ok(())
    }

    /// Send a message to a specific agent
    pub async fn send_message_to_agent(
        &self,
        user_id: UserId,
        agent_id: Option<&str>,
        message: &str,
    ) -> Result<String> {
        // Determine which agent to use
        let target_agent = agent_id
            .and_then(|id| AgentId::new(id).ok())
            .unwrap_or_else(|| AgentId::new("pattern").unwrap());

        // Get the agent config
        let config =
            self.agent_configs
                .get(&target_agent)
                .ok_or_else(|| AgentError::UnknownAgent {
                    agent: target_agent.clone(),
                })?;

        // Create or get the agent for this user
        let letta_agent_id = self
            .create_or_get_agent(user_id, &target_agent, config)
            .await?;

        // Send message to the agent
        debug!(
            "Sending message to agent {} (ID: {})",
            target_agent.as_str(),
            letta_agent_id
        );
        let user_message = MessageCreate::user(message);

        let request = CreateMessagesRequest::builder()
            .messages(vec![user_message])
            .build();

        debug!("Calling Letta API to send message...");
        let response = self
            .letta
            .messages()
            .create(&letta_agent_id, request.clone())
            .await
            .map_err(|e| {
                error!("Message send failed after timeout: {}", e);
                debug!("Request contents:\n{:?}", request);
                AgentError::MessageFailed {
                    agent: target_agent.clone(),
                    source: e,
                }
            })?;

        debug!(
            "Received response from Letta with {} messages\n {}",
            response.messages.len(),
            response
                .messages
                .as_slice()
                .iter()
                .map(|m| format!("{:?}", m))
                .collect::<Vec<_>>()
                .join("\n---------\n")
        );

        // Extract the assistant's response
        let assistant_message = response
            .messages
            .iter()
            .filter_map(|msg| match msg {
                LettaMessageUnion::AssistantMessage(assistant) => Some(assistant.content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if assistant_message.is_empty() {
            Ok("I'm still processing that thought...".to_string())
        } else {
            Ok(assistant_message)
        }
    }

    /// Get reference to the agent coordinator if needed
    pub fn agent_coordinator(&self) -> Option<&crate::agent::coordination::AgentCoordinator> {
        None // Multi-agent system handles coordination internally
    }

    /// Force update all agents for a user with current configurations
    /// Useful for applying system prompt changes or other updates
    pub async fn update_all_agents(&self, user_id: UserId) -> Result<()> {
        info!("Updating all agents for user {}", user_id.0);

        let user_hash = format!("{:x}", user_id.0 % 1000000);

        // Get all agents for this user
        match self.letta.agents().list(None).await {
            Ok(agents) => {
                for agent in agents {
                    // Check if this agent belongs to the user
                    if agent.name.ends_with(&format!("_{}", user_hash)) {
                        // Extract agent type from name
                        for (agent_id, config) in &self.agent_configs {
                            let expected_name = format!("{}_{}", agent_id.as_str(), user_hash);
                            if agent.name == expected_name {
                                info!("Updating agent {} ({})", agent.name, agent.id);
                                self.update_agent(&agent.id, agent_id, config).await?;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to list agents for update: {:?}", e);
            }
        }

        info!("Completed updating all agents for user {}", user_id.0);
        Ok(())
    }

    /// Log agent chat histories to disk for debugging
    pub async fn log_agent_histories(&self, user_id: UserId, log_dir: &str) -> Result<()> {
        use std::fmt::Write as FmtWrite;
        use tokio::fs;
        use tokio::io::AsyncWriteExt;

        // Create log directory if it doesn't exist
        fs::create_dir_all(log_dir).await.map_err(|e| {
            crate::error::PatternError::Config(crate::error::ConfigError::Invalid {
                field: "log_dir".to_string(),
                reason: format!("Failed to create log directory: {}", e),
            })
        })?;

        let timestamp = sqlx::types::chrono::Local::now().format("%Y%m%d_%H%M%S");

        // Get all agents for this user
        let user_hash = format!("{:x}", user_id.0 % 1000000);

        for (agent_id, config) in &self.agent_configs {
            let agent_name = format!("{}_{}", agent_id.as_str(), user_hash);

            // Try to find the agent ID
            match self.letta.agents().list(None).await {
                Ok(agents) => {
                    for agent in agents {
                        if agent.name == agent_name {
                            info!(
                                "Logging history for agent {} (ID: {})",
                                agent_name, agent.id
                            );

                            // Get message history
                            match self.letta.messages().list(&agent.id, None).await {
                                Ok(messages) => {
                                    let log_file = format!(
                                        "{}/{}_{}_history_{}.log",
                                        log_dir,
                                        agent_id.as_str(),
                                        user_hash,
                                        timestamp
                                    );

                                    // Build the entire log content as a string first
                                    let mut content = String::new();

                                    writeln!(
                                        &mut content,
                                        "=== Agent: {} ({}) ===",
                                        config.name, agent_name
                                    )
                                    .ok();
                                    writeln!(&mut content, "Agent ID: {}", agent.id).ok();
                                    writeln!(&mut content, "User ID: {}", user_id.0).ok();
                                    writeln!(&mut content, "Timestamp: {}", timestamp).ok();
                                    writeln!(&mut content, "Total messages: {}\n", messages.len())
                                        .ok();

                                    for (idx, msg) in messages.iter().enumerate() {
                                        writeln!(&mut content, "--- Message {} ---", idx + 1).ok();
                                        match msg {
                                            LettaMessageUnion::UserMessage(m) => {
                                                writeln!(&mut content, "Type: User").ok();
                                                writeln!(&mut content, "Content: {}", m.content)
                                                    .ok();
                                            }
                                            LettaMessageUnion::AssistantMessage(m) => {
                                                writeln!(&mut content, "Type: Assistant").ok();
                                                writeln!(&mut content, "Content: {}", m.content)
                                                    .ok();
                                            }
                                            LettaMessageUnion::SystemMessage(m) => {
                                                writeln!(&mut content, "Type: System").ok();
                                                writeln!(&mut content, "Content: {}", m.content)
                                                    .ok();
                                            }
                                            LettaMessageUnion::ToolCallMessage(m) => {
                                                writeln!(&mut content, "Type: Tool Call").ok();
                                                writeln!(
                                                    &mut content,
                                                    "Tool: {}",
                                                    m.tool_call.name
                                                )
                                                .ok();
                                                writeln!(
                                                    &mut content,
                                                    "Arguments: {}",
                                                    m.tool_call.arguments
                                                )
                                                .ok();
                                            }
                                            LettaMessageUnion::ToolReturnMessage(m) => {
                                                writeln!(&mut content, "Type: Tool Return").ok();
                                                writeln!(
                                                    &mut content,
                                                    "Tool Call ID: {}",
                                                    m.tool_call_id
                                                )
                                                .ok();
                                                writeln!(&mut content, "Status: {:?}", m.status)
                                                    .ok();
                                                writeln!(&mut content, "Return: {}", m.tool_return)
                                                    .ok();
                                            }
                                            LettaMessageUnion::ReasoningMessage(m) => {
                                                writeln!(&mut content, "Type: Reasoning").ok();
                                                writeln!(
                                                    &mut content,
                                                    "Reasoning: {}",
                                                    m.reasoning
                                                )
                                                .ok();
                                            }
                                            LettaMessageUnion::HiddenReasoningMessage(m) => {
                                                writeln!(&mut content, "Type: Hidden Reasoning")
                                                    .ok();
                                                writeln!(&mut content, "State: {:?}", m.state).ok();
                                            }
                                        }
                                        writeln!(&mut content, "").ok();
                                    }

                                    // Write all content at once
                                    let mut file =
                                        fs::File::create(&log_file).await.map_err(|e| {
                                            crate::error::PatternError::Config(
                                                crate::error::ConfigError::Invalid {
                                                    field: "log_file".to_string(),
                                                    reason: format!(
                                                        "Failed to create log file: {}",
                                                        e
                                                    ),
                                                },
                                            )
                                        })?;

                                    file.write_all(content.as_bytes()).await.map_err(|e| {
                                        crate::error::PatternError::Config(
                                            crate::error::ConfigError::Invalid {
                                                field: "log_file".to_string(),
                                                reason: format!("Failed to write log file: {}", e),
                                            },
                                        )
                                    })?;

                                    info!("Logged {} messages to {}", messages.len(), log_file);
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to get message history for {}: {:?}",
                                        agent_name, e
                                    );
                                }
                            }
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to list agents: {:?}", e);
                }
            }
        }

        info!(
            "Agent history logging complete. Check {} directory",
            log_dir
        );
        Ok(())
    }
}

/// Builder pattern for easier configuration
pub struct MultiAgentSystemBuilder {
    letta: Arc<LettaClient>,
    db: Arc<Database>,
    agent_configs: Vec<AgentConfig>,
    memory_configs: Vec<MemoryBlockConfig>,
    system_prompt_template: Option<String>,
    pattern_specific_prompt: Option<String>,
    model_config: Option<crate::config::ModelConfig>,
}

impl MultiAgentSystemBuilder {
    pub fn new(letta: Arc<LettaClient>, db: Arc<Database>) -> Self {
        Self {
            letta,
            db,
            agent_configs: Vec::new(),
            memory_configs: Vec::new(),
            system_prompt_template: None,
            pattern_specific_prompt: None,
            model_config: None,
        }
    }

    /// Load agent configurations from a TOML file
    pub async fn with_config_file<P: AsRef<std::path::Path>>(mut self, path: P) -> Result<Self> {
        let path = path.as_ref();
        info!("Loading agent configuration from: {}", path.display());

        let contents = tokio::fs::read_to_string(path).await.map_err(|_| {
            crate::error::ConfigError::NotFound {
                path: path.display().to_string(),
            }
        })?;

        // Parse the TOML structure with agents and memory sections
        let config_data: toml::Value = toml::from_str(&contents)
            .map_err(|e| crate::error::ConfigError::ParseFailed { source: e })?;

        // Extract system prompt template if present
        if let Some(template) = config_data.get("system_prompt_template") {
            if let Some(content) = template.get("content").and_then(|v| v.as_str()) {
                self.system_prompt_template = Some(content.to_string());
            }
        }

        // Extract pattern-specific prompt if present
        if let Some(pattern) = config_data.get("pattern_specific_prompt") {
            if let Some(content) = pattern.get("content").and_then(|v| v.as_str()) {
                self.pattern_specific_prompt = Some(content.to_string());
            }
        }

        // Extract agents section
        if let Some(agents) = config_data.get("agents").and_then(|v| v.as_table()) {
            for (_key, agent_value) in agents {
                if let Ok(agent_toml) = agent_value.clone().try_into::<AgentConfigToml>() {
                    let agent_id = AgentId::new(&agent_toml.id)?;

                    // Build agent config with persona
                    let config = AgentConfig {
                        id: agent_id,
                        name: agent_toml.name,
                        description: agent_toml.description,
                        system_prompt: agent_toml.persona, // Store persona in system_prompt for now
                        is_sleeptime: agent_toml.is_sleeptime,
                        sleeptime_interval: agent_toml.sleeptime_interval,
                    };

                    self.agent_configs.push(config);
                }
            }
        }

        // Extract memory blocks section
        if let Some(memory) = config_data.get("memory").and_then(|v| v.as_table()) {
            for (_key, memory_value) in memory {
                if let Ok(memory_toml) = memory_value.clone().try_into::<MemoryBlockConfigToml>() {
                    let memory_id = MemoryBlockId::new(&memory_toml.name)?;

                    let config = MemoryBlockConfig {
                        name: memory_id,
                        max_length: memory_toml.max_length,
                        default_value: memory_toml.default_value,
                        description: memory_toml.description,
                    };

                    self.memory_configs.push(config);
                }
            }
        }

        info!(
            "Loaded {} agents and {} memory blocks from config",
            self.agent_configs.len(),
            self.memory_configs.len()
        );

        Ok(self)
    }

    pub fn with_agent(mut self, config: AgentConfig) -> Self {
        self.agent_configs.push(config);
        self
    }

    pub fn with_memory_block(mut self, config: MemoryBlockConfig) -> Self {
        self.memory_configs.push(config);
        self
    }

    pub fn with_system_prompt_template(mut self, template: &str) -> Self {
        self.system_prompt_template = Some(template.to_string());
        self
    }

    pub fn with_pattern_specific_prompt(mut self, prompt: &str) -> Self {
        self.pattern_specific_prompt = Some(prompt.to_string());
        self
    }

    pub fn with_model_config(mut self, config: crate::config::ModelConfig) -> Self {
        self.model_config = Some(config);
        self
    }

    pub fn build(self) -> MultiAgentSystem {
        let mut system = MultiAgentSystem::new(self.letta, self.db);
        system.system_prompt_template = self.system_prompt_template;
        system.pattern_specific_prompt = self.pattern_specific_prompt;

        // Apply model config if provided
        if let Some(model_config) = self.model_config {
            system.model_config = Arc::new(model_config);
        }

        for config in self.agent_configs {
            system.add_agent_config(config);
        }

        for config in self.memory_configs {
            system.add_memory_config(config);
        }

        system
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentType, StandardMemoryBlock};

    #[tokio::test]
    async fn test_memory_block_validation() {
        let db = Arc::new(Database::new(":memory:").await.unwrap());
        db.migrate().await.unwrap();
        let letta = Arc::new(letta::LettaClient::local().unwrap());

        let mut system = MultiAgentSystem::new(letta, db);

        // Add memory config
        system.add_memory_config(MemoryBlockConfig {
            name: StandardMemoryBlock::CurrentState.id(),
            max_length: 50,
            default_value: "test".to_string(),
            description: "Test block".to_string(),
        });

        let user_id = UserId(1);
        system.initialize_user(user_id).await.unwrap();

        // Test length validation
        let long_value = "a".repeat(100);
        let result = system
            .update_shared_memory(user_id, "current_state", &long_value)
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::PatternError::Agent(AgentError::MemoryTooLong {
                max, actual, ..
            }) => {
                assert_eq!(max, 50);
                assert_eq!(actual, 100);
            }
            _ => panic!("Expected MemoryTooLong error"),
        }

        // Valid update should work
        let result = system
            .update_shared_memory(user_id, "current_state", "valid value")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_agent_config_uniqueness() {
        let db = Arc::new(Database::new(":memory:").await.unwrap());
        db.migrate().await.unwrap();
        let letta = Arc::new(letta::LettaClient::local().unwrap());

        let mut system = MultiAgentSystem::new(letta, db);

        // Add two agents with same ID (second should override)
        let config1 = AgentConfig {
            id: AgentType::Pattern.id(),
            name: "Pattern v1".to_string(),
            description: "First version".to_string(),
            system_prompt: "Prompt 1".to_string(),
            is_sleeptime: true,
            sleeptime_interval: Some(1800),
        };

        let config2 = AgentConfig {
            id: AgentType::Pattern.id(),
            name: "Pattern v2".to_string(),
            description: "Second version".to_string(),
            system_prompt: "Prompt 2".to_string(),
            is_sleeptime: false,
            sleeptime_interval: None,
        };

        system.add_agent_config(config1);
        system.add_agent_config(config2);

        assert_eq!(system.agent_configs().len(), 1);
        let stored = system
            .agent_configs()
            .get(&AgentType::Pattern.id())
            .unwrap();
        assert_eq!(stored.name, "Pattern v2");
        assert!(!stored.is_sleeptime);
    }

    #[tokio::test]
    async fn test_builder_pattern() {
        let db = Arc::new(Database::new(":memory:").await.unwrap());
        db.migrate().await.unwrap();
        let letta = Arc::new(letta::LettaClient::local().unwrap());

        let system = MultiAgentSystemBuilder::new(letta, db)
            .with_agent(AgentConfig {
                id: AgentType::Entropy.id(),
                name: "Entropy".to_string(),
                description: "Task agent".to_string(),
                system_prompt: "Break down tasks".to_string(),
                is_sleeptime: false,
                sleeptime_interval: None,
            })
            .with_memory_block(MemoryBlockConfig {
                name: StandardMemoryBlock::ActiveContext.id(),
                max_length: 400,
                default_value: "idle".to_string(),
                description: "Current context".to_string(),
            })
            .build();

        assert_eq!(system.agent_configs().len(), 1);
        assert_eq!(system.memory_configs().len(), 1);
        assert!(
            system
                .agent_configs()
                .contains_key(&AgentType::Entropy.id())
        );
    }

    #[tokio::test]
    async fn test_unknown_agent_error() {
        let db = Arc::new(Database::new(":memory:").await.unwrap());
        db.migrate().await.unwrap();
        let letta = Arc::new(letta::LettaClient::local().unwrap());

        let system = MultiAgentSystem::new(letta, db);
        let user_id = UserId(1);

        // Try to send message to non-existent agent
        let result = system
            .send_message_to_agent(user_id, Some("nonexistent"), "hello")
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::PatternError::Agent(AgentError::UnknownAgent { agent }) => {
                assert_eq!(agent.as_str(), "nonexistent");
            }
            _ => panic!("Expected UnknownAgent error"),
        }
    }

    #[tokio::test]
    async fn test_memory_block_not_found() {
        let db = Arc::new(Database::new(":memory:").await.unwrap());
        db.migrate().await.unwrap();
        let letta = Arc::new(letta::LettaClient::local().unwrap());

        let system = MultiAgentSystem::new(letta, db);
        let user_id = UserId(1);

        // Try to update non-configured memory block
        let result = system
            .update_shared_memory(user_id, "unknown_block", "value")
            .await;

        assert!(result.is_err());
        // Should fail because block isn't configured
    }

    #[test]
    fn test_agent_config_serialization() {
        let config = AgentConfig {
            id: AgentType::Flux.id(),
            name: "Flux".to_string(),
            description: "Time agent".to_string(),
            system_prompt: "Manage time".to_string(),
            is_sleeptime: false,
            sleeptime_interval: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"id\":\"flux\""));
        assert!(json.contains("\"is_sleeptime\":false"));

        let deserialized: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, config.id);
        assert_eq!(deserialized.is_sleeptime, false);
    }
}
