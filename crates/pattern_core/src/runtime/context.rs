//! RuntimeContext: Centralized agent runtime management
//!
//! RuntimeContext centralizes agent management, providing:
//! - Agent registry (load/create/get agents)
//! - Shared infrastructure (heartbeat, queue polling)
//! - Single point for managing the constellation
//! - Default providers for model and embedding operations
//!
//! Uses DashMap for the agent registry to avoid async locks on access.

use dashmap::DashMap;
use pattern_db::ConstellationDb;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::db::ConstellationDatabases;

use crate::agent::{Agent, DatabaseAgent};
use crate::config::{
    AgentConfig, AgentOverrides, GroupConfig, GroupMemberConfig, PartialAgentConfig,
    ResolvedAgentConfig, merge_agent_configs,
};
use crate::context::heartbeat::{HeartbeatReceiver, HeartbeatSender, heartbeat_channel};
use crate::context::{ActivityConfig, ActivityLogger, ActivityRenderer};
use crate::embeddings::EmbeddingProvider;
use crate::error::{ConfigError, CoreError, Result};
use crate::id::AgentId;
use crate::memory::{BlockSchema, BlockType, MemoryCache, MemoryStore};
use crate::messages::MessageStore;
use crate::model::ModelProvider;
use crate::queue::{QueueConfig, QueueProcessor};
use crate::realtime::AgentEventSink;
use crate::runtime::{AgentRuntime, RuntimeConfig};
use crate::tool::ToolRegistry;

/// Configuration for RuntimeContext
#[derive(Debug, Clone)]
pub struct RuntimeContextConfig {
    /// Queue processor configuration
    pub queue_config: QueueConfig,

    /// Whether to automatically start queue processing on context creation
    pub auto_start_queue: bool,

    /// Whether to automatically start heartbeat processing on context creation
    pub auto_start_heartbeat: bool,

    /// Activity rendering configuration
    pub activity_config: ActivityConfig,
}

impl Default for RuntimeContextConfig {
    fn default() -> Self {
        Self {
            queue_config: QueueConfig::default(),
            auto_start_queue: false,
            auto_start_heartbeat: false,
            activity_config: ActivityConfig::default(),
        }
    }
}

/// Centralized runtime context for managing agents and background tasks
///
/// RuntimeContext provides:
/// - Thread-safe agent registry using DashMap
/// - Shared memory cache and tool registry
/// - Heartbeat processing for agent continuations
/// - Queue processing for message polling
/// - Default model and embedding providers for agents
///
/// # Agent Registry
///
/// Uses `DashMap<String, Arc<dyn Agent>>` for the agent registry:
/// - No await needed for access (unlike RwLock<HashMap>)
/// - Wrap in Arc for sharing across tasks
/// - Be careful with references - don't hold refs across async boundaries
///
/// # Example
///
/// ```ignore
/// let ctx = RuntimeContext::builder()
///     .db(db)
///     .model_provider(model)
///     .build()
///     .await?;
///
/// // Register an agent
/// ctx.register_agent(agent);
///
/// // Get an agent (returns cloned Arc)
/// if let Some(agent) = ctx.get_agent("agent_id") {
///     // Use agent...
/// }
///
/// // Start background processors
/// ctx.start_heartbeat_processor(event_handler);
/// ctx.start_queue_processor();
/// ```
pub struct RuntimeContext {
    /// Combined database connections (constellation + auth)
    dbs: Arc<ConstellationDatabases>,

    /// Agent registry - DashMap for lock-free concurrent access
    agents: Arc<DashMap<String, Arc<dyn Agent>>>,

    /// Shared memory cache
    memory: Arc<MemoryCache>,

    /// Shared tool registry
    tools: Arc<ToolRegistry>,

    /// Default model provider for agents
    model_provider: Arc<dyn ModelProvider>,

    /// Default embedding provider (optional)
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,

    /// Default agent configuration
    default_config: AgentConfig,

    /// Heartbeat sender for agents to request continuations
    heartbeat_tx: HeartbeatSender,

    /// Heartbeat receiver - taken when starting processor
    heartbeat_rx: RwLock<Option<HeartbeatReceiver>>,

    /// Background task abort handles for cleanup on shutdown
    ///
    /// Uses std::sync::RwLock instead of tokio::sync::RwLock to enable
    /// synchronous access in Drop implementation.
    background_tasks: std::sync::RwLock<Vec<tokio::task::AbortHandle>>,

    /// Event sinks for forwarding agent events
    event_sinks: RwLock<Vec<Arc<dyn AgentEventSink>>>,

    /// Activity renderer for generating activity context
    activity_renderer: ActivityRenderer,

    /// Configuration
    config: RuntimeContextConfig,
}

impl std::fmt::Debug for RuntimeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeContext")
            .field("dbs", &"<ConstellationDatabases>")
            .field("agents", &format!("{} agents", self.agents.len()))
            .field("memory", &"<MemoryCache>")
            .field("tools", &self.tools)
            .field("model_provider", &"<ModelProvider>")
            .field(
                "embedding_provider",
                &self
                    .embedding_provider
                    .as_ref()
                    .map(|_| "<EmbeddingProvider>"),
            )
            .field("default_config", &self.default_config)
            .field("activity_renderer", &"<ActivityRenderer>")
            .field("config", &self.config)
            .finish()
    }
}

impl RuntimeContext {
    /// Create a new RuntimeContextBuilder
    ///
    /// The builder pattern is the primary way to construct RuntimeContext.
    /// Required fields: `db`, `model_provider`
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = RuntimeContext::builder()
    ///     .db(db)
    ///     .model_provider(model)
    ///     .memory(memory_cache)
    ///     .build()
    ///     .await?;
    /// ```
    pub fn builder() -> RuntimeContextBuilder {
        RuntimeContextBuilder::new()
    }

