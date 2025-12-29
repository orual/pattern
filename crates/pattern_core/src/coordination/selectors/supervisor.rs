//! Supervisor-based agent selection
//!
//! Uses a supervisor agent to decide which agents should handle a message.
//!
//! Behavior varies by role:
//! - Supervisor: Can select any agent including itself
//! - Specialist (routing): Cannot select itself, only routes to other agents
//! - Specialist (other): Can select any agent including itself

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use super::SelectionContext;
use crate::coordination::AgentSelector;
use crate::coordination::groups::AgentWithMembership;
use crate::coordination::types::GroupMemberRole;
use crate::{
    CoreError, Result,
    agent::Agent,
    messages::{ChatRole, Message, MessageContent},
};

/// Selects agents by asking a supervisor to decide
#[derive(Debug, Clone)]
pub struct SupervisorSelector;

#[async_trait]
impl AgentSelector for SupervisorSelector {
    async fn select_agents<'a>(
        &'a self,
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        context: &SelectionContext,
        config: &HashMap<String, String>,
    ) -> Result<super::SelectionResult<'a>> {
        // Find supervisor agents or specified specialist
        let specialist_domain = config.get("specialist_domain");

        let supervisors: Vec<_> = agents
            .iter()
            .filter(|awm| {
                awm.membership.is_active
                    && match &awm.membership.role {
                        GroupMemberRole::Specialist { domain } => {
                            specialist_domain.map_or(false, |d| d == domain)
                        }
                        GroupMemberRole::Supervisor => true,
                        GroupMemberRole::Regular => false,
                        GroupMemberRole::Observer => false, // Observers don't respond
                    }
            })
            .collect();

        if supervisors.is_empty() {
            return Err(CoreError::CoordinationFailed {
                group: "unknown".to_string(),
                pattern: "supervisor".to_string(),
                participating_agents: agents.iter().map(|a| a.agent.name().to_string()).collect(),
                cause: "No supervisor or matching specialist found in group".to_string(),
            });
        }

        // Pick first available supervisor (could be enhanced with load balancing)
        let supervisor = supervisors[0];

        let supervisor_name = supervisor.agent.name();

        // Build prompt for supervisor
        let prompt = build_selection_prompt(&context.message, agents, config);

        // Create a message for the supervisor, preserving original metadata
        let metadata = context.message.metadata.clone();

        // Add coordination flag to the custom metadata
        // if let Some(custom) = metadata.custom.as_object_mut() {
        //     custom.insert("coordination_message".to_string(), serde_json::json!(true));
        // } else {
        //     // If custom was not an object, preserve the original value and add our flag
        //     let original_custom = metadata.custom.clone();
        //     metadata.custom = serde_json::json!({
        //         "coordination_message": true,
        //         "original_custom": original_custom
        //     });
        // }
        // temporarily removing to see if persisting these is fine now

        let supervisor_message = Message {
            id: crate::MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content: MessageContent::from_text(prompt),
            metadata,
            options: Default::default(),
            has_tool_calls: false,
            word_count: 0,
            created_at: chrono::Utc::now(),
            position: None,
            batch: None,
            sequence_num: None,
            batch_type: None,
        };

        // Ask supervisor to decide
        let mut stream = supervisor.agent.clone().process(supervisor_message).await?;

        // Stream response while collecting just enough text to make decision
        use tokio::sync::mpsc;
        use tokio_stream::StreamExt;

        let (event_tx, event_rx) = mpsc::channel(100);
        let (decision_tx, mut decision_rx) = mpsc::channel(1);

        // Spawn task to stream events and collect initial text for decision
        let supervisor_name_clone = supervisor_name.to_string();
        let agent_names: Vec<String> = agents.iter().map(|a| a.agent.name().to_string()).collect();
        tokio::spawn(async move {
            let mut response_text = String::new();
            let mut decision_made = false;

            while let Some(event) = stream.next().await {
                // Make decision on first substantive event
                if !decision_made {
                    match &event {
                        crate::agent::ResponseEvent::TextChunk { text, .. } => {
                            response_text.push_str(text);

                            // On first text chunk, analyze it
                            if !response_text.is_empty() {
                                let selected_names = parse_supervisor_response(&response_text);

                                // Determine if this is a direct response or delegation
                                let is_direct = if response_text.trim() == "."
                                    || response_text.trim().is_empty()
                                {
                                    // Empty or just "." = no selection, supervisor handles
                                    true
                                } else if selected_names.is_empty() {
                                    // No agent names found, check if it's substantive
                                    response_text.len() > 50
                                        || response_text.contains('.')
                                        || response_text.contains('?')
                                } else if selected_names.len() == 1
                                    && selected_names[0] == supervisor_name_clone
                                {
                                    // Self-selection
                                    true
                                } else {
                                    // Check if all names are valid agents
                                    let all_match_agents = selected_names
                                        .iter()
                                        .all(|name| agent_names.contains(name));
                                    !all_match_agents // If not all valid, treat as direct response
                                };

                                tracing::debug!(
                                    "Supervisor first text: '{}', is_direct: {}",
                                    response_text.trim(),
                                    is_direct
                                );
                                let _ = decision_tx
                                    .send((response_text.clone(), selected_names, is_direct))
                                    .await;
                                decision_made = true;
                            }
                        }
                        crate::agent::ResponseEvent::ToolCalls { .. } => {
                            // Tool calls = supervisor is handling it themselves
                            tracing::debug!("Supervisor using tools, treating as self-selection");
                            let _ = decision_tx
                                .send((response_text.clone(), vec![], true))
                                .await;
                            decision_made = true;
                        }
                        crate::agent::ResponseEvent::Complete { .. } => {
                            // Complete without text or tools = empty response
                            if !decision_made {
                                let selected_names = parse_supervisor_response(&response_text);
                                let is_direct = true; // Empty response = self-handling
                                tracing::debug!(
                                    "Supervisor completed with no text/tools, self-selecting"
                                );
                                let _ = decision_tx
                                    .send((response_text.clone(), selected_names, is_direct))
                                    .await;
                                decision_made = true;
                            }
                        }
                        _ => {}
                    }
                }

                // Forward all events regardless
                if event_tx.send(event).await.is_err() {
                    break; // Receiver dropped
                }
            }

            // If we never made a decision (shouldn't happen), send what we have
            if !decision_made {
                let selected_names = parse_supervisor_response(&response_text);
                let is_direct = response_text.is_empty() || selected_names.is_empty();
                let _ = decision_tx
                    .send((response_text, selected_names, is_direct))
                    .await;
            }
        });

        // Wait for decision from the spawned task
        let (response_text, selected_names, is_direct_response) = decision_rx
            .recv()
            .await
            .ok_or_else(|| crate::CoreError::AgentGroupError {
                group_name: "supervisor".to_string(),
                operation: "select_agents".to_string(),
                cause: "Supervisor decision channel closed unexpectedly".to_string(),
            })?;

        tracing::debug!(
            "Supervisor {} response: text='{}', parsed_names={:?}",
            supervisor.agent.name(),
            response_text.trim(),
            selected_names
        );

        // Check if this selector can select itself
        let can_select_self = match &supervisor.membership.role {
            GroupMemberRole::Specialist { domain } if domain == "routing" => false,
            _ => true,
        };

        tracing::debug!(
            "Supervisor decision: is_direct_response={}, can_select_self={}, role={:?}",
            is_direct_response,
            can_select_self,
            supervisor.membership.role
        );

        // If supervisor provided a direct response and can select self, return self
        if is_direct_response && can_select_self {
            tracing::info!(
                "Supervisor {} selecting self to handle the message directly",
                supervisor.agent.name()
            );
            // Use the streaming channel we already have
            let response_stream = Box::new(tokio_stream::wrappers::ReceiverStream::new(event_rx))
                as Box<dyn Stream<Item = crate::agent::ResponseEvent> + Send + Unpin>;

            return Ok(super::SelectionResult {
                agents: vec![supervisor],
                selector_response: Some(response_stream),
            });
        }

        // Not a direct response, we don't need the event stream
        drop(event_rx);

        // Find the selected agents
        let mut selected = Vec::new();
        for name in selected_names {
            if let Some(awm) = agents.iter().find(|a| a.agent.name() == name) {
                // Skip self-selection for routing specialists
                if !can_select_self && awm.agent.id() == supervisor.agent.id() {
                    tracing::info!(
                        "Skipping self-selection for routing specialist {}",
                        supervisor.agent.name()
                    );
                    continue;
                }
                selected.push(awm);
                tracing::debug!("Selected agent: {}", awm.agent.name());
            } else {
                tracing::warn!("Agent name '{}' not found in group", name);
            }
        }

        // If supervisor didn't select anyone valid, handle fallback
        if selected.is_empty() {
            tracing::info!("No valid agents selected, applying fallback logic");
            if can_select_self {
                // Supervisors and non-routing specialists can fall back to themselves
                selected.push(supervisor);
            } else {
                // Routing specialists broadcast to all other agents
                for awm in agents {
                    if awm.membership.is_active && awm.agent.id() != supervisor.agent.id() {
                        selected.push(awm);
                    }
                }
            }
        }

        tracing::info!(
            "Supervisor selection complete: {} agents selected: {:?}",
            selected.len(),
            selected.iter().map(|a| a.agent.name()).collect::<Vec<_>>()
        );

        Ok(super::SelectionResult {
            agents: selected,
            selector_response: None,
        })
    }

    fn name(&self) -> &str {
        "supervisor"
    }

    fn description(&self) -> &str {
        "Supervisor agent decides which agents should handle the message"
    }
}

