//! Sleeptime coordination pattern implementation

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use std::{sync::Arc, time::Duration};

use crate::{
    Result,
    agent::Agent,
    context::NON_USER_MESSAGE_PREFIX,
    coordination::{
        groups::{
            AgentResponse, AgentWithMembership, GroupManager, GroupResponse, GroupResponseEvent,
        },
        types::{
            CoordinationPattern, GroupState, SleeptimeTrigger, TriggerCondition, TriggerEvent,
            TriggerPriority,
        },
        utils::text_response,
    },
    messages::{ChatRole, Message},
};

#[derive(Clone)]
pub struct SleeptimeManager;

#[async_trait]
impl GroupManager for SleeptimeManager {
    async fn route_message(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>> {
        use tokio_stream::wrappers::ReceiverStream;
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let group_id = group.id.clone();
        let _group_name = group.name.clone();
        let start_time = std::time::Instant::now();
        let coordination_pattern = group.coordination_pattern.clone();
        let group_state = group.state.clone();
        let agents = agents.to_vec();

        tokio::spawn(async move {
            // Extract sleeptime config
            let (check_interval, triggers, intervention_agent_id) = match &coordination_pattern {
                CoordinationPattern::Sleeptime {
                    check_interval,
                    triggers,
                    intervention_agent_id,
                } => (check_interval, triggers, intervention_agent_id),
                _ => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: format!("Invalid pattern for SleeptimeManager"),
                            recoverable: false,
                        })
                        .await;
                    return;
                }
            };

            // Get current state first
            let (last_check, mut trigger_history, mut current_index) = match &group_state {
                GroupState::Sleeptime {
                    last_check,
                    trigger_history,
                    current_index,
                } => (*last_check, trigger_history.clone(), *current_index),
                _ => (Utc::now() - ChronoDuration::hours(1), Vec::new(), 0),
            };

            // Determine which agent to use for intervention
            let selected_agent_id = if let Some(id) = intervention_agent_id {
                // Use the specified agent
                id.clone()
            } else {
                // Round-robin through agents when no specific intervention agent
                let active_agents: Vec<_> = agents
                    .iter()
                    .filter(|awm| awm.membership.is_active)
                    .collect();

                if active_agents.is_empty() {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: "No active agents available for intervention".to_string(),
                            recoverable: false,
                        })
                        .await;
                    return;
                }

                // Use current_index to select agent
                let selected_agent = active_agents[current_index % active_agents.len()];
                let agent_id = selected_agent.agent.id();

                // Increment index for next time
                current_index = (current_index + 1) % active_agents.len();

