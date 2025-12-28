//! Slash command handling for interactive chat modes
//!
//! This module handles slash commands in the CLI chat interface.
//! Commands provide quick access to agent information, memory inspection,
//! and database queries.
//!
//! ## Migration Status
//!
//! Some commands are STUBBED during the pattern_db migration:
//! - /list: Previously used db_v1::ops to list all agents
//! - /archival, /context, /search: Previously used agent.handle() which no longer exists
//!
//! Commands that work:
//! - /help, /exit, /quit, /status, /memory, /permit, /deny, /query

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::{
    Agent,
    coordination::groups::{AgentGroup, AgentWithMembership},
};
use std::sync::Arc;

use crate::output::Output;

/// Context for slash command execution
pub enum CommandContext<'a> {
    SingleAgent(&'a Arc<dyn Agent>),
    Group {
        #[allow(dead_code)]
        group: &'a AgentGroup,
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        default_agent: Option<&'a Arc<dyn Agent>>,
    },
}

impl<'a> CommandContext<'a> {
    /// Get an agent by name or use the default/current agent
    async fn get_agent(&self, name: Option<&str>) -> Result<Arc<dyn Agent>> {
        match self {
            CommandContext::SingleAgent(agent) => {
                if name.is_some() {
                    return Err(miette::miette!("Cannot specify agent in single agent chat"));
                }
                Ok((*agent).clone())
            }
            CommandContext::Group {
                agents,
                default_agent,
                ..
            } => {
                if let Some(agent_name) = name {
                    // Find agent in group by name
                    for agent_with_membership in *agents {
                        if agent_with_membership.agent.name() == agent_name {
                            return Ok(agent_with_membership.agent.clone());
                        }
                    }
                    Err(miette::miette!("Agent '{}' not found in group", agent_name))
                } else if let Some(agent) = default_agent {
                    Ok((*agent).clone())
                } else {
                    Err(miette::miette!(
                        "No default agent available, please specify an agent"
                    ))
                }
            }
        }
    }

    /// List available agents
    fn list_agents(&self) -> Vec<String> {
        match self {
            CommandContext::SingleAgent(agent) => vec![agent.name().to_string()],
            CommandContext::Group { agents, .. } => {
                agents.iter().map(|a| a.agent.name().to_string()).collect()
            }
        }
    }
}

