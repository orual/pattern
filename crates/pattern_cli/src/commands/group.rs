//! Agent group management commands for the Pattern CLI
//!
//! This module provides commands for creating, listing, and managing agent groups
//! with various coordination patterns.
//!
//! Uses pattern_db::queries for database access via shared helpers.

use chrono::Utc;
use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::PatternConfig;
use pattern_db::models::{GroupMember, GroupMemberRole};
use std::path::Path;

use crate::helpers::{get_db, require_agent_by_name, require_group_by_name};
use crate::output::Output;

/// Parse member role from string
fn parse_role(s: &str) -> Result<GroupMemberRole> {
    match s.to_lowercase().as_str() {
        "supervisor" => Ok(GroupMemberRole::Supervisor),
        "regular" => Ok(GroupMemberRole::Regular),

        "observer" => Ok(GroupMemberRole::Observer),

        _ => Err(miette::miette!(
            "Unknown role: '{}'. Valid roles: supervisor, worker (or regular), observer",
            s
        )),
    }
}

// =============================================================================
// Group Listing
// =============================================================================

/// List all groups
pub async fn list(config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let groups = pattern_db::queries::list_groups(db.pool())
        .await
        .map_err(|e| miette::miette!("Failed to list groups: {}", e))?;

    if groups.is_empty() {
        output.info(
            "No groups found",
            "Create one with: pattern-cli group create <name>",
        );
        return Ok(());
    }

    output.status(&format!("Found {} group(s):", groups.len()));
    output.status("");

    for group in groups {
        // Get member count
        let members = pattern_db::queries::get_group_members(db.pool(), &group.id)
            .await
            .map_err(|e| miette::miette!("Failed to get group members: {}", e))?;

        output.info("•", &group.name.bright_cyan().to_string());
        output.kv("  ID", &group.id);
        output.kv("  Pattern", &format!("{:?}", group.pattern_type));
        output.kv("  Members", &members.len().to_string());
        if let Some(desc) = &group.description {
            output.kv("  Description", desc);
        }
        output.status("");
    }

    Ok(())
}

// =============================================================================
// Add Member
// =============================================================================

/// Add an agent to a group
pub async fn add_member(
    group_name: &str,
    agent_name: &str,
    role: &str,
    capabilities: Option<&str>,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Find group and agent by name using shared helpers
    let group = require_group_by_name(&db, group_name).await?;
    let agent = require_agent_by_name(&db, agent_name).await?;

    let member_role = parse_role(role)?;

    // Parse comma-separated capabilities
    let caps: Vec<String> = capabilities
        .map(|s| {
            s.split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let member = GroupMember {
        group_id: group.id.clone(),
        agent_id: agent.id.clone(),
        role: Some(pattern_db::Json(member_role.clone())),
        capabilities: pattern_db::Json(caps.clone()),
        joined_at: Utc::now(),
    };

    pattern_db::queries::add_group_member(db.pool(), &member)
        .await
        .map_err(|e| miette::miette!("Failed to add member: {}", e))?;

    output.success(&format!(
        "Added '{}' to group '{}'",
        agent_name.bright_cyan(),
        group_name.bright_cyan()
    ));
    output.kv("Role", &format!("{:?}", member_role));
    if !caps.is_empty() {
        output.kv("Capabilities", &caps.join(", "));
    }

    Ok(())
}

// =============================================================================
// Group Status
// =============================================================================

/// Show group status and members
pub async fn status(name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Find group by name using shared helper
    let group = require_group_by_name(&db, name).await?;

    // Get members
    let members = pattern_db::queries::get_group_members(db.pool(), &group.id)
        .await
        .map_err(|e| miette::miette!("Failed to get group members: {}", e))?;

    output.status(&format!("Group: {}", group.name.bright_cyan()));
    output.status("");

    // Basic info
    output.kv("ID", &group.id);
    output.kv("Pattern", &format!("{:?}", group.pattern_type));
    if let Some(desc) = &group.description {
        output.kv("Description", desc);
    }

    // Timestamps
    output.status("");
    output.kv("Created", &group.created_at.to_string());
    output.kv("Updated", &group.updated_at.to_string());

    // Members
    output.status("");
    output.status(&format!("Members ({}):", members.len()));

    if members.is_empty() {
        output.info("  (no members)", "Add with: pattern-cli group add-member");
    } else {
        for member in members {
            // Look up agent name
            let agent = pattern_db::queries::get_agent(db.pool(), &member.agent_id)
                .await
                .map_err(|e| miette::miette!("Failed to get agent: {}", e))?;

            let agent_name = agent
                .map(|a| a.name)
                .unwrap_or_else(|| member.agent_id.clone());
            let role_str = member
                .role
                .map(|r| format!("{:?}", r))
                .unwrap_or_else(|| "None".to_string());

            output.info(
                &format!("  • {}", agent_name.bright_cyan()),
                &role_str.dimmed().to_string(),
            );
        }
    }

    Ok(())
}

// =============================================================================
// Group Export
// =============================================================================

/// Export group configuration (members and pattern only)
///
/// NOTE: Export functionality creates a TOML config that can be used to recreate the group.
pub async fn export(name: &str, output_path: Option<&Path>, config: &PatternConfig) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Find group by name using shared helper
    let group = require_group_by_name(&db, name).await?;

    // Get members
    let members = pattern_db::queries::get_group_members(db.pool(), &group.id)
        .await
        .map_err(|e| miette::miette!("Failed to get group members: {}", e))?;

    // Build export structure
    let mut member_configs = Vec::new();
    for member in &members {
        let agent = pattern_db::queries::get_agent(db.pool(), &member.agent_id)
            .await
            .map_err(|e| miette::miette!("Failed to get agent: {}", e))?;

        if let Some(agent) = agent {
            member_configs.push(serde_json::json!({
                "agent_id": agent.id,
                "name": agent.name,
                "role": member.role.as_ref().map(|r| format!("{:?}", r.0).to_lowercase()),
            }));
        }
    }

    let export_data = serde_json::json!({
        "name": group.name,
        "description": group.description,
        "pattern": format!("{:?}", group.pattern_type).to_lowercase(),
        "pattern_config": group.pattern_config.0,
        "members": member_configs,
    });

    // Convert to TOML
    let toml_str = toml::to_string_pretty(&export_data)
        .map_err(|e| miette::miette!("Failed to serialize to TOML: {}", e))?;

    // Determine output path
    let path = output_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(format!("{}_group.toml", name)));

    // Write to file
    std::fs::write(&path, toml_str).map_err(|e| miette::miette!("Failed to write file: {}", e))?;

    output.success(&format!(
        "Exported group '{}' to {}",
        name.bright_cyan(),
        path.display()
    ));
    output.kv("Members", &members.len().to_string());

    Ok(())
}

