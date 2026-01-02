//! AgentRuntime: The "doing" layer for agents
//!
//! Holds all agent dependencies and handles:
//! - Tool execution with permission checks via ToolExecutor
//! - Message sending via router
//! - Message storage
//! - Context building (delegates to ContextBuilder)
//!
//! Also provides RuntimeContext for centralized agent management.

use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::{Arc, Weak};

use crate::ModelProvider;
use crate::agent::tool_rules::ToolRule;
use crate::context::ContextBuilder;
use crate::db::ConstellationDatabases;
use crate::error::CoreError;
use crate::id::AgentId;
use crate::memory::{
    MemoryResult, MemorySearchResult, MemoryStore, SearchOptions, SharedBlockManager,
};
use crate::messages::{BatchType, Message, MessageStore, Request, ToolCall, ToolResponse};
use crate::tool::{ExecutionMeta, ToolRegistry};
use crate::{SnowflakePosition, utils::get_next_message_position_sync};

mod context;
pub mod endpoints;
mod executor;
pub mod router;
mod tool_context;
mod types;

pub use context::{RuntimeContext, RuntimeContextBuilder, RuntimeContextConfig};
pub use executor::{
    ProcessToolState, ToolExecutionError, ToolExecutionResult, ToolExecutor, ToolExecutorConfig,
};
pub use router::{AgentMessageRouter, MessageEndpoint, MessageOrigin};
pub use tool_context::{SearchScope, ToolContext};
pub use types::RuntimeConfig;

/// AgentRuntime holds all agent dependencies and executes actions
pub struct AgentRuntime {
    agent_id: String,
    agent_name: String,

    // Stores
    memory: Arc<dyn MemoryStore>,
    messages: MessageStore,

    // Execution
    tools: Arc<ToolRegistry>,
    tool_executor: ToolExecutor,
    router: AgentMessageRouter,

    // Model (for compression, summarization)
    model: Option<Arc<dyn ModelProvider>>,

    // Combined databases (constellation + auth)
    dbs: ConstellationDatabases,

    // Block sharing
    shared_blocks: Arc<SharedBlockManager>,

    // Configuration
    config: RuntimeConfig,

    /// Weak reference to RuntimeContext for constellation-level operations
    ///
    /// Used for source management, cross-agent communication, etc.
    /// Weak reference avoids reference cycles since RuntimeContext holds agents.
    runtime_context: Option<Weak<RuntimeContext>>,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntime")
            .field("agent_id", &self.agent_id)
            .field("agent_name", &self.agent_name)
            .field("memory", &"<MemoryStore>")
            .field("messages", &self.messages)
            .field("tools", &self.tools)
            .field("tool_executor", &self.tool_executor)
            .field("router", &self.router)
            .field("model", &self.model.as_ref().map(|_| "<ModelProvider>"))
            .field("pool", &"<SqlitePool>")
            .field("config", &self.config)
            .finish()
    }
}

impl AgentRuntime {
    /// Create a new builder for constructing an AgentRuntime
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    /// Get the agent ID
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Get the agent name
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// Get the tool registry
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Get the database pool for the constellation database
    pub fn pool(&self) -> &SqlitePool {
        self.dbs.constellation.pool()
    }

    /// Get the combined database connections
    pub fn dbs(&self) -> &ConstellationDatabases {
        &self.dbs
    }

    /// Get the message store
    pub fn messages(&self) -> &MessageStore {
        &self.messages
    }

    /// Get the memory store
    pub fn memory(&self) -> &Arc<dyn MemoryStore> {
        &self.memory
    }

    /// Get the model provider (if configured)
    pub fn model(&self) -> Option<&Arc<dyn ModelProvider>> {
        self.model.as_ref()
    }

    /// Get the runtime configuration
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Get the message router
    pub fn router(&self) -> &AgentMessageRouter {
        &self.router
    }

    // ============================================================================
    // Message and Context Operations
    // ============================================================================