/// Handle slash commands in chat
pub async fn handle_slash_command(
    command: &str,
    context: CommandContext<'_>,
    output: &Output,
) -> Result<bool> {
    let parts: Vec<&str> = command.trim().split_whitespace().collect();
    if parts.is_empty() {
        return Ok(false);
    }

    match parts[0] {
        "/help" | "/?" => {
            output.section("Available Commands");
            output.list_item("/help or /? - Show this help message");
            output.list_item("/exit or /quit - Exit the chat");
            output.list_item("/status [agent] - Show agent status");
            output.print("");

            // Show available agents in group chat
            if let CommandContext::Group { agents, .. } = &context {
                output.section("Agents in Group");
                for agent_with_membership in *agents {
                    output.list_item(&format!(
                        "{} - Role: {:?}",
                        agent_with_membership.agent.name().bright_cyan(),
                        agent_with_membership.membership.role
                    ));
                }
                output.print("");
                output
                    .status("Tip: Specify agent name with commands, e.g., /memory Pattern system");
                output.print("");
            }

            output.section("Memory Commands");
            output.list_item("/memory [agent] [block] - List or show memory blocks");
            output.print("");
            output.section("Permission Commands");
            output.list_item("/permit <id> [once|always|ttl=N] - Approve a permission request");
            output.list_item("/deny <id> - Deny a permission request");
            output.print("");
            output.section("Database Commands");
            output.list_item("/query <sql> - Run a database query");
            output.print("");
            output.section("Temporarily Disabled (migration in progress)");
            output.list_item("/list - List all agents from database");
            output.list_item("/archival - Search archival memory");
            output.list_item("/context - Show conversation context");
            output.list_item("/search - Search conversation history");
            output.print("");
            Ok(false)
        }
        "/exit" | "/quit" => Ok(true),
        "/list" => {
            // TODO: Reimplement for pattern_db
            // Previously used db_v1::ops::list_entities::<AgentRecord, _>(&DB)
            output.warning("Agent listing temporarily disabled during database migration");
            output.status("Reason: Needs pattern_db query implementation");
            output.status("");
            output.status("Available agents in current context:");
            for name in context.list_agents() {
                output.list_item(&name.bright_cyan().to_string());
            }
            Ok(false)
        }
        "/status" => {
            // Parse optional agent name for group context
            let agent_name = if parts.len() > 1 {
                Some(parts[1])
            } else {
                None
            };

            match context.get_agent(agent_name).await {
                Ok(agent) => {
                    output.section("Agent Status");
                    output.kv("Name", &agent.name().bright_cyan().to_string());
                    output.kv("ID", &agent.id().to_string().dimmed().to_string());

                    // Get memory block count through runtime
                    let agent_id = agent.id().to_string();
                    let runtime = agent.runtime();
                    let memory = runtime.memory();
                    match memory.list_blocks(&agent_id).await {
                        Ok(blocks) => {
                            output.kv("Memory blocks", &blocks.len().to_string());
                            for block in &blocks {
                                output.list_item(&format!(
                                    "{} ({:?})",
                                    block.label.bright_cyan(),
                                    block.block_type
                                ));
                            }
                        }
                        Err(e) => {
                            output.kv("Memory blocks", &format!("error: {}", e));
                        }
                    }
                }
                Err(e) => {
                    output.error(&format!("Failed to get agent: {}", e));
                }
            }

            output.print("");
            Ok(false)
        }
        "/permit" => {
            if parts.len() < 2 {
                output.error("Usage: /permit <request_id> [once|always|ttl=600]");
                return Ok(false);
            }
            let id = parts[1];
            let mode = parts.get(2).copied();
            match crate::permission_sink::cli_permit(id, mode).await {
                Ok(_) => output.success(&format!("Approved {}", id)),
                Err(e) => output.error(&format!("Failed to approve: {}", e)),
            }
            Ok(false)
        }
        "/deny" => {
            if parts.len() != 2 {
                output.error("Usage: /deny <request_id>");
                return Ok(false);
            }
            let id = parts[1];
            match crate::permission_sink::cli_deny(id).await {
                Ok(_) => output.success(&format!("Denied {}", id)),
                Err(e) => output.error(&format!("Failed to deny: {}", e)),
            }
            Ok(false)
        }
        "/memory" => {
            // Parse command format: /memory [agent_name] [block_name]
            let (agent_name, block_name) = if parts.len() == 1 {
                (None, None)
            } else if parts.len() == 2 {
                // Could be agent name or block name - try to determine
                // If it matches an agent name in group, treat as agent name
                if context.list_agents().contains(&parts[1].to_string()) {
                    (Some(parts[1]), None)
                } else {
                    (None, Some(parts[1..].join(" ")))
                }
            } else {
                // parts[1] is agent name, rest is block name
                (Some(parts[1]), Some(parts[2..].join(" ")))
            };

            match context.get_agent(agent_name).await {
                Ok(agent) => {
                    let agent_id = agent.id().to_string();
                    let runtime = agent.runtime();
                    let memory = runtime.memory();

                    if let Some(block_label) = block_name {
                        // Show specific block content
                        match memory.get_block(&agent_id, &block_label).await {
                            Ok(Some(doc)) => {
                                output.section(&format!("Memory Block: {}", block_label));
                                output.kv("Schema", &format!("{:?}", doc.schema()));
                                let content = doc.render();
                                output.kv("Characters", &content.len().to_string());
                                output.print("");
                                output.print(&content);
                            }
                            Ok(None) => {
                                output.error(&format!("Memory block '{}' not found", block_label));
                            }
                            Err(e) => {
                                output.error(&format!("Failed to get memory block: {}", e));
                            }
                        }
                    } else {
                        // List all blocks
                        match memory.list_blocks(&agent_id).await {
                            Ok(blocks) => {
                                output.section(&format!("Memory Blocks for {}", agent.name()));
                                if blocks.is_empty() {
                                    output.status("No memory blocks found");
                                } else {
                                    for block in blocks {
                                        let desc = if block.description.is_empty() {
                                            "no description"
                                        } else {
                                            &block.description
                                        };
                                        output.list_item(&format!(
                                            "{} ({:?}) - {}",
                                            block.label.bright_cyan(),
                                            block.block_type,
                                            desc
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                output.error(&format!("Failed to list memory blocks: {}", e));
                            }
                        }
                    }
                }
                Err(e) => {
                    output.error(&format!("Failed to get agent: {}", e));
                }
            }
            Ok(false)
        }
        "/archival" => {
            // TODO: Reimplement for pattern_db
            // Previously used agent.handle().search_archival_memories()
            output.warning("Archival search temporarily disabled during database migration");
            output.status("Reason: Agent handle pattern removed in new architecture");
            output.status("Use /memory to view core and working memory blocks");
            Ok(false)
        }
        "/context" => {
            // TODO: Reimplement for pattern_db
            // Previously used agent.handle().search_conversations()
            output.warning("Context view temporarily disabled during database migration");
            output.status("Reason: Agent handle pattern removed in new architecture");
            output.status("Conversation history available through runtime().messages()");
            Ok(false)
        }
        "/search" => {
            // TODO: Reimplement for pattern_db
            // Previously used agent.handle().search_conversations()
            output.warning("Conversation search temporarily disabled during database migration");
            output.status("Reason: Agent handle pattern removed in new architecture");
            Ok(false)
        }
        "/query" => {
            if parts.len() < 2 {
                output.error("Usage: /query <sql>");
                return Ok(false);
            }

            let sql = parts[1..].join(" ");
            output.status(&format!("Running query: {}", sql.bright_cyan()));

            // Run the query directly
            match crate::commands::db::query(&sql, output).await {
                Ok(_) => {
                    // Query output is handled by the query function
                }
                Err(e) => {
                    output.error(&format!("Query failed: {}", e));
                }
            }
            Ok(false)
        }
        _ => {
            output.error(&format!("Unknown command: {}", parts[0]));
            output.status("Type /help for available commands");
            Ok(false)
        }
    }
}