    /// Create a RuntimeContext with explicit providers
    ///
    /// This is the internal constructor used by the builder. Most code should
    /// use `RuntimeContext::builder()` instead.
    ///
    /// # Arguments
    /// * `dbs` - Combined database connections (already wrapped in Arc)
    /// * `model_provider` - Default model provider for agents
    /// * `embedding_provider` - Optional embedding provider for semantic search
    /// * `memory` - Shared memory cache
    /// * `tools` - Shared tool registry
    /// * `default_config` - Default agent configuration
    /// * `config` - Runtime context configuration
    pub async fn new_with_providers(
        dbs: Arc<ConstellationDatabases>,
        model_provider: Arc<dyn ModelProvider>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
        memory: Arc<MemoryCache>,
        tools: Arc<ToolRegistry>,
        default_config: AgentConfig,
        config: RuntimeContextConfig,
    ) -> Result<Self> {
        let (heartbeat_tx, heartbeat_rx) = heartbeat_channel();

        // Create activity renderer with config
        let activity_renderer = ActivityRenderer::new(dbs.clone(), config.activity_config.clone());

        Ok(Self {
            dbs,
            agents: Arc::new(DashMap::new()),
            memory,
            tools,
            model_provider,
            embedding_provider,
            default_config,
            heartbeat_tx,
            heartbeat_rx: RwLock::new(Some(heartbeat_rx)),
            background_tasks: std::sync::RwLock::new(Vec::new()),
            event_sinks: RwLock::new(Vec::new()),
            activity_renderer,
            config,
        })
    }

    // ============================================================================
    // Getters
    // ============================================================================

    /// Get the combined database connections
    pub fn dbs(&self) -> &Arc<ConstellationDatabases> {
        &self.dbs
    }

    /// Get just the constellation database connection
    ///
    /// Convenience method for code that only needs the constellation database.
    pub fn constellation_db(&self) -> &ConstellationDb {
        &self.dbs.constellation
    }

    /// Get just the auth database connection
    ///
    /// Convenience method for code that needs auth/token operations.
    pub fn auth_db(&self) -> &pattern_auth::AuthDb {
        &self.dbs.auth
    }

    /// Get the shared memory cache
    pub fn memory(&self) -> &Arc<MemoryCache> {
        &self.memory
    }

    /// Get the shared tool registry
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    /// Get the default model provider
    pub fn model_provider(&self) -> &Arc<dyn ModelProvider> {
        &self.model_provider
    }

    /// Get the embedding provider (if configured)
    pub fn embedding_provider(&self) -> Option<&Arc<dyn EmbeddingProvider>> {
        self.embedding_provider.as_ref()
    }

    /// Get the default agent configuration
    pub fn default_config(&self) -> &AgentConfig {
        &self.default_config
    }

    /// Get a clone of the heartbeat sender for agents
    ///
    /// Agents use this to request continuation turns.
    pub fn heartbeat_sender(&self) -> HeartbeatSender {
        self.heartbeat_tx.clone()
    }

    /// Get the activity renderer
    ///
    /// The activity renderer generates activity context for agents, showing
    /// what other agents have been doing recently.
    pub fn activity_renderer(&self) -> &ActivityRenderer {
        &self.activity_renderer
    }

    /// Create an activity logger for a specific agent
    ///
    /// The activity logger allows an agent to log its own activity events
    /// to the database for tracking and constellation awareness.
    ///
    /// # Arguments
    /// * `agent_id` - The ID of the agent to create a logger for
    ///
    /// # Example
    /// ```ignore
    /// let logger = ctx.activity_logger("my_agent");
    /// logger.log_message_sent("Hello world").await?;
    /// ```
    pub fn activity_logger(&self, agent_id: impl Into<String>) -> ActivityLogger {
        ActivityLogger::new(self.dbs.clone(), agent_id)
    }

    /// Get the agent registry (for advanced use cases)
    ///
    /// Most code should use `get_agent`, `register_agent`, etc.
    pub fn agents(&self) -> &Arc<DashMap<String, Arc<dyn Agent>>> {
        &self.agents
    }

    // ============================================================================
    // Agent Registry Operations
    // ============================================================================

    /// Register an agent in the registry
    ///
    /// The agent's ID is used as the key.
    pub fn register_agent(&self, agent: Arc<dyn Agent>) {
        let id = agent.id().to_string();
        self.agents.insert(id, agent);
    }

    /// Get an agent by ID
    ///
    /// Returns a cloned Arc if found. This is cheap since Arc cloning
    /// only increments the reference count.
    ///
    /// # Important
    /// Don't hold the returned Arc across async boundaries longer than needed.
    /// Extract the data you need and drop the reference.
    pub fn get_agent(&self, id: &str) -> Option<Arc<dyn Agent>> {
        self.agents.get(id).map(|entry| entry.value().clone())
    }

    /// Check if an agent is registered
    pub fn has_agent(&self, id: &str) -> bool {
        self.agents.contains_key(id)
    }

    /// Remove an agent from the registry
    ///
    /// Returns the removed agent if it existed.
    pub fn remove_agent(&self, id: &str) -> Option<Arc<dyn Agent>> {
        self.agents.remove(id).map(|(_, agent)| agent)
    }

