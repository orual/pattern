//! Load-balancing agent selection

use async_trait::async_trait;
use chrono::{Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;

use super::SelectionContext;
use crate::coordination::AgentSelector;
use crate::coordination::groups::AgentWithMembership;
use crate::{Result, agent::Agent};

/// Selects agents based on load balancing (least recently used)
#[derive(Debug, Clone)]
pub struct LoadBalancingSelector;

#[async_trait]
impl AgentSelector for LoadBalancingSelector {
    async fn select_agents<'a>(
        &'a self,
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        context: &SelectionContext,
        config: &HashMap<String, String>,
    ) -> Result<super::SelectionResult<'a>> {
        // Get window for considering recent selections (default 5 minutes)
        let window_minutes = config
            .get("window_minutes")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(5);

        let cutoff_time = Utc::now() - Duration::minutes(window_minutes);

        // Only consider active agents
        let active_agents: Vec<_> = agents
            .iter()
            .filter(|awm| awm.membership.is_active)
            .collect();

        // Count recent uses per agent
        let mut usage_counts = HashMap::new();

        // Initialize all active agents with 0 count
        for awm in &active_agents {
            let agent_id = awm.agent.as_ref().id();
            usage_counts.insert(agent_id, (0, awm));
        }

        // Count recent selections
        for (timestamp, agent_id) in &context.recent_selections {
            if *timestamp > cutoff_time {
                if let Some((count, _)) = usage_counts.get_mut(agent_id) {
                    *count += 1;
                }
            }
        }

        // Sort agents by usage (least used first)
        let mut sorted_agents: Vec<_> = usage_counts
            .into_iter()
            .map(|(_, (count, awm))| (count, awm))
            .collect();
        sorted_agents.sort_by_key(|(count, _)| *count);

        // Get number of agents to select
        let select_count = config
            .get("count")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);

        // Select the least used agents
        let selected: Vec<_> = sorted_agents
            .into_iter()
            .take(select_count)
            .map(|(_, awm)| *awm)
            .collect();

        Ok(super::SelectionResult {
            agents: selected,
            selector_response: None,
        })
    }

    fn name(&self) -> &str {
        "load_balancing"
    }

    fn description(&self) -> &str {
        "Selects least recently used agents to balance load"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coordination::{
            groups::GroupMembership,
            test_utils::test::{create_test_agent, create_test_message},
            types::GroupMemberRole,
        },
        id::{AgentId, GroupId},
    };
    use chrono::Utc;

    #[tokio::test]
    async fn test_load_balancing_selector() {
        let selector = LoadBalancingSelector;

        // Create agents first to get their IDs
        let agent1 = create_test_agent("agent1").await;
        let agent2 = create_test_agent("agent2").await;
        let agent3 = create_test_agent("agent3").await;

        let agent1_id = agent1.id.clone();
        let agent2_id = agent2.id.clone();
        let agent3_id = agent3.id.clone();

        // Create agents
        let agents: Vec<AgentWithMembership<Arc<dyn crate::agent::Agent>>> = vec![
            AgentWithMembership {
                agent: Arc::new(agent1) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec![],
                },
            },
            AgentWithMembership {
                agent: Arc::new(agent2) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec![],
                },
            },
            AgentWithMembership {
                agent: Arc::new(agent3) as Arc<dyn crate::agent::Agent>,
                membership: GroupMembership {
                    agent_id: AgentId::generate(),
                    group_id: GroupId::generate(),
                    joined_at: Utc::now(),
                    role: GroupMemberRole::Regular,
                    is_active: true,
                    capabilities: vec![],
                },
            },
        ];

        // Create recent selections showing agent1 used twice, agent2 once, agent3 never
        let recent_selections = vec![
            (Utc::now() - Duration::minutes(2), agent1_id.clone()),
            (Utc::now() - Duration::minutes(3), agent1_id.clone()),
            (Utc::now() - Duration::minutes(4), agent2_id.clone()),
        ];

        let context = SelectionContext {
            message: create_test_message("Test"),
            recent_selections,
            available_agents: vec![], // Not used in new implementation
            agent_capabilities: Default::default(), // Not used in new implementation
        };

        // Should select agent3 (never used)
        let selected = selector
            .select_agents(&agents, &context, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(selected.agents.len(), 1);
        assert_eq!(selected.agents[0].agent.id(), agent3_id);

        // Select multiple - should get agent3 and agent2 (least used)
        let mut config = HashMap::new();
        config.insert("count".to_string(), "2".to_string());

        let selected = selector
            .select_agents(&agents, &context, &config)
            .await
            .unwrap();
        assert_eq!(selected.agents.len(), 2);
        let selected_ids: Vec<_> = selected.agents.iter().map(|awm| awm.agent.id()).collect();
        assert!(selected_ids.contains(&agent3_id));
        assert!(selected_ids.contains(&agent2_id));
        assert!(!selected_ids.contains(&agent1_id)); // Most used

        // Test with different time window
        config.insert("window_minutes".to_string(), "1".to_string());

        // Now only selections from last minute count - all agents equal
        let selected = selector
            .select_agents(&agents, &context, &config)
            .await
            .unwrap();
        assert_eq!(selected.agents.len(), 2);
    }
}