    /// Prepare a request for the model by processing incoming messages and building context.
    ///
    /// # Arguments
    /// * `incoming` - Incoming message(s) to add to the conversation
    /// * `model_id` - Optional model ID to use (None uses default)
    /// * `active_batch` - Optional batch ID to use (None determines from incoming or creates new)
    /// * `batch_type` - Optional batch type for new batches (None = infer from existing or UserRequest)
    /// * `base_instructions` - Optional base instructions (system prompt) for context building
    ///
    /// # Returns
    /// A `Request` ready to send to the model provider
    ///
    /// # Errors
    /// Returns `CoreError` if message storage or context building fails
    pub async fn prepare_request(
        &self,
        incoming: impl Into<Vec<Message>>,
        model_id: Option<&str>,
        active_batch: Option<SnowflakePosition>,
        batch_type: Option<BatchType>,
        base_instructions: Option<&str>,
    ) -> Result<Request, CoreError> {
        let mut incoming_messages: Vec<Message> = incoming.into();

        // Determine the batch ID to use
        let batch_id = if let Some(batch) = active_batch {
            batch
        } else if let Some(first_msg) = incoming_messages.first() {
            // Check if first message already has a batch ID
            if let Some(existing_batch) = first_msg.batch {
                existing_batch
            } else {
                // Create new batch ID directly (no wasteful MessageBatch creation)
                get_next_message_position_sync()
            }
        } else {
            // No incoming messages, create batch ID anyway
            get_next_message_position_sync()
        };

        // Query existing batch ONCE to get sequence count and batch type
        let existing_batch_messages = self.messages.get_batch(&batch_id.to_string()).await?;
        let mut next_seq = existing_batch_messages.len() as u32;

        // Infer batch type: use provided > from existing batch > default to UserRequest
        let inferred_batch_type = batch_type
            .or_else(|| existing_batch_messages.first().and_then(|m| m.batch_type))
            .unwrap_or(BatchType::UserRequest);

        // Process each incoming message
        for message in &mut incoming_messages {
            // Assign batch ID if not set
            if message.batch.is_none() {
                message.batch = Some(batch_id);
            }

            // Assign position if not set
            if message.position.is_none() {
                message.position = Some(get_next_message_position_sync());
            }

            // Assign sequence number if not set
            if message.sequence_num.is_none() {
                message.sequence_num = Some(next_seq);
                next_seq += 1;
            }

            // Assign batch type - use inferred type from existing batch
            if message.batch_type.is_none() {
                message.batch_type = Some(inferred_batch_type);
            }

            // Store the message
            self.messages.store(message).await?;
        }

        // Build context using ContextBuilder
        let mut builder = ContextBuilder::new(self.memory.as_ref(), &self.config.context_config)
            .for_agent(&self.agent_id)
            .with_messages(&self.messages)
            .with_tools(&self.tools)
            .with_active_batch(batch_id);

        // Add base instructions if provided
        if let Some(instructions) = base_instructions {
            builder = builder.with_base_instructions(instructions);
        }

        // Add model info if we have it from config
        if let Some(id) = model_id {
            if let Some(response_opts) = self.config.get_model_options(id) {
                builder = builder.with_model_info(&response_opts.model_info);
            }
        }

        // Add model provider if available
        if let Some(ref model_provider) = self.model {
            builder = builder.with_model_provider(model_provider.clone());
        }

        // Build and return the request
        builder.build().await
    }

    /// Store a message in the message store.
    ///
    /// This is a convenience wrapper around MessageStore::store.
    pub async fn store_message(&self, message: &Message) -> Result<(), CoreError> {
        self.messages.store(message).await
    }

    /// Get recent messages from the message store.
    ///
    /// This is a convenience wrapper around MessageStore::get_recent.
    pub async fn get_recent_messages(&self, limit: usize) -> Result<Vec<Message>, CoreError> {
        self.messages.get_recent(limit).await
    }

    // ============================================================================
    // Tool Execution (via ToolExecutor)
    // ============================================================================