// =============================================================================
// Quick Add/Remove Commands
// =============================================================================

/// Add a shared memory block to a group
pub async fn add_memory(
    group_name: &str,
    label: &str,
    content: Option<&str>,
    path: Option<&std::path::Path>,
    config: &PatternConfig,
) -> Result<()> {
    use pattern_core::memory::{
        BlockSchema, BlockType, MemoryCache, MemoryStore, SharedBlockManager,
    };
    use pattern_db::models::MemoryPermission;
    use std::sync::Arc;

    let output = Output::new();
    let dbs = crate::helpers::get_dbs(config).await?;
    let dbs = Arc::new(dbs);

    let group = pattern_db::queries::get_group_by_name(dbs.constellation.pool(), group_name)
        .await
        .map_err(|e| miette::miette!("Database error: {}", e))?
        .ok_or_else(|| miette::miette!("Group '{}' not found", group_name))?;

    // Get all members of the group
    let members = pattern_db::queries::get_group_members(dbs.constellation.pool(), &group.id)
        .await
        .map_err(|e| miette::miette!("Failed to get group members: {}", e))?;

    // Determine content
    let block_content = if let Some(c) = content {
        c.to_string()
    } else if let Some(p) = path {
        std::fs::read_to_string(p)
            .map_err(|e| miette::miette!("Failed to read file '{}': {}", p.display(), e))?
    } else {
        String::new()
    };

    // Create memory cache
    let cache = MemoryCache::new(dbs.clone());

    // Create the block with group as owner
    let block_id = cache
        .create_block(
            &group.id,
            label,
            &format!("Shared memory for group {}: {}", group_name, label),
            BlockType::Working,
            BlockSchema::text(),
            2000,
        )
        .await
        .map_err(|e| miette::miette!("Failed to create shared memory block: {:?}", e))?;

    // Set initial content if provided
    if !block_content.is_empty() {
        if let Some(doc) = cache
            .get_block(&group.id, label)
            .await
            .map_err(|e| miette::miette!("Failed to get block: {:?}", e))?
        {
            doc.set_text(&block_content, true)
                .map_err(|e| miette::miette!("Failed to set content: {}", e))?;
            cache
                .persist_block(&group.id, label)
                .await
                .map_err(|e| miette::miette!("Failed to persist block: {:?}", e))?;
        }
    }

    // Share the block with all agents in the group
    let sharing_manager = SharedBlockManager::new(dbs);
    for member in &members {
        sharing_manager
            .share_block(&block_id, &member.agent_id, MemoryPermission::ReadWrite)
            .await
            .map_err(|e| {
                miette::miette!(
                    "Failed to share block with agent '{}': {:?}",
                    member.agent_id,
                    e
                )
            })?;
    }

    output.success(&format!(
        "Added shared memory block '{}' to group '{}'",
        label.bright_cyan(),
        group_name.bright_cyan()
    ));
    output.kv("Block ID", &block_id);
    output.kv("Shared with", &format!("{} agent(s)", members.len()));

    Ok(())
}