    /// List all registered agent IDs
    ///
    /// This collects IDs to avoid holding references across async boundaries.
    pub fn list_agent_ids(&self) -> Vec<String> {
        self.agents
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// List all registered agents
    ///
    /// Returns cloned Arcs for all agents. Use sparingly as this
    /// iterates over the entire registry.
    pub fn list_agents(&self) -> Vec<Arc<dyn Agent>> {
        self.agents
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get the number of registered agents
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    // ============================================================================
    // Event Sinks
    // ============================================================================

    /// Add an event sink for receiving agent events
    pub async fn add_event_sink(&self, sink: Arc<dyn AgentEventSink>) {
        self.event_sinks.write().await.push(sink);
    }

    /// Get all event sinks
    pub async fn event_sinks(&self) -> Vec<Arc<dyn AgentEventSink>> {
        self.event_sinks.read().await.clone()
    }

    // ============================================================================
    // Background Processors
    // ============================================================================

    /// Start the heartbeat processor
    ///
    /// The heartbeat processor handles agent continuation requests.
    /// It receives heartbeat requests from agents and triggers their
    /// process() method with continuation messages.
    ///
    /// # Arguments
    /// * `event_handler` - Callback for handling response events
    ///
    /// # Returns
    /// Ok(()) if started successfully, Err if already started
    ///
    /// # Note
    /// This takes ownership of the heartbeat receiver, so it can only
    /// be called once per RuntimeContext.
    pub async fn start_heartbeat_processor<F, Fut>(&self, event_handler: F) -> Result<()>
    where
        F: Fn(crate::agent::ResponseEvent, crate::AgentId, String) -> Fut
            + Clone
            + Send
            + Sync
            + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        // Take the receiver - can only start once
        let heartbeat_rx =
            self.heartbeat_rx
                .write()
                .await
                .take()
                .ok_or_else(|| CoreError::AlreadyStarted {
                    component: "HeartbeatProcessor".to_string(),
                    details: "Heartbeat processor can only be started once per RuntimeContext"
                        .to_string(),
                })?;

        // Clone agents DashMap for the processor
        let agents = self.agents.clone();

        let handle = tokio::spawn(async move {
            process_heartbeats_with_dashmap(heartbeat_rx, agents, event_handler).await;
        });

        self.background_tasks
            .write()
            .expect("background_tasks lock poisoned")
            .push(handle.abort_handle());
        Ok(())
    }

    /// Start the queue processor
    ///
    /// The queue processor polls for pending messages and dispatches
    /// them to the appropriate agents. Uses the DashMap agent registry
    /// so dynamically registered agents will receive messages.
    ///
    /// # Returns
    /// The JoinHandle for the processor task
    pub async fn start_queue_processor(&self) -> JoinHandle<()> {
        let sinks = self.event_sinks().await;

        let dbs = self.dbs.as_ref().clone();
        // Pass the DashMap directly so dynamically registered agents receive messages
        let mut processor =
            QueueProcessor::new(dbs, self.agents.clone(), self.config.queue_config.clone());

        processor = processor.with_sinks(sinks);

        let handle = processor.start();

        self.background_tasks
            .write()
            .expect("background_tasks lock poisoned")
            .push(handle.abort_handle());

        handle
    }

    // ============================================================================
    // Agent Loading
    // ============================================================================

    /// Load an agent from the database with a specific model provider
    ///
    /// This method loads an agent using a custom model provider instead of
    /// the context's default. Use `load_agent` for the simpler case of
    /// using the context's default model provider.
    ///
    /// This method:
    /// 1. Loads the agent record from the database
    /// 2. Builds an AgentRuntime using RuntimeBuilder
    /// 3. Creates a DatabaseAgent using the builder
    /// 4. Registers the agent with this context
    /// 5. Returns the agent
    ///
    /// # Arguments
    /// * `agent_id` - The ID of the agent to load
    /// * `model` - The model provider to use for this agent
    ///
    /// # Returns
    /// The loaded and registered agent, or an error if loading fails
    pub async fn load_agent_with_model(
        &self,
        agent_id: &str,
        model: Arc<dyn ModelProvider>,
    ) -> Result<Arc<dyn Agent>> {
        use crate::agent::DatabaseAgent;
        use crate::id::AgentId;
        use crate::messages::MessageStore;
        use crate::runtime::AgentRuntime;

        // 1. Load agent record from DB
        let agent_record = pattern_db::queries::get_agent(self.dbs.constellation.pool(), agent_id)
            .await
            .map_err(CoreError::from)?
            .ok_or_else(|| CoreError::AgentNotFound {
                identifier: agent_id.to_string(),
            })?;

        // 2. Build AgentRuntime using RuntimeBuilder
        let agent_id_typed = AgentId::new(agent_id);
        let messages = MessageStore::new(self.dbs.constellation.pool().clone(), agent_id);

        // Parse tool rules from agent record if present
        let tool_rules: Vec<crate::agent::tool_rules::ToolRule> = agent_record
            .tool_rules
            .as_ref()
            .and_then(|json| serde_json::from_value(json.0.clone()).ok())
            .unwrap_or_default();

        let runtime = AgentRuntime::builder()
            .agent_id(agent_id)
            .agent_name(&agent_record.name)
            .memory(self.memory.clone())
            .messages(messages)
            .tools_shared(self.tools.clone())
            .model(model.clone())
            .dbs(self.dbs.as_ref().clone())
            .tool_rules(tool_rules)
            .build()?;

        // 3. Build DatabaseAgent using the builder pattern
        let agent = DatabaseAgent::builder()
            .id(agent_id_typed)
            .name(&agent_record.name)
            .runtime(Arc::new(runtime))
            .model(model)
            .model_id(&agent_record.model_name)
            .heartbeat_sender(self.heartbeat_sender())
            .build()?;

        // 4. Wrap in Arc and register
        let agent: Arc<dyn Agent> = Arc::new(agent);
        self.register_agent(agent.clone());

        // 5. Return the agent
        Ok(agent)
    }

    /// Shutdown all background tasks
    ///
    /// Aborts all running background processors. Call this before
    /// dropping the RuntimeContext for clean shutdown.
    pub async fn shutdown(&self) {
        let mut tasks = self
            .background_tasks
            .write()
            .expect("background_tasks lock poisoned");
        for handle in tasks.drain(..) {
            handle.abort();
        }
    }

    // ============================================================================
    // Config Resolution and Agent Creation
    // ============================================================================

    /// Resolve configuration cascade: defaults -> DB -> overrides
    ///
    /// This implements the three-layer config cascade:
    /// 1. Start with RuntimeContext's default_config
    /// 2. Overlay DB stored config from the agent record
    /// 3. Apply any runtime overrides
    fn resolve_config(
        &self,
        db_agent: &pattern_db::models::Agent,
        overrides: Option<&AgentOverrides>,
    ) -> ResolvedAgentConfig {
        // 1. Start with defaults
        let config = self.default_config.clone();

        // 2. Overlay DB stored config
        let db_partial: PartialAgentConfig = db_agent.into();
        let config = merge_agent_configs(config, db_partial);

        // 3. Resolve to concrete config
        let mut resolved = ResolvedAgentConfig::from_agent_config(&config, &self.default_config);

        // 4. Apply overrides if provided
        if let Some(ovr) = overrides {
            resolved = resolved.apply_overrides(ovr);
        }

        resolved
    }

    /// Create a new agent from config (persists to DB)
    ///
    /// This method:
    /// 1. Generates an agent ID if not provided
    /// 2. Persists the agent record to the database
    /// 3. Creates memory blocks from the config
    /// 4. Creates a persona block if specified
    /// 5. Loads and registers the agent
    ///
    /// # Arguments
    /// * `config` - The agent configuration
    ///
    /// # Returns
    /// The created and registered agent, or an error if creation fails
    pub async fn create_agent(&self, config: &AgentConfig) -> Result<Arc<dyn Agent>> {
        let id = config
            .id
            .clone()
            .map(|id| id.0)
            .unwrap_or_else(|| AgentId::generate().0);

        // Check if agent already exists
        if pattern_db::queries::get_agent(self.dbs.constellation.pool(), &id)
            .await?
            .is_some()
        {
            return Err(CoreError::InvalidFormat {
                data_type: "agent".to_string(),
                details: format!("Agent with id '{}' already exists", id),
            });
        }

        // 1. Convert to DB model and persist
        let db_agent = config.to_db_agent(&id);
        pattern_db::queries::create_agent(self.dbs.constellation.pool(), &db_agent).await?;

        // Determine memory char limit: use agent config or fall back to cache default
        // Passing 0 to create_block will use the cache's default_char_limit
        let memory_char_limit = config
            .context
            .as_ref()
            .and_then(|ctx| ctx.memory_char_limit)
            .unwrap_or(0);

        // 2. Create memory blocks from config
        for (label, block_config) in &config.memory {
            let content = block_config.load_content().await?;
            let description = block_config
                .description
                .clone()
                .unwrap_or_else(|| format!("{} memory block", label));

            // Convert MemoryType to BlockType
            let block_type = match block_config.memory_type {
                crate::memory::MemoryType::Core => BlockType::Core,
                crate::memory::MemoryType::Working => BlockType::Working,
                crate::memory::MemoryType::Archival => BlockType::Archival,
            };

            // Create the block with schema and char limit from config
            let block_id = self
                .memory
                .create_block(
                    &id,
                    label,
                    &description,
                    block_type,
                    BlockSchema::Text,
                    memory_char_limit,
                )
                .await
                .map_err(|e| CoreError::InvalidFormat {
                    data_type: "memory_block".to_string(),
                    details: format!("Failed to create memory block '{}': {}", label, e),
                })?;

            // If content is not empty, set it
            if !content.is_empty() {
                self.memory
                    .update_block_text(&id, label, &content)
                    .await
                    .map_err(|e| CoreError::InvalidFormat {
                        data_type: "memory_block".to_string(),
                        details: format!("Failed to set content for block '{}': {}", label, e),
                    })?;
            }

            // Update permission if not the default (ReadWrite)
            if block_config.permission != crate::memory::MemoryPermission::ReadWrite {
                pattern_db::queries::update_block_permission(
                    self.dbs.constellation.pool(),
                    &block_id,
                    block_config.permission.into(),
                )
                .await
                .map_err(|e| CoreError::InvalidFormat {
                    data_type: "memory_block".to_string(),
                    details: format!("Failed to set permission for block '{}': {}", label, e),
                })?;
            }
        }

        // 3. Create persona block if specified
        if let Some(ref persona) = config.persona {
            self.memory
                .create_block(
                    &id,
                    "persona",
                    "Agent persona and personality",
                    BlockType::Core,
                    BlockSchema::Text,
                    memory_char_limit,
                )
                .await
                .map_err(|e| CoreError::InvalidFormat {
                    data_type: "memory_block".to_string(),
                    details: format!("Failed to create persona block: {}", e),
                })?;

            self.memory
                .update_block_text(&id, "persona", persona)
                .await
                .map_err(|e| CoreError::InvalidFormat {
                    data_type: "memory_block".to_string(),
                    details: format!("Failed to set persona content: {}", e),
                })?;
        }

        // 4. Load and register the agent using the context's model provider
        self.load_agent(&id).await
    }

    /// Load an agent with per-agent overrides
    ///
    /// This method loads an agent from the database and applies runtime
    /// overrides that won't be persisted. Use this for temporary
    /// configuration changes like switching models for a single request.
    ///
    /// # Arguments
    /// * `agent_id` - The ID of the agent to load
    /// * `overrides` - Runtime configuration overrides
    ///
    /// # Returns
    /// The loaded agent with overrides applied
    pub async fn load_agent_with(
        &self,
        agent_id: &str,
        overrides: AgentOverrides,
    ) -> Result<Arc<dyn Agent>> {
        let db_agent = pattern_db::queries::get_agent(self.dbs.constellation.pool(), agent_id)
            .await?
            .ok_or_else(|| CoreError::AgentNotFound {
                identifier: agent_id.to_string(),
            })?;

        let resolved = self.resolve_config(&db_agent, Some(&overrides));
        self.build_agent_from_resolved(agent_id, &resolved).await
    }

    /// Load an agent from the database using the context's default model provider
    ///
    /// This is the preferred method for loading agents as it uses the context's
    /// default model provider and applies the full config resolution cascade.
    ///
    /// # Arguments
    /// * `agent_id` - The ID of the agent to load
    ///
    /// # Returns
    /// The loaded and registered agent, or an error if loading fails
    pub async fn load_agent(&self, agent_id: &str) -> Result<Arc<dyn Agent>> {
        // Check if already loaded - avoid duplicate registration
        if let Some(agent) = self.get_agent(agent_id) {
            return Ok(agent);
        }

        let db_agent = pattern_db::queries::get_agent(self.dbs.constellation.pool(), agent_id)
            .await?
            .ok_or_else(|| CoreError::AgentNotFound {
                identifier: agent_id.to_string(),
            })?;

        // Resolve config with no overrides
        let resolved = self.resolve_config(&db_agent, None);
        self.build_agent_from_resolved(agent_id, &resolved).await
    }

    // ============================================================================
    // Group Loading
    // ============================================================================

    /// Load a group of agents by their IDs
    ///
    /// All agents share this context's stores (memory, tools).
    /// Returns error if any agent doesn't exist.
    pub async fn load_group(&self, agent_ids: &[String]) -> Result<Vec<Arc<dyn Agent>>> {
        let mut agents = Vec::with_capacity(agent_ids.len());
        for id in agent_ids {
            let agent = self.load_agent(id).await?;
            agents.push(agent);
        }
        Ok(agents)
    }

    /// Load a group from GroupConfig, creating agents as needed
    ///
    /// For each member in the config:
    /// - If `agent_id` is provided and the agent exists, load it
    /// - Otherwise, create the agent from the member's config
    pub async fn load_group_from_config(
        &self,
        config: &GroupConfig,
    ) -> Result<Vec<Arc<dyn Agent>>> {
        let mut agents = Vec::with_capacity(config.members.len());
        for member in &config.members {
            let agent = self.load_or_create_group_member(member).await?;
            agents.push(agent);
        }
        Ok(agents)
    }

    /// Internal: load or create a single group member
    ///
    /// Priority:
    /// 1. If `agent_id` is provided, try to load existing agent
    /// 2. If load fails or no `agent_id`, create from:
    ///    - `agent_config` (inline config)
    ///    - `config_path` (load from file)
    ///    - Minimal config from member info
    async fn load_or_create_group_member(
        &self,
        member: &GroupMemberConfig,
    ) -> Result<Arc<dyn Agent>> {
        // If agent_id is provided, try to load it
        if let Some(ref agent_id) = member.agent_id {
            if let Ok(agent) = self.load_agent(&agent_id.0).await {
                return Ok(agent);
            }
            // Agent doesn't exist, fall through to creation
        }

        // Get agent config from member
        let agent_config = if let Some(ref config) = member.agent_config {
            config.clone()
        } else if let Some(ref config_path) = member.config_path {
            AgentConfig::load_from_file(config_path).await?
        } else {
            // Create minimal config from member info
            AgentConfig {
                id: member.agent_id.clone(),
                name: member.name.clone(),
                ..Default::default()
            }
        };

        // Create the agent
        self.create_agent(&agent_config).await
    }

    /// Internal: build agent from resolved config
    ///
    /// Constructs the agent runtime and DatabaseAgent from a fully
    /// resolved configuration. This is the final step in agent creation/loading.
    async fn build_agent_from_resolved(
        &self,
        agent_id: &str,
        resolved: &ResolvedAgentConfig,
    ) -> Result<Arc<dyn Agent>> {
        let agent_id_typed = AgentId::new(agent_id);
        let messages = MessageStore::new(self.dbs.constellation.pool().clone(), agent_id);

        // Filter tools based on enabled_tools list
        let tools = if !resolved.enabled_tools.is_empty() {
            let filtered = Arc::new(ToolRegistry::new());
            for tool_name in &resolved.enabled_tools {
                if let Some(tool) = self.tools.get(tool_name) {
                    filtered.register_dynamic(tool.clone_box());
                } else {
                    tracing::warn!(
                        agent_id = %agent_id,
                        tool = %tool_name,
                        "Tool in enabled_tools not found in registry - skipping"
                    );
                }
            }
            filtered
        } else {
            // Use all tools if no filter specified
            self.tools.clone()
        };

        // Build runtime config from resolved settings
        let mut runtime_config = RuntimeConfig::default();

        // Apply context settings if provided
        if let Some(max_msgs) = resolved.context.max_messages {
            runtime_config.context_config.max_messages_cap = max_msgs;
        }
        if let Some(ref strategy) = resolved.context.compression_strategy {
            runtime_config.context_config.compression_strategy = strategy.clone();
        }
        if let Some(include_desc) = resolved.context.include_descriptions {
            runtime_config.context_config.include_descriptions = include_desc;
        }
        if let Some(include_schemas) = resolved.context.include_schemas {
            runtime_config.context_config.include_schemas = include_schemas;
        }
        if let Some(limit) = resolved.context.activity_entries_limit {
            runtime_config.context_config.activity_entries_limit = limit;
        }

        // Configure response options if temperature or enable_thinking is set
        // NOTE: ResponseOptions requires ModelInfo, so we create minimal model info.
        // The actual model info will be updated when the agent retrieves its response options.
        let needs_response_opts =
            resolved.temperature.is_some() || resolved.context.enable_thinking.is_some();

        if needs_response_opts {
            // Get or create default response options
            if runtime_config.default_response_options.is_none() {
                let model_info = crate::model::ModelInfo {
                    id: resolved.model_name.clone(),
                    name: resolved.model_name.clone(),
                    provider: resolved.model_provider.clone(),
                    capabilities: vec![],
                    context_window: 128000, // Reasonable default
                    max_output_tokens: Some(8192),
                    cost_per_1k_prompt_tokens: None,
                    cost_per_1k_completion_tokens: None,
                };
                runtime_config.set_default_options(crate::model::ResponseOptions::new(model_info));
            }

            // Now apply the settings
            if let Some(ref mut opts) = runtime_config.default_response_options {
                if let Some(temp) = resolved.temperature {
                    opts.temperature = Some(temp as f64);
                }
                if let Some(enable) = resolved.context.enable_thinking {
                    opts.capture_reasoning_content = Some(enable);
                }
            }
        }

        // Build runtime with config
        let runtime = AgentRuntime::builder()
            .agent_id(agent_id)
            .agent_name(&resolved.name)
            .memory(self.memory.clone())
            .messages(messages)
            .tools_shared(tools)
            .model(self.model_provider.clone())
            .dbs(self.dbs.as_ref().clone())
            .tool_rules(resolved.tool_rules.clone())
            .config(runtime_config)
            .build()?;

        // Build agent
        // NOTE: system_prompt is passed as base_instructions to the agent, which
        // then passes it through to ContextBuilder during prepare_request().
        let mut agent_builder = DatabaseAgent::builder()
            .id(agent_id_typed)
            .name(&resolved.name)
            .runtime(Arc::new(runtime))
            .model(self.model_provider.clone())
            .model_id(&resolved.model_name)
            .heartbeat_sender(self.heartbeat_sender());

        // Add base_instructions if system_prompt is not empty
        if !resolved.system_prompt.is_empty() {
            agent_builder = agent_builder.base_instructions(&resolved.system_prompt);
        }

        let agent = agent_builder.build()?;

        let agent: Arc<dyn Agent> = Arc::new(agent);
        self.register_agent(agent.clone());

        Ok(agent)
    }
}

impl Drop for RuntimeContext {
    fn drop(&mut self) {
        // NOTE: This uses abort() which is not graceful. In-flight messages
        // may be left in inconsistent state. A proper implementation would use
        // a cancellation token pattern to signal shutdown and wait for tasks
        // to complete cleanly.
        //
        // TODO: Implement graceful shutdown with cancellation tokens.
        // The current approach:
        // 1. May leave database operations incomplete
        // 2. May drop messages that were being processed
        // 3. May not flush pending writes
        //
        // For production use, call shutdown() explicitly before dropping.
        if let Ok(mut tasks) = self.background_tasks.write() {
            for handle in tasks.drain(..) {
                handle.abort();
            }
        }
    }
}

// ============================================================================
// RuntimeContextBuilder
// ============================================================================

/// Builder for RuntimeContext
///
/// Provides a fluent API for constructing a RuntimeContext with all necessary
/// dependencies.
///
/// # Required Fields
/// - `dbs`: Combined database connections (constellation + auth)
/// - `model_provider`: Default model provider for agents
///
/// # Optional Fields
/// - `embedding_provider`: Embedding provider for semantic search
/// - `memory`: Pre-configured memory cache (defaults to new MemoryCache)
/// - `tools`: Pre-configured tool registry (defaults to empty ToolRegistry)
/// - `default_config`: Default agent configuration (defaults to AgentConfig::default())
/// - `context_config`: Runtime context configuration (defaults to RuntimeContextConfig::default())
///
/// # Example
///
/// ```ignore
/// let ctx = RuntimeContextBuilder::new()
///     .dbs(dbs)
///     .model_provider(anthropic_provider)
///     .embedding_provider(embedding_provider)
///     .memory(memory_cache)
///     .tools(tool_registry)
///     .build()
///     .await?;
/// ```
pub struct RuntimeContextBuilder {
    dbs: Option<Arc<ConstellationDatabases>>,
    model_provider: Option<Arc<dyn ModelProvider>>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    memory: Option<Arc<MemoryCache>>,
    tools: Option<Arc<ToolRegistry>>,
    default_config: Option<AgentConfig>,
    context_config: RuntimeContextConfig,
    memory_char_limit: Option<usize>,
}

impl RuntimeContextBuilder {
    /// Create a new builder with default values
    pub fn new() -> Self {
        Self {
            dbs: None,
            model_provider: None,
            embedding_provider: None,
            memory: None,
            tools: None,
            default_config: None,
            context_config: RuntimeContextConfig::default(),
            memory_char_limit: None,
        }
    }

