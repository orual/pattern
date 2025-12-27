//! Shared test utilities for coordination pattern tests

#[cfg(test)]
pub(crate) mod test {
    use std::sync::Arc;

    use chrono::Utc;
    use tokio_stream::Stream;

    use crate::{
        AgentId, UserId,
        agent::{Agent, AgentState, ResponseEvent},
        coordination::groups::GroupResponseEvent,
        error::CoreError,
        message::{ChatRole, Message, MessageContent, MessageMetadata, MessageOptions, Response},
        runtime::{AgentRuntime, test_support::test_runtime},
    };

    /// Test agent implementation for coordination pattern tests
    ///
    /// Uses the new slim Agent trait with a real AgentRuntime
    pub struct TestAgent {
        pub id: AgentId,
        pub name: String,
        runtime: AgentRuntime,
        state: std::sync::RwLock<AgentState>,
    }

    impl std::fmt::Debug for TestAgent {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("TestAgent")
                .field("id", &self.id)
                .field("name", &self.name)
                .finish()
        }
    }

    impl AsRef<TestAgent> for TestAgent {
        fn as_ref(&self) -> &TestAgent {
            self
        }
    }

    #[async_trait::async_trait]
    impl Agent for TestAgent {
        fn id(&self) -> AgentId {
            self.id.clone()
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn runtime(&self) -> &AgentRuntime {
            &self.runtime
        }

        async fn process(
            self: Arc<Self>,
            message: Message,
        ) -> std::result::Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError>
        {
            use crate::message::ResponseMetadata;

            // Create a simple stream that emits a complete response
            let events = vec![
                ResponseEvent::TextChunk {
                    text: format!("{} test response", self.name),
                    is_final: true,
                },
                ResponseEvent::Complete {
                    message_id: message.id,
                    metadata: ResponseMetadata::default(),
                },
            ];

            Ok(Box::new(tokio_stream::iter(events)))
        }

        async fn state(&self) -> (AgentState, Option<tokio::sync::watch::Receiver<AgentState>>) {
            let state = self.state.read().unwrap().clone();
            (state, None)
        }

        async fn set_state(&self, state: AgentState) -> std::result::Result<(), CoreError> {
            *self.state.write().unwrap() = state;
            Ok(())
        }
    }

    /// Create a test agent with the given name
    pub async fn create_test_agent(name: &str) -> TestAgent {
        let id = AgentId::generate();
        let runtime = test_runtime(&id.to_string()).await;

        TestAgent {
            id,
            name: name.to_string(),
            runtime,
            state: std::sync::RwLock::new(AgentState::Ready),
        }
    }

    /// Create a test message with the given content
    pub fn create_test_message(content: &str) -> Message {
        Message {
            id: crate::id::MessageId::generate(),
            role: ChatRole::User,
            owner_id: Some(UserId::generate()),
            content: MessageContent::Text(content.to_string()),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: content.split_whitespace().count() as u32,
            created_at: Utc::now(),
            position: None,
            batch: None,
            sequence_num: None,
            batch_type: None,
        }
    }

    /// Helper to collect events from stream and extract Complete event
    pub async fn collect_complete_event(
        mut stream: Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>,
    ) -> (
        Vec<crate::coordination::groups::AgentResponse>,
        Option<crate::coordination::types::GroupState>,
    ) {
        use tokio_stream::StreamExt;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }

        events
            .iter()
            .find_map(|event| {
                if let GroupResponseEvent::Complete {
                    agent_responses,
                    state_changes,
                    ..
                } = event
                {
                    Some((agent_responses.clone(), state_changes.clone()))
                } else {
                    None
                }
            })
            .expect("Should have Complete event")
    }

    /// Helper to collect all agent responses from a streaming round-robin implementation
    pub async fn collect_agent_responses(
        mut stream: Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>,
    ) -> Vec<crate::coordination::groups::AgentResponse> {
        use crate::message::ResponseMetadata;
        use tokio_stream::StreamExt;

        let mut responses = Vec::new();
        let mut current_agent_id = None;
        let mut text_chunks = Vec::new();

        while let Some(event) = stream.next().await {
            match event {
                GroupResponseEvent::AgentStarted { agent_id, .. } => {
                    current_agent_id = Some(agent_id);
                    text_chunks.clear();
                }
                GroupResponseEvent::TextChunk { text, .. } => {
                    text_chunks.push(text);
                }
                GroupResponseEvent::AgentCompleted { agent_id, .. } => {
                    if let Some(ref current_id) = current_agent_id {
                        if *current_id == agent_id {
                            // Create a response from the collected chunks
                            let content = if text_chunks.is_empty() {
                                vec![MessageContent::Text("Test response".to_string())]
                            } else {
                                vec![MessageContent::Text(text_chunks.join(""))]
                            };

                            responses.push(crate::coordination::groups::AgentResponse {
                                agent_id,
                                response: Response {
                                    content,
                                    reasoning: None,
                                    metadata: ResponseMetadata::default(),
                                },
                                responded_at: Utc::now(),
                            });

                            current_agent_id = None;
                            text_chunks.clear();
                        }
                    }
                }
                _ => {}
            }
        }

        responses
    }
}
