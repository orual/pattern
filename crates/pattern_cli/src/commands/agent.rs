//! Agent management commands for the Pattern CLI
//!
//! This module provides commands for creating, listing, and managing agents.
//!
//! Uses pattern_db::queries for database access via shared helpers.

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::PatternConfig;
use std::path::Path;

use crate::helpers::{get_db, require_agent_by_name};
use crate::output::Output;

// =============================================================================
// Agent Listing
// =============================================================================

/// List all agents in the database
pub async fn list(config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let agents = pattern_db::queries::list_agents(db.pool())
        .await
        .map_err(|e| miette::miette!("Failed to list agents: {}", e))?;

    if agents.is_empty() {
        output.info(
            "No agents found",
            "Create one with: pattern-cli agent create <name>",
        );
        return Ok(());
    }

    output.status(&format!("Found {} agent(s):", agents.len()));
    output.status("");

    for agent in agents {
        output.info("•", &agent.name.bright_cyan().to_string());
        output.kv("  ID", &agent.id);
        output.kv(
            "  Model",
            &format!("{}/{}", agent.model_provider, agent.model_name),
        );
        output.kv("  Status", &format!("{:?}", agent.status));
        if let Some(desc) = &agent.description {
            output.kv("  Description", desc);
        }
        output.status("");
    }

    Ok(())
}

// =============================================================================
// Agent Creation - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Parsed agent type from string
// 2. Created AgentRecord with default fields
// 3. Saved via agent.store_with_relations(&DB)
// 4. Created constellation membership
// 5. Optionally saved agent ID back to config
//
// Should use: RuntimeContext.create_agent(config)

/// Create a new agent
///
/// NOTE: Currently STUBBED. Should use RuntimeContext.create_agent().
pub async fn create(name: &str, agent_type: Option<&str>, _config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Agent creation for '{}' temporarily disabled during database migration",
        name.bright_cyan()
    ));

    if let Some(atype) = agent_type {
        output.info("Requested type:", atype);
    }

    output.info("Reason:", "Should use RuntimeContext.create_agent()");
    output.status("Previous functionality:");
    output.list_item("Parsed agent type");
    output.list_item("Created AgentRecord in SurrealDB");
    output.list_item("Created constellation membership");
    output.list_item("Saved agent ID to config file");

    // TODO: When RuntimeContext is available:
    //
    // let agent_config = AgentConfig {
    //     name: name.to_string(),
    //     ..config.agent.clone()
    // };
    // let agent = ctx.create_agent(&agent_config).await?;
    // output.success(&format!("Created agent '{}'", agent.name()));

    Ok(())
}

// =============================================================================
// Agent Status
// =============================================================================

