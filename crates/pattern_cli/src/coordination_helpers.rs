//! Shared coordination helpers for group pattern conversion
//!
//! This module consolidates pattern conversion logic used across CLI commands
//! to reduce code duplication between discord.rs and endpoints.rs.

use chrono::Utc;
use pattern_core::{
    Agent,
    coordination::groups::{AgentGroup, AgentWithMembership, GroupMembership},
    coordination::types::{
        CoordinationPattern, DelegationRules, DelegationStrategy, FallbackBehavior,
        GroupMemberRole, GroupState, TieBreaker, VotingRules,
    },
    id::{AgentId, GroupId, RelationId},
};
use pattern_db::models::{
    AgentGroup as DbAgentGroup, GroupMember, GroupMemberRole as DbGroupMemberRole, PatternType,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Convert a database GroupMemberRole to the core GroupMemberRole
pub fn convert_db_role(db_role: Option<pattern_db::Json<DbGroupMemberRole>>) -> GroupMemberRole {
    match db_role.as_ref().map(|j| &j.0) {
        Some(DbGroupMemberRole::Supervisor) => GroupMemberRole::Supervisor,
        Some(DbGroupMemberRole::Regular) => GroupMemberRole::Regular,
        Some(DbGroupMemberRole::Observer) => GroupMemberRole::Observer,
        Some(DbGroupMemberRole::Specialist { domain }) => GroupMemberRole::Specialist {
            domain: domain.clone(),
        },
        None => GroupMemberRole::Regular,
    }
}

/// Convert database PatternType to core CoordinationPattern
///
/// The `first_agent_id` is used for patterns that need a leader (e.g., Supervisor)
pub fn convert_pattern_type(
    pattern_type: PatternType,
    first_agent_id: AgentId,
) -> CoordinationPattern {
    match pattern_type {
        PatternType::RoundRobin => CoordinationPattern::RoundRobin {
            current_index: 0,
            skip_unavailable: true,
        },
        PatternType::Dynamic => CoordinationPattern::Dynamic {
            selector_name: "random".to_string(),
            selector_config: HashMap::new(),
        },
        PatternType::Pipeline => CoordinationPattern::Pipeline {
            stages: vec![],
            parallel_stages: false,
        },
        PatternType::Supervisor => CoordinationPattern::Supervisor {
            leader_id: first_agent_id,
            delegation_rules: DelegationRules {
                max_delegations_per_agent: None,
                delegation_strategy: DelegationStrategy::RoundRobin,
                fallback_behavior: FallbackBehavior::HandleSelf,
            },
        },
        PatternType::Voting => CoordinationPattern::Voting {
            quorum: 1,
            voting_rules: VotingRules {
                voting_timeout: Duration::from_secs(30),
                tie_breaker: TieBreaker::Random,
                weight_by_expertise: false,
            },
        },
        PatternType::Sleeptime => CoordinationPattern::Sleeptime {
            check_interval: Duration::from_secs(60),
            triggers: vec![],
            intervention_agent_id: None,
        },
    }
}

/// Create the initial GroupState for a given pattern type
pub fn create_initial_state(pattern_type: PatternType) -> GroupState {
    match pattern_type {
        PatternType::RoundRobin => GroupState::RoundRobin {
            current_index: 0,
            last_rotation: Utc::now(),
        },
        PatternType::Supervisor => GroupState::Supervisor {
            current_delegations: HashMap::new(),
        },
        PatternType::Voting => GroupState::Voting {
            active_session: None,
        },
        PatternType::Pipeline => GroupState::Pipeline {
            active_executions: vec![],
        },
        PatternType::Dynamic => GroupState::Dynamic {
            recent_selections: vec![],
        },
        PatternType::Sleeptime => GroupState::Sleeptime {
            last_check: Utc::now(),
            trigger_history: vec![],
            current_index: 0,
        },
    }
}

/// Build a core AgentGroup from database group data
pub fn build_agent_group(db_group: &DbAgentGroup, first_agent_id: AgentId) -> AgentGroup {
    let coordination_pattern = convert_pattern_type(db_group.pattern_type, first_agent_id);
    let initial_state = create_initial_state(db_group.pattern_type);

    AgentGroup {
        id: GroupId(db_group.id.clone()),
        name: db_group.name.clone(),
        description: db_group.description.clone().unwrap_or_default(),
        coordination_pattern,
        created_at: db_group.created_at,
        updated_at: db_group.updated_at,
        is_active: true,
        state: initial_state,
        members: vec![], // We use agents_with_membership instead
    }
}

/// Build AgentWithMembership list from loaded agents and database members
pub fn build_agents_with_membership(
    agents: Vec<Arc<dyn Agent>>,
    db_group_id: &str,
    db_members: &[GroupMember],
) -> Vec<AgentWithMembership<Arc<dyn Agent>>> {
    agents
        .into_iter()
        .map(|agent| {
            let db_membership = db_members
                .iter()
                .find(|m| m.agent_id == agent.id().as_str());

            let membership = GroupMembership {
                id: RelationId::generate(),
                in_id: agent.id(),
                out_id: GroupId(db_group_id.to_string()),
                joined_at: db_membership.map(|m| m.joined_at).unwrap_or_else(Utc::now),
                role: convert_db_role(db_membership.and_then(|m| m.role.clone())),
                is_active: true,
                capabilities: db_membership
                    .map(|m| m.capabilities.0.clone())
                    .unwrap_or_default(),
            };

            AgentWithMembership { agent, membership }
        })
        .collect()
}