    /// Set the combined database connections (required)
    ///
    /// The databases will be wrapped in an Arc for shared ownership.
    pub fn dbs(mut self, dbs: Arc<ConstellationDatabases>) -> Self {
        self.dbs = Some(dbs);
        self
    }

    /// Set the combined database connections from an owned ConstellationDatabases
    ///
    /// Convenience method that wraps the databases in an Arc.
    pub fn dbs_owned(mut self, dbs: ConstellationDatabases) -> Self {
        self.dbs = Some(Arc::new(dbs));
        self
    }

    /// Set the default model provider (required)
    ///
    /// This provider will be used for agents that don't specify their own.
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    /// Set the embedding provider (optional)
    ///
    /// Used for semantic search in memory and archival systems.
    pub fn embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Set a pre-configured memory cache (optional)
    ///
    /// If not provided, a new MemoryCache will be created using the database.
    pub fn memory(mut self, memory: Arc<MemoryCache>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Set a pre-configured tool registry (optional)
    ///
    /// If not provided, a new empty ToolRegistry will be created.
    pub fn tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the default agent configuration (optional)
    ///
    /// This configuration is used as defaults when loading or creating agents.
    pub fn default_config(mut self, config: AgentConfig) -> Self {
        self.default_config = Some(config);
        self
    }

    /// Set the runtime context configuration (optional)
    ///
    /// Controls queue processing, heartbeat behavior, and other runtime settings.
    pub fn context_config(mut self, config: RuntimeContextConfig) -> Self {
        self.context_config = config;
        self
    }

    /// Set the default memory block character limit (optional)
    ///
    /// This limit is used when creating memory blocks without an explicit limit.
    /// If not set, the MemoryCache default (5000) is used.
    pub fn memory_char_limit(mut self, limit: usize) -> Self {
        self.memory_char_limit = Some(limit);
        self
    }

    /// Set the activity rendering configuration (optional)
    ///
    /// Controls how recent activity is rendered in agent context, including
    /// max events, lookback period, and self-event limits.
    pub fn activity_config(mut self, config: ActivityConfig) -> Self {
        self.context_config.activity_config = config;
        self
    }

    /// Build the RuntimeContext
    ///
    /// # Errors
    ///
    /// Returns a `CoreError::ConfigurationError` if required fields are missing:
    /// - `dbs`: Database connections are required
    /// - `model_provider`: Model provider is required
    pub async fn build(self) -> Result<RuntimeContext> {
        let dbs = self.dbs.ok_or_else(|| CoreError::ConfigurationError {
            field: "dbs".to_string(),
            config_path: "RuntimeContextBuilder".to_string(),
            expected: "database connections".to_string(),
            cause: ConfigError::MissingField("dbs".to_string()),
        })?;

        let model_provider = self
            .model_provider
            .ok_or_else(|| CoreError::ConfigurationError {
                field: "model_provider".to_string(),
                config_path: "RuntimeContextBuilder".to_string(),
                expected: "model provider".to_string(),
                cause: ConfigError::MissingField("model_provider".to_string()),
            })?;

        // Create memory cache with embedding provider if available
        // Apply memory_char_limit if set and we're creating a new cache
        let memory = self.memory.unwrap_or_else(|| {
            let mut cache = if let Some(ref emb) = self.embedding_provider {
                MemoryCache::with_embedding_provider(dbs.clone(), emb.clone())
            } else {
                MemoryCache::new(dbs.clone())
            };

            // Apply custom char limit if specified
            if let Some(limit) = self.memory_char_limit {
                cache = cache.with_default_char_limit(limit);
            }

            Arc::new(cache)
        });
        let tools = self.tools.unwrap_or_else(|| Arc::new(ToolRegistry::new()));
        let default_config = self.default_config.unwrap_or_default();

        RuntimeContext::new_with_providers(
            dbs,
            model_provider,
            self.embedding_provider,
            memory,
            tools,
            default_config,
            self.context_config,
        )
        .await
    }
}

impl Default for RuntimeContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Process heartbeat requests using a DashMap-based agent registry
///
/// This is similar to `crate::context::heartbeat::process_heartbeats` but
/// works with a DashMap instead of a Vec, allowing dynamic agent registration.
async fn process_heartbeats_with_dashmap<F, Fut>(
    mut heartbeat_rx: HeartbeatReceiver,
    agents: Arc<DashMap<String, Arc<dyn Agent>>>,
    event_handler: F,
) where
    F: Fn(crate::agent::ResponseEvent, crate::AgentId, String) -> Fut
        + Clone
        + Send
        + Sync
        + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    use crate::agent::{AgentState, ResponseEvent};
    use crate::context::NON_USER_MESSAGE_PREFIX;
    use crate::message::{ChatRole, Message};
    use futures::StreamExt;
    use std::time::Duration;