    /// Create fresh process state for a process() call
    pub fn new_process_state(&self) -> ProcessToolState {
        self.tool_executor.new_process_state()
    }

    /// Execute a single tool with full rule validation, permission checks, and state tracking
    ///
    /// # Arguments
    /// * `call` - The tool call to execute
    /// * `batch_id` - Current batch ID for batch-scoped constraints
    /// * `process_state` - Mutable process state for this process() call
    /// * `meta` - Execution metadata (heartbeat request, caller info, etc.)
    ///
    /// # Returns
    /// A ToolExecutionResult with the response and continuation info, or a ToolExecutionError
    pub async fn execute_tool(
        &self,
        call: &ToolCall,
        batch_id: SnowflakePosition,
        process_state: &mut ProcessToolState,
        meta: &ExecutionMeta,
    ) -> Result<ToolExecutionResult, ToolExecutionError> {
        self.tool_executor
            .execute(call, batch_id, process_state, meta)
            .await
    }

    /// Execute multiple tool calls in sequence with full rule validation
    ///
    /// Returns (responses, needs_continuation).
    /// Stops early if a tool execution errors (not tool-returned error, but execution error).
    pub async fn execute_tools(
        &self,
        calls: &[ToolCall],
        batch_id: SnowflakePosition,
        process_state: &mut ProcessToolState,
        meta: &ExecutionMeta,
    ) -> (Vec<ToolResponse>, bool) {
        self.tool_executor
            .execute_batch(calls, batch_id, process_state, meta)
            .await
    }

    // ============================================================================
    // ToolExecutor Query/State Methods
    // ============================================================================

    /// Get unsatisfied start constraint tools
    ///
    /// Returns list of tool names that must be called before other tools.
    pub fn get_unsatisfied_start_constraints(
        &self,
        process_state: &ProcessToolState,
    ) -> Vec<String> {
        self.tool_executor
            .get_unsatisfied_start_constraints(process_state)
    }

    /// Get pending exit requirement tools
    ///
    /// Returns list of tool names that must be called before exit.
    pub fn get_pending_exit_requirements(&self, process_state: &ProcessToolState) -> Vec<String> {
        self.tool_executor
            .get_pending_exit_requirements(process_state)
    }

    /// Check if loop should exit based on process state
    pub fn should_exit_loop(&self, process_state: &ProcessToolState) -> bool {
        self.tool_executor.should_exit_loop(process_state)
    }

    /// Check if tool requires heartbeat (no ContinueLoop rule)
    pub fn requires_heartbeat(&self, tool_name: &str) -> bool {
        self.tool_executor.requires_heartbeat(tool_name)
    }

    /// Mark start constraints as satisfied
    pub fn mark_start_constraints_done(&self, process_state: &mut ProcessToolState) {
        self.tool_executor
            .mark_start_constraints_done(process_state)
    }

    /// Mark processing as complete
    pub fn mark_complete(&self, process_state: &mut ProcessToolState) {
        self.tool_executor.mark_complete(process_state)
    }

    /// Mark a batch as complete (allows cleanup of batch constraints)
    pub fn complete_batch(&self, batch_id: SnowflakePosition) {
        self.tool_executor.complete_batch(batch_id)
    }

    /// Prune expired entries from persistent state
    pub fn prune_expired(&self) {
        self.tool_executor.prune_expired()
    }

    /// Get direct access to the tool executor (for advanced use cases)
    pub fn tool_executor(&self) -> &ToolExecutor {
        &self.tool_executor
    }

    /// Get this runtime as a ToolContext trait object
    pub fn tool_context(&self) -> &dyn ToolContext {
        self
    }

    // ============================================================================
    // Permission Check Helpers
    // ============================================================================

