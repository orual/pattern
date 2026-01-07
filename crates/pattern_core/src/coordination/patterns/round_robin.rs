//! Round-robin coordination pattern implementation

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::{
    Result,
    agent::Agent,
    coordination::{
        groups::{AgentWithMembership, GroupManager, GroupResponse},
        types::{CoordinationPattern, GroupState},
    },
    messages::Message,
};

#[derive(Clone)]
pub struct RoundRobinManager;

#[async_trait]
impl GroupManager for RoundRobinManager {
    async fn route_message(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<
        Box<
            dyn futures::Stream<Item = crate::coordination::groups::GroupResponseEvent>
                + Send
                + Unpin,
        >,
    > {
        use crate::coordination::groups::GroupResponseEvent;
        use tokio_stream::wrappers::ReceiverStream;

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let start_time = std::time::Instant::now();

        // Clone data for the spawned task
        let group_id = group.id.clone();
        let _group_name = group.name.clone();
        let coordination_pattern = group.coordination_pattern.clone();
        let agents = agents.to_vec();

        // Spawn task to handle the routing
        tokio::spawn(async move {
            // Extract round-robin config
            let (mut current_index, skip_unavailable) = match &coordination_pattern {
                CoordinationPattern::RoundRobin {
                    current_index,
                    skip_unavailable,
                } => (*current_index, *skip_unavailable),
                _ => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: format!("Invalid pattern for RoundRobinManager"),
                            recoverable: false,
                        })
                        .await;
                    return;
                }
            };

            // Get active agents if skip_unavailable is true
            let available_agents: Vec<_> = if skip_unavailable {
                agents
                    .iter()
                    .enumerate()
                    .filter(|(_, awm)| awm.membership.is_active)
                    .collect()
            } else {
                agents.iter().enumerate().collect()
            };

            if available_agents.is_empty() {
                let _ = tx
                    .send(GroupResponseEvent::Error {
                        agent_id: None,
                        message: format!("No available agents in group"),
                        recoverable: false,
                    })
                    .await;
                return;
            }

            // Send start event
            let _ = tx
                .send(GroupResponseEvent::Started {
                    group_id: group_id.clone(),
                    pattern: "round_robin".to_string(),
                    agent_count: available_agents.len(),
                })
                .await;

            // Ensure current_index is within bounds of available agents
            current_index = current_index % available_agents.len();

            // Get the agent at the current index
            let (_original_index, awm) = &available_agents[current_index];
            tracing::debug!("Getting agent ID...");
            let agent_id = awm.agent.id();
            tracing::debug!("Got agent ID: {}", agent_id);
            tracing::debug!("Getting agent name...");
            let agent_name = awm.agent.name();
            tracing::debug!("Got agent name: {}", agent_name);

            // Send agent started event
            let _ = tx
                .send(GroupResponseEvent::AgentStarted {
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.to_string(),
                    role: awm.membership.role.clone(),
                })
                .await;

            // Process message with streaming
            match awm.agent.clone().process(vec![message.clone()]).await {
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
                            crate::agent::ResponseEvent::ReasoningChunk { text, is_final } => {
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
                                let _ = tx
                                    .send(GroupResponseEvent::ToolCallStarted {
                                        agent_id: agent_id.clone(),
                                        call_id,
                                        fn_name,
                                        args,
                                    })
                                    .await;
                            }
                            crate::agent::ResponseEvent::ToolCallCompleted { call_id, result } => {
                                let _ = tx
                                    .send(GroupResponseEvent::ToolCallCompleted {
                                        agent_id: agent_id.clone(),
                                        call_id,
                                        result: result.map_err(|e| e.to_string()),
                                    })
                                    .await;
                            }
                            crate::agent::ResponseEvent::Complete { message_id, .. } => {
                                let _ = tx
                                    .send(GroupResponseEvent::AgentCompleted {
                                        agent_id: agent_id.clone(),
                                        agent_name: agent_name.to_string(),
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

            // Calculate next index
            let next_index = if skip_unavailable {
                // Move to next available agent index
                (current_index + 1) % available_agents.len()
            } else {
                // Simple increment in full agent array
                (current_index + 1) % agents.len()
            };

            // Update state
            let new_state = GroupState::RoundRobin {
                current_index: next_index,
                last_rotation: Utc::now(),
            };

            // Send completion event
            let _ = tx
                .send(GroupResponseEvent::Complete {
                    group_id,
                    pattern: "round_robin".to_string(),
                    execution_time: start_time.elapsed(),
                    agent_responses: vec![], // TODO: Collect actual responses
                    state_changes: Some(new_state),
                })
                .await;
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }

    async fn update_state(
        &self,
        _current_state: &GroupState,
        response: &GroupResponse,
    ) -> Result<Option<GroupState>> {
        // State is already updated in route_message for round-robin
        Ok(response.state_changes.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        coordination::{
            AgentGroup,
            groups::{AgentWithMembership, GroupMembership},
            test_utils::test::{collect_agent_responses, create_test_agent, create_test_message},
            types::GroupMemberRole,
        },
        id::{AgentId, GroupId},
    };
    use chrono::Utc;

    #[tokio::test]
    async fn test_round_robin_basic() {
        let manager = RoundRobinManager;

        let agents: Vec<AgentWithMembership<Arc<dyn crate::agent::Agent>>> = vec![
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent1").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec![],
                },
            },
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent2").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec![],
                },
            },
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent3").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec![],
                },
            },
        ];

