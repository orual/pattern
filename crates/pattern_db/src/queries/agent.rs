//! Agent-related database queries.

use sqlx::SqlitePool;
use sqlx::types::Json;

use crate::error::DbResult;
use crate::models::{Agent, AgentGroup, AgentStatus, GroupMember, GroupMemberRole, PatternType};

/// Get an agent by ID.
pub async fn get_agent(pool: &SqlitePool, id: &str) -> DbResult<Option<Agent>> {
    let agent = sqlx::query_as!(
        Agent,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            model_provider as "model_provider!",
            model_name as "model_name!",
            system_prompt as "system_prompt!",
            config as "config!: _",
            enabled_tools as "enabled_tools!: _",
            tool_rules as "tool_rules: _",
            status as "status!: AgentStatus",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agents WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(agent)
}

/// Get an agent by name.
pub async fn get_agent_by_name(pool: &SqlitePool, name: &str) -> DbResult<Option<Agent>> {
    let agent = sqlx::query_as!(
        Agent,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            model_provider as "model_provider!",
            model_name as "model_name!",
            system_prompt as "system_prompt!",
            config as "config!: _",
            enabled_tools as "enabled_tools!: _",
            tool_rules as "tool_rules: _",
            status as "status!: AgentStatus",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agents WHERE name = ?
        "#,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(agent)
}

/// List all agents.
pub async fn list_agents(pool: &SqlitePool) -> DbResult<Vec<Agent>> {
    let agents = sqlx::query_as!(
        Agent,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            model_provider as "model_provider!",
            model_name as "model_name!",
            system_prompt as "system_prompt!",
            config as "config!: _",
            enabled_tools as "enabled_tools!: _",
            tool_rules as "tool_rules: _",
            status as "status!: AgentStatus",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agents ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(agents)
}

/// List agents with a specific status.
pub async fn list_agents_by_status(pool: &SqlitePool, status: AgentStatus) -> DbResult<Vec<Agent>> {
    let agents = sqlx::query_as!(
        Agent,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            model_provider as "model_provider!",
            model_name as "model_name!",
            system_prompt as "system_prompt!",
            config as "config!: _",
            enabled_tools as "enabled_tools!: _",
            tool_rules as "tool_rules: _",
            status as "status!: AgentStatus",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agents WHERE status = ? ORDER BY name
        "#,
        status
    )
    .fetch_all(pool)
    .await?;
    Ok(agents)
}

/// Create a new agent.
pub async fn create_agent(pool: &SqlitePool, agent: &Agent) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO agents (id, name, description, model_provider, model_name,
                           system_prompt, config, enabled_tools, tool_rules,
                           status, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        agent.id,
        agent.name,
        agent.description,
        agent.model_provider,
        agent.model_name,
        agent.system_prompt,
        agent.config,
        agent.enabled_tools,
        agent.tool_rules,
        agent.status,
        agent.created_at,
        agent.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update an agent's status.
pub async fn update_agent_status(pool: &SqlitePool, id: &str, status: AgentStatus) -> DbResult<()> {
    sqlx::query!(
        "UPDATE agents SET status = ?, updated_at = datetime('now') WHERE id = ?",
        status,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update an agent's tool rules.
pub async fn update_agent_tool_rules(
    pool: &SqlitePool,
    id: &str,
    tool_rules: Option<serde_json::Value>,
) -> DbResult<()> {
    let rules_json = tool_rules.map(|v| serde_json::to_string(&v).unwrap_or_default());
    sqlx::query!(
        "UPDATE agents SET tool_rules = ?, updated_at = datetime('now') WHERE id = ?",
        rules_json,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete an agent.
pub async fn delete_agent(pool: &SqlitePool, id: &str) -> DbResult<()> {
    sqlx::query!("DELETE FROM agents WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update an agent's core fields.
pub async fn update_agent(pool: &SqlitePool, agent: &Agent) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE agents SET
            name = ?,
            description = ?,
            model_provider = ?,
            model_name = ?,
            system_prompt = ?,
            config = ?,
            enabled_tools = ?,
            tool_rules = ?,
            status = ?,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
        agent.name,
        agent.description,
        agent.model_provider,
        agent.model_name,
        agent.system_prompt,
        agent.config,
        agent.enabled_tools,
        agent.tool_rules,
        agent.status,
        agent.id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get an agent group by ID.
pub async fn get_group(pool: &SqlitePool, id: &str) -> DbResult<Option<AgentGroup>> {
    let group = sqlx::query_as!(
        AgentGroup,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            pattern_type as "pattern_type!: PatternType",
            pattern_config as "pattern_config!: _",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agent_groups WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(group)
}

/// Get an agent group by name.
pub async fn get_group_by_name(pool: &SqlitePool, name: &str) -> DbResult<Option<AgentGroup>> {
    let group = sqlx::query_as!(
        AgentGroup,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            pattern_type as "pattern_type!: PatternType",
            pattern_config as "pattern_config!: _",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agent_groups WHERE name = ?
        "#,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(group)
}

/// List all agent groups.
pub async fn list_groups(pool: &SqlitePool) -> DbResult<Vec<AgentGroup>> {
    let groups = sqlx::query_as!(
        AgentGroup,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            description,
            pattern_type as "pattern_type!: PatternType",
            pattern_config as "pattern_config!: _",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM agent_groups ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(groups)
}

/// Create a new agent group.
pub async fn create_group(pool: &SqlitePool, group: &AgentGroup) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO agent_groups (id, name, description, pattern_type, pattern_config, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        group.id,
        group.name,
        group.description,
        group.pattern_type,
        group.pattern_config,
        group.created_at,
        group.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get members of a group.
pub async fn get_group_members(pool: &SqlitePool, group_id: &str) -> DbResult<Vec<GroupMember>> {
    let members = sqlx::query_as!(
        GroupMember,
        r#"
        SELECT
            group_id as "group_id!",
            agent_id as "agent_id!",
            role as "role: _",
            capabilities as "capabilities!: _",
            joined_at as "joined_at!: _"
        FROM group_members WHERE group_id = ?
        "#,
        group_id
    )
    .fetch_all(pool)
    .await?;
    Ok(members)
}

/// Add an agent to a group.
pub async fn add_group_member(pool: &SqlitePool, member: &GroupMember) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO group_members (group_id, agent_id, role, capabilities, joined_at)
        VALUES (?, ?, ?, ?, ?)
        "#,
        member.group_id,
        member.agent_id,
        member.role,
        member.capabilities,
        member.joined_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Remove an agent from a group.
pub async fn remove_group_member(
    pool: &SqlitePool,
    group_id: &str,
    agent_id: &str,
) -> DbResult<()> {
    sqlx::query!(
        "DELETE FROM group_members WHERE group_id = ? AND agent_id = ?",
        group_id,
        agent_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a group member's role.
pub async fn update_group_member_role(
    pool: &SqlitePool,
    group_id: &str,
    agent_id: &str,
    role: Option<&Json<GroupMemberRole>>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE group_members SET role = ? WHERE group_id = ? AND agent_id = ?",
        role,
        group_id,
        agent_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a group member's capabilities.
pub async fn update_group_member_capabilities(
    pool: &SqlitePool,
    group_id: &str,
    agent_id: &str,
    capabilities: &Json<Vec<String>>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE group_members SET capabilities = ? WHERE group_id = ? AND agent_id = ?",
        capabilities,
        group_id,
        agent_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a group member's role and capabilities.
pub async fn update_group_member(
    pool: &SqlitePool,
    group_id: &str,
    agent_id: &str,
    role: Option<&Json<GroupMemberRole>>,
    capabilities: &Json<Vec<String>>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE group_members SET role = ?, capabilities = ? WHERE group_id = ? AND agent_id = ?",
        role,
        capabilities,
        group_id,
        agent_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get all groups an agent belongs to.
pub async fn get_agent_groups(pool: &SqlitePool, agent_id: &str) -> DbResult<Vec<AgentGroup>> {
    let groups = sqlx::query_as!(
        AgentGroup,
        r#"
        SELECT
            g.id as "id!",
            g.name as "name!",
            g.description,
            g.pattern_type as "pattern_type!: PatternType",
            g.pattern_config as "pattern_config!: _",
            g.created_at as "created_at!: _",
            g.updated_at as "updated_at!: _"
        FROM agent_groups g
        INNER JOIN group_members m ON g.id = m.group_id
        WHERE m.agent_id = ?
        ORDER BY g.name
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(groups)
}

/// Update an agent group.
pub async fn update_group(pool: &SqlitePool, group: &AgentGroup) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE agent_groups SET
            name = ?,
            description = ?,
            pattern_type = ?,
            pattern_config = ?,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
        group.name,
        group.description,
        group.pattern_type,
        group.pattern_config,
        group.id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete an agent group and its members.
pub async fn delete_group(pool: &SqlitePool, id: &str) -> DbResult<()> {
    // Delete members first (foreign key constraint)
    sqlx::query!("DELETE FROM group_members WHERE group_id = ?", id)
        .execute(pool)
        .await?;

    // Delete the group
    sqlx::query!("DELETE FROM agent_groups WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Check if an agent has a specific capability in any of their group memberships.
///
/// Returns true if the agent has the capability with specialist role in any group.
/// This is used for permission checks on cross-agent operations like constellation-wide search.
pub async fn agent_has_capability(
    pool: &SqlitePool,
    agent_id: &str,
    capability: &str,
) -> DbResult<bool> {
    // Query checks:
    // 1. Agent matches
    // 2. Role is a specialist (JSON type field = 'specialist')
    // 3. Capabilities JSON array contains the capability string
    let result = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM group_members
            WHERE agent_id = ?
              AND json_extract(role, '$.type') = 'specialist'
              AND EXISTS (
                  SELECT 1 FROM json_each(capabilities)
                  WHERE json_each.value = ?
              )
        ) as "exists!: bool"
        "#,
        agent_id,
        capability
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

/// Check if two agents share any group membership.
///
/// Returns true if both agents are members of at least one common group.
/// This is used for permission checks on cross-agent search operations.
pub async fn agents_share_group(
    pool: &SqlitePool,
    agent_id_1: &str,
    agent_id_2: &str,
) -> DbResult<bool> {
    let result = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM group_members m1
            INNER JOIN group_members m2 ON m1.group_id = m2.group_id
            WHERE m1.agent_id = ? AND m2.agent_id = ?
        ) as "exists!: bool"
        "#,
        agent_id_1,
        agent_id_2
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConstellationDb;
    use crate::models::{Agent, AgentGroup, AgentStatus, PatternType};
    use chrono::Utc;

    async fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().await.unwrap()
    }

    async fn create_test_agent(db: &ConstellationDb, id: &str, name: &str) {
        let agent = Agent {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "Test prompt".to_string(),
            config: Json(serde_json::json!({})),
            enabled_tools: Json(vec![]),
            tool_rules: None,
            status: AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        create_agent(db.pool(), &agent).await.unwrap();
    }

    async fn create_test_group(db: &ConstellationDb, id: &str, name: &str) {
        let group = AgentGroup {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            pattern_type: PatternType::RoundRobin,
            pattern_config: Json(serde_json::json!({})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        create_group(db.pool(), &group).await.unwrap();
    }

    // ============================================================================
    // Tests for agent_has_capability
    // ============================================================================

    #[tokio::test]
    async fn test_agent_has_capability_specialist_with_matching_capability() {
        let db = setup_test_db().await;

        // Create agent and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add agent as specialist with "memory" capability.
        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Specialist {
                domain: "memory-management".to_string(),
            })),
            capabilities: Json(vec!["memory".to_string(), "search".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member).await.unwrap();

        // Should have the "memory" capability.
        let has_memory = agent_has_capability(db.pool(), "agent1", "memory")
            .await
            .unwrap();
        assert!(
            has_memory,
            "Specialist with 'memory' capability should return true"
        );

        // Should also have the "search" capability.
        let has_search = agent_has_capability(db.pool(), "agent1", "search")
            .await
            .unwrap();
        assert!(
            has_search,
            "Specialist with 'search' capability should return true"
        );
    }

    #[tokio::test]
    async fn test_agent_has_capability_specialist_without_matching_capability() {
        let db = setup_test_db().await;

        // Create agent and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add agent as specialist with "search" capability only.
        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Specialist {
                domain: "search".to_string(),
            })),
            capabilities: Json(vec!["search".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member).await.unwrap();

        // Should NOT have the "memory" capability.
        let has_memory = agent_has_capability(db.pool(), "agent1", "memory")
            .await
            .unwrap();
        assert!(
            !has_memory,
            "Specialist without 'memory' capability should return false"
        );
    }

    #[tokio::test]
    async fn test_agent_has_capability_non_specialist_role() {
        let db = setup_test_db().await;

        // Create agent and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add agent as regular member with capabilities.
        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec!["memory".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member).await.unwrap();

        // Regular role should NOT grant capability access even with matching capability.
        let has_memory = agent_has_capability(db.pool(), "agent1", "memory")
            .await
            .unwrap();
        assert!(
            !has_memory,
            "Regular role should not grant capability access"
        );
    }

    #[tokio::test]
    async fn test_agent_has_capability_agent_not_in_any_group() {
        let db = setup_test_db().await;

        // Create agent but don't add to any group.
        create_test_agent(&db, "agent1", "Agent 1").await;

        // Agent not in any group should return false.
        let has_memory = agent_has_capability(db.pool(), "agent1", "memory")
            .await
            .unwrap();
        assert!(!has_memory, "Agent not in any group should return false");
    }

    #[tokio::test]
    async fn test_agent_has_capability_observer_role() {
        let db = setup_test_db().await;

        // Create agent and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add agent as observer with capabilities.
        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Observer)),
            capabilities: Json(vec!["memory".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member).await.unwrap();

        // Observer role should NOT grant capability access.
        let has_memory = agent_has_capability(db.pool(), "agent1", "memory")
            .await
            .unwrap();
        assert!(
            !has_memory,
            "Observer role should not grant capability access"
        );
    }

    #[tokio::test]
    async fn test_agent_has_capability_supervisor_role() {
        let db = setup_test_db().await;

        // Create agent and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add agent as supervisor with capabilities.
        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Supervisor)),
            capabilities: Json(vec!["memory".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member).await.unwrap();

        // Supervisor role should NOT grant capability access.
        let has_memory = agent_has_capability(db.pool(), "agent1", "memory")
            .await
            .unwrap();
        assert!(
            !has_memory,
            "Supervisor role should not grant capability access"
        );
    }

    // ============================================================================
    // Tests for agents_share_group
    // ============================================================================

    #[tokio::test]
    async fn test_agents_share_group_in_same_group() {
        let db = setup_test_db().await;

        // Create agents and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add both agents to the same group.
        let member1 = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member1).await.unwrap();

        let member2 = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent2".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member2).await.unwrap();

        // They should share a group.
        let share = agents_share_group(db.pool(), "agent1", "agent2")
            .await
            .unwrap();
        assert!(share, "Agents in same group should return true");

        // Order shouldn't matter.
        let share_reversed = agents_share_group(db.pool(), "agent2", "agent1")
            .await
            .unwrap();
        assert!(share_reversed, "agents_share_group should be symmetric");
    }

    #[tokio::test]
    async fn test_agents_share_group_in_different_groups() {
        let db = setup_test_db().await;

        // Create agents and separate groups.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;
        create_test_group(&db, "group1", "Group 1").await;
        create_test_group(&db, "group2", "Group 2").await;

        // Add agents to different groups.
        let member1 = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member1).await.unwrap();

        let member2 = GroupMember {
            group_id: "group2".to_string(),
            agent_id: "agent2".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member2).await.unwrap();

        // They should NOT share a group.
        let share = agents_share_group(db.pool(), "agent1", "agent2")
            .await
            .unwrap();
        assert!(!share, "Agents in different groups should return false");
    }

    #[tokio::test]
    async fn test_agents_share_group_agent_not_in_any_group() {
        let db = setup_test_db().await;

        // Create agents and one group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Only add agent1 to the group.
        let member1 = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member1).await.unwrap();

        // They should NOT share a group (agent2 not in any group).
        let share = agents_share_group(db.pool(), "agent1", "agent2")
            .await
            .unwrap();
        assert!(
            !share,
            "Should return false when one agent not in any group"
        );
    }

    #[tokio::test]
    async fn test_agents_share_group_multiple_shared_groups() {
        let db = setup_test_db().await;

        // Create agents and multiple groups.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_agent(&db, "agent2", "Agent 2").await;
        create_test_group(&db, "group1", "Group 1").await;
        create_test_group(&db, "group2", "Group 2").await;

        // Add both agents to both groups.
        for group_id in ["group1", "group2"] {
            let member1 = GroupMember {
                group_id: group_id.to_string(),
                agent_id: "agent1".to_string(),
                role: Some(Json(GroupMemberRole::Regular)),
                capabilities: Json(vec![]),
                joined_at: Utc::now(),
            };
            add_group_member(db.pool(), &member1).await.unwrap();

            let member2 = GroupMember {
                group_id: group_id.to_string(),
                agent_id: "agent2".to_string(),
                role: Some(Json(GroupMemberRole::Regular)),
                capabilities: Json(vec![]),
                joined_at: Utc::now(),
            };
            add_group_member(db.pool(), &member2).await.unwrap();
        }

        // They should share a group (even multiple).
        let share = agents_share_group(db.pool(), "agent1", "agent2")
            .await
            .unwrap();
        assert!(share, "Agents in multiple shared groups should return true");
    }

    #[tokio::test]
    async fn test_agents_share_group_same_agent() {
        let db = setup_test_db().await;

        // Create agent and group.
        create_test_agent(&db, "agent1", "Agent 1").await;
        create_test_group(&db, "group1", "Group 1").await;

        // Add agent to group.
        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        };
        add_group_member(db.pool(), &member).await.unwrap();

        // Same agent should share a group with itself.
        let share = agents_share_group(db.pool(), "agent1", "agent1")
            .await
            .unwrap();
        assert!(
            share,
            "Agent should share a group with itself if in any group"
        );
    }
}