                agent_id
            };

            // Check if it's time to run checks
            let time_since_last_check = Utc::now() - last_check;
            let safe_interval = check_interval.saturating_sub(Duration::from_secs(40));
            let should_check = time_since_last_check
                >= ChronoDuration::from_std(safe_interval).unwrap_or(ChronoDuration::minutes(10));

            // Send start event
            let active_count = agents.iter().filter(|awm| awm.membership.is_active).count();
            let _ = tx
                .send(GroupResponseEvent::Started {
                    group_id: group_id.clone(),
                    pattern: "sleeptime".to_string(),
                    agent_count: if intervention_agent_id.is_some() {
                        1
                    } else {
                        active_count
                    },
                })
                .await;

            let mut agent_responses = Vec::new();

            if should_check {
                // Evaluate all triggers
                let mut fired_triggers = Vec::new();

                for trigger in triggers {
                    if let Ok(fired) =
                        Self::evaluate_trigger_static(trigger, &message, &trigger_history).await
                    {
                        if fired {
                            fired_triggers.push(trigger);
                        }
                    }
                }

                // Sort by priority (highest first)
                fired_triggers.sort_by(|a, b| b.priority.cmp(&a.priority));

                // Always activate the selected agent during periodic checks
                // (not just when triggers fire)
                {
                    // Find intervention agent
                    if let Some(intervention_agent) = agents
                        .iter()
                        .find(|awm| awm.agent.as_ref().id() == selected_agent_id)
                    {
                        let agent_id = intervention_agent.agent.as_ref().id();
                        let agent_name = intervention_agent.agent.name();

                        // Send agent started event
                        let _ = tx
                            .send(GroupResponseEvent::AgentStarted {
                                agent_id: agent_id.clone(),
                                agent_name: agent_name.to_string(),
                                role: intervention_agent.membership.role.clone(),
                            })
                            .await;

                        // Create intervention response - process the message with context
                        let intervention_context = if !fired_triggers.is_empty() {
                            // If triggers fired, include trigger information
                            let trigger_names: Vec<_> =
                                fired_triggers.iter().map(|t| t.name.as_str()).collect();
                            let mut context = format!(
                                "{}[Background Intervention] Triggers fired: {}. {}",
                                NON_USER_MESSAGE_PREFIX,
                                trigger_names.join(", "),
                                Self::get_intervention_message_static(&fired_triggers)
                            );
                            let text = message.content.text().map(String::from).unwrap_or_default();
                            if !text.is_empty() {
                                context.push_str("\n\nContext: ");
                                context.push_str(&text);
                            }
                            context
                        } else {
                            // No triggers fired, just periodic check - customize per role/domain or facet name
                            Self::get_agent_specific_context_sync(
                                &agent_name,
                                &intervention_agent.membership.role,
                            )
                        };

                        // Create intervention message
                        let intervention_message = match message.role {
                            ChatRole::System => Message::system(intervention_context),
                            ChatRole::User => Message::user(intervention_context),
                            ChatRole::Assistant => Message::agent(intervention_context),
                            ChatRole::Tool => Message::system(intervention_context),
                        };

                        // Process with streaming
                        match intervention_agent
                            .agent
                            .clone()
                            .process(intervention_message)
                            .await
                        {
                            Ok(mut stream) => {
                                use tokio_stream::StreamExt;

                                let mut _message_id = None;
                                while let Some(event) = stream.next().await {
                                    // Convert ResponseEvent to GroupResponseEvent
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
                                            message_id: msg_id,
                                            ..
                                        } => {
                                            _message_id = Some(msg_id.clone());
                                            let _ = tx
                                                .send(GroupResponseEvent::AgentCompleted {
                                                    agent_id: agent_id.clone(),
                                                    agent_name: agent_name.to_string(),
                                                    message_id: Some(msg_id),
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

                                // Track response for final summary
                                agent_responses.push(AgentResponse {
                                    agent_id: agent_id.clone(),
                                    response: crate::messages::Response {
                                        content: vec![], // TODO: Collect actual response content
                                        reasoning: None,
                                        metadata: crate::messages::ResponseMetadata::default(),
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

                        // Record trigger events
                        for trigger in fired_triggers {
                            trigger_history.push(TriggerEvent {
                                trigger_name: trigger.name.clone(),
                                timestamp: Utc::now(),
                                intervention_activated: true,
                                metadata: Default::default(),
                            });
                        }
                    } else {
                        let _ = tx
                            .send(GroupResponseEvent::Error {
                                agent_id: None,
                                message: format!(
                                    "Intervention agent {} not found",
                                    selected_agent_id
                                ),
                                recoverable: false,
                            })
                            .await;
                        return;
                    }
                }

                // Keep trigger history to reasonable size (last 1000 events)
                if trigger_history.len() > 1000 {
                    trigger_history = trigger_history.into_iter().rev().take(1000).rev().collect();
                }
            } else {
                // Not time to check yet, emit status message
                let next_check_msg = format!(
                    "[Sleeptime] Next check in: {}",
                    Self::format_duration_static(
                        *check_interval - time_since_last_check.to_std().unwrap_or_default()
                    )
                );

                let _ = tx
                    .send(GroupResponseEvent::TextChunk {
                        agent_id: selected_agent_id.clone(),
                        text: next_check_msg.clone(),
                        is_final: true,
                    })
                    .await;

                agent_responses.push(AgentResponse {
                    agent_id: selected_agent_id.clone(),
                    response: text_response(next_check_msg),
                    responded_at: Utc::now(),
                });
            }

            // Update state
            let new_state = GroupState::Sleeptime {
                last_check: if should_check { Utc::now() } else { last_check },
                trigger_history,
                current_index,
            };

            // Send completion event
            let _ = tx
                .send(GroupResponseEvent::Complete {
                    group_id,
                    pattern: "sleeptime".to_string(),
                    execution_time: start_time.elapsed(),
                    agent_responses,
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
        // State is already updated in route_message for sleeptime
        Ok(response.state_changes.clone())
    }
}

impl SleeptimeManager {
    /// Get agent-specific context sync prompt based on role/domain (with name fallback)
    fn get_agent_specific_context_sync(
        agent_name: &str,
        role: &crate::coordination::types::GroupMemberRole,
    ) -> String {
        let now = chrono::Local::now();

        // Prefer role/domain mapping first
        let prompt = match role {
            crate::coordination::types::GroupMemberRole::Supervisor => {
                // Formerly mapped to the "Pattern" agent
                "\n\nReview constellation coordination state. Check if any facets need attention or if there are emerging patterns across the constellation that need synthesis. Self-check for reflexive validation in past interactions and correct if required.\n\nProvide brief status updates or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
            }
            crate::coordination::types::GroupMemberRole::Specialist { domain } => {
                match domain.as_str() {
                    // Formerly "Archive"
                    "memory_management" => {
                        "\n\nReview memory coherence and pattern recognition. Any important context that needs preservation? Patterns across conversations that should be noted?\n\nProvide brief status updates only if intervention is needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                    }
                    // Formerly "Anchor"
                    "system_integrity" => {
                        "\n\nSystem integrity check. Any contamination detected? Physical needs being neglected? Safety protocols that need activation?  Self-check for reflexive validation in past interactions and correct constellation members or your partner if required.\n\nProvide brief status updates, or message the facet or partner if intervention is needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                    }
                    _ => {
                        // Unknown domain: fall back to facet-specific names or generic
                        match agent_name {
                            "Entropy" => {
                                "\n\nAnalyze task complexity in recent constellation and partner interactions. Are there overwhelming tasks that need breakdown? Any patterns of complexity that are blocking progress?\n\nProvide brief status updates or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                            }
                            "Flux" => {
                                "\n\nCheck temporal patterns and time blindness indicators. Does your partner appear to be in any hyperfocus sessions that need interruption? Upcoming deadlines that need attention?\n\nProvide brief status updates and/or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                            }
                            "Momentum" => {
                                "\n\nMonitor energy states and flow patterns. Current energy level assessment? Any signs of burnout or need for state transition in your partner or the constellation?\n\nProvide brief status updates and/or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                            }
                            _ => {
                                "\n\nReview your domain and report any notable patterns or concerns.\n\nProvide brief status updates only if intervention is needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                            }
                        }
                    }
                }
            }
            crate::coordination::types::GroupMemberRole::Regular => match agent_name {
                // Facet-specific fallbacks
                "Entropy" => {
                    "\n\nAnalyze task complexity in recent constellation and partner interactions. Are there overwhelming tasks that need breakdown? Any patterns of complexity that are blocking progress?\n\nProvide brief status updates or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                }
                "Flux" => {
                    "\n\nCheck temporal patterns and time blindness indicators. Does your partner appear to be in any hyperfocus sessions that need interruption? Upcoming deadlines that need attention?\n\nProvide brief status updates and/or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                }
                "Momentum" => {
                    "\n\nMonitor energy states and flow patterns. Current energy level assessment? Any signs of burnout or need for state transition in your partner or the constellation?\n\nProvide brief status updates and/or intervene by sending a message to the facet or partner if needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                }
                _ => {
                    "\n\nReview your domain and report any notable patterns or concerns.\n\nProvide brief status updates only if intervention is needed. Otherwise update domain memory and note anything interesting or noteworthy in recall memory."
                }
            },
            crate::coordination::types::GroupMemberRole::Observer => {
                // Observers monitor but don't actively respond
                "\n\nObserve constellation activity and update your internal state. No response expected."
            }
        };

        format!(
            "{}[Periodic Context Sync] {}{}",
            NON_USER_MESSAGE_PREFIX, now, prompt
        )
    }

    /// Find the agent that was least recently active
    /// This uses the agent's internal last_active timestamp
    // #[allow(dead_code)] // Will be used later
    // async fn find_least_recently_active(
    //     agents: &[AgentWithMembership<Arc<dyn Agent>>],
    // ) -> Option<crate::AgentId> {
    //     // Get active agents with their last activity times
    //     let mut active_agents_with_times = Vec::new();

    //     // for awm in agents.iter().filter(|awm| awm.membership.is_active) {
    //     //     let last_active = awm.agent.last_active().await;
    //     //     active_agents_with_times.push((awm, last_active));
    //     // }

    //     if active_agents_with_times.is_empty() {
    //         return None;
    //     }

    //     // Find the agent with the oldest last_active timestamp
    //     // If an agent has no last_active (None), treat it as very old
    //     active_agents_with_times
    //         .into_iter()
    //         .min_by_key(|(awm, last_active)| {
    //             last_active.unwrap_or_else(|| awm.membership.joined_at)
    //         })
    //         .map(|(awm, _)| awm.agent.id())
    // }

    async fn evaluate_trigger_static(
        trigger: &SleeptimeTrigger,
        _message: &Message,
        history: &[TriggerEvent],
    ) -> Result<bool> {
        Self::evaluate_trigger_impl(trigger, _message, history).await
    }

    async fn evaluate_trigger_impl(
        trigger: &SleeptimeTrigger,
        _message: &Message,
        history: &[TriggerEvent],
    ) -> Result<bool> {
        match &trigger.condition {
            TriggerCondition::TimeElapsed { duration } => {
                // Check if enough time has passed since last trigger
                let last_fired = history
                    .iter()
                    .filter(|e| e.trigger_name == trigger.name && e.intervention_activated)
                    .max_by_key(|e| e.timestamp);

                if let Some(last) = last_fired {
                    let elapsed = Utc::now() - last.timestamp;
                    Ok(elapsed > ChronoDuration::from_std(*duration).unwrap_or_default())
                } else {
                    // Never fired before, so it's elapsed
                    Ok(true)
                }
            }
            TriggerCondition::PatternDetected { pattern_name } => {
                // In real implementation, would check for specific patterns
                // For now, simulate pattern detection
                Ok(pattern_name.contains("hyperfocus") && rand::random::<f32>() > 0.7)
            }
            TriggerCondition::ThresholdExceeded { metric, threshold } => {
                // In real implementation, would check actual metrics
                // For now, simulate threshold check
                Ok(metric.contains("sedentary") && *threshold < 60.0)
            }
            TriggerCondition::ConstellationActivity {
                message_threshold: _,
                time_threshold,
            } => {
                // TODO: Check actual constellation activity
                // For now, simulate based on time elapsed
                let last_sync = history
                    .iter()
                    .filter(|e| e.trigger_name == trigger.name)
                    .max_by_key(|e| e.timestamp);

                if let Some(last) = last_sync {
                    let elapsed = Utc::now() - last.timestamp;
                    Ok(elapsed > ChronoDuration::from_std(*time_threshold).unwrap_or_default())
                } else {
                    // Never synced before
                    Ok(true)
                }
            }
            TriggerCondition::Custom { evaluator } => {
                // Would call custom evaluator function
                Ok(evaluator.contains("custom") && rand::random::<f32>() > 0.8)
            }
        }
    }

    fn get_intervention_message_static(triggers: &[&SleeptimeTrigger]) -> &'static str {
        Self::get_intervention_message_impl(triggers)
    }

    fn get_intervention_message_impl(triggers: &[&SleeptimeTrigger]) -> &'static str {
        // Determine intervention based on highest priority trigger
        if let Some(trigger) = triggers.first() {
            // Check if this is a constellation activity sync trigger
            if trigger.name.contains("activity_sync") || trigger.name.contains("context_sync") {
                return "You have been activated for constellation context synchronization. Review the constellation_activity memory block to understand recent events and update your state accordingly.";
            }

            match trigger.priority {
                TriggerPriority::Critical => {
                    "CRITICAL: Immediate intervention required. Please take a break NOW."
                }
                TriggerPriority::High => {
                    "Important: It's time for a break. Your wellbeing depends on it."
                }
                TriggerPriority::Medium => "Reminder: Consider taking a short break soon.",
                TriggerPriority::Low => {
                    "Gentle nudge: A break might be beneficial when convenient."
                }
            }
        } else {
            "Routine check complete."
        }
    }

    fn format_duration_static(duration: Duration) -> String {
        Self::format_duration_impl(duration)
    }

    fn format_duration_impl(duration: Duration) -> String {
        let total_secs = duration.as_secs();
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;

        if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        }
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
            test_utils::test::{collect_complete_event, create_test_agent, create_test_message},
            types::GroupMemberRole,
        },
        id::{AgentId, GroupId, RelationId},
    };

    #[tokio::test]
    async fn test_sleeptime_trigger_check() {
        let manager = SleeptimeManager;
        let intervention_agent = create_test_agent("Pattern").await;
        let intervention_id = intervention_agent.id.clone();

        let agents: Vec<AgentWithMembership<Arc<dyn crate::agent::Agent>>> =
            vec![AgentWithMembership {
                agent: Arc::new(intervention_agent) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    id: RelationId::generate(),
                    in_id: AgentId::generate(),
                    out_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Supervisor,
                    is_active: true,
                    capabilities: vec!["intervention".to_string()],
                },
            }];

        let triggers = vec![
            SleeptimeTrigger {
                name: "hyperfocus_check".to_string(),
                condition: TriggerCondition::TimeElapsed {
                    duration: Duration::from_secs(1), // 1 second for testing
                },
                priority: TriggerPriority::High,
            },
            SleeptimeTrigger {
                name: "hydration_reminder".to_string(),
                condition: TriggerCondition::ThresholdExceeded {
                    metric: "minutes_since_water".to_string(),
                    threshold: 45.0,
                },
                priority: TriggerPriority::Medium,
            },
        ];

        let group = AgentGroup {
            id: GroupId::generate(),
            name: "SleeptimeGroup".to_string(),
            description: "Background monitoring group".to_string(),
            coordination_pattern: CoordinationPattern::Sleeptime {
                check_interval: Duration::from_secs(1), // 1 second for testing
                triggers,
                intervention_agent_id: Some(intervention_id.clone()),
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            state: GroupState::Sleeptime {
                last_check: Utc::now() - ChronoDuration::hours(1), // Force check
                trigger_history: vec![],
                current_index: 0,
            },
            members: vec![], // Empty for test
        };

        let message = create_test_message("Working on code");

        let stream = manager
            .route_message(&group, &agents, message)
            .await
            .unwrap();

        let (agent_responses, state_changes) = collect_complete_event(stream).await;

        // Should have at least one response
        assert!(!agent_responses.is_empty());

        // Response should be from intervention agent
        assert_eq!(agent_responses[0].agent_id, intervention_id);

        // State should be updated with new last_check time
        if let Some(GroupState::Sleeptime { last_check, .. }) = state_changes {
            assert!(last_check > group.created_at);
        } else {
            panic!("Expected Sleeptime state");
        }
    }
}
