//! Dynamic coordination pattern implementation

use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;

use crate::{
    Result,
    agent::Agent,
    coordination::{
        groups::{
            AgentResponse, AgentWithMembership, GroupManager, GroupResponse, GroupResponseEvent,
        },
        types::{CoordinationPattern, GroupState, SelectionContext},
    },
    message::Message,
};

#[derive(Clone)]
pub struct DynamicManager {
    selectors: Arc<dyn crate::coordination::selectors::SelectorRegistry>,
}

impl DynamicManager {
    pub fn new(selectors: Arc<dyn crate::coordination::selectors::SelectorRegistry>) -> Self {
        Self { selectors }
    }
}

#[async_trait]
impl GroupManager for DynamicManager {
    async fn route_message(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>> {
        use tokio_stream::wrappers::ReceiverStream;

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let start_time = std::time::Instant::now();

        // Clone data for the spawned task
        let group_id = group.id.clone();
        let _group_name = group.name.clone();
        let coordination_pattern = group.coordination_pattern.clone();
        let agents = agents.to_vec();
        let selectors = self.selectors.clone();
        let group_state = group.state.clone();

        // Spawn task to handle the routing
        tokio::spawn(async move {
            // Extract dynamic config
            let (selector_name, selector_config) = match &coordination_pattern {
                CoordinationPattern::Dynamic {
                    selector_name,
                    selector_config,
                } => (selector_name.clone(), selector_config.clone()),
                _ => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: format!("Invalid pattern for DynamicManager"),
                            recoverable: false,
                        })
                        .await;
                    return;
                }
            };

            // Get recent selections from state
            let recent_selections = match &group_state {
                GroupState::Dynamic { recent_selections } => recent_selections.clone(),
                _ => Vec::new(),
            };

            // Get the selector
            let selector = match selectors.get(&selector_name) {
                Some(s) => s,
                None => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: format!("Selector '{}' not found", selector_name),
                            recoverable: false,
                        })
                        .await;
                    return;
                }
            };

            // Check if message directly addresses an agent by name
            let message_text = match &message.content {
                crate::message::MessageContent::Text(text) => Some(text.as_str()),
                crate::message::MessageContent::Parts(parts) => {
                    parts.iter().find_map(|p| match p {
                        crate::message::ContentPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                }
                _ => None,
            };

            // Check for @all broadcast
            let is_broadcast_to_all = if let Some(text) = message_text {
                let lower_text = text.to_lowercase();
                lower_text.contains("@all")
            } else {
                false
            };

            // Check for direct agent addressing (e.g., "entropy, ..." or "@entropy" or "hey entropy")
            let directly_addressed_agent = if let Some(text) = message_text {
                let lower_text = text.to_lowercase();
                agents.iter().find(|awm| {
                    let agent_name = awm.agent.name().to_lowercase();
                    // Check various addressing patterns
                    lower_text.starts_with(&format!("{},", agent_name))
                        || lower_text.contains(&format!("@{} ", agent_name))
                        || lower_text.contains(&format!("@{},", agent_name))
                        || lower_text.starts_with(&format!("{}:", agent_name))
                        || lower_text.starts_with(&format!("{} -", agent_name))
                        || lower_text.starts_with(&format!("hey {}", agent_name))
                        || lower_text.starts_with(&format!("{} ", agent_name))
                })
            } else {
                None
            };

            // Build selection context
            let available_agents = agents
                .iter()
                .filter(|awm| awm.membership.is_active)
                .map(|awm| (awm.agent.as_ref().id(), crate::AgentState::Ready))
                .collect();

            let agent_capabilities = agents
                .iter()
                .filter(|awm| awm.membership.is_active)
                .map(|awm| (awm.agent.as_ref().id(), awm.membership.capabilities.clone()))
                .collect();

            let context = SelectionContext {
                message: message.clone(),
                recent_selections: recent_selections.clone(),
                available_agents,
                agent_capabilities,
            };

            // Log addressing detection
            if is_broadcast_to_all {
                tracing::info!("@all broadcast detected - will route to all active agents");
            } else if let Some(addressed) = directly_addressed_agent {
                tracing::info!(
                    "Direct addressing detected for agent: {}",
                    addressed.agent.name()
                );
            }

            // Use the actual selector to select agents, unless directly addressed or broadcasting to all
            let (selected_agents, selector_response) = if is_broadcast_to_all {
                // Broadcast to all active agents
                tracing::info!("Broadcasting to all active agents due to @all addressing");
                let all_active_agents = agents
                    .iter()
                    .filter(|awm| awm.membership.is_active)
                    .collect::<Vec<_>>();
                (all_active_agents, None)
            } else if let Some(addressed_agent) = directly_addressed_agent {
                // Bypass selector for directly addressed agents
                tracing::info!("Bypassing selector due to direct addressing");
                (vec![addressed_agent], None)
            } else {
                tracing::info!("Using {} selector for agent selection", selector_name);
                match selector
                    .select_agents(&agents, &context, &selector_config)
                    .await
                {
                    Ok(result) => (result.agents, result.selector_response),
                    Err(e) => {
                        let _ = tx
                            .send(GroupResponseEvent::Error {
                                agent_id: None,
                                message: e.to_string(),
                                recoverable: false,
                            })
                            .await;
                        return;
                    }
                }
            };

            if selected_agents.is_empty() {
                let _ = tx
                    .send(GroupResponseEvent::Error {
                        agent_id: None,
                        message: format!("No agents selected by dynamic selector"),
                        recoverable: false,
                    })
                    .await;
                return;
            }

            // Send start event
            let _ = tx
                .send(GroupResponseEvent::Started {
                    group_id: group_id.clone(),
                    pattern: format!("dynamic:{}", selector_name),
                    agent_count: selected_agents.len(),
                })
                .await;

            // If supervisor provided a direct response stream, handle it
            if let Some(mut supervisor_stream) = selector_response {
                // Find which agent is the supervisor (should be the only selected agent)
                if selected_agents.len() == 1 {
                    let supervisor_awm = selected_agents[0];
                    let supervisor_id = supervisor_awm.agent.as_ref().id();
                    let supervisor_name = supervisor_awm.agent.name();

                    // Send supervisor's response as if it came from their normal processing
                    let _ = tx
                        .send(GroupResponseEvent::AgentStarted {
                            agent_id: supervisor_id.clone(),
                            agent_name: supervisor_name.to_string(),
                            role: supervisor_awm.membership.role.clone(),
                        })
                        .await;

                    // Forward the stream events
                    use tokio_stream::StreamExt;
                    let mut message_id = None;

                    while let Some(event) = supervisor_stream.next().await {
                        match event {
                            crate::agent::ResponseEvent::TextChunk { text, is_final } => {
                                let _ = tx
                                    .send(GroupResponseEvent::TextChunk {
                                        agent_id: supervisor_id.clone(),
                                        text,
                                        is_final,
                                    })
                                    .await;
                            }
                            crate::agent::ResponseEvent::ReasoningChunk { text, is_final } => {
                                let _ = tx
                                    .send(GroupResponseEvent::ReasoningChunk {
                                        agent_id: supervisor_id.clone(),
                                        text,
                                        is_final,
                                    })
                                    .await;
                            }
                            crate::agent::ResponseEvent::ToolCallStarted {
                                call_id,
                                fn_name,
                                args,
                            } => {
                                tracing::debug!(
                                    "Dynamic: Forwarding ToolCallStarted {} from supervisor {}",
                                    fn_name,
                                    supervisor_name
                                );
                                let _ = tx
                                    .send(GroupResponseEvent::ToolCallStarted {
                                        agent_id: supervisor_id.clone(),
                                        call_id,
                                        fn_name,
                                        args,
                                    })
                                    .await;
                            }
                            crate::agent::ResponseEvent::ToolCallCompleted { call_id, result } => {
                                let _ = tx
                                    .send(GroupResponseEvent::ToolCallCompleted {
                                        agent_id: supervisor_id.clone(),
                                        call_id,
                                        result: result.map_err(|e| e.to_string()),
                                    })
                                    .await;
                            }
                            crate::agent::ResponseEvent::Complete {
                                message_id: msg_id, ..
                            } => {
                                message_id = Some(msg_id);
                            }
                            crate::agent::ResponseEvent::Error {
                                message,
                                recoverable,
                            } => {
                                let _ = tx
                                    .send(GroupResponseEvent::Error {
                                        agent_id: Some(supervisor_id.clone()),
                                        message,
                                        recoverable,
                                    })
                                    .await;
                            }
                            _ => {} // Skip other events
                        }
                    }

                    // Send completion
                    let _ = tx
                        .send(GroupResponseEvent::AgentCompleted {
                            agent_id: supervisor_id.clone(),
                            agent_name: supervisor_name.to_string(),
                            message_id,
                        })
                        .await;

                    // Track the response
                    let agent_responses = vec![AgentResponse {
                        agent_id: supervisor_id.clone(),
                        response: crate::message::Response {
                            content: vec![], // TODO: We'd need to collect content from the stream
                            reasoning: None,
                            metadata: crate::message::ResponseMetadata::default(),
                        },
                        responded_at: Utc::now(),
                    }];

                    // Update recent selections
                    let mut new_recent_selections = recent_selections.clone();
                    new_recent_selections.push((Utc::now(), supervisor_id));

                    // Clean up old selections
                    let one_hour_ago = Utc::now() - chrono::Duration::hours(1);
                    new_recent_selections.retain(|(timestamp, _)| *timestamp > one_hour_ago);
                    if new_recent_selections.len() > 100 {
                        new_recent_selections = new_recent_selections
                            .into_iter()
                            .rev()
                            .take(100)
                            .rev()
                            .collect();
                    }

                    let new_state = GroupState::Dynamic {
                        recent_selections: new_recent_selections,
                    };

                    // Send completion event and return early
                    let _ = tx
                        .send(GroupResponseEvent::Complete {
                            group_id,
                            pattern: format!("dynamic:{}", selector_name),
                            execution_time: start_time.elapsed(),
                            agent_responses,
                            state_changes: Some(new_state),
                        })
                        .await;

                    return; // Don't process the supervisor again
                }
            }

            // Process each selected agent in parallel
            let (response_tx, mut response_rx) = tokio::sync::mpsc::channel(selected_agents.len());
            let agent_count = selected_agents.len();

            for awm in selected_agents {
                let agent_id = awm.agent.as_ref().id();
                let agent_name = awm.agent.name().to_string();
                let tx = tx.clone();
                let message = message.clone();
                let agent = awm.agent.clone();
                let role = awm.membership.role.clone();
                let response_tx = response_tx.clone();

                // Spawn a task for each agent to process in parallel
                tokio::spawn(async move {
                    // Send agent started event
                    let _ = tx
                        .send(GroupResponseEvent::AgentStarted {
                            agent_id: agent_id.clone(),
                            agent_name: agent_name.clone(),
                            role,
                        })
                        .await;

                    // Process message with streaming
                    match agent.process(message).await {
                        Ok(mut stream) => {
                            use tokio_stream::StreamExt;

                            while let Some(event) = stream.next().await {
                                // Convert ResponseEvent to GroupResponseEvent
                                match event {
                                    crate::agent::ResponseEvent::TextChunk { text, is_final } => {
                                        let _ = tx
                                            .send(GroupResponseEvent::TextChunk {
                                                agent_id: agent_id.clone(),
                                                text,
                                                is_final,
                                            })
                                            .await;
                                    }
                                    crate::agent::ResponseEvent::ReasoningChunk {
                                        text,
                                        is_final,
                                    } => {
                                        let _ = tx
                                            .send(GroupResponseEvent::ReasoningChunk {
                                                agent_id: agent_id.clone(),
                                                text,
                                                is_final,
                                            })
                                            .await;
                                    }
                                    crate::agent::ResponseEvent::ToolCallStarted {
                                        call_id,
                                        fn_name,
                                        args,
                                    } => {
                                        tracing::debug!(
                                            "Dynamic: Forwarding ToolCallStarted {} from agent {}",
                                            fn_name,
                                            agent_name
                                        );
                                        let _ = tx
                                            .send(GroupResponseEvent::ToolCallStarted {
                                                agent_id: agent_id.clone(),
                                                call_id,
                                                fn_name,
                                                args,
                                            })
                                            .await;
                                    }
                                    crate::agent::ResponseEvent::ToolCallCompleted {
                                        call_id,
                                        result,
                                    } => {
                                        let _ = tx
                                            .send(GroupResponseEvent::ToolCallCompleted {
                                                agent_id: agent_id.clone(),
                                                call_id,
                                                result: result.map_err(|e| e.to_string()),
                                            })
                                            .await;
                                    }
                                    crate::agent::ResponseEvent::Complete {
                                        message_id, ..
                                    } => {
                                        let _ = tx
                                            .send(GroupResponseEvent::AgentCompleted {
                                                agent_id: agent_id.clone(),
                                                agent_name: agent_name.clone(),
                                                message_id: Some(message_id),
                                            })
                                            .await;
                                    }
                                    crate::agent::ResponseEvent::Error {
                                        message,
                                        recoverable,
                                    } => {
                                        let _ = tx
                                            .send(GroupResponseEvent::Error {
                                                agent_id: Some(agent_id.clone()),
                                                message,
                                                recoverable,
                                            })
                                            .await;
                                    }
                                    _ => {} // Skip other events
                                }
                            }

                            // Send response for final summary
                            let _ = response_tx
                                .send(AgentResponse {
                                    agent_id: agent_id.clone(),
                                    response: crate::message::Response {
                                        content: vec![], // TODO: Collect actual response content
                                        reasoning: None,
                                        metadata: crate::message::ResponseMetadata::default(),
                                    },
                                    responded_at: Utc::now(),
                                })
                                .await;
                        }
                        Err(e) => {
                            let _ = tx
                                .send(GroupResponseEvent::Error {
                                    agent_id: Some(agent_id.clone()),
                                    message: e.to_string(),
                                    recoverable: false,
                                })
                                .await;
                        }
                    }
                });
            }

            // Drop the original sender so only agent tasks hold references
            drop(response_tx);

            // Spawn a monitor task to handle completion
            tokio::spawn(async move {
                // Collect all responses from the channel
                let mut agent_responses = Vec::new();
                let mut completed_count = 0;

                while let Some(response) = response_rx.recv().await {
                    agent_responses.push(response.clone());
                    completed_count += 1;

                    // If all agents have completed, we can finish
                    if completed_count >= agent_count {
                        break;
                    }
                }

                // Update recent selections based on responses
                let mut new_recent_selections = recent_selections.clone();
                for response in &agent_responses {
                    new_recent_selections.push((Utc::now(), response.agent_id.clone()));
                }

                // Keep only recent selections (last 100 or from last hour)
                let one_hour_ago = Utc::now() - chrono::Duration::hours(1);
                new_recent_selections.retain(|(timestamp, _)| *timestamp > one_hour_ago);
                if new_recent_selections.len() > 100 {
                    new_recent_selections = new_recent_selections
                        .into_iter()
                        .rev()
                        .take(100)
                        .rev()
                        .collect();
                }

                let new_state = GroupState::Dynamic {
                    recent_selections: new_recent_selections,
                };

                // Send completion event
                let _ = tx
                    .send(GroupResponseEvent::Complete {
                        group_id,
                        pattern: format!("dynamic:{}", selector_name),
                        execution_time: start_time.elapsed(),
                        agent_responses,
                        state_changes: Some(new_state),
                    })
                    .await;
            });
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }

    async fn update_state(
        &self,
        _current_state: &GroupState,
        response: &GroupResponse,
    ) -> Result<Option<GroupState>> {
        // State is already updated in route_message for dynamic
        Ok(response.state_changes.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coordination::{
            AgentGroup, AgentSelector,
            groups::{AgentWithMembership, GroupMembership},
            selectors::SelectorRegistry,
            test_utils::test::{create_test_agent, create_test_message},
            types::GroupMemberRole,
        },
        id::{AgentId, GroupId, RelationId},
    };
    use chrono::Utc;
    use std::collections::HashMap;

    // Mock selector registry for testing
    struct MockSelectorRegistry {
        selectors: HashMap<String, Arc<dyn AgentSelector>>,
    }

    impl MockSelectorRegistry {
        fn new() -> Self {
            let mut registry = Self {
                selectors: HashMap::new(),
            };
            // Add a default random selector for testing
            registry.register(
                "random".to_string(),
                Arc::new(crate::coordination::selectors::RandomSelector),
            );
            registry
        }
    }

    impl crate::coordination::selectors::SelectorRegistry for MockSelectorRegistry {
        fn get(&self, name: &str) -> Option<Arc<dyn AgentSelector>> {
            self.selectors.get(name).map(|s| s.clone())
        }

        fn register(&mut self, name: String, selector: Arc<dyn AgentSelector>) {
            self.selectors.insert(name, selector);
        }

        fn list(&self) -> Vec<String> {
            self.selectors.keys().map(|s| s.clone()).collect()
        }
    }

    #[tokio::test]
    async fn test_dynamic_with_random_selector() {
        let registry = Arc::new(MockSelectorRegistry::new());
        let manager = DynamicManager::new(registry);

        let agents: Vec<AgentWithMembership<Arc<dyn crate::agent::Agent>>> = vec![
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent1").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    id: RelationId::generate(),
                    in_id: AgentId::generate(),
                    out_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec!["general".to_string()],
                },
            },
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent2").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    id: RelationId::generate(),
                    in_id: AgentId::generate(),
                    out_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec!["technical".to_string()],
                },
            },
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent3").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    id: RelationId::generate(),
                    in_id: AgentId::generate(),
                    out_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: false, // Inactive - should not be selected
                    capabilities: vec!["creative".to_string()],
                },
            },
        ];

        let group = AgentGroup {
            id: GroupId::generate(),
            name: "TestGroup".to_string(),
            description: "Test dynamic group".to_string(),
            coordination_pattern: CoordinationPattern::Dynamic {
                selector_name: "random".to_string(),
                selector_config: HashMap::new(),
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            state: GroupState::Dynamic {
                recent_selections: vec![],
            },
            members: vec![], // Empty members for test
        };

        let message = create_test_message("Test message");

        let mut stream = manager
            .route_message(&group, &agents, message)
            .await
            .unwrap();

        // Collect all events from the stream
        use tokio_stream::StreamExt;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }

        // Should have at least one event
        assert!(!events.is_empty());

        // Find the Complete event
        let (agent_responses, state_changes) = events
            .iter()
            .find_map(|event| {
                if let crate::coordination::groups::GroupResponseEvent::Complete {
                    agent_responses,
                    state_changes,
                    ..
                } = event
                {
                    Some((agent_responses, state_changes))
                } else {
                    None
                }
            })
            .expect("Should have a Complete event");

        // Should have selected at least one agent
        assert!(!agent_responses.is_empty());

        // Selected agent should be active (not Agent3)
        let selected_id = &agent_responses[0].agent_id;
        assert!(selected_id != &agents[2].agent.id());

        // State should be updated with recent selection
        if let Some(GroupState::Dynamic { recent_selections }) = state_changes {
            assert_eq!(recent_selections.len(), agent_responses.len());
        } else {
            panic!("Expected Dynamic state");
        }
    }
}
