//! CLI-specific message endpoints for agent communication
//!
//! This module provides endpoint implementations for routing agent messages
//! to the terminal and external services.

use crate::coordination_helpers;
use crate::output::Output;
use async_trait::async_trait;
use owo_colors::OwoColorize;
use pattern_core::{
    Result,
    agent::Agent,
    coordination::groups::{AgentGroup, AgentWithMembership, GroupManager, GroupResponseEvent},
    messages::{ContentBlock, ContentPart, Message, MessageContent},
    runtime::{
        endpoints::BlueskyEndpoint,
        router::{MessageEndpoint, MessageOrigin},
    },
};
use pattern_db::models::PatternType;
use serde_json::Value;
use std::sync::Arc;
use tokio_stream::StreamExt;

/// CLI endpoint that formats messages using Output
pub struct CliEndpoint {
    output: Output,
}

impl CliEndpoint {
    pub fn new(output: Output) -> Self {
        Self { output }
    }
}

#[async_trait::async_trait]
impl MessageEndpoint for CliEndpoint {
    async fn send(
        &self,
        message: Message,
        _metadata: Option<Value>,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        // Extract text content from the message
        let text = match &message.content {
            MessageContent::Text(text) => text.as_str(),
            MessageContent::Parts(parts) => {
                // Find first text part
                parts
                    .iter()
                    .find_map(|part| match part {
                        ContentPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("")
            }
            MessageContent::Blocks(blocks) => {
                // Extract text from blocks, skipping thinking blocks
                blocks
                    .iter()
                    .find_map(|block| match block {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("")
            }
            _ => "",
        };

        // Use Output to format the message nicely
        // Format based on origin and extract sender name
        let sender_name = if let Some(origin) = origin {
            self.output.status(&format!(
                "[send_message] Message from {}",
                origin.description()
            ));

            // Choose a reasonable short sender label per origin type
            match origin {
                MessageOrigin::Agent { name, .. } => name.clone(),
                MessageOrigin::Bluesky { handle, .. } => format!("@{}", handle),
                MessageOrigin::Discord { .. } => "Discord".to_string(),
                MessageOrigin::DataSource { source_id, .. } => source_id.clone(),
                MessageOrigin::Cli { .. } => "CLI".to_string(),
                MessageOrigin::Api { .. } => "API".to_string(),
                MessageOrigin::Other { origin_type, .. } => origin_type.clone(),
                _ => "Runtime".to_string(),
            }
        } else {
            self.output
                .status("[send_message] Sending message to user:");
            "Runtime".to_string()
        };

        // Add a tiny delay to let reasoning chunks finish printing
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        self.output.agent_message(&sender_name, text);

        Ok(None)
    }

    fn endpoint_type(&self) -> &'static str {
        "cli"
    }
}

// =============================================================================
// Group CLI Endpoint
// =============================================================================

/// CLI endpoint for routing messages through agent groups
///
/// This wraps the core GroupEndpoint functionality and adds CLI-specific
/// output formatting for group coordination events.
pub struct GroupCliEndpoint {
    pub group: AgentGroup,
    pub agents: Vec<AgentWithMembership<Arc<dyn Agent>>>,
    pub manager: Arc<dyn GroupManager>,
    pub output: Output,
}

impl GroupCliEndpoint {
    /// Create a new GroupCliEndpoint
    pub fn new(
        group: AgentGroup,
        agents: Vec<AgentWithMembership<Arc<dyn Agent>>>,
        manager: Arc<dyn GroupManager>,
        output: Output,
    ) -> Self {
        Self {
            group,
            agents,
            manager,
            output,
        }
    }
}

#[async_trait]
// TODO: refactor the print logic to be re-used elsewhere!
impl MessageEndpoint for GroupCliEndpoint {
    async fn send(
        &self,
        mut message: Message,
        metadata: Option<Value>,
        _origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        // Merge any provided metadata into the message
        if let Some(meta) = metadata {
            if let Some(obj) = meta.as_object() {
                if let Some(existing_obj) = message.metadata.custom.as_object_mut() {
                    for (key, value) in obj {
                        existing_obj.insert(key.clone(), value.clone());
                    }
                } else {
                    message.metadata.custom = meta;
                }
            }
        }

        self.output.status(&format!(
            "Routing message through group '{}' ({:?} pattern)",
            self.group.name.bright_cyan(),
            self.group.coordination_pattern
        ));

        let mut stream = self
            .manager
            .route_message(&self.group, &self.agents, message)
            .await?;

        // Process and display events
        while let Some(event) = stream.next().await {
            match &event {
                GroupResponseEvent::Started { agent_count, .. } => {
                    self.output
                        .status(&format!("Processing with {} agent(s)...", agent_count));
                }
                GroupResponseEvent::AgentStarted {
                    agent_name, role, ..
                } => {
                    self.output.info(
                        &format!("  {} starting", agent_name.bright_cyan()),
                        &format!("{:?}", role).dimmed().to_string(),
                    );
                }
                GroupResponseEvent::TextChunk {
                    agent_id,
                    text,
                    is_final,
                } => {
                    if *is_final {
                        self.output.agent_message(&agent_id.to_string(), text);
                    }
                }
                GroupResponseEvent::ToolCallStarted {
                    agent_id,
                    fn_name,
                    args,
                    ..
                } => {
                    self.output.tool_call(
                        &format!("[{}] {}", agent_id, fn_name),
                        &serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string()),
                    );
                }
                GroupResponseEvent::ToolCallCompleted { result, .. } => match result {
                    Ok(content) => self.output.tool_result(content),
                    Err(error) => self.output.error(&format!("Tool error: {}", error)),
                },
                GroupResponseEvent::AgentCompleted { agent_name, .. } => {
                    self.output
                        .status(&format!("  {} completed", agent_name.bright_green()));
                }
                GroupResponseEvent::Complete {
                    agent_responses,
                    execution_time,
                    ..
                } => {
                    self.output.success(&format!(
                        "Group processing complete: {} response(s) in {:?}",
                        agent_responses.len(),
                        execution_time
                    ));
                }
                GroupResponseEvent::Error {
                    message,
                    agent_id,
                    recoverable,
                } => {
                    let prefix = if *recoverable { "Warning" } else { "Error" };
                    if let Some(id) = agent_id {
                        self.output
                            .error(&format!("{} from {}: {}", prefix, id, message));
                    } else {
                        self.output.error(&format!("{}: {}", prefix, message));
                    }
                }
                _ => {} // ReasoningChunk handled silently
            }
        }

        Ok(None)
    }

    fn endpoint_type(&self) -> &'static str {
        "group_cli"
    }
}

/// Create a GroupManager for the given pattern type
pub fn create_group_manager(pattern_type: PatternType) -> Arc<dyn GroupManager> {
    use pattern_core::coordination::{
        DynamicManager, PipelineManager, RoundRobinManager, SleeptimeManager, SupervisorManager,
        VotingManager, selectors::DefaultSelectorRegistry,
    };

    match pattern_type {
        PatternType::RoundRobin => Arc::new(RoundRobinManager),
        PatternType::Dynamic => {
            let registry = DefaultSelectorRegistry::new();
            Arc::new(DynamicManager::new(Arc::new(registry)))
        }
        PatternType::Pipeline => Arc::new(PipelineManager),
        PatternType::Supervisor => Arc::new(SupervisorManager),
        PatternType::Voting => Arc::new(VotingManager),
        PatternType::Sleeptime => Arc::new(SleeptimeManager),
    }
}

/// Build a GroupCliEndpoint from database data and loaded agents
pub async fn build_group_cli_endpoint(
    db_group: &pattern_db::models::AgentGroup,
    db_members: &[pattern_db::models::GroupMember],
    agents: Vec<Arc<dyn Agent>>,
    output: Output,
) -> Result<GroupCliEndpoint> {
    use pattern_core::id::AgentId;

    // Get the first agent's ID for patterns that need a leader
    let first_agent_id = agents
        .first()
        .map(|a| a.id())
        .unwrap_or_else(AgentId::generate);

    // Build core AgentGroup using shared helpers
    let group = coordination_helpers::build_agent_group(db_group, first_agent_id);

    // Build agents with membership using shared helpers
    let agents_with_membership =
        coordination_helpers::build_agents_with_membership(agents, &db_group.id, db_members);

    // Create the appropriate manager
    let manager = create_group_manager(db_group.pattern_type);

    Ok(GroupCliEndpoint::new(
        group,
        agents_with_membership,
        manager,
        output,
    ))
}

// =============================================================================
// Bluesky Endpoint Setup
// =============================================================================

/// Set up Bluesky endpoint for an agent if configured
#[allow(dead_code)]
pub async fn setup_bluesky_endpoint(agent: &Arc<dyn Agent>) -> Result<()> {
    let runtime = agent.runtime();
    let router = runtime.router();
    let bsky = BlueskyEndpoint::new(agent.id().0, runtime.dbs().clone()).await?;
    router
        .register_endpoint("bluesky".into(), Arc::new(bsky))
        .await;

    Ok(())
}