    /// Check if this agent has a specific capability as a specialist.
    ///
    /// Returns true if the agent has the capability with specialist role in any group.
    /// Used for permission checks on constellation-wide operations.
    pub async fn has_capability(&self, capability: &str) -> bool {
        match pattern_db::queries::agent_has_capability(self.pool(), &self.agent_id, capability)
            .await
        {
            Ok(has_cap) => has_cap,
            Err(e) => {
                tracing::warn!(
                    agent_id = %self.agent_id,
                    capability = %capability,
                    error = %e,
                    "Failed to check agent capability"
                );
                false
            }
        }
    }

    /// Check if this agent shares a group with another agent.
    ///
    /// Returns true if both agents are members of at least one common group.
    /// Used for permission checks on cross-agent search operations.
    pub async fn shares_group_with(&self, other_agent_id: &str) -> bool {
        match pattern_db::queries::agents_share_group(self.pool(), &self.agent_id, other_agent_id)
            .await
        {
            Ok(shares) => shares,
            Err(e) => {
                tracing::warn!(
                    agent_id = %self.agent_id,
                    other_agent_id = %other_agent_id,
                    error = %e,
                    "Failed to check group membership"
                );
                false
            }
        }
    }
}

// ============================================================================
// ToolContext Implementation
// ============================================================================

#[async_trait]
impl ToolContext for AgentRuntime {
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn memory(&self) -> &dyn MemoryStore {
        self.memory.as_ref()
    }

    fn router(&self) -> &AgentMessageRouter {
        &self.router
    }

    fn model(&self) -> Option<&dyn ModelProvider> {
        self.model.as_ref().map(|m| m.as_ref())
    }

    fn permission_broker(&self) -> &'static crate::permission::PermissionBroker {
        crate::permission::broker()
    }

    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        match scope {
            SearchScope::CurrentAgent => self.memory.search(&self.agent_id, query, options).await,
            SearchScope::Agent(ref id) => {
                // Permission check: agents must share a group to search each other's memory.
                if !self.shares_group_with(id.as_str()).await {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        target_agent = %id,
                        "Cross-agent search denied: agents do not share a group"
                    );
                    return Ok(Vec::new());
                }

                tracing::debug!(
                    agent_id = %self.agent_id,
                    target_agent = %id,
                    "Cross-agent search permitted: agents share a group"
                );
                self.memory.search(id.as_str(), query, options).await
            }
            SearchScope::Agents(ref ids) => {
                // Permission check: filter to only agents that share a group with the requester.
                // NOTE: This does sequential permission checks per agent. For very large agent lists,
                // consider adding a batch query. In practice, groups are small so this is fine.
                let mut permitted_ids = Vec::new();
                for id in ids {
                    if self.shares_group_with(id.as_str()).await {
                        permitted_ids.push(id.clone());
                    } else {
                        tracing::warn!(
                            agent_id = %self.agent_id,
                            target_agent = %id,
                            "Cross-agent search denied for agent: no shared group"
                        );
                    }
                }

                if permitted_ids.is_empty() {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        "Multi-agent search denied: no target agents share a group"
                    );
                    return Ok(Vec::new());
                }

                tracing::debug!(
                    agent_id = %self.agent_id,
                    permitted_count = permitted_ids.len(),
                    total_requested = ids.len(),
                    "Multi-agent search: searching permitted agents"
                );

                // Search each permitted agent and merge results.
                let mut all_results = Vec::new();
                for id in &permitted_ids {
                    match self
                        .memory
                        .search(id.as_str(), query, options.clone())
                        .await
                    {
                        Ok(results) => all_results.extend(results),
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %self.agent_id,
                                target_agent = %id,
                                error = %e,
                                "Failed to search agent memory"
                            );
                        }
                    }
                }
                Ok(all_results)
            }
            SearchScope::Constellation => {
                // Permission check: agent must have "memory" capability as a specialist.
                if !self.has_capability("memory").await {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        "Constellation-wide search denied: agent lacks 'memory' capability"
                    );
                    return Ok(Vec::new());
                }

                tracing::debug!(
                    agent_id = %self.agent_id,
                    "Constellation-wide search permitted: agent has 'memory' capability"
                );
                self.memory.search_all(query, options).await
            }
        }
    }

    fn sources(&self) -> Option<Arc<dyn crate::data_source::SourceManager>> {
        self.runtime_context
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .map(|arc| arc as Arc<dyn crate::data_source::SourceManager>)
    }

    fn shared_blocks(&self) -> Option<Arc<SharedBlockManager>> {
        Some(self.shared_blocks.clone())
    }
}