/// Show detailed status for an agent
pub async fn status(name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Try to find agent by name using shared helper
    let agent = require_agent_by_name(&db, name).await?;

    output.status(&format!("Agent: {}", agent.name.bright_cyan()));
    output.status("");

    // Basic info
    output.kv("ID", &agent.id);
    output.kv("Status", &format!("{:?}", agent.status));
    output.kv(
        "Model",
        &format!("{}/{}", agent.model_provider, agent.model_name),
    );

    if let Some(desc) = &agent.description {
        output.kv("Description", desc);
    }

    // System prompt (truncated)
    let prompt_preview = if agent.system_prompt.len() > 200 {
        format!("{}...", &agent.system_prompt[..200])
    } else {
        agent.system_prompt.clone()
    };
    output.status("");
    output.status("System Prompt:");
    output.status(&format!("  {}", prompt_preview.dimmed()));

    // Timestamps
    output.status("");
    output.kv("Created", &agent.created_at.to_string());
    output.kv("Updated", &agent.updated_at.to_string());

    // Memory blocks
    let blocks = pattern_db::queries::list_blocks(db.pool(), &agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to list memory blocks: {}", e))?;

    if !blocks.is_empty() {
        output.status("");
        output.status(&format!("Memory Blocks ({}):", blocks.len()));
        for block in blocks {
            let preview = block.content_preview.as_deref().unwrap_or("(empty)");
            let preview_short = if preview.len() > 50 {
                format!("{}...", &preview[..50])
            } else {
                preview.to_string()
            };
            output.info(
                &format!(
                    "  {} ({})",
                    block.label,
                    format!("{:?}", block.block_type).to_lowercase()
                ),
                &preview_short.dimmed().to_string(),
            );
        }
    }

    // Enabled tools
    let tools = &agent.enabled_tools.0;
    if !tools.is_empty() {
        output.status("");
        output.status(&format!("Enabled Tools ({}):", tools.len()));
        for tool in tools {
            output.list_item(tool);
        }
    }

    Ok(())
}

// =============================================================================
// Agent Export - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Queried agent by name to get ID
// 2. Loaded full agent with relations
// 3. Got memory blocks via ops::get_agent_memories
// 4. Converted to AgentConfig format
// 5. Serialized to TOML and wrote to file
//
// Needs: pattern_db::queries::get_agent_memories()

/// Export agent configuration (persona and memory only)
///
/// NOTE: Currently STUBBED. Export functionality needs pattern_db queries.
pub async fn export(name: &str, output_path: Option<&Path>) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Agent export for '{}' temporarily disabled during database migration",
        name.bright_cyan()
    ));

    if let Some(path) = output_path {
        output.info("Requested output:", &path.display().to_string());
    }

    output.info("Reason:", "Needs pattern_db::queries::get_agent_memories()");
    output.status("Previous functionality:");
    output.list_item("Loaded agent record from database");
    output.list_item("Retrieved all memory blocks");
    output.list_item("Converted to TOML config format");
    output.list_item("Wrote to file (default: {name}.toml)");
    output.status("Note: Message history was NOT exported");

    Ok(())
}

// =============================================================================
// Workflow Rules - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Tool rules are stored in the agent record's tool_rules field.
// This needs pattern_db queries for:
// 1. get_agent_by_name() to find agent
// 2. update_agent() to save modified rules
//
// The rule types themselves are in pattern_core::agent::tool_rules

/// Add a workflow rule to an agent
///
/// NOTE: Currently STUBBED. Needs pattern_db agent queries.
pub async fn add_rule(
    agent_name: &str,
    rule_type: &str,
    tool_name: &str,
    _params: Option<&str>,
    _conditions: Option<&str>,
    _priority: u8,
) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Adding rule to '{}' temporarily disabled during database migration",
        agent_name.bright_cyan()
    ));

    output.info("Rule type:", rule_type);
    output.info("Tool:", tool_name);
    output.info("Reason:", "Needs pattern_db agent queries");
    output.status("Available rule types:");
    output.list_item("start-constraint - Call tool first");
    output.list_item("exit-loop - End after calling tool");
    output.list_item("continue-loop - Continue after tool");
    output.list_item("max-calls - Limit call count");
    output.list_item("cooldown - Minimum time between calls");
    output.list_item("requires-preceding - Dependencies");

    Ok(())
}

/// List workflow rules for an agent
///
/// NOTE: Currently STUBBED. Needs pattern_db agent queries.
pub async fn list_rules(agent_name: &str) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Rule listing for '{}' temporarily disabled during database migration",
        agent_name.bright_cyan()
    ));

    output.info("Reason:", "Needs pattern_db::queries::get_agent_by_name()");

    Ok(())
}

/// Remove workflow rules from an agent
///
/// NOTE: Currently STUBBED. Needs pattern_db agent queries.
pub async fn remove_rule(agent_name: &str, tool_name: &str, rule_type: Option<&str>) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Removing rule from '{}' temporarily disabled during database migration",
        agent_name.bright_cyan()
    ));

    output.info("Tool:", tool_name);
    if let Some(rt) = rule_type {
        output.info("Rule type:", rt);
    } else {
        output.info("Scope:", "All rules for this tool");
    }
    output.info("Reason:", "Needs pattern_db agent queries");

    Ok(())
}

// =============================================================================
// Interactive Rule Builder - Removed
// =============================================================================

// The interactive rule builder used stdin which isn't compatible with
// the async CLI. It will be reimplemented as a non-interactive command
// with full arguments, or as a TUI component.