    while let Some(heartbeat) = heartbeat_rx.recv().await {
        tracing::debug!(
            "RuntimeContext: Received heartbeat from agent {}: tool {} (call_id: {})",
            heartbeat.agent_id,
            heartbeat.tool_name,
            heartbeat.tool_call_id
        );

        // Look up agent in DashMap - get and immediately clone to avoid holding ref
        let agent = agents
            .get(heartbeat.agent_id.as_str())
            .map(|entry| entry.value().clone());

        if let Some(agent) = agent {
            let handler = event_handler.clone();
            let agent_id = heartbeat.agent_id.clone();
            let agent_name = agent.name().to_string();

            tokio::spawn(async move {
                // Wait for agent to be ready
                let (state, maybe_receiver) = agent.state().await;
                if state != AgentState::Ready {
                    if let Some(mut receiver) = maybe_receiver {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(200),
                            receiver.wait_for(|s| *s == AgentState::Ready),
                        )
                        .await;
                    }
                }

                tracing::info!(
                    "RuntimeContext: Processing heartbeat from tool: {}",
                    heartbeat.tool_name
                );

                // Determine role based on vendor
                let role = match heartbeat.model_vendor {
                    Some(vendor) if vendor.is_openai_compatible() => ChatRole::System,
                    Some(crate::model::ModelVendor::Gemini) => ChatRole::User,
                    _ => ChatRole::User, // Anthropic and default
                };

                // Create continuation message in same batch
                let content = format!(
                    "{}Function called using request_heartbeat=true, returning control {}",
                    NON_USER_MESSAGE_PREFIX, heartbeat.tool_name
                );
                let message = if let (Some(batch_id), Some(seq_num)) =
                    (heartbeat.batch_id, heartbeat.next_sequence_num)
                {
                    match role {
                        ChatRole::System => Message::system_in_batch(batch_id, seq_num, content),
                        ChatRole::Assistant => {
                            Message::assistant_in_batch(batch_id, seq_num, content)
                        }
                        _ => Message::user_in_batch(batch_id, seq_num, content),
                    }
                } else {
                    tracing::warn!("Heartbeat without batch info - creating new batch");
                    Message::user(content)
                };

                // Process and handle events
                match agent.process(message).await {
                    Ok(mut stream) => {
                        while let Some(event) = stream.next().await {
                            handler(event, agent_id.clone(), agent_name.clone()).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error processing heartbeat: {:?}", e);
                        handler(
                            ResponseEvent::Error {
                                message: format!("Heartbeat processing failed: {:?}", e),
                                recoverable: true,
                            },
                            agent_id,
                            agent_name,
                        )
                        .await;
                    }
                }
            });
        } else {
            tracing::warn!(
                "RuntimeContext: No agent found for heartbeat from {}",
                heartbeat.agent_id
            );
        }
    }

