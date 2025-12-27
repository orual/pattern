//! Supervisor coordination pattern implementation

use async_trait::async_trait;
use chrono::Utc;
use std::{collections::HashMap, sync::Arc};

use crate::{
    AgentId, Result,
    agent::Agent,
    coordination::{
        groups::{
            AgentResponse, AgentWithMembership, GroupManager, GroupResponse, GroupResponseEvent,
        },
        types::{CoordinationPattern, DelegationStrategy, FallbackBehavior, GroupState},
        utils::text_response,
    },
    message::Message,
};

#[derive(Clone)]
pub struct SupervisorManager;

#[async_trait]
impl GroupManager for SupervisorManager {
    async fn route_message(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>> {
        use tokio_stream::wrappers::ReceiverStream;
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let start_time = std::time::Instant::now();
        let group_id = group.id.clone();
        let _group_name = group.name.clone();
        let coordination_pattern = group.coordination_pattern.clone();
        let group_state = group.state.clone();
        let agents = agents.to_vec();

        tokio::spawn(async move {
            // Extract supervisor config
            let (leader_id, delegation_rules) = match &coordination_pattern {
                CoordinationPattern::Supervisor {
                    leader_id,
                    delegation_rules,
                } => (leader_id, delegation_rules),
                _ => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: format!("Invalid pattern for SupervisorManager"),
                            recoverable: false,
                        })
                        .await;
                    return;
                }
            };

            // Extract current delegations from state
            let current_delegations = match &group_state {
                GroupState::Supervisor {
                    current_delegations,
                } => current_delegations.clone(),
                _ => HashMap::new(),
            };

            // Find the leader agent
            let leader = match agents
                .iter()
                .find(|awm| awm.agent.as_ref().id() == *leader_id)
            {
                Some(l) => l,
                _ => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: format!("Leader agent {} not found", leader_id),
                            recoverable: false,
                        })
                        .await;
                    return;
                }
            };

            // Send start event
            tracing::info!(
                "Supervisor: sending GroupResponseEvent::Started (agents={}, group_id={})",
                agents.len(),
                group_id
            );
            let _ = tx
                .send(GroupResponseEvent::Started {
                    group_id: group_id.clone(),
                    pattern: "supervisor".to_string(),
                    agent_count: agents.len(),
                })
                .await;
            tracing::debug!("Supervisor: Started event queued");

            // Decide if leader should delegate
            let should_delegate = Self::should_delegate_static(
                &message,
                &current_delegations,
                leader_id,
                delegation_rules.max_delegations_per_agent,
            );

            let mut agent_responses = Vec::new();
            let mut new_delegations = current_delegations.clone();

            if should_delegate {
                // Select delegate based on strategy
                let delegate = Self::select_delegate_static(
                    &agents,
                    leader_id,
                    &delegation_rules.delegation_strategy,
                    &current_delegations,
                    delegation_rules.max_delegations_per_agent,
                );

                if let Ok(Some(delegate_awm)) = delegate {
                    // Delegate handles the message
                    let agent_id = delegate_awm.agent.as_ref().id();
                    let agent_name = delegate_awm.agent.name();

                    let _ = tx
                        .send(GroupResponseEvent::AgentStarted {
                            agent_id: agent_id.clone(),
                            agent_name: agent_name.to_string(),
                            role: delegate_awm.membership.role.clone(),
                        })
                        .await;

                    // Process with streaming
                    match delegate_awm.agent.clone().process(message.clone()).await {
                        Ok(mut stream) => {
                            use tokio_stream::StreamExt;

                            while let Some(event) = stream.next().await {
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

                            // Update delegation count
                            *new_delegations.entry(agent_id.clone()).or_insert(0) += 1;

                            agent_responses.push(AgentResponse {
                                agent_id: agent_id.clone(),
                                response: crate::message::Response {
                                    content: vec![],
                                    reasoning: None,
                                    metadata: crate::message::ResponseMetadata::default(),
                                },
                                responded_at: Utc::now(),
                            });
                        }
                        Err(e) => {
                            let _ = tx
                                .send(GroupResponseEvent::Error {
                                    agent_id: Some(agent_id),
                                    message: e.to_string(),
                                    recoverable: false,
                                })
                                .await;
                        }
                    }
                } else {
                    // No delegate available, use fallback behavior
                    match &delegation_rules.fallback_behavior {
                        FallbackBehavior::HandleSelf => {
                            // Leader handles it
                            let agent_id = leader.agent.as_ref().id();
                            let agent_name = leader.agent.name();

                            let _ = tx
                                .send(GroupResponseEvent::AgentStarted {
                                    agent_id: agent_id.clone(),
                                    agent_name: agent_name.to_string(),
                                    role: leader.membership.role.clone(),
                                })
                                .await;

                            match leader.agent.clone().process(message.clone()).await {
                                Ok(mut stream) => {
                                    use tokio_stream::StreamExt;

                                    while let Some(event) = stream.next().await {
                                        match event {
                                            crate::agent::ResponseEvent::TextChunk {
                                                text,
                                                is_final,
                                            } => {
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
                                                message_id,
                                                ..
                                            } => {
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

                                    agent_responses.push(AgentResponse {
                                        agent_id: leader_id.clone(),
                                        response: crate::message::Response {
                                            content: vec![],
                                            reasoning: None,
                                            metadata: crate::message::ResponseMetadata::default(),
                                        },
                                        responded_at: Utc::now(),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(GroupResponseEvent::Error {
                                            agent_id: Some(leader_id.clone()),
                                            message: e.to_string(),
                                            recoverable: false,
                                        })
                                        .await;
                                }
                            }
                        }
                        FallbackBehavior::Queue => {
                            let _ = tx
                                .send(GroupResponseEvent::TextChunk {
                                    agent_id: leader_id.clone(),
                                    text: "[Supervisor] Message queued for later processing"
                                        .to_string(),
                                    is_final: true,
                                })
                                .await;

                            agent_responses.push(AgentResponse {
                                agent_id: leader_id.clone(),
                                response: text_response(
                                    "[Supervisor] Message queued for later processing",
                                ),
                                responded_at: Utc::now(),
                            });
                        }
                        FallbackBehavior::Fail => {
                            let _ = tx
                                .send(GroupResponseEvent::Error {
                                    agent_id: None,
                                    message: "No delegates available and fallback is set to fail"
                                        .to_string(),
                                    recoverable: false,
                                })
                                .await;
                            return;
                        }
                    }
                }
            } else {
                // Leader handles directly
                let agent_id = leader.agent.as_ref().id();
                let agent_name = leader.agent.name();

                let _ = tx
                    .send(GroupResponseEvent::AgentStarted {
                        agent_id: agent_id.clone(),
                        agent_name: agent_name.to_string(),
                        role: leader.membership.role.clone(),
                    })
                    .await;

                match leader.agent.clone().process(message.clone()).await {
                    Ok(mut stream) => {
                        use tokio_stream::StreamExt;

                        while let Some(event) = stream.next().await {
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

                        agent_responses.push(AgentResponse {
                            agent_id: leader_id.clone(),
                            response: crate::message::Response {
                                content: vec![],
                                reasoning: None,
                                metadata: crate::message::ResponseMetadata::default(),
                            },
                            responded_at: Utc::now(),
                        });
                    }
                    Err(e) => {
                        let _ = tx
                            .send(GroupResponseEvent::Error {
                                agent_id: Some(leader_id.clone()),
                                message: e.to_string(),
                                recoverable: false,
                            })
                            .await;
                    }
                }
            }

            // Update state with new delegation counts
            let new_state = if should_delegate && !new_delegations.is_empty() {
                Some(GroupState::Supervisor {
                    current_delegations: new_delegations,
                })
            } else {
                None
            };

            // Send completion event
            let _ = tx
                .send(GroupResponseEvent::Complete {
                    group_id,
                    pattern: "supervisor".to_string(),
                    execution_time: start_time.elapsed(),
                    agent_responses,
                    state_changes: new_state,
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
        // State is already updated in route_message for supervisor pattern
        Ok(response.state_changes.clone())
    }
}

impl SupervisorManager {
    fn should_delegate_static(
        _message: &Message,
        current_delegations: &HashMap<AgentId, usize>,
        leader_id: &AgentId,
        max_delegations: Option<usize>,
    ) -> bool {
        Self::should_delegate_impl(_message, current_delegations, leader_id, max_delegations)
    }

    fn should_delegate_impl(
        _message: &Message,
        current_delegations: &HashMap<AgentId, usize>,
        leader_id: &AgentId,
        max_delegations: Option<usize>,
    ) -> bool {
        // Simple heuristic: delegate if leader has too many active delegations
        if let Some(max) = max_delegations {
            let leader_count = current_delegations.get(leader_id).copied().unwrap_or(0);
            leader_count >= max
        } else {
            // Could add more sophisticated logic here based on message content
            false
        }
    }

    fn select_delegate_static<'a>(
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        leader_id: &AgentId,
        strategy: &DelegationStrategy,
        current_delegations: &HashMap<AgentId, usize>,
        max_delegations: Option<usize>,
    ) -> Result<Option<&'a AgentWithMembership<Arc<dyn Agent>>>> {
        Self::select_delegate_impl(
            agents,
            leader_id,
            strategy,
            current_delegations,
            max_delegations,
        )
    }

    fn select_delegate_impl<'a>(
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        leader_id: &AgentId,
        strategy: &DelegationStrategy,
        current_delegations: &HashMap<AgentId, usize>,
        max_delegations: Option<usize>,
    ) -> Result<Option<&'a AgentWithMembership<Arc<dyn Agent>>>> {
        // Filter out leader and unavailable agents
        let available_agents: Vec<_> = agents
            .iter()
            .filter(|awm| {
                let agent_id = awm.agent.as_ref().id();
                agent_id != *leader_id
                    && awm.membership.is_active
                    && Self::can_accept_delegation_impl(
                        &agent_id,
                        current_delegations,
                        max_delegations,
                    )
            })
            .collect();

        if available_agents.is_empty() {
            return Ok(None);
        }

        match strategy {
            DelegationStrategy::RoundRobin => {
                // Simple round-robin: pick the agent with fewest delegations
                Ok(available_agents.into_iter().min_by_key(|&awm| {
                    current_delegations
                        .get(&awm.agent.as_ref().id())
                        .copied()
                        .unwrap_or(0)
                }))
            }
            DelegationStrategy::LeastBusy => {
                // Same as round-robin for now (would check actual workload in real impl)
                Ok(available_agents.into_iter().min_by_key(|&awm| {
                    current_delegations
                        .get(&awm.agent.as_ref().id())
                        .copied()
                        .unwrap_or(0)
                }))
            }
            DelegationStrategy::Capability => {
                // Check capabilities stored in membership
                Ok(available_agents
                    .into_iter()
                    .find(|&awm| !awm.membership.capabilities.is_empty()))
            }
            DelegationStrategy::Random => {
                let mut rng = rand::rng();
                let index = rand::Rng::random_range(&mut rng, 0..available_agents.len());
                Ok(available_agents.get(index).copied())
            }
        }
    }

    fn can_accept_delegation_impl(
        agent_id: &AgentId,
        current_delegations: &HashMap<AgentId, usize>,
        max_delegations: Option<usize>,
    ) -> bool {
        if let Some(max) = max_delegations {
            let current = current_delegations.get(agent_id).copied().unwrap_or(0);
            current < max
        } else {
            true
        }
    }
}