/// Builder for constructing an AgentRuntime
#[derive(Default)]
pub struct RuntimeBuilder {
    agent_id: Option<String>,
    agent_name: Option<String>,
    memory: Option<Arc<dyn MemoryStore>>,
    messages: Option<MessageStore>,
    tools: Option<Arc<ToolRegistry>>,
    tool_rules: Vec<ToolRule>,
    executor_config: Option<ToolExecutorConfig>,
    model: Option<Arc<dyn ModelProvider>>,
    dbs: Option<ConstellationDatabases>,
    config: RuntimeConfig,
    runtime_context: Option<Weak<RuntimeContext>>,
}

impl RuntimeBuilder {
    /// Set the agent ID (required)
    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// Set the agent name (optional, defaults to agent_id)
    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = Some(name.into());
        self
    }

    /// Set the memory store (required)
    pub fn memory(mut self, memory: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Set the message store (required)
    pub fn messages(mut self, messages: MessageStore) -> Self {
        self.messages = Some(messages);
        self
    }

    /// Set the tool registry by value (will be wrapped in Arc)
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = Some(Arc::new(tools));
        self
    }

    /// Set the tool registry as a shared Arc
    pub fn tools_shared(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the model provider (optional)
    pub fn model(mut self, model: Arc<dyn ModelProvider>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the combined database connections (required)
    pub fn dbs(mut self, dbs: ConstellationDatabases) -> Self {
        self.dbs = Some(dbs);
        self
    }

    /// Set the runtime configuration
    pub fn config(mut self, config: RuntimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Set tool execution rules (combined from tools and explicit rules)
    pub fn tool_rules(mut self, rules: Vec<ToolRule>) -> Self {
        self.tool_rules = rules;
        self
    }

    /// Add a single tool rule
    pub fn add_tool_rule(mut self, rule: ToolRule) -> Self {
        self.tool_rules.push(rule);
        self
    }

    /// Set tool executor configuration
    pub fn executor_config(mut self, config: ToolExecutorConfig) -> Self {
        self.executor_config = Some(config);
        self
    }

    /// Set the runtime context (weak reference to avoid cycles)
    ///
    /// The runtime context provides access to constellation-level operations
    /// like source management and cross-agent communication.
    pub fn runtime_context(mut self, ctx: Weak<RuntimeContext>) -> Self {
        self.runtime_context = Some(ctx);
        self
    }

    /// Build the AgentRuntime, validating that all required fields are present
    pub fn build(self) -> Result<AgentRuntime, CoreError> {
        // Validate required fields
        let agent_id = self.agent_id.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "AgentRuntime".to_string(),
            details: "agent_id is required".to_string(),
        })?;

        let agent_name = self.agent_name.unwrap_or_else(|| agent_id.clone());

        let memory = self.memory.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "AgentRuntime".to_string(),
            details: "memory store is required".to_string(),
        })?;

        let messages = self.messages.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "AgentRuntime".to_string(),
            details: "message store is required".to_string(),
        })?;

        let dbs = self.dbs.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "AgentRuntime".to_string(),
            details: "database connections are required".to_string(),
        })?;

        // Optional fields with defaults
        let tools = self.tools.unwrap_or_else(|| Arc::new(ToolRegistry::new()));
        let executor_config = self.executor_config.unwrap_or_default();

        // Create ToolExecutor with AgentId
        let tool_executor = ToolExecutor::new(
            AgentId::new(&agent_id),
            tools.clone(),
            self.tool_rules,
            executor_config,
        );

        // Create router with agent info (uses combined databases)
        let router = AgentMessageRouter::new(agent_id.clone(), agent_name.clone(), dbs.clone());

        // Create shared block manager
        let shared_blocks = Arc::new(SharedBlockManager::new(Arc::new(dbs.clone())));

        Ok(AgentRuntime {
            agent_id,
            agent_name,
            memory,
            messages,
            tools,
            tool_executor,
            router,
            model: self.model,
            dbs,
            shared_blocks,
            config: self.config,
            runtime_context: self.runtime_context,
        })
    }
}

