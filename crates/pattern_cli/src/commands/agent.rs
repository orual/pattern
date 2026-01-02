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
// Workflow Rules
// =============================================================================

/// Add a workflow rule to an agent
pub async fn add_rule(
    agent_name: &str,
    rule_type: &str,
    tool_name: &str,
    params: Option<&str>,
    conditions: Option<&str>,
    priority: u8,
) -> Result<()> {
    let output = Output::new();
    let config = crate::helpers::load_config().await?;
    let db = get_db(&config).await?;

    let agent = require_agent_by_name(&db, agent_name).await?;

    // Build the rule based on type
    let rule = match rule_type {
        "start-constraint" | "start_constraint" => {
            serde_json::json!({
                "type": "start_constraint",
                "priority": priority
            })
        }
        "exit-loop" | "exit_loop" => {
            serde_json::json!({
                "type": "exit_loop",
                "priority": priority
            })
        }
        "continue-loop" | "continue_loop" => {
            serde_json::json!({
                "type": "continue_loop",
                "priority": priority
            })
        }
        "max-calls" | "max_calls" => {
            let max = params.and_then(|p| p.parse::<u32>().ok()).unwrap_or(5);
            serde_json::json!({
                "type": "max_calls",
                "max": max,
                "priority": priority
            })
        }
        "cooldown" => {
            let seconds = params.and_then(|p| p.parse::<u64>().ok()).unwrap_or(60);
            serde_json::json!({
                "type": "cooldown",
                "seconds": seconds,
                "priority": priority
            })
        }
        "requires-preceding" | "requires_preceding" => {
            let deps: Vec<&str> = conditions
                .map(|c| c.split(',').map(|s| s.trim()).collect())
                .unwrap_or_default();
            serde_json::json!({
                "type": "requires_preceding",
                "tools": deps,
                "priority": priority
            })
        }
        _ => {
            return Err(miette::miette!(
                "Unknown rule type: {}. Valid types: start-constraint, exit-loop, continue-loop, max-calls, cooldown, requires-preceding",
                rule_type
            ));
        }
    };

    // Get existing rules or create new object
    let mut rules = agent
        .tool_rules
        .as_ref()
        .map(|r| r.0.clone())
        .unwrap_or_else(|| serde_json::json!({}));

    // Get or create array for this tool
    let tool_rules = rules
        .as_object_mut()
        .ok_or_else(|| miette::miette!("Invalid tool_rules format"))?
        .entry(tool_name)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| miette::miette!("Invalid tool rules array"))?;

    tool_rules.push(rule);

    // Save updated rules
    pattern_db::queries::update_agent_tool_rules(db.pool(), &agent.id, Some(rules))
        .await
        .map_err(|e| miette::miette!("Failed to update rules: {}", e))?;

    output.success(&format!(
        "Added {} rule for '{}' on agent '{}'",
        rule_type.bright_yellow(),
        tool_name.bright_cyan(),
        agent_name.bright_cyan()
    ));

    Ok(())
}

/// Remove workflow rules from an agent
pub async fn remove_rule(agent_name: &str, tool_name: &str, rule_type: Option<&str>) -> Result<()> {
    let output = Output::new();
    let config = crate::helpers::load_config().await?;
    let db = get_db(&config).await?;

    let agent = require_agent_by_name(&db, agent_name).await?;

    let mut rules = agent
        .tool_rules
        .as_ref()
        .map(|r| r.0.clone())
        .unwrap_or_else(|| serde_json::json!({}));

    let rules_obj = rules
        .as_object_mut()
        .ok_or_else(|| miette::miette!("Invalid tool_rules format"))?;

    if let Some(rt) = rule_type {
        // Remove specific rule type
        let normalized_type = rt.replace('-', "_");
        if let Some(tool_rules) = rules_obj.get_mut(tool_name) {
            if let Some(arr) = tool_rules.as_array_mut() {
                let before = arr.len();
                arr.retain(|r| r.get("type").and_then(|t| t.as_str()) != Some(&normalized_type));
                let removed = before - arr.len();
                if removed > 0 {
                    output.success(&format!(
                        "Removed {} '{}' rule(s) for '{}'",
                        removed, rt, tool_name
                    ));
                } else {
                    output.warning(&format!("No '{}' rules found for '{}'", rt, tool_name));
                }
            }
        } else {
            output.warning(&format!("No rules found for tool '{}'", tool_name));
            return Ok(());
        }
    } else {
        // Remove all rules for tool
        if rules_obj.remove(tool_name).is_some() {
            output.success(&format!("Removed all rules for '{}'", tool_name));
        } else {
            output.warning(&format!("No rules found for tool '{}'", tool_name));
            return Ok(());
        }
    }

    // Save updated rules
    let new_rules = if rules_obj.is_empty() {
        None
    } else {
        Some(rules)
    };

    pattern_db::queries::update_agent_tool_rules(db.pool(), &agent.id, new_rules)
        .await
        .map_err(|e| miette::miette!("Failed to update rules: {}", e))?;

    Ok(())
}

