//! Random agent selection

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::SelectionContext;
use crate::coordination::AgentSelector;
use crate::coordination::groups::AgentWithMembership;
use crate::{Result, agent::Agent};

/// Selects agents randomly
#[derive(Debug, Clone)]
pub struct RandomSelector;

#[async_trait]
impl AgentSelector for RandomSelector {
    async fn select_agents<'a>(
        &'a self,
        agents: &'a [AgentWithMembership<Arc<dyn Agent>>],
        _context: &SelectionContext,
        config: &HashMap<String, String>,
    ) -> Result<super::SelectionResult<'a>> {
        let mut rng = rand::rng();

        // Get number of agents to select (default 1)
        let count = config
            .get("count")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);

        // Filter active agents
        let available: Vec<_> = agents
            .iter()
            .filter(|awm| awm.membership.is_active)
            .collect();

        if available.is_empty() {
            return Ok(super::SelectionResult {
                agents: vec![],
                selector_response: None,
            });
        }

        // Randomly select up to 'count' agents
        let selected_count = count.min(available.len());
        // Manually select random indices
        let mut indices: Vec<usize> = (0..available.len()).collect();
        use rand::seq::SliceRandom;
        indices.shuffle(&mut rng);
        let selected: Vec<_> = indices
            .into_iter()
            .take(selected_count)
            .map(|i| available[i])
            .collect();

        Ok(super::SelectionResult {
            agents: selected,
            selector_response: None,
        })
    }

    fn name(&self) -> &str {
        "random"
    }

    fn description(&self) -> &str {
        "Randomly selects one or more agents from the available pool"
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
    async fn test_random_selector() {
        let selector = RandomSelector;

        // Create mock agents with membership
        let agents: Vec<AgentWithMembership<Arc<dyn Agent>>> = vec![
            AgentWithMembership {
                agent: Arc::new(create_test_agent("agent1").await) as Arc<dyn crate::agent::Agent>,
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
                agent: Arc::new(create_test_agent("agent2").await) as Arc<dyn crate::agent::Agent>,
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
                agent: Arc::new(create_test_agent("agent3").await) as Arc<dyn crate::agent::Agent>,
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

        let context = SelectionContext {
            message: create_test_message("Test"),
            recent_selections: vec![],
            available_agents: vec![], // Not used in new implementation
            agent_capabilities: Default::default(),
        };

        // Select default (1 agent)
        let selected = selector
            .select_agents(&agents, &context, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(selected.agents.len(), 1);

        // Select multiple
        let mut config = HashMap::new();
        config.insert("count".to_string(), "2".to_string());
        let selected = selector
            .select_agents(&agents, &context, &config)
            .await
            .unwrap();
        assert_eq!(selected.agents.len(), 2);

        // Request more than available
        config.insert("count".to_string(), "10".to_string());
        let selected = selector
            .select_agents(&agents, &context, &config)
            .await
            .unwrap();
        assert_eq!(selected.agents.len(), 3); // Only 3 available
    }
}
