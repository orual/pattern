//! ContextBuilder: Assembles model requests from memory, messages, and tools
//!
//! This is the core of the v2 context system. It reads from MemoryStore to get
//! memory blocks, MessageStore to get recent messages, and ToolRegistry to get
//! available tools, then assembles everything into a `Request` ready for model calls.

use crate::ModelProvider;
use crate::agent::SnowflakePosition;
use crate::agent::tool_rules::ToolRule;
use crate::context::activity::ActivityRenderer;
use crate::context::compression::MessageCompressor;
use crate::context::types::ContextConfig;
use crate::error::CoreError;
use crate::memory::{BlockType, MemoryStore, SharedBlockInfo};
use crate::message::{ChatRole, Message, MessageContent, Request};
use crate::messages::MessageStore;
use crate::model::ModelInfo;
use crate::tool::ToolRegistry;
use std::sync::Arc;

/// Builder for constructing model requests with context
///
/// Combines memory blocks, message history, and tools into a complete
/// request ready for sending to a language model.
pub struct ContextBuilder<'a> {
    memory: &'a dyn MemoryStore,
    messages: Option<&'a MessageStore>,
    tools: Option<&'a ToolRegistry>,
    config: &'a ContextConfig,
    agent_id: Option<String>,
    agent_name: Option<String>,
    model_info: Option<&'a ModelInfo>,
    active_batch_id: Option<SnowflakePosition>,
    model_provider: Option<Arc<dyn ModelProvider>>,
    base_instructions: Option<String>,
    tool_rules: Vec<ToolRule>,
    activity_renderer: Option<&'a ActivityRenderer>,
}

impl<'a> ContextBuilder<'a> {
    /// Create a new ContextBuilder with memory store and config
    ///
    /// # Arguments
    /// * `memory` - Memory store for accessing memory blocks
    /// * `config` - Configuration for context limits and options
    pub fn new(memory: &'a dyn MemoryStore, config: &'a ContextConfig) -> Self {
        Self {
            memory,
            messages: None,
            tools: None,
            config,
            agent_id: None,
            agent_name: None,
            model_info: None,
            active_batch_id: None,
            model_provider: None,
            base_instructions: None,
            tool_rules: Vec::new(),
            activity_renderer: None,
        }
    }

    /// Set the agent ID for this context
    ///
    /// # Arguments
    /// * `agent_id` - The ID of the agent this context is for
    pub fn for_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Add a message store for retrieving message history
    ///
    /// # Arguments
    /// * `messages` - Message store to retrieve recent messages from
    pub fn with_messages(mut self, messages: &'a MessageStore) -> Self {
        self.messages = Some(messages);
        self
    }

    /// Add a tool registry for providing available tools
    ///
    /// # Arguments
    /// * `tools` - Tool registry containing available tools
    pub fn with_tools(mut self, tools: &'a ToolRegistry) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Add model information for provider-specific optimizations
    ///
    /// # Arguments
    /// * `model_info` - Information about the model being used
    pub fn with_model_info(mut self, model_info: &'a ModelInfo) -> Self {
        self.model_info = Some(model_info);
        self
    }

    /// Set the active batch (currently being processed)
    ///
    /// # Arguments
    /// * `batch_id` - The ID of the batch currently being processed
    ///
    /// The active batch will always be included in the context and never compressed,
    /// even if incomplete. Other incomplete batches will be excluded entirely.
    pub fn with_active_batch(mut self, batch_id: SnowflakePosition) -> Self {
        self.active_batch_id = Some(batch_id);
        self
    }

    /// Set the model provider for compression strategies that need it
    ///
    /// # Arguments
    /// * `provider` - The model provider for generating summaries
    pub fn with_model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    /// Set the base instructions (system prompt) for this context
    ///
    /// # Arguments
    /// * `instructions` - Base instructions to prepend to the system prompt
    pub fn with_base_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.base_instructions = Some(instructions.into());
        self
    }

    /// Set the tool rules for this context
    ///
    /// # Arguments
    /// * `rules` - Tool execution rules to include in the system prompt
    pub fn with_tool_rules(mut self, rules: Vec<ToolRule>) -> Self {
        self.tool_rules = rules;
        self
    }

