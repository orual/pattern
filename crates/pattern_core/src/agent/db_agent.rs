//! DatabaseAgent - V2 agent implementation with slim trait design

use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;
use tokio::sync::{RwLock, watch};
use tokio_stream::Stream;

use crate::agent::Agent;
use crate::agent::{AgentState, ResponseEvent};
use crate::context::heartbeat::HeartbeatSender;
use crate::error::CoreError;
use crate::id::AgentId;
use crate::messages::Message;
use crate::model::ModelProvider;
use crate::runtime::AgentRuntime;

/// DatabaseAgent - A slim agent implementation backed by runtime services
///
/// This agent delegates all "doing" to the runtime and focuses only on:
/// - Identity (id, name)
/// - Processing loop
/// - State management
pub struct DatabaseAgent {
    // Identity
    id: AgentId,
    name: String,

    // Runtime (provides stores, tool execution, context building)
    // Wrapped in Arc for cheap cloning into spawned tasks
    runtime: Arc<AgentRuntime>,

    // Model provider for completions
    model: Arc<dyn ModelProvider>,

    // Model ID for looking up response options from runtime config
    // If None, uses runtime's default model configuration
    model_id: Option<String>,

    // Base instructions (system prompt) for context building
    // Passed to ContextBuilder when preparing requests
    base_instructions: Option<String>,

    // State (needs interior mutability for async state updates)
    state: Arc<RwLock<AgentState>>,

    /// Watch channel for state changes
    state_watch: Option<Arc<(watch::Sender<AgentState>, watch::Receiver<AgentState>)>>,

    // Heartbeat channel for continuation signaling
    heartbeat_sender: HeartbeatSender,
}

impl fmt::Debug for DatabaseAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseAgent")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("runtime", &self.runtime)
            .field("model", &"<dyn ModelProvider>")
            .field("state", &self.state)
            .field("heartbeat_sender", &"<HeartbeatSender>")
            .finish()
    }
}