    tracing::debug!("RuntimeContext: Heartbeat processor task exiting");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MockModelProvider;

    async fn test_dbs() -> ConstellationDatabases {
        ConstellationDatabases::open_in_memory().await.unwrap()
    }

    fn mock_model_provider() -> Arc<dyn ModelProvider> {
        Arc::new(MockModelProvider {
            response: "test response".to_string(),
        })
    }

    #[tokio::test]
    async fn test_context_creation() {
        let dbs = test_dbs().await;

        let ctx = RuntimeContext::builder()
            .dbs_owned(dbs)
            .model_provider(mock_model_provider())
            .build()
            .await
            .unwrap();

        assert_eq!(ctx.agent_count(), 0);
    }

    #[tokio::test]
    async fn test_builder_requires_dbs() {
        let result = RuntimeContext::builder()
            .model_provider(mock_model_provider())
            .build()
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ConfigurationError { field, .. } => {
                assert_eq!(field, "dbs");
            }
            err => panic!("Expected ConfigurationError, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_model_provider() {
        let dbs = test_dbs().await;

        let result = RuntimeContext::builder().dbs_owned(dbs).build().await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ConfigurationError { field, .. } => {
                assert_eq!(field, "model_provider");
            }
            err => panic!("Expected ConfigurationError, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_agent_registration() {
        use crate::AgentId;
        use crate::agent::{Agent, AgentState, ResponseEvent};
        use crate::error::CoreError;
        use crate::message::Message;
        use crate::runtime::AgentRuntime;
        use async_trait::async_trait;
        use tokio_stream::Stream;

        // Simple mock agent for testing
        #[derive(Debug)]
        struct MockAgent {
            id: AgentId,
            name: String,
        }

        #[async_trait]
        impl Agent for MockAgent {
            fn id(&self) -> AgentId {
                self.id.clone()
            }

            fn name(&self) -> &str {
                &self.name
            }

            fn runtime(&self) -> &AgentRuntime {
                unimplemented!("Mock agent")
            }

            async fn process(
                self: Arc<Self>,
                _message: Message,
            ) -> std::result::Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError>
            {
                unimplemented!("Mock agent")
            }

            async fn state(
                &self,
            ) -> (AgentState, Option<tokio::sync::watch::Receiver<AgentState>>) {
                (AgentState::Ready, None)
            }

            async fn set_state(&self, _state: AgentState) -> std::result::Result<(), CoreError> {
                Ok(())
            }
        }

        let dbs = test_dbs().await;
        let ctx = RuntimeContext::builder()
            .dbs_owned(dbs)
            .model_provider(mock_model_provider())
            .build()
            .await
            .unwrap();

        // Register an agent
        let agent = Arc::new(MockAgent {
            id: AgentId::new("test_agent"),
            name: "Test Agent".to_string(),
        });

        ctx.register_agent(agent.clone());

        // Verify registration
        assert!(ctx.has_agent("test_agent"));
        assert_eq!(ctx.agent_count(), 1);

        // Get agent
        let retrieved = ctx.get_agent("test_agent").unwrap();
        assert_eq!(retrieved.id().as_str(), "test_agent");

        // List agents
        let ids = ctx.list_agent_ids();
        assert_eq!(ids, vec!["test_agent".to_string()]);

        // Remove agent
        let removed = ctx.remove_agent("test_agent");
        assert!(removed.is_some());
        assert!(!ctx.has_agent("test_agent"));
        assert_eq!(ctx.agent_count(), 0);
    }

    #[tokio::test]
    async fn test_heartbeat_sender() {
        let dbs = test_dbs().await;
        let ctx = RuntimeContext::builder()
            .dbs_owned(dbs)
            .model_provider(mock_model_provider())
            .build()
            .await
            .unwrap();

        // Should be able to clone heartbeat sender
        let sender1 = ctx.heartbeat_sender();
        let sender2 = ctx.heartbeat_sender();

        // Both should be valid senders (can't easily test sending without receiver)
        assert!(!sender1.is_closed());
        assert!(!sender2.is_closed());
    }

    #[tokio::test]
    async fn test_shutdown() {
        let dbs = test_dbs().await;
        let ctx = RuntimeContext::builder()
            .dbs_owned(dbs)
            .model_provider(mock_model_provider())
            .build()
            .await
            .unwrap();

        // Shutdown should not panic even with no tasks
        ctx.shutdown().await;
    }

    #[tokio::test]
    async fn test_provider_getters() {
        let dbs = test_dbs().await;
        let model = mock_model_provider();
        let default_config = AgentConfig::default();

        let ctx = RuntimeContext::builder()
            .dbs_owned(dbs)
            .model_provider(model.clone())
            .default_config(default_config.clone())
            .build()
            .await
            .unwrap();

        // Verify model provider is accessible
        assert_eq!(ctx.model_provider().name(), model.name());

        // Verify embedding provider is None by default
        assert!(ctx.embedding_provider().is_none());

        // Verify default config is accessible
        assert_eq!(ctx.default_config().name, default_config.name);
    }
}