    /// Set the agent name for activity attribution
    ///
    /// # Arguments
    /// * `name` - The display name of the agent (used in activity attribution)
    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = Some(name.into());
        self
    }

    /// Set the activity renderer for including recent constellation activity
    ///
    /// # Arguments
    /// * `renderer` - The ActivityRenderer to use for rendering recent activity
    pub fn with_activity_renderer(mut self, renderer: &'a ActivityRenderer) -> Self {
        self.activity_renderer = Some(renderer);
        self
    }

    /// Build the final Request with system prompt, messages, and tools
    ///
    /// # Returns
    /// A `Request` ready to send to a language model
    ///
    /// # Errors
    /// Returns `CoreError` if:
    /// - Agent ID was not set
    /// - Memory operations fail
    /// - Message retrieval fails
    pub async fn build(self) -> Result<Request, CoreError> {
        let agent_id = self
            .agent_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidFormat {
                data_type: "ContextBuilder".to_string(),
                details: "agent_id must be set before building".to_string(),
            })?;

        // Build system prompt from memory blocks
        let system = self.build_system_prompt(agent_id).await?;

        // Get recent messages if message store is provided
        let mut messages = if let Some(msg_store) = self.messages {
            self.get_recent_messages(msg_store).await?
        } else {
            Vec::new()
        };

        // Apply model-specific adjustments if model_info is available
        if let Some(model_info) = self.model_info {
            self.apply_model_adjustments(model_info, &mut messages)?;
        }

        // Get tools in genai format if tool registry is provided
        let tools = self.tools.map(|registry| registry.to_genai_tools());

        Ok(Request {
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            messages,
            tools,
        })
    }

    /// Build system prompt from base instructions, Core and Working memory blocks, and tool rules
    async fn build_system_prompt(&self, agent_id: &str) -> Result<Vec<String>, CoreError> {
        let mut prompt_parts = Vec::new();

        // Add base instructions first if set
        if let Some(ref instructions) = self.base_instructions {
            prompt_parts.push(instructions.clone());
        } else {
            prompt_parts.push(super::DEFAULT_BASE_INSTRUCTIONS.to_string());
        }

        // Get owned blocks
        let owned_core_blocks = self
            .memory
            .list_blocks_by_type(agent_id, BlockType::Core)
            .await
            .map_err(|e| CoreError::InvalidFormat {
                data_type: "memory blocks".to_string(),
                details: format!("Failed to list Core blocks: {}", e),
            })?;

        let owned_working_blocks = self
            .memory
            .list_blocks_by_type(agent_id, BlockType::Working)
            .await
            .map_err(|e| CoreError::InvalidFormat {
                data_type: "memory blocks".to_string(),
                details: format!("Failed to list Working blocks: {}", e),
            })?;

        // Get shared blocks
        let shared_blocks = self
            .memory
            .list_shared_blocks(agent_id)
            .await
            .map_err(|e| CoreError::InvalidFormat {
                data_type: "shared blocks".to_string(),
                details: format!("Failed to list shared blocks: {}", e),
            })?;

        // Render Core blocks (owned + shared Core blocks)
        let core_rendered = self
            .render_blocks(agent_id, owned_core_blocks, &shared_blocks, BlockType::Core)
            .await?;
        prompt_parts.extend(core_rendered);

        // Render Working blocks (owned + shared Working blocks)
        let working_rendered = self
            .render_blocks(
                agent_id,
                owned_working_blocks,
                &shared_blocks,
                BlockType::Working,
            )
            .await?;
        prompt_parts.extend(working_rendered);

        // Add activity section if renderer is provided
        if let Some(renderer) = self.activity_renderer {
            let activity = renderer.render_for_agent(agent_id).await.map_err(|e| {
                CoreError::InvalidFormat {
                    data_type: "activity".to_string(),
                    details: format!("Failed to render activity: {}", e),
                }
            })?;
            if !activity.is_empty() {
                prompt_parts.push(activity);
            }
        }

        // Add tool rules at the end if any are set
        if !self.tool_rules.is_empty() {
            let rules_text = self.render_tool_rules();
            prompt_parts.push(rules_text);
        }

        Ok(prompt_parts)
    }

    /// Render tool rules as a formatted block for the system prompt
    fn render_tool_rules(&self) -> String {
        let mut rules_section = String::from("# Tool Execution Rules\n\n");

        for rule in &self.tool_rules {
            let description = rule.to_usage_description();
            rules_section.push_str(&format!("- {}\n", description));
        }

        rules_section
    }

    /// Get recent messages from the message store
    async fn get_recent_messages(
        &self,
        msg_store: &MessageStore,
    ) -> Result<Vec<Message>, CoreError> {
        // Get limit from config, using model_info if available
        let model_id = self.model_info.map(|info| info.id.as_str());
        let limits = self.config.limits_for_model(model_id);

        // Get messages as MessageBatches from store (already uses BTreeMap for ordering)
        let batches = msg_store.get_batches(self.config.max_messages_cap).await?;

        // Use existing MessageCompressor from context/compression.rs
        let strategy = self.config.compression_strategy.clone();
        let mut compressor = MessageCompressor::new(strategy);

        // Add model provider for RecursiveSummarization if available
        if let Some(ref provider) = self.model_provider {
            compressor = compressor.with_model_provider(provider.clone());
        }

        // Compress batches
        let result = compressor
            .compress(
                batches,
                self.config.max_messages_cap,
                Some(limits.history_tokens),
            )
            .await
            .map_err(|e| CoreError::InvalidFormat {
                data_type: "compression".to_string(),
                details: format!("Compression failed: {}", e),
            })?;

        // Filter: include complete batches + active batch, exclude other incomplete
        let mut messages: Vec<Message> = Vec::new();

        for batch in result.active_batches {
            let is_active = self.active_batch_id.as_ref() == Some(&batch.id);

            if batch.is_complete || is_active {
                messages.extend(batch.messages);
            }
            // Incomplete non-active: dropped
        }

        // INCORRECT SORT REMOVED

        Ok(messages)
    }

    /// Apply model-specific adjustments to messages
    fn apply_model_adjustments(
        &self,
        model_info: &ModelInfo,
        messages: &mut Vec<Message>,
    ) -> Result<(), CoreError> {
        // Check if this is a Gemini model
        if model_info.provider.to_lowercase().contains("gemini")
            || model_info.id.to_lowercase().starts_with("gemini")
        {
            self.adjust_for_gemini(messages);
        }

        Ok(())
    }

    /// Adjust messages for Gemini compatibility
    fn adjust_for_gemini(&self, messages: &mut Vec<Message>) {
        // Gemini requires:
        // 1. First message must be user role
        // 2. No empty content

        // Remove empty messages
        messages.retain(|m| !Self::is_empty_content(&m.content));

        // Ensure first message is user
        if let Some(first) = messages.first() {
            if first.role != ChatRole::User {
                messages.insert(0, Message::user("[Conversation start]"));
            }
        }
    }

    /// Check if message content is empty
    fn is_empty_content(content: &MessageContent) -> bool {
        match content {
            MessageContent::Text(text) => text.is_empty(),
            MessageContent::Parts(parts) => parts.is_empty(),
            MessageContent::Blocks(blocks) => blocks.is_empty(),
            MessageContent::ToolCalls(calls) => calls.is_empty(),
            MessageContent::ToolResponses(responses) => responses.is_empty(),
        }
    }

    /// Render owned and shared blocks of a specific type with permission info
    async fn render_blocks(
        &self,
        agent_id: &str,
        owned_blocks: Vec<crate::memory::BlockMetadata>,
        shared_blocks: &[SharedBlockInfo],
        block_type: BlockType,
    ) -> Result<Vec<String>, CoreError> {
        let mut prompt_parts = Vec::new();

        // Render owned blocks with permission info
        for block_meta in owned_blocks {
            if let Some(content) = self
                .memory
                .get_rendered_content(agent_id, &block_meta.label)
                .await
                .map_err(|e| CoreError::InvalidFormat {
                    data_type: "memory content".to_string(),
                    details: format!(
                        "Failed to get rendered content for {}: {}",
                        block_meta.label, e
                    ),
                })?
            {
                let permission_str = block_meta.permission.to_string();

                // Format: <block:label permission="...">content</block:label>
                let block_content =
                    if self.config.include_descriptions && !block_meta.description.is_empty() {
                        format!(
                            "<block:{} permission=\"{}\">\n{}\n\n{}\n</block:{}>",
                            block_meta.label,
                            permission_str,
                            block_meta.description,
                            content,
                            block_meta.label
                        )
                    } else {
                        format!(
                            "<block:{} permission=\"{}\">\n{}\n</block:{}>",
                            block_meta.label, permission_str, content, block_meta.label
                        )
                    };

                prompt_parts.push(block_content);
            }
        }

        // Render shared blocks of the matching type
        for shared_info in shared_blocks.iter().filter(|s| s.block_type == block_type) {
            // Get the shared block content using get_shared_block
            if let Some(doc) = self
                .memory
                .get_shared_block(agent_id, &shared_info.owner_agent_id, &shared_info.label)
                .await
                .map_err(|e| CoreError::InvalidFormat {
                    data_type: "shared block content".to_string(),
                    details: format!("Failed to get shared block {}: {}", shared_info.label, e),
                })?
            {
                let content = doc.render();
                let permission_str = shared_info.permission.to_string();

                // Use agent name if available, fall back to agent ID
                let owner_display = shared_info
                    .owner_agent_name
                    .as_deref()
                    .unwrap_or(&shared_info.owner_agent_id);

                // Format: <block:label permission="..." shared_from="owner_name">content</block:label>
                let block_content = if self.config.include_descriptions
                    && !shared_info.description.is_empty()
                {
                    format!(
                        "<block:{} permission=\"{}\" shared_from=\"{}\">\n{}\n\n{}\n</block:{}>",
                        shared_info.label,
                        permission_str,
                        owner_display,
                        shared_info.description,
                        content,
                        shared_info.label
                    )
                } else {
                    format!(
                        "<block:{} permission=\"{}\" shared_from=\"{}\">\n{}\n</block:{}>",
                        shared_info.label,
                        permission_str,
                        owner_display,
                        content,
                        shared_info.label
                    )
                };

                prompt_parts.push(block_content);
            }
        }

        Ok(prompt_parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{
        ArchivalEntry, BlockMetadata, BlockSchema, MemoryResult, MemorySearchResult, SearchOptions,
        StructuredDocument,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use serde_json::Value as JsonValue;

    // Mock MemoryStore for testing
    #[derive(Debug)]
    struct MockMemoryStore;

    #[async_trait]
    impl MemoryStore for MockMemoryStore {
        async fn create_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _description: &str,
            _block_type: BlockType,
            _schema: BlockSchema,
            _char_limit: usize,
        ) -> MemoryResult<String> {
            Ok("test-block-id".to_string())
        }

        async fn get_block(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        async fn get_block_metadata(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<BlockMetadata>> {
            Ok(None)
        }

        async fn list_blocks(&self, _agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
            Ok(Vec::new())
        }

        async fn list_blocks_by_type(
            &self,
            _agent_id: &str,
            block_type: BlockType,
        ) -> MemoryResult<Vec<BlockMetadata>> {
            // Return mock blocks based on type
            match block_type {
                BlockType::Core => Ok(vec![BlockMetadata {
                    id: "core-1".to_string(),
                    agent_id: "test-agent".to_string(),
                    label: "core_memory".to_string(),
                    description: "Core agent memory".to_string(),
                    block_type: BlockType::Core,
                    schema: BlockSchema::Text,
                    char_limit: 1000,
                    permission: pattern_db::models::MemoryPermission::ReadWrite,
                    pinned: true,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }]),
                BlockType::Working => Ok(vec![BlockMetadata {
                    id: "working-1".to_string(),
                    agent_id: "test-agent".to_string(),
                    label: "working_memory".to_string(),
                    description: "Working context".to_string(),
                    block_type: BlockType::Working,
                    schema: BlockSchema::Text,
                    char_limit: 2000,
                    permission: pattern_db::models::MemoryPermission::ReadWrite,
                    pinned: false,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }]),
                _ => Ok(Vec::new()),
            }
        }

        async fn delete_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        async fn get_rendered_content(
            &self,
            _agent_id: &str,
            label: &str,
        ) -> MemoryResult<Option<String>> {
            // Return mock content based on label
            Ok(Some(format!("Content for {}", label)))
        }

        async fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn mark_dirty(&self, _agent_id: &str, _label: &str) {}

        async fn insert_archival(
            &self,
            _agent_id: &str,
            _content: &str,
            _metadata: Option<JsonValue>,
        ) -> MemoryResult<String> {
            Ok("archival-id".to_string())
        }

        async fn search_archival(
            &self,
            _agent_id: &str,
            _query: &str,
            _limit: usize,
        ) -> MemoryResult<Vec<ArchivalEntry>> {
            Ok(Vec::new())
        }

        async fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
            Ok(())
        }

        async fn search(
            &self,
            _agent_id: &str,
            _query: &str,
            _options: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        async fn search_all(
            &self,
            _query: &str,
            _options: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        async fn update_block_text(
            &self,
            _agent_id: &str,
            _label: &str,
            _new_content: &str,
        ) -> MemoryResult<()> {
            Ok(())
        }

        async fn append_to_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _content: &str,
        ) -> MemoryResult<()> {
            Ok(())
        }

        async fn replace_in_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _old: &str,
            _new: &str,
        ) -> MemoryResult<bool> {
            Ok(false)
        }

        async fn list_shared_blocks(
            &self,
            _agent_id: &str,
        ) -> MemoryResult<Vec<crate::memory::SharedBlockInfo>> {
            // Return empty list for tests (no shared blocks by default)
            Ok(Vec::new())
        }

        async fn get_shared_block(
            &self,
            _requester_agent_id: &str,
            _owner_agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn test_builder_basic() {
        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let builder = ContextBuilder::new(&memory, &config).for_agent("test-agent");

        let request = builder.build().await.unwrap();

        // Should have system prompt from Core and Working blocks
        assert!(request.system.is_some());
        let system = request.system.unwrap();
        assert_eq!(system.len(), 3); // One Core, one Working

        // Should have no messages (no MessageStore provided)
        assert_eq!(request.messages.len(), 0);

        // Should have no tools (no ToolRegistry provided)
        assert!(request.tools.is_none());
    }

    #[tokio::test]
    async fn test_builder_requires_agent_id() {
        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let builder = ContextBuilder::new(&memory, &config);

        // Should fail because agent_id not set
        let result = builder.build().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_builder_with_descriptions() {
        let memory = MockMemoryStore;
        let mut config = ContextConfig::default();
        config.include_descriptions = true;

        let builder = ContextBuilder::new(&memory, &config).for_agent("test-agent");

        let request = builder.build().await.unwrap();

        let system = request.system.unwrap();
        // Check that descriptions are included
        assert!(system[1].contains("Core agent memory"));
        assert!(system[2].contains("Working context"));
    }

    #[tokio::test]
    async fn test_builder_without_descriptions() {
        let memory = MockMemoryStore;
        let mut config = ContextConfig::default();
        config.include_descriptions = false;

        let builder = ContextBuilder::new(&memory, &config).for_agent("test-agent");

        let request = builder.build().await.unwrap();

        let system = request.system.unwrap();
        // Check that descriptions are NOT included
        assert!(!system[1].contains("Core agent memory"));
        assert!(!system[2].contains("Working context"));
    }

    #[tokio::test]
    async fn test_builder_with_model_info() {
        use crate::model::{ModelCapability, ModelInfo};

        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let model_info = ModelInfo {
            id: "gemini-pro".to_string(),
            name: "Gemini Pro".to_string(),
            provider: "gemini".to_string(),
            capabilities: vec![ModelCapability::TextGeneration],
            context_window: 128000,
            max_output_tokens: Some(8192),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };

        let builder = ContextBuilder::new(&memory, &config)
            .for_agent("test-agent")
            .with_model_info(&model_info);

        let request = builder.build().await.unwrap();

        // Should build successfully with model info
        assert!(request.system.is_some());
    }

    #[tokio::test]
    async fn test_gemini_message_validation() {
        use crate::model::ModelInfo;

        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let model_info = ModelInfo {
            id: "gemini-1.5-flash".to_string(),
            name: "Gemini 1.5 Flash".to_string(),
            provider: "Gemini".to_string(),
            capabilities: vec![],
            context_window: 128000,
            max_output_tokens: Some(8192),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };

        let builder = ContextBuilder::new(&memory, &config)
            .for_agent("test-agent")
            .with_model_info(&model_info);

        let mut request = builder.build().await.unwrap();

        // Add some test messages including an agent message first (not user)
        request.messages.insert(0, Message::agent("Hello"));
        request.messages.insert(1, Message::user("Hi"));

        // Apply Gemini adjustments directly
        let test_builder = ContextBuilder::new(&memory, &config);
        test_builder.adjust_for_gemini(&mut request.messages);

        // First message should now be user
        assert_eq!(request.messages[0].role, ChatRole::User);
        // Check the content is the conversation start message
        match &request.messages[0].content {
            MessageContent::Text(text) => assert_eq!(text, "[Conversation start]"),
            _ => panic!("Expected Text content"),
        }
    }

    #[test]
    fn test_is_empty_content() {
        // Test empty text
        assert!(ContextBuilder::is_empty_content(&MessageContent::Text(
            String::new()
        )));

        // Test non-empty text
        assert!(!ContextBuilder::is_empty_content(&MessageContent::Text(
            "Hello".to_string()
        )));

        // Test empty parts
        assert!(ContextBuilder::is_empty_content(&MessageContent::Parts(
            vec![]
        )));

        // Test empty blocks
        assert!(ContextBuilder::is_empty_content(&MessageContent::Blocks(
            vec![]
        )));

        // Test empty tool calls
        assert!(ContextBuilder::is_empty_content(
            &MessageContent::ToolCalls(vec![])
        ));

        // Test empty tool responses
        assert!(ContextBuilder::is_empty_content(
            &MessageContent::ToolResponses(vec![])
        ));
    }

    #[tokio::test]
    async fn test_builder_with_base_instructions() {
        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let base_instr = "You are a helpful assistant specialized in ADHD support.";

        let builder = ContextBuilder::new(&memory, &config)
            .for_agent("test-agent")
            .with_base_instructions(base_instr);

        let request = builder.build().await.unwrap();

        // Should have system prompt
        assert!(request.system.is_some());
        let system = request.system.unwrap();

        // Base instructions should be first element
        assert!(system.len() >= 1);
        assert_eq!(system[0], base_instr);

        // Should still have Core and Working blocks after base instructions
        assert!(system.len() >= 3); // base_instructions + core_memory + working_memory
    }

    #[tokio::test]
    async fn test_builder_with_tool_rules() {
        use crate::agent::tool_rules::ToolRule;

        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        // Create some test tool rules
        let rules = vec![
            ToolRule::start_constraint("context".to_string()),
            ToolRule::exit_loop("send_message".to_string()),
            ToolRule::max_calls("search".to_string(), 3),
        ];

        let builder = ContextBuilder::new(&memory, &config)
            .for_agent("test-agent")
            .with_tool_rules(rules);

        let request = builder.build().await.unwrap();

        // Should have system prompt
        assert!(request.system.is_some());
        let system = request.system.unwrap();

        // Tool rules should be last element
        let last_part = system.last().unwrap();
        assert!(last_part.contains("# Tool Execution Rules"));

        // Check that individual rules are present
        assert!(last_part.contains("Call `context` first before any other tools"));
        assert!(last_part.contains("The conversation will end after calling `send_message`"));
        assert!(last_part.contains("Call `search` at most 3 times"));
    }

    #[tokio::test]
    async fn test_builder_with_base_instructions_and_tool_rules() {
        use crate::agent::tool_rules::ToolRule;

        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let base_instr = "You are a test agent.";
        let rules = vec![ToolRule::continue_loop("fast_tool".to_string())];

        let builder = ContextBuilder::new(&memory, &config)
            .for_agent("test-agent")
            .with_base_instructions(base_instr)
            .with_tool_rules(rules);

        let request = builder.build().await.unwrap();

        let system = request.system.unwrap();

        // Verify order: base_instructions, Core blocks, Working blocks, tool_rules
        assert!(system.len() >= 4);

        // First should be base instructions
        assert_eq!(system[0], base_instr);

        // Last should be tool rules
        let last_part = system.last().unwrap();
        assert!(last_part.contains("# Tool Execution Rules"));
        assert!(last_part.contains("The conversation will be continued after calling `fast_tool`"));

        // Middle should have Core and Working blocks
        assert!(system[1].contains("<block:core_memory"));
        assert!(system[2].contains("<block:working_memory"));
    }

    #[tokio::test]
    async fn test_builder_without_base_instructions_or_tool_rules() {
        let memory = MockMemoryStore;
        let config = ContextConfig::default();

        let builder = ContextBuilder::new(&memory, &config).for_agent("test-agent");

        let request = builder.build().await.unwrap();

        let system = request.system.unwrap();

        // Should only have Core and Working blocks
        assert_eq!(system.len(), 3);
        assert!(system[1].contains("<block:core_memory"));
        assert!(system[2].contains("<block:working_memory"));

        // Should not have base instructions or tool rules
        assert!(!system.iter().any(|s| s.contains("# Tool Execution Rules")));
    }
}