// =============================================================================
// Quick Add/Remove Commands
// =============================================================================

/// Add a data source subscription to an agent (interactive or from TOML file)
pub async fn add_source(
    agent_name: &str,
    source_name: &str,
    source_type: Option<&str>,
    from_toml: Option<&std::path::Path>,
    config: &PatternConfig,
) -> Result<()> {
    use crate::commands::builder::editors::select_menu;
    use crate::data_source_config;

    let output = Output::new();
    let db = get_db(config).await?;

    let mut agent = require_agent_by_name(&db, agent_name).await?;

    // Parse existing config
    let mut agent_config: pattern_core::config::AgentConfig =
        serde_json::from_value(agent.config.0.clone()).map_err(|e| {
            miette::miette!("Failed to parse agent config for '{}': {}", agent_name, e)
        })?;

    // Check if source already exists
    if agent_config.data_sources.contains_key(source_name) {
        output.warning(&format!(
            "Data source '{}' already exists on agent '{}'",
            source_name.bright_cyan(),
            agent_name.bright_cyan()
        ));
        return Ok(());
    }

    // Build the data source config
    let data_source = if let Some(toml_path) = from_toml {
        // Load from TOML file
        data_source_config::load_source_from_toml(toml_path)?
    } else {
        // Interactive builder
        let stype = if let Some(t) = source_type {
            t.to_string()
        } else {
            // Prompt for type
            let source_types = ["bluesky", "discord", "file", "custom"];
            let idx = select_menu("Source type", &source_types, 0)?;
            source_types[idx].to_string()
        };
        data_source_config::build_source_interactive(source_name, &stype)?
    };

    // Show summary
    println!(
        "\n{}",
        data_source_config::render_source_summary(source_name, &data_source)
    );

    // Add to config
    agent_config
        .data_sources
        .insert(source_name.to_string(), data_source);

    // Update agent's config JSON
    agent.config = pattern_db::Json(
        serde_json::to_value(&agent_config)
            .map_err(|e| miette::miette!("Failed to serialize config: {}", e))?,
    );

    // Save to database
    pattern_db::queries::update_agent(db.pool(), &agent)
        .await
        .map_err(|e| miette::miette!("Failed to update agent: {}", e))?;

    output.success(&format!(
        "Added source '{}' to agent '{}'",
        source_name.bright_cyan(),
        agent_name.bright_cyan()
    ));

    Ok(())
}

/// Add a memory block to an agent
pub async fn add_memory(
    agent_name: &str,
    label: &str,
    content: Option<&str>,
    path: Option<&std::path::Path>,
    memory_type: &str,
    permission: &str,
    pinned: bool,
    config: &PatternConfig,
) -> Result<()> {
    use pattern_core::memory::{BlockSchema, BlockType, MemoryCache, MemoryStore};
    use std::sync::Arc;

    let output = Output::new();
    let dbs = crate::helpers::get_dbs(config).await?;

    let agent = pattern_db::queries::get_agent_by_name(dbs.constellation.pool(), agent_name)
        .await
        .map_err(|e| miette::miette!("Database error: {}", e))?
        .ok_or_else(|| miette::miette!("Agent '{}' not found", agent_name))?;

    // Determine content
    let block_content = if let Some(c) = content {
        c.to_string()
    } else if let Some(p) = path {
        std::fs::read_to_string(p)
            .map_err(|e| miette::miette!("Failed to read file '{}': {}", p.display(), e))?
    } else {
        String::new()
    };

    // Parse block type using FromStr
    let block_type: BlockType = memory_type.parse().map_err(|e| miette::miette!("{}", e))?;

    // Parse permission using FromStr (we'll need this when we can set it on blocks)
    let _permission: pattern_core::memory::MemoryPermission =
        permission.parse().map_err(|e| miette::miette!("{}", e))?;

    // Create memory cache
    let cache = MemoryCache::new(Arc::new(dbs));

    // Create the block (now returns StructuredDocument directly)
    let doc = cache
        .create_block(
            &agent.id,
            label,
            &format!("Memory block: {}", label),
            block_type,
            BlockSchema::text(),
            2000, // default char limit
        )
        .await
        .map_err(|e| miette::miette!("Failed to create memory block: {:?}", e))?;
    let block_id = doc.id().to_string();

    // Set initial content if provided
    if !block_content.is_empty() {
        doc.set_text(&block_content, true)
            .map_err(|e| miette::miette!("Failed to set content: {}", e))?;
        cache
            .persist_block(&agent.id, label)
            .await
            .map_err(|e| miette::miette!("Failed to persist block: {:?}", e))?;
    }

    // Set pinned status if requested
    if pinned {
        cache
            .set_block_pinned(&agent.id, label, true)
            .await
            .map_err(|e| miette::miette!("Failed to pin block: {:?}", e))?;
    }

    output.success(&format!(
        "Added memory block '{}' to agent '{}'",
        label.bright_cyan(),
        agent_name.bright_cyan()
    ));
    output.kv("Block ID", &block_id);
    output.kv("Type", &block_type.to_string());
    output.kv("Permission", permission);
    if pinned {
        output.kv("Pinned", "yes");
    }

    Ok(())
}