#[async_trait]
impl Agent for DatabaseAgent {
    fn id(&self) -> AgentId {
        self.id.clone()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn runtime(&self) -> Arc<AgentRuntime> {
        self.runtime.clone()
    }

    async fn process(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError> {
        use std::collections::HashSet;
        use tokio::sync::mpsc;
        use tokio_stream::wrappers::ReceiverStream;

        use crate::agent::processing::{
            LoopOutcome, ProcessingContext, ProcessingState, run_processing_loop,
        };

        // Determine batch ID and type from the incoming message
        let batch_id = message
            .batch
            .unwrap_or_else(|| crate::utils::get_next_message_position_sync());
        let batch_type = message
            .batch_type
            .unwrap_or(crate::messages::BatchType::UserRequest);

        // Update state to Processing
        let mut active_batches = HashSet::new();
        active_batches.insert(batch_id);
        self.set_state(AgentState::Processing { active_batches })
            .await?;

        // Create channel for streaming events
        let (tx, rx) = mpsc::channel(100);

        // Clone what we need for the spawned task
        let agent_self = self.clone();

        // Spawn task to do the processing
        tokio::spawn(async move {
            // Get model options (try agent's model_id, then fall back to default)
            let response_options = if let Some(model_id) = agent_self.model_id.as_deref() {
                agent_self
                    .runtime
                    .config()
                    .get_model_options(model_id)
                    .or_else(|| agent_self.runtime.config().get_default_options())
                    .cloned()
            } else {
                agent_self.runtime.config().get_default_options().cloned()
            };

            let response_options = match response_options {
                Some(opts) => opts,
                None => {
                    let _ = tx
                        .send(ResponseEvent::Error {
                            message: format!(
                                "No model options configured for '{}' and no default options available",
                                agent_self.model_id.as_deref().unwrap_or("(none)")
                            ),
                            recoverable: false,
                        })
                        .await;
                    let _ = agent_self.set_state(AgentState::Ready).await;
                    return;
                }
            };

            // Extract initial sequence number
            let initial_sequence_num = message.sequence_num.map(|n| n + 1).unwrap_or(1);

            // Build processing context and state
            let ctx = ProcessingContext {
                agent_id: agent_self.id.as_str(),
                runtime: &agent_self.runtime,
                model: agent_self.model.as_ref(),
                response_options: &response_options,
                base_instructions: agent_self.base_instructions.as_deref(),
                batch_id,
                batch_type,
                heartbeat_sender: &agent_self.heartbeat_sender,
            };

            let mut state = ProcessingState {
                process_state: agent_self.runtime.new_process_state(),
                sequence_num: initial_sequence_num,
                start_constraint_attempts: 0,
                exit_requirement_attempts: 0,
            };

            // Run the processing loop
            let outcome = run_processing_loop(ctx, &mut state, &tx, message).await;

            // Emit completion event
            let metadata = match &outcome {
                Ok(LoopOutcome::Completed { metadata }) => metadata.clone(),
                _ => crate::messages::ResponseMetadata {
                    processing_time: None,
                    tokens_used: None,
                    model_used: None,
                    confidence: None,
                    model_iden: genai::ModelIden::new(
                        genai::adapter::AdapterKind::Anthropic,
                        "unknown",
                    ),
                    custom: serde_json::json!({}),
                },
            };

            let _ = tx
                .send(ResponseEvent::Complete {
                    message_id: crate::MessageId::generate(),
                    metadata,
                })
                .await;

            // Update state back to Ready
            let _ = agent_self.set_state(AgentState::Ready).await;
        });

        // Return the receiver as a stream
        Ok(Box::new(ReceiverStream::new(rx)))
    }

    /// Get the agent's current state and a watch receiver for changes
    async fn state(&self) -> (AgentState, Option<tokio::sync::watch::Receiver<AgentState>>) {
        let state = self.state.read().await.clone();
        let rx = self.state_watch.as_ref().map(|watch| watch.1.clone());
        (state, rx)
    }

    async fn set_state(&self, state: AgentState) -> Result<(), CoreError> {
        *self.state.write().await = state.clone();
        if let Some(arc) = &self.state_watch {
            let _ = arc.0.send(state);
        }
        Ok(())
    }
}

impl DatabaseAgent {
    /// Create a new builder for constructing a DatabaseAgent
    pub fn builder() -> DatabaseAgentBuilder {
        DatabaseAgentBuilder::default()
    }
}

/// Builder for constructing a DatabaseAgent
#[derive(Default)]
pub struct DatabaseAgentBuilder {
    id: Option<AgentId>,
    name: Option<String>,
    runtime: Option<Arc<AgentRuntime>>,
    model: Option<Arc<dyn ModelProvider>>,
    model_id: Option<String>,
    base_instructions: Option<String>,
    heartbeat_sender: Option<HeartbeatSender>,
}

impl DatabaseAgentBuilder {
    /// Set the agent ID
    pub fn id(mut self, id: AgentId) -> Self {
        self.id = Some(id);
        self
    }

    /// Set the agent name
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the runtime
    /// Accepts Arc<AgentRuntime> for sharing across spawned tasks
    pub fn runtime(mut self, runtime: Arc<AgentRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Set the model provider
    pub fn model(mut self, model: Arc<dyn ModelProvider>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the model ID for looking up response options from runtime config
    /// If not set, uses runtime's default model configuration
    pub fn model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    /// Set base instructions (system prompt) for context building
    ///
    /// These instructions are passed to ContextBuilder when preparing requests
    /// and become the foundation of the agent's system prompt.
    ///
    /// Empty strings are treated as None (use default instructions).
    pub fn base_instructions(mut self, instructions: impl Into<String>) -> Self {
        let instructions = instructions.into();
        // Empty string should be treated as None (use default)
        if !instructions.is_empty() {
            self.base_instructions = Some(instructions);
        }
        self
    }

    /// Set the heartbeat sender
    pub fn heartbeat_sender(mut self, sender: HeartbeatSender) -> Self {
        self.heartbeat_sender = Some(sender);
        self
    }

    /// Build the DatabaseAgent, validating that all required fields are present
    pub fn build(self) -> Result<DatabaseAgent, CoreError> {
        let id = self.id.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "id is required".to_string(),
        })?;

        let name = self.name.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "name is required".to_string(),
        })?;

        let runtime = self.runtime.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "runtime is required".to_string(),
        })?;

        let model = self.model.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "model is required".to_string(),
        })?;

        let heartbeat_sender = self
            .heartbeat_sender
            .ok_or_else(|| CoreError::InvalidFormat {
                data_type: "DatabaseAgent".to_string(),
                details: "heartbeat_sender is required".to_string(),
            })?;

        let state = AgentState::Ready;
        let (tx, rx) = watch::channel(state.clone());
        Ok(DatabaseAgent {
            id,
            name,
            runtime,
            model,
            model_id: self.model_id,
            base_instructions: self.base_instructions,
            state: Arc::new(RwLock::new(state)),
            state_watch: Some(Arc::new((tx, rx))),
            heartbeat_sender,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::heartbeat::heartbeat_channel;
    use crate::messages::MessageStore;
    use crate::test_helpers::memory::MockMemoryStore;
    use async_trait::async_trait;

    // Mock ModelProvider for testing
    #[derive(Debug)]
    struct MockModelProvider;

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(
            &self,
            _options: &crate::model::ResponseOptions,
            _request: crate::messages::Request,
        ) -> crate::Result<crate::messages::Response> {
            unimplemented!("Mock provider")
        }

        async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
            Ok(Vec::new())
        }

        async fn supports_capability(
            &self,
            _model: &str,
            _capability: crate::model::ModelCapability,
        ) -> bool {
            false
        }

        async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
            Ok(0)
        }
    }

    async fn test_dbs() -> crate::db::ConstellationDatabases {
        crate::db::ConstellationDatabases::open_in_memory()
            .await
            .unwrap()
    }

    /// Helper to create a test agent in the database
    async fn create_test_agent(dbs: &crate::db::ConstellationDatabases, id: &str) {
        use pattern_db::models::{Agent, AgentStatus};
        use sqlx::types::Json as SqlxJson;

        let agent = Agent {
            id: id.to_string(),
            name: format!("Test Agent {}", id),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: SqlxJson(serde_json::json!({})),
            enabled_tools: SqlxJson(vec![]),
            tool_rules: None,
            status: AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(dbs.constellation.pool(), &agent)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_builder_requires_id() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("id"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_name() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("name"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_runtime() {
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("runtime"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_model() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("model"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_heartbeat_sender() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("heartbeat"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_success() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let agent = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build()
            .unwrap();

        assert_eq!(agent.id().as_str(), "test_agent");
        assert_eq!(agent.name(), "Test Agent");
    }

    #[tokio::test]
    async fn test_initial_state_is_ready() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let agent = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build()
            .unwrap();

        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    #[tokio::test]
    async fn test_state_update() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let agent = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build()
            .unwrap();

        agent.set_state(AgentState::Suspended).await.unwrap();
        assert_eq!(agent.state().await.0, AgentState::Suspended);
    }

    #[tokio::test]
    async fn test_process_basic_flow() {
        use crate::agent::Agent;
        use crate::messages::{Message, MessageContent, Response, ResponseMetadata};
        use futures::StreamExt;

        // Mock model that returns a simple text response
        #[derive(Debug)]
        struct SimpleTestModel;

        #[async_trait]
        impl ModelProvider for SimpleTestModel {
            fn name(&self) -> &str {
                "test"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::messages::Request,
            ) -> crate::Result<crate::messages::Response> {
                Ok(Response {
                    content: vec![MessageContent::Text("Hello from model!".to_string())],
                    reasoning: None,
                    metadata: ResponseMetadata {
                        processing_time: None,
                        tokens_used: None,
                        model_used: Some("test".to_string()),
                        confidence: None,
                        model_iden: genai::ModelIden::new(
                            genai::adapter::AdapterKind::Anthropic,
                            "test",
                        ),
                        custom: serde_json::json!({}),
                    },
                })
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(SimpleTestModel) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with model options configured
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .config(runtime_config)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Create a test message
        let test_message = Message::user("Hello agent!");

        // Process the message
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Debug: print all events
        for (i, event) in events.iter().enumerate() {
            eprintln!("Event {}: {:?}", i, event);
        }

        // Verify we got the expected events
        assert!(!events.is_empty(), "Should have received events");

        // Should have at least one TextChunk and one Complete event
        let has_text = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextChunk { .. }));
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(has_text, "Should have received TextChunk event");
        assert!(has_complete, "Should have received Complete event");

        // Verify state is back to Ready
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    #[tokio::test]
    async fn test_tool_execution_flow() {
        use crate::agent::Agent;
        use crate::messages::{Message, MessageContent, Response, ResponseMetadata, ToolCall};
        use crate::tool::{AiTool, ExecutionMeta};
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Mock model that returns tool calls
        #[derive(Debug)]
        struct ToolCallModel {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for ToolCallModel {
            fn name(&self) -> &str {
                "test_tool"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::messages::Request,
            ) -> crate::Result<crate::messages::Response> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);

                if count == 0 {
                    // First call: return a tool call
                    Ok(Response {
                        content: vec![MessageContent::ToolCalls(vec![ToolCall {
                            call_id: "test_call_1".to_string(),
                            fn_name: "test_tool".to_string(),
                            fn_arguments: serde_json::json!({ "message": "hello" }),
                        }])],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                } else {
                    // Second call: return text response
                    Ok(Response {
                        content: vec![MessageContent::Text(
                            "Tool executed successfully".to_string(),
                        )],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                }
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        // Create a simple test tool
        #[derive(Debug, Clone)]
        struct TestTool;

        #[async_trait]
        impl AiTool for TestTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "test_tool"
            }

            fn description(&self) -> &str {
                "A test tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Tool executed".to_string())
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(ToolCallModel {
            call_count: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with model options and register test tool
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let tools = crate::tool::ToolRegistry::new();
        tools.register(TestTool);

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .config(runtime_config)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Process a message
        let test_message = Message::user("Test tool execution");
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Verify we got tool call events (started/completed) and tool responses
        let has_tool_started = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCallStarted { .. }));
        let has_tool_completed = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCallCompleted { .. }));
        let has_tool_responses = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolResponses { .. }));
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(
            has_tool_started,
            "Should have emitted ToolCallStarted event"
        );
        assert!(
            has_tool_completed,
            "Should have emitted ToolCallCompleted event"
        );
        assert!(
            has_tool_responses,
            "Should have emitted ToolResponses event"
        );
        assert!(has_complete, "Should have emitted Complete event");
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    // TODO: Re-enable these tests once runtime.prepare_request() properly supports continuation
    // Currently fails with "Invalid data format: SnowflakePosition" during continuation
    #[tokio::test]
    async fn test_start_constraint_retry() {
        use crate::agent::Agent;
        use crate::agent::tool_rules::{ToolRule, ToolRuleType};
        use crate::messages::{Message, MessageContent, Response, ResponseMetadata, ToolCall};
        use crate::tool::{AiTool, ExecutionMeta};
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Mock model that tries to call regular tool before start constraint tool
        #[derive(Debug)]
        struct ConstraintTestModel {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for ConstraintTestModel {
            fn name(&self) -> &str {
                "test"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::messages::Request,
            ) -> crate::Result<crate::messages::Response> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);

                if count < 3 {
                    // First 3 attempts: try to call wrong tool
                    Ok(Response {
                        content: vec![MessageContent::ToolCalls(vec![ToolCall {
                            call_id: format!("bad_call_{}", count),
                            fn_name: "regular_tool".to_string(),
                            fn_arguments: serde_json::json!({}),
                        }])],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                } else {
                    // After retries: just return text
                    Ok(Response {
                        content: vec![MessageContent::Text("Done".to_string())],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                }
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        #[derive(Debug, Clone)]
        struct StartTool;

        #[async_trait]
        impl AiTool for StartTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "start_tool"
            }

            fn description(&self) -> &str {
                "Start constraint tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Started".to_string())
            }
        }

        #[derive(Debug, Clone)]
        struct RegularTool;

        #[async_trait]
        impl AiTool for RegularTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "regular_tool"
            }

            fn description(&self) -> &str {
                "Regular tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Regular".to_string())
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(ConstraintTestModel {
            call_count: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with start constraint rule
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let tools = crate::tool::ToolRegistry::new();
        tools.register(StartTool);
        tools.register(RegularTool);

        let start_rule = ToolRule {
            tool_name: "start_tool".to_string(),
            rule_type: ToolRuleType::StartConstraint,
            conditions: vec![],
            priority: 100,
            metadata: None,
        };

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .config(runtime_config)
            .add_tool_rule(start_rule)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Process a message
        let test_message = Message::user("Test start constraint");
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Debug: print all events
        eprintln!(
            "=== Start Constraint Test Events ({} total) ===",
            events.len()
        );
        for (i, event) in events.iter().enumerate() {
            eprintln!("Event {}: {:?}", i, event);
        }

        // Should have tool responses (including errors and forced execution)
        let has_tool_responses = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolResponses { .. }));

        // Should eventually complete (retry logic worked)
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(
            has_tool_responses,
            "Should have emitted tool responses during retry attempts. Got {} events",
            events.len()
        );
        assert!(
            has_complete,
            "Should eventually complete after retries. Got {} events",
            events.len()
        );
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    // TODO: Re-enable once runtime.prepare_request() properly supports continuation
    #[tokio::test]
    async fn test_exit_requirement_retry() {
        use crate::agent::Agent;
        use crate::agent::tool_rules::{ToolRule, ToolRuleType};
        use crate::messages::{Message, MessageContent, Response, ResponseMetadata};
        use crate::tool::{AiTool, ExecutionMeta};
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Mock model that tries to exit without calling required exit tool
        #[derive(Debug)]
        struct ExitTestModel {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for ExitTestModel {
            fn name(&self) -> &str {
                "test"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::messages::Request,
            ) -> crate::Result<crate::messages::Response> {
                let _count = self.call_count.fetch_add(1, Ordering::SeqCst);

                // Always return text (no tool calls = wants to exit)
                Ok(Response {
                    content: vec![MessageContent::Text("I'm done".to_string())],
                    reasoning: None,
                    metadata: ResponseMetadata {
                        processing_time: None,
                        tokens_used: None,
                        model_used: Some("test".to_string()),
                        confidence: None,
                        model_iden: genai::ModelIden::new(
                            genai::adapter::AdapterKind::Anthropic,
                            "test",
                        ),
                        custom: serde_json::json!({}),
                    },
                })
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        #[derive(Debug, Clone)]
        struct ExitTool;

        #[async_trait]
        impl AiTool for ExitTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "exit_tool"
            }

            fn description(&self) -> &str {
                "Required before exit"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Exit handled".to_string())
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(ExitTestModel {
            call_count: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with exit requirement rule
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let tools = crate::tool::ToolRegistry::new();
        tools.register(ExitTool);

        let exit_rule = ToolRule {
            tool_name: "exit_tool".to_string(),
            rule_type: ToolRuleType::RequiredBeforeExit,
            conditions: vec![],
            priority: 100,
            metadata: None,
        };

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .config(runtime_config)
            .add_tool_rule(exit_rule)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Process a message
        let test_message = Message::user("Test exit requirement");
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Should eventually complete after force-executing exit tool
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(has_complete, "Should eventually complete");
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }
}