fn build_selection_prompt(
    message: &Message,
    agents: &[AgentWithMembership<Arc<dyn Agent>>],
    config: &HashMap<String, String>,
) -> String {
    // Extract text content from the message
    let message_text = match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                crate::messages::ContentPart::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => "[non-text content]".to_string(),
    };

    let mut prompt = format!(
        "you're coordinating routing of messages within your constellation. a new message has come in:\n\n\
        message: {}\n\n\
        constellation members:\n",
        message_text
    );

    for awm in agents {
        if awm.membership.is_active {
            prompt.push_str(&format!(
                "- {} (capabilities: {})\n",
                awm.agent.name(),
                awm.membership.capabilities.join(", ")
            ));
        }
    }

    prompt.push_str(
        "\nbased on the message content and the capabilities of your constellation members, who should handle it?\n\n",
    );

    // Note: The supervisor/specialist's own name may be included in the available agents list.
    // The dynamic manager will handle self-selection appropriately.

    if let Some(max_agents) = config.get("max_agents") {
        prompt.push_str(&format!("select up to {} members.\n", max_agents));
    }

    prompt.push_str(
        "if you select, respond with only constellation member names, one per line. \
                     if more than one should see it, list all of them. \
                     if you are able to select yourself (see the preceding list), you should respond directly \
                     if you think you are the most appropriate, or take other response actions, like using tools. If you think no response is needed, you can say nothing.\
                     if you respond directly using send_message, consider the correct target (e.g. user, discord/channel, bluesky).",
    );

    prompt
}

fn parse_supervisor_response(response: &str) -> Vec<String> {
    response
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                // Remove any bullet points or numbering
                let name = trimmed
                    .trim_start_matches(|c: char| {
                        c.is_numeric() || c == '.' || c == '-' || c == '*'
                    })
                    .trim();
                if !name.is_empty() {
                    Some(name.to_string())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_supervisor_response() {
        let response = "Entropy\nFlux\n";
        let names = parse_supervisor_response(response);
        assert_eq!(names, vec!["Entropy", "Flux"]);

        let response_with_bullets = "- Pattern\n* Archive\n1. Anchor\n";
        let names = parse_supervisor_response(response_with_bullets);
        assert_eq!(names, vec!["Pattern", "Archive", "Anchor"]);

        let response_with_comments = "# These agents should handle it:\nMomentum\n# Done\n";
        let names = parse_supervisor_response(response_with_comments);
        assert_eq!(names, vec!["Momentum"]);
    }
}