/// Add a data source subscription to a group (interactive or from TOML file)
pub async fn add_source(
    group_name: &str,
    source_name: &str,
    source_type: Option<&str>,
    from_toml: Option<&std::path::Path>,
    config: &PatternConfig,
) -> Result<()> {
    use crate::commands::builder::editors::select_menu;
    use crate::data_source_config;

    let output = Output::new();
    let db = get_db(config).await?;

    let group = require_group_by_name(&db, group_name).await?;

    // Parse existing pattern_config to get data_sources
    let mut pattern_cfg = group.pattern_config.0.clone();
    let data_sources = pattern_cfg
        .get_mut("data_sources")
        .and_then(|v| v.as_object_mut());

    // Check if source already exists
    if let Some(sources) = data_sources {
        if sources.contains_key(source_name) {
            output.warning(&format!(
                "Data source '{}' already exists on group '{}'",
                source_name.bright_cyan(),
                group_name.bright_cyan()
            ));
            return Ok(());
        }
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

    // Add to pattern_config
    let source_value = serde_json::to_value(&data_source)
        .map_err(|e| miette::miette!("Failed to serialize data source: {}", e))?;

    if !pattern_cfg.is_object() {
        pattern_cfg = serde_json::json!({});
    }

    let pattern_obj = pattern_cfg.as_object_mut().unwrap();
    let sources = pattern_obj
        .entry("data_sources")
        .or_insert_with(|| serde_json::json!({}));

    if let Some(sources_obj) = sources.as_object_mut() {
        sources_obj.insert(source_name.to_string(), source_value);
    }

    // Update the group
    let updated_group = pattern_db::models::AgentGroup {
        id: group.id.clone(),
        name: group.name.clone(),
        description: group.description.clone(),
        pattern_type: group.pattern_type,
        pattern_config: pattern_db::Json(pattern_cfg),
        created_at: group.created_at,
        updated_at: chrono::Utc::now(),
    };

    pattern_db::queries::update_group(db.pool(), &updated_group)
        .await
        .map_err(|e| miette::miette!("Failed to update group: {}", e))?;

    output.success(&format!(
        "Added source '{}' to group '{}'",
        source_name.bright_cyan(),
        group_name.bright_cyan()
    ));

    Ok(())
}

/// Remove a member from a group
pub async fn remove_member(
    group_name: &str,
    agent_name: &str,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let group = require_group_by_name(&db, group_name).await?;
    let agent = require_agent_by_name(&db, agent_name).await?;

    pattern_db::queries::remove_group_member(db.pool(), &group.id, &agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to remove member: {}", e))?;

    output.success(&format!(
        "Removed '{}' from group '{}'",
        agent_name.bright_cyan(),
        group_name.bright_cyan()
    ));

    Ok(())
}

/// Remove a shared memory block from a group
pub async fn remove_memory(group_name: &str, label: &str, config: &PatternConfig) -> Result<()> {
    use pattern_core::memory::{MemoryCache, MemoryStore};
    use std::sync::Arc;

    let output = Output::new();
    let dbs = crate::helpers::get_dbs(config).await?;

    let group = pattern_db::queries::get_group_by_name(dbs.constellation.pool(), group_name)
        .await
        .map_err(|e| miette::miette!("Database error: {}", e))?
        .ok_or_else(|| miette::miette!("Group '{}' not found", group_name))?;

    // Create memory cache
    let cache = MemoryCache::new(Arc::new(dbs));

    // Shared memory uses group ID as owner, plain label
    cache.delete_block(&group.id, label).await.map_err(|e| {
        miette::miette!("Failed to delete shared memory block '{}': {:?}", label, e)
    })?;

    output.success(&format!(
        "Removed shared memory block '{}' from group '{}'",
        label.bright_cyan(),
        group_name.bright_cyan()
    ));

    Ok(())
}

/// Remove a data source subscription from a group
pub async fn remove_source(
    group_name: &str,
    source_name: &str,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    let group = require_group_by_name(&db, group_name).await?;

    // Parse existing pattern_config to get data_sources
    let mut pattern_cfg = group.pattern_config.0.clone();

    // Check if data_sources exists and has the source
    let removed = if let Some(sources) = pattern_cfg
        .get_mut("data_sources")
        .and_then(|v| v.as_object_mut())
    {
        sources.remove(source_name).is_some()
    } else {
        false
    };

    if !removed {
        output.warning(&format!(
            "Data source '{}' not found on group '{}'",
            source_name.bright_cyan(),
            group_name.bright_cyan()
        ));
        return Ok(());
    }

    // Update the group
    let updated_group = pattern_db::models::AgentGroup {
        id: group.id.clone(),
        name: group.name.clone(),
        description: group.description.clone(),
        pattern_type: group.pattern_type,
        pattern_config: pattern_db::Json(pattern_cfg),
        created_at: group.created_at,
        updated_at: chrono::Utc::now(),
    };

    pattern_db::queries::update_group(db.pool(), &updated_group)
        .await
        .map_err(|e| miette::miette!("Failed to update group: {}", e))?;

    output.success(&format!(
        "Removed source '{}' from group '{}'",
        source_name.bright_cyan(),
        group_name.bright_cyan()
    ));

    Ok(())
}
