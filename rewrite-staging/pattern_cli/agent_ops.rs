//! Agent operations for the Pattern CLI
//!
//! This module handles agent loading and creation using RuntimeContext from pattern_core
//! and pattern_db for database access.
//!
//! Key functions:
//! - load_or_create_agent: Load by name using get_agent_by_name() or create new
//! - register_data_sources: Set up Bluesky monitoring (stubbed pending rework)
//! - agent_is_supervisor: Check if agent has supervisor role in any group

use miette::Result;
use pattern_core::{Agent, config::PatternConfig};
use std::sync::Arc;

use crate::helpers::get_db;
use crate::output::Output;

/// Register data sources for an agent (e.g., Bluesky monitoring).
///
/// NOTE: Data source management is being reworked. Bluesky monitoring
/// will be reimplemented once the core agent functionality is stable.
pub async fn register_data_sources(
    _agent: Arc<dyn Agent>,
    _config: &PatternConfig,
    _output: &Output,
) -> Result<()> {
    // Data source registration is being reworked as part of the
    // overall data source refactoring. This is legitimately stubbed.
    Ok(())
}

/// Check if an agent has supervisor role in any group
///
/// Queries the database for the agent's group memberships and checks if
/// any have the supervisor role.
pub async fn agent_is_supervisor(agent: &Arc<dyn Agent>, config: &PatternConfig) -> bool {
    // Open database connection using shared helper
    let db = match get_db(config).await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!("Failed to open database for supervisor check: {}", e);
            return false;
        }
    };

    // Get agent's group memberships
    let groups = match pattern_db::queries::get_agent_groups(db.pool(), agent.id().as_str()).await {
        Ok(groups) => groups,
        Err(e) => {
            tracing::warn!("Failed to get agent groups: {}", e);
            return false;
        }
    };

    // Check if agent is supervisor in any group by checking the group pattern type
    for group in groups {
        if group.pattern_type == pattern_db::models::PatternType::Supervisor {
            // For supervisor pattern groups, check if this agent is the supervisor
            let members = match pattern_db::queries::get_group_members(db.pool(), &group.id).await {
                Ok(m) => m,
                Err(_) => continue,
            };

            for member in members {
                if member.agent_id == agent.id().as_str() {
                    if let Some(role) = member.role {
                        if role == pattern_db::models::GroupMemberRole::Supervisor {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}