/// Enable a tool for an agent
pub async fn add_tool(agent_name: &str, tool_name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let mut agent = require_agent_by_name(&db, agent_name).await?;

    let mut tools = agent.enabled_tools.0.clone();
    if tools.contains(&tool_name.to_string()) {
        output.info("Tool already enabled", tool_name);
        return Ok(());
    }

    tools.push(tool_name.to_string());
    agent.enabled_tools = pattern_db::Json(tools);

    pattern_db::queries::update_agent(db.pool(), &agent)
        .await
        .map_err(|e| miette::miette!("Failed to update agent: {}", e))?;

    output.success(&format!(
        "Enabled tool '{}' for agent '{}'",
        tool_name.bright_yellow(),
        agent_name.bright_cyan()
    ));

    Ok(())
}

/// Remove a data source subscription from an agent
pub async fn remove_source(
    agent_name: &str,
    source_name: &str,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let mut agent = require_agent_by_name(&db, agent_name).await?;

    // Parse existing config
    let mut agent_config: pattern_core::config::AgentConfig =
        serde_json::from_value(agent.config.0.clone()).map_err(|e| {
            miette::miette!("Failed to parse agent config for '{}': {}", agent_name, e)
        })?;

    // Check if source exists
    if !agent_config.data_sources.contains_key(source_name) {
        output.warning(&format!(
            "Data source '{}' not found on agent '{}'",
            source_name.bright_cyan(),
            agent_name.bright_cyan()
        ));
        return Ok(());
    }

    // Remove from config
    agent_config.data_sources.remove(source_name);

    // Update agent's config JSON
    agent.config = pattern_db::Json(
        serde_json::to_value(&agent_config)
            .map_err(|e| miette::miette!("Failed to serialize config: {}", e))?,
    );

    // Save to database
    pattern_db::queries::update_agent(db.pool(), &agent)
        .await
        .map_err(|e| miette::miette!("Failed to update agent: {}", e))?;

    output.success(&format!(
        "Removed source '{}' from agent '{}'",
        source_name.bright_cyan(),
        agent_name.bright_cyan()
    ));

    Ok(())
}

/// Remove a memory block from an agent
pub async fn remove_memory(agent_name: &str, label: &str, config: &PatternConfig) -> Result<()> {
    use pattern_core::memory::{MemoryCache, MemoryStore};
    use std::sync::Arc;

    let output = Output::new();
    let dbs = crate::helpers::get_dbs(config).await?;

    let agent = pattern_db::queries::get_agent_by_name(dbs.constellation.pool(), agent_name)
        .await
        .map_err(|e| miette::miette!("Database error: {}", e))?
        .ok_or_else(|| miette::miette!("Agent '{}' not found", agent_name))?;

    // Create memory cache and delete the block
    let cache = MemoryCache::new(Arc::new(dbs));

    cache
        .delete_block(&agent.id, label)
        .await
        .map_err(|e| miette::miette!("Failed to delete block '{}': {:?}", label, e))?;

    output.success(&format!(
        "Removed memory block '{}' from agent '{}'",
        label.bright_cyan(),
        agent_name.bright_cyan()
    ));

    Ok(())
}

/// Disable a tool for an agent
pub async fn remove_tool(agent_name: &str, tool_name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let mut agent = require_agent_by_name(&db, agent_name).await?;

    let mut tools = agent.enabled_tools.0.clone();
    if !tools.contains(&tool_name.to_string()) {
        output.info("Tool not enabled", tool_name);
        return Ok(());
    }

    tools.retain(|t| t != tool_name);
    agent.enabled_tools = pattern_db::Json(tools);

    pattern_db::queries::update_agent(db.pool(), &agent)
        .await
        .map_err(|e| miette::miette!("Failed to update agent: {}", e))?;

    output.success(&format!(
        "Disabled tool '{}' for agent '{}'",
        tool_name.bright_yellow(),
        agent_name.bright_cyan()
    ));

    Ok(())
}