        let group = AgentGroup {
            id: GroupId::generate(),
            name: "TestGroup".to_string(),
            description: "Test round-robin group".to_string(),
            coordination_pattern: CoordinationPattern::RoundRobin {
                current_index: 0,
                skip_unavailable: false,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            state: GroupState::RoundRobin {
                current_index: 0,
                last_rotation: Utc::now(),
            },
            members: vec![], // Empty for test
        };

        let message = create_test_message("Test message");

        // First call should route to agent 0
        let stream = manager
            .route_message(&group, &agents, message.clone())
            .await
            .unwrap();

        let agent_responses = collect_agent_responses(stream).await;

        assert_eq!(agent_responses.len(), 1);
        assert_eq!(agent_responses[0].agent_id, agents[0].agent.id());
    }

    #[tokio::test]
    async fn test_round_robin_skip_inactive() {
        let manager = RoundRobinManager;

        let agents: Vec<AgentWithMembership<Arc<dyn crate::agent::Agent>>> = vec![
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent1").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec!["general".to_string()],
                },
            },
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent2").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec!["technical".to_string()],
                },
            },
            AgentWithMembership {
                agent: Arc::new(create_test_agent("Agent3").await) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
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
            description: "Test round-robin with skip".to_string(),
            coordination_pattern: CoordinationPattern::RoundRobin {
                current_index: 0,
                skip_unavailable: true,
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            state: GroupState::RoundRobin {
                current_index: 0,
                last_rotation: Utc::now(),
            },
            members: vec![], // Empty for test
        };

        let message = create_test_message("Test message");

        // Should route to agent1 first
        let stream1 = manager
            .route_message(&group, &agents, message.clone())
            .await
            .unwrap();

        let agent_responses1 = collect_agent_responses(stream1).await;
        assert_eq!(agent_responses1[0].agent_id, agents[0].agent.id());

        // Update group state for next call - agent 0 was selected, so next should be 1
        let mut group2 = group.clone();
        group2.state = GroupState::RoundRobin {
            current_index: 1,
            last_rotation: Utc::now(),
        };
        if let CoordinationPattern::RoundRobin { current_index, .. } =
            &mut group2.coordination_pattern
        {
            *current_index = 1;
        }

        // Next call should go to agent2 (skipping inactive agent3)
        let stream2 = manager
            .route_message(&group2, &agents, message)
            .await
            .unwrap();

        let agent_responses2 = collect_agent_responses(stream2).await;
        assert_eq!(agent_responses2[0].agent_id, agents[1].agent.id());
    }
}
