//! Agent group management commands for the Pattern CLI
//!
//! This module provides commands for creating, listing, and managing agent groups
//! with various coordination patterns.
//!
//! Uses pattern_db::queries for database access via shared helpers.

use chrono::Utc;
use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::{GroupPatternConfig, PatternConfig};
use pattern_db::{
    Json,
    models::{AgentGroup, GroupMember, GroupMemberRole, PatternType},
};
use std::path::Path;

use crate::helpers::{
    generate_id, get_agent_by_name, get_db, get_group_by_name, require_agent_by_name,
    require_group_by_name,
};
use crate::output::Output;

/// Parse pattern type from string
fn parse_pattern_type(s: &str) -> Result<PatternType> {
    match s.to_lowercase().as_str() {
        "round_robin" | "roundrobin" | "round-robin" => Ok(PatternType::RoundRobin),
        "dynamic" => Ok(PatternType::Dynamic),
        "pipeline" => Ok(PatternType::Pipeline),
        "supervisor" => Ok(PatternType::Supervisor),
        "voting" => Ok(PatternType::Voting),
        "sleeptime" => Ok(PatternType::Sleeptime),
        _ => Err(miette::miette!(
            "Unknown pattern type: '{}'. Valid patterns: round_robin, dynamic, pipeline, supervisor, voting, sleeptime",
            s
        )),
    }
}

/// Convert GroupPatternConfig enum to PatternType
fn pattern_config_to_type(config: &GroupPatternConfig) -> Result<PatternType> {
    Ok(match config {
        GroupPatternConfig::Supervisor { .. } => PatternType::Supervisor,
        GroupPatternConfig::RoundRobin { .. } => PatternType::RoundRobin,
        GroupPatternConfig::Pipeline { .. } => PatternType::Pipeline,
        GroupPatternConfig::Dynamic { .. } => PatternType::Dynamic,
        GroupPatternConfig::Sleeptime { .. } => PatternType::Sleeptime,
    })
}

/// Parse member role from string
fn parse_role(s: &str) -> Result<GroupMemberRole> {
    match s.to_lowercase().as_str() {
        "supervisor" => Ok(GroupMemberRole::Supervisor),
        "worker" | "regular" => Ok(GroupMemberRole::Worker),
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
// Group Creation
// =============================================================================

/// Create a new group
pub async fn create(
    name: &str,
    description: &str,
    pattern: &str,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Check if group already exists using shared helper
    if get_group_by_name(&db, name).await?.is_some() {
        output.error(&format!("Group '{}' already exists", name));
        return Ok(());
    }

    let pattern_type = parse_pattern_type(pattern)?;

    let group = AgentGroup {
        id: generate_id("grp"),
        name: name.to_string(),
        description: Some(description.to_string()),
        pattern_type,
        pattern_config: Json(serde_json::json!({})),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    pattern_db::queries::create_group(db.pool(), &group)
        .await
        .map_err(|e| miette::miette!("Failed to create group: {}", e))?;

    output.success(&format!("Created group '{}'", name.bright_cyan()));
    output.kv("ID", &group.id);
    output.kv("Pattern", &format!("{:?}", pattern_type));
    output.kv("Description", description);

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
    _capabilities: Option<&str>,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Find group and agent by name using shared helpers
    let group = require_group_by_name(&db, group_name).await?;
    let agent = require_agent_by_name(&db, agent_name).await?;

    let member_role = parse_role(role)?;

    let member = GroupMember {
        group_id: group.id.clone(),
        agent_id: agent.id.clone(),
        role: Some(member_role),
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
                "role": member.role.map(|r| format!("{:?}", r).to_lowercase()),
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
// Initialize from Config
// =============================================================================

/// Initialize groups from configuration
///
/// Creates groups and adds members as specified in the config file.
#[allow(dead_code)]
pub async fn initialize_from_config(
    config: &PatternConfig,
    _heartbeat_sender: pattern_core::context::heartbeat::HeartbeatSender,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Check if there are any groups in config
    if config.groups.is_empty() {
        output.info(
            "No groups in config",
            "Add groups to your pattern.toml file",
        );
        return Ok(());
    }

    for group_config in &config.groups {
        output.status(&format!(
            "Initializing group: {}",
            group_config.name.bright_cyan()
        ));

        // Check if group exists using shared helper
        if get_group_by_name(&db, &group_config.name).await?.is_some() {
            output.info("  Group exists", "skipping creation");
            continue;
        }

        // Parse pattern from config
        let pattern_type = pattern_config_to_type(&group_config.pattern)?;

        let group = AgentGroup {
            id: generate_id("grp"),
            name: group_config.name.clone(),
            description: Some(group_config.description.clone()),
            pattern_type,
            pattern_config: Json(serde_json::json!({})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        pattern_db::queries::create_group(db.pool(), &group)
            .await
            .map_err(|e| miette::miette!("Failed to create group: {}", e))?;

        output.success(&format!("  Created group '{}'", group_config.name));

        // Add members if specified
        for member_config in &group_config.members {
            // Try to find agent by name or ID
            let agent_id = if let Some(ref id) = member_config.agent_id {
                id.0.clone()
            } else {
                // Use name to look up agent using shared helper
                let name = &member_config.name;
                match get_agent_by_name(&db, name).await? {
                    Some(a) => a.id,
                    None => {
                        output.warning(&format!("  Agent '{}' not found, skipping", name));
                        continue;
                    }
                }
            };

            let member = GroupMember {
                group_id: group.id.clone(),
                agent_id,
                role: Some(GroupMemberRole::Worker),
                joined_at: Utc::now(),
            };

            pattern_db::queries::add_group_member(db.pool(), &member)
                .await
                .map_err(|e| miette::miette!("Failed to add member: {}", e))?;
        }
    }

    Ok(())
}