/// Test utilities for runtime - available to other test modules in the crate
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    // Re-export shared MockMemoryStore from test_helpers
    pub use crate::test_helpers::memory::MockMemoryStore;

    /// Create in-memory test databases
    pub async fn test_dbs() -> ConstellationDatabases {
        ConstellationDatabases::open_in_memory().await.unwrap()
    }

    /// Create a minimal test runtime
    pub async fn test_runtime(agent_id: &str) -> AgentRuntime {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), agent_id);

        AgentRuntime::builder()
            .agent_id(agent_id)
            .memory(memory)
            .messages(messages)
            .dbs(dbs)
            .build()
            .expect("Failed to build test runtime")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{MockMemoryStore, test_dbs};
    use super::*;

    #[tokio::test]
    async fn test_runtime_builder_requires_agent_id() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test");

        let result = AgentRuntime::builder()
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "AgentRuntime");
                assert!(details.contains("agent_id"));
            }
            _ => panic!("Expected InvalidFormat error, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_runtime_builder_requires_memory() {
        let dbs = test_dbs().await;
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");

        let result = AgentRuntime::builder()
            .agent_id("test_agent")
            .messages(messages)
            .dbs(dbs.clone())
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "AgentRuntime");
                assert!(details.contains("memory"));
            }
            _ => panic!("Expected InvalidFormat error, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_runtime_builder_requires_messages() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());

        let result = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .dbs(dbs.clone())
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "AgentRuntime");
                assert!(details.contains("message"));
            }
            _ => panic!("Expected InvalidFormat error, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_runtime_builder_requires_dbs() {
        let memory = Arc::new(MockMemoryStore::new());
        // Create temp dbs just for the MessageStore
        let temp_dbs = test_dbs().await;

        let result = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory.clone())
            .messages(MessageStore::new(
                temp_dbs.constellation.pool().clone(),
                "test",
            ))
            .build();

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "AgentRuntime");
                assert!(details.contains("database"));
            }
            _ => panic!("Expected InvalidFormat error, got: {:?}", err),
        }
    }

    #[tokio::test]
    async fn test_runtime_construction() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let tools = ToolRegistry::new();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        assert_eq!(runtime.agent_id(), "test_agent");
        assert_eq!(runtime.agent_name(), "test_agent");
    }

    #[tokio::test]
    async fn test_runtime_construction_with_name() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .agent_name("Test Agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        assert_eq!(runtime.agent_id(), "test_agent");
        assert_eq!(runtime.agent_name(), "Test Agent");
    }

    #[tokio::test]
    async fn test_runtime_default_tools() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");

        // Don't provide tools - should get default empty registry
        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        assert_eq!(runtime.tools().list_tools().len(), 0);
    }

    #[tokio::test]
    async fn test_tool_context_returns_agent_id() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs)
            .build()
            .unwrap();

        let ctx = runtime.tool_context();
        assert_eq!(ctx.agent_id(), "test_agent");
    }

    #[tokio::test]
    async fn test_tool_context_provides_memory() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs)
            .build()
            .unwrap();

        let ctx = runtime.tool_context();
        // Just verify we can call memory() without panic
        let _ = ctx.memory();
    }

    #[test]
    fn test_search_scope_default() {
        let scope = SearchScope::default();
        assert!(matches!(scope, SearchScope::CurrentAgent));
    }
}
