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
