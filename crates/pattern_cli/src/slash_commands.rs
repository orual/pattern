//! Slash command handling for interactive chat modes
//!
//! This module handles slash commands in the CLI chat interface.
//! Commands provide quick access to agent information, memory inspection,
//! and database queries.
//!
//! All commands are fully implemented using pattern_db.

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::{
    Agent,
    coordination::groups::{AgentGroup, AgentWithMembership},
    messages::ChatRole,
};
use pattern_db::ConstellationDb;
use std::sync::Arc;

use crate::output::Output;

// ============================================================================
// Constants
// ============================================================================

/// Default number of archival search results.
const DEFAULT_ARCHIVAL_SEARCH_LIMIT: usize = 20;

/// Maximum characters for content preview in archival results.
const ARCHIVAL_PREVIEW_CHARS: usize = 200;

/// Maximum characters for message preview.
const MESSAGE_PREVIEW_CHARS: usize = 60;

/// Maximum characters for system prompt preview.
const SYSTEM_PROMPT_PREVIEW_CHARS: usize = 100;

// ============================================================================
// Display Helpers
// ============================================================================

/// Truncate content with an ellipsis suffix if it exceeds the limit.
fn truncate_with_ellipsis(content: &str, max_chars: usize) -> String {
    let truncated: String = content.chars().take(max_chars).collect();
    if content.chars().count() > max_chars {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

/// Format a ChatRole with appropriate terminal color.
fn format_role_colored(role: &ChatRole) -> String {
    match role {
        ChatRole::User => "user".bright_green().to_string(),
        ChatRole::Assistant => "assistant".bright_cyan().to_string(),
        ChatRole::System => "system".bright_yellow().to_string(),
        ChatRole::Tool => "tool".bright_magenta().to_string(),
    }
}

/// Extract a text preview from message content.
///
/// Tries content_preview first, then falls back to extracting text from
/// the raw content. Replaces newlines with spaces for single-line display.
fn extract_message_preview(msg: &pattern_core::messages::Message) -> String {
    use pattern_core::messages::{ContentBlock, ContentPart, MessageContent};

    let raw_preview = match &msg.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                ContentBlock::Thinking { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        MessageContent::ToolCalls(_) => "(tool calls)".to_string(),
        MessageContent::ToolResponses(responses) => {
            if responses.is_empty() {
                "(empty tool response)".to_string()
            } else {
                responses
                    .iter()
                    .map(|r| r.content.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
    };

    if raw_preview.is_empty() {
        "(no content)".to_string()
    } else {
        raw_preview.replace('\n', " ")
    }
}

// ============================================================================
// Command Context
// ============================================================================

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

// ============================================================================
// Main Command Handler
// ============================================================================

/// Handle slash commands in chat
///
/// The `db` parameter provides access to the constellation database for commands
/// like `/list` that need to query agent information directly.
pub async fn handle_slash_command(
    command: &str,
    context: CommandContext<'_>,
    output: &Output,
    db: &ConstellationDb,
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
            output.list_item("/archival <query> - Search archival memory");
            output.print("");
            output.section("Permission Commands");
            output.list_item("/permit <id> [once|always|ttl=N] - Approve a permission request");
            output.list_item("/deny <id> - Deny a permission request");
            output.print("");
            output.section("Context Commands");
            output.list_item(
                "/context [agent] [batch_id] - Show the actual context window sent to model",
            );
            output.print("");
            output.section("Database Commands");
            output.list_item("/list - List all agents from database");
            output.list_item("/query <sql> - Run a database query");
            output.print("");
            output.section("Search Commands");
            output.list_item("/search [domain] <query> - Search memory (domains: messages, archival, blocks, all)");
            output.print("");
            Ok(false)
        }
        "/exit" | "/quit" => Ok(true),
        "/list" => {
            // Query agents directly from the database (correct - lists ALL agents, not just current)
            match pattern_db::queries::list_agents(db.pool()).await {
                Ok(mut agents) => {
                    // Sort agents by name for consistent display
                    agents.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

                    output.section(&format!("Agents in Database ({})", agents.len()));
                    if agents.is_empty() {
                        output.status("No agents found in database");
                    } else {
                        for agent in agents {
                            let status_str = match agent.status {
                                pattern_db::models::AgentStatus::Active => {
                                    "active".green().to_string()
                                }
                                pattern_db::models::AgentStatus::Hibernated => {
                                    "hibernated".yellow().to_string()
                                }
                                pattern_db::models::AgentStatus::Archived => {
                                    "archived".red().to_string()
                                }
                            };
                            output.list_item(&format!(
                                "{} ({}) - {}",
                                agent.name.bright_cyan(),
                                agent.model_name.dimmed(),
                                status_str
                            ));
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to list agents from database");
                    output.error(&format!("Failed to list agents: {}", e));
                }
            }

            // Also show agents in current runtime context
            output.print("");
            let context_agents = context.list_agents();
            output.section(&format!(
                "Agents in Current Context ({})",
                context_agents.len()
            ));
            if context_agents.is_empty() {
                output.status("No agents loaded in current context");
            } else {
                let mut sorted_agents = context_agents;
                sorted_agents.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
                for name in sorted_agents {
                    output.list_item(&name.bright_cyan().to_string());
                }
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
                                // Use Display impl for block_type
                                output.list_item(&format!(
                                    "{} ({})",
                                    block.label.bright_cyan(),
                                    block.block_type
                                ));
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %agent_id,
                                error = %e,
                                "Failed to list memory blocks in /status"
                            );
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
                                tracing::warn!(
                                    agent_id = %agent_id,
                                    block_label = %block_label,
                                    error = %e,
                                    "Failed to get memory block in /memory"
                                );
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
                                        // Use Display impl for block_type
                                        output.list_item(&format!(
                                            "{} ({}) - {}",
                                            block.label.bright_cyan(),
                                            block.block_type,
                                            desc
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    agent_id = %agent_id,
                                    error = %e,
                                    "Failed to list memory blocks in /memory"
                                );
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
            let query = if parts.len() > 1 {
                parts[1..].join(" ")
            } else {
                output.error("Usage: /archival <search query>");
                return Ok(false);
            };

            // Validate query syntax before searching
            if let Err(e) = pattern_db::fts::validate_fts_query(&query) {
                output.error(&format!("Invalid query: {}", e));
                return Ok(false);
            }

            match context.get_agent(None).await {
                Ok(agent) => {
                    let agent_id = agent.id().to_string();
                    let runtime = agent.runtime();
                    let memory = runtime.memory();

                    // Use runtime's memory store for archival search
                    match memory
                        .search_archival(&agent_id, &query, DEFAULT_ARCHIVAL_SEARCH_LIMIT)
                        .await
                    {
                        Ok(results) => {
                            output.section(&format!("Archival Search: '{}'", query));
                            if results.is_empty() {
                                output.status("No results found");
                            } else {
                                for (i, result) in results.iter().enumerate() {
                                    let preview = truncate_with_ellipsis(
                                        &result.content,
                                        ARCHIVAL_PREVIEW_CHARS,
                                    );
                                    output.print(&format!("{}. {}", i + 1, preview));
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %agent_id,
                                query = %query,
                                error = %e,
                                "Archival search failed"
                            );
                            output.error(&format!("Search failed: {}", e));
                        }
                    }
                }
                Err(e) => output.error(&format!("Failed to get agent: {}", e)),
            }
            Ok(false)
        }
        "/context" => {
            // Parse: /context [agent] [batch_id]
            let (agent_name, batch_id_str) = match parts.len() {
                1 => (None, None),
                2 => {
                    // Could be agent name or batch ID - if it's numeric, treat as batch
                    if parts[1].parse::<u64>().is_ok() {
                        (None, Some(parts[1]))
                    } else {
                        (Some(parts[1]), None)
                    }
                }
                _ => (Some(parts[1]), Some(parts[2])),
            };

            // Parse batch ID if provided (supports base32 or raw u64)
            let active_batch: Option<pattern_core::SnowflakePosition> =
                batch_id_str.and_then(|s| s.parse().ok());

            match context.get_agent(agent_name).await {
                Ok(agent) => {
                    let agent_id = agent.id().to_string();
                    let runtime = agent.runtime();

                    output.section(&format!(
                        "Context Window for: {}",
                        agent.name().bright_cyan()
                    ));

                    if let Some(batch) = active_batch {
                        output.status(&format!("Including active batch: {}", batch));
                    }

                    // Get base_instructions from database
                    let base_instructions = match pattern_db::queries::get_agent(
                        db.pool(),
                        &agent_id,
                    )
                    .await
                    {
                        Ok(Some(ref db_agent)) => Some(db_agent.system_prompt.clone()),
                        Ok(None) => {
                            tracing::warn!(agent_id = %agent_id, "Agent not found in database");
                            None
                        }
                        Err(e) => {
                            tracing::error!(agent_id = %agent_id, error = %e, "Failed to get agent from database");
                            output.warning(&format!("Could not load base instructions: {}", e));
                            None
                        }
                    };

                    // Build the ACTUAL context using prepare_request with empty messages
                    match runtime
                        .prepare_request(
                            Vec::<pattern_core::messages::Message>::new(),
                            None,
                            active_batch,
                            None,
                            base_instructions.as_deref(),
                        )
                        .await
                    {
                        Ok(request) => {
                            // 1. System prompt section (composed from base + memory blocks + tool rules)
                            output.print("");
                            if let Some(ref system_parts) = request.system {
                                let total_chars: usize = system_parts.iter().map(|s| s.len()).sum();
                                output.kv(
                                    "System Prompt",
                                    &format!(
                                        "({} parts, {} chars total)",
                                        system_parts.len(),
                                        total_chars
                                    )
                                    .dimmed()
                                    .to_string(),
                                );

                                for (i, part) in system_parts.iter().enumerate() {
                                    let part_type = if i == 0 {
                                        "base instructions"
                                    } else if part.contains("<block:") {
                                        "memory block"
                                    } else if part.contains("# Tool Execution Rules") {
                                        "tool rules"
                                    } else {
                                        "section"
                                    };

                                    let preview = truncate_with_ellipsis(
                                        &part.replace('\n', " "),
                                        SYSTEM_PROMPT_PREVIEW_CHARS,
                                    );
                                    output.list_item(&format!(
                                        "[{}] {} chars - {}",
                                        part_type.dimmed(),
                                        part.len(),
                                        preview.dimmed()
                                    ));
                                }
                            } else {
                                output.kv("System Prompt", &"(none)".dimmed().to_string());
                            }

                            // 2. Tools section
                            output.print("");
                            if let Some(ref tools) = request.tools {
                                output.kv(
                                    "Tools",
                                    &format!("({})", tools.len()).dimmed().to_string(),
                                );
                                let tool_names: Vec<_> =
                                    tools.iter().map(|t| t.name.as_str()).collect();
                                output.status(&tool_names.join(", "));
                            } else {
                                output.kv("Tools", &"(none)".dimmed().to_string());
                            }

                            // 3. Messages section (as they would be sent to model)
                            output.print("");
                            output.kv(
                                "Messages",
                                &format!("({})", request.messages.len()).dimmed().to_string(),
                            );

                            if request.messages.is_empty() {
                                output.status("No messages in context");
                            } else {
                                for msg in &request.messages {
                                    let role_str = format_role_colored(&msg.role);
                                    let preview = extract_message_preview(msg);
                                    let truncated =
                                        truncate_with_ellipsis(&preview, MESSAGE_PREVIEW_CHARS);

                                    // Show batch info if present
                                    let batch_info = msg
                                        .batch
                                        .map(|b| format!(" [batch:{}]", b).dimmed().to_string())
                                        .unwrap_or_default();

                                    output.list_item(&format!(
                                        "[{}]{} {}",
                                        role_str, batch_info, truncated
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                agent_id = %agent_id,
                                error = %e,
                                "Failed to build context preview"
                            );
                            output.error(&format!("Failed to build context: {}", e));
                        }
                    }

                    output.print("");
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to get agent for /context");
                    output.error(&format!("Failed to get agent: {}", e));
                }
            }
            Ok(false)
        }
        "/search" => {
            // Syntax: /search [domain] <query>
            // domain: messages, archival, blocks, all (default)
            if parts.len() < 2 {
                output.error("Usage: /search [domain] <query>");
                output.status("Domains: messages, archival, blocks, all (default)");
                return Ok(false);
            }

            // Check if first arg is a domain filter
            let (domain, query) = match parts[1] {
                "messages" | "archival" | "blocks" | "all" => {
                    if parts.len() < 3 {
                        output.error("Missing search query after domain");
                        return Ok(false);
                    }
                    (Some(parts[1]), parts[2..].join(" "))
                }
                _ => (None, parts[1..].join(" ")),
            };

            // Validate query
            if let Err(e) = pattern_db::fts::validate_fts_query(&query) {
                output.error(&format!("Invalid query: {}", e));
                return Ok(false);
            }

            match context.get_agent(None).await {
                Ok(agent) => {
                    let agent_id = agent.id().to_string();
                    let runtime = agent.runtime();
                    let memory = runtime.memory();

                    // Build search options based on domain
                    use pattern_core::memory::SearchOptions;
                    let options = match domain {
                        Some("messages") => SearchOptions::new().messages_only().limit(20),
                        Some("archival") => SearchOptions::new().archival_only().limit(20),
                        Some("blocks") => SearchOptions::new().blocks_only().limit(20),
                        _ => SearchOptions::new().limit(20), // all domains
                    };

                    let domain_str = domain.unwrap_or("all");
                    output.section(&format!("Search [{}]: '{}'", domain_str, query));

                    match memory.search(&agent_id, &query, options).await {
                        Ok(results) => {
                            if results.is_empty() {
                                output.status("No results found");
                            } else {
                                for (i, result) in results.iter().enumerate() {
                                    let type_str = match result.content_type {
                                        pattern_core::memory::SearchContentType::Messages => "msg",
                                        pattern_core::memory::SearchContentType::Archival => "arc",
                                        pattern_core::memory::SearchContentType::Blocks => "blk",
                                    };

                                    let preview = result
                                        .content
                                        .as_deref()
                                        .map(|c| {
                                            truncate_with_ellipsis(
                                                &c.replace('\n', " "),
                                                ARCHIVAL_PREVIEW_CHARS,
                                            )
                                        })
                                        .unwrap_or_else(|| "(no content)".to_string());

                                    output.list_item(&format!(
                                        "{}. [{}] {:.2} - {}",
                                        i + 1,
                                        type_str.dimmed(),
                                        result.score,
                                        preview
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %agent_id,
                                query = %query,
                                error = %e,
                                "Search failed"
                            );
                            output.error(&format!("Search failed: {}", e));
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to get agent for /search");
                    output.error(&format!("Failed to get agent: {}", e));
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
