//! Agent-related database queries.

use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{Agent, AgentGroup, AgentStatus, GroupMember, GroupMemberRole};
use crate::Json;

// ============================================================================
// from_row implementations
// ============================================================================

impl Agent {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            model_provider: row.get("model_provider")?,
            model_name: row.get("model_name")?,
            system_prompt: row.get("system_prompt")?,
            config: row.get("config")?,
            enabled_tools: row.get("enabled_tools")?,
            tool_rules: row.get("tool_rules")?,
            status: row.get("status")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl AgentGroup {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            pattern_type: row.get("pattern_type")?,
            pattern_config: row.get("pattern_config")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl GroupMember {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            group_id: row.get("group_id")?,
            agent_id: row.get("agent_id")?,
            role: row.get("role")?,
            capabilities: row.get("capabilities")?,
            joined_at: row.get("joined_at")?,
        })
    }
}

// ============================================================================
// Agent queries
// ============================================================================

/// Get an agent by ID.
pub fn get_agent(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, model_provider, model_name, system_prompt,
                config, enabled_tools, tool_rules, status, created_at, updated_at
         FROM agents WHERE id = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![id], Agent::from_row).optional()?;
    Ok(result)
}

/// Get an agent by name.
pub fn get_agent_by_name(conn: &rusqlite::Connection, name: &str) -> DbResult<Option<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, model_provider, model_name, system_prompt,
                config, enabled_tools, tool_rules, status, created_at, updated_at
         FROM agents WHERE name = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![name], Agent::from_row).optional()?;
    Ok(result)
}

/// List all agents.
pub fn list_agents(conn: &rusqlite::Connection) -> DbResult<Vec<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, model_provider, model_name, system_prompt,
                config, enabled_tools, tool_rules, status, created_at, updated_at
         FROM agents ORDER BY name",
    )?;
    let rows = stmt.query_map([], Agent::from_row)?;
    let mut agents = Vec::new();
    for row in rows {
        agents.push(row?);
    }
    Ok(agents)
}

/// List agents with a specific status.
pub fn list_agents_by_status(
    conn: &rusqlite::Connection,
    status: AgentStatus,
) -> DbResult<Vec<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, model_provider, model_name, system_prompt,
                config, enabled_tools, tool_rules, status, created_at, updated_at
         FROM agents WHERE status = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(rusqlite::params![status], Agent::from_row)?;
    let mut agents = Vec::new();
    for row in rows {
        agents.push(row?);
    }
    Ok(agents)
}

/// Create a new agent.
pub fn create_agent(conn: &rusqlite::Connection, agent: &Agent) -> DbResult<()> {
    conn.execute(
        "INSERT INTO agents (id, name, description, model_provider, model_name,
                            system_prompt, config, enabled_tools, tool_rules,
                            status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
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
        ],
    )?;
    Ok(())
}

/// Create or update an agent (upsert).
///
/// If an agent with the same ID exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
pub fn upsert_agent(conn: &rusqlite::Connection, agent: &Agent) -> DbResult<()> {
    conn.execute(
        "INSERT INTO agents (id, name, description, model_provider, model_name,
                            system_prompt, config, enabled_tools, tool_rules,
                            status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             description = excluded.description,
             model_provider = excluded.model_provider,
             model_name = excluded.model_name,
             system_prompt = excluded.system_prompt,
             config = excluded.config,
             enabled_tools = excluded.enabled_tools,
             tool_rules = excluded.tool_rules,
             status = excluded.status,
             updated_at = excluded.updated_at",
        rusqlite::params![
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
        ],
    )?;
    Ok(())
}

/// Update an agent's status.
pub fn update_agent_status(
    conn: &rusqlite::Connection,
    id: &str,
    status: AgentStatus,
) -> DbResult<()> {
    conn.execute(
        "UPDATE agents SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![status, id],
    )?;
    Ok(())
}

/// Update an agent's tool rules.
pub fn update_agent_tool_rules(
    conn: &rusqlite::Connection,
    id: &str,
    tool_rules: Option<serde_json::Value>,
) -> DbResult<()> {
    let rules_json = tool_rules.map(|v| serde_json::to_string(&v).unwrap_or_default());
    conn.execute(
        "UPDATE agents SET tool_rules = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![rules_json, id],
    )?;
    Ok(())
}

/// Delete an agent.
pub fn delete_agent(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    conn.execute("DELETE FROM agents WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// Update an agent's core fields.
pub fn update_agent(conn: &rusqlite::Connection, agent: &Agent) -> DbResult<()> {
    conn.execute(
        "UPDATE agents SET
             name = ?1, description = ?2, model_provider = ?3, model_name = ?4,
             system_prompt = ?5, config = ?6, enabled_tools = ?7, tool_rules = ?8,
             status = ?9, updated_at = datetime('now')
         WHERE id = ?10",
        rusqlite::params![
            agent.name,
            agent.description,
            agent.model_provider,
            agent.model_name,
            agent.system_prompt,
            agent.config,
            agent.enabled_tools,
            agent.tool_rules,
            agent.status,
            agent.id,
        ],
    )?;
    Ok(())
}

// ============================================================================
// Group queries
// ============================================================================

/// Get an agent group by ID.
pub fn get_group(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<AgentGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, pattern_type, pattern_config, created_at, updated_at
         FROM agent_groups WHERE id = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![id], AgentGroup::from_row).optional()?;
    Ok(result)
}

/// Get an agent group by name.
pub fn get_group_by_name(conn: &rusqlite::Connection, name: &str) -> DbResult<Option<AgentGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, pattern_type, pattern_config, created_at, updated_at
         FROM agent_groups WHERE name = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![name], AgentGroup::from_row).optional()?;
    Ok(result)
}

/// List all agent groups.
pub fn list_groups(conn: &rusqlite::Connection) -> DbResult<Vec<AgentGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, pattern_type, pattern_config, created_at, updated_at
         FROM agent_groups ORDER BY name",
    )?;
    let rows = stmt.query_map([], AgentGroup::from_row)?;
    let mut groups = Vec::new();
    for row in rows {
        groups.push(row?);
    }
    Ok(groups)
}

/// Create a new agent group.
pub fn create_group(conn: &rusqlite::Connection, group: &AgentGroup) -> DbResult<()> {
    conn.execute(
        "INSERT INTO agent_groups (id, name, description, pattern_type, pattern_config, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            group.id,
            group.name,
            group.description,
            group.pattern_type,
            group.pattern_config,
            group.created_at,
            group.updated_at,
        ],
    )?;
    Ok(())
}

/// Create or update an agent group (upsert).
///
/// If a group with the same ID exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
pub fn upsert_group(conn: &rusqlite::Connection, group: &AgentGroup) -> DbResult<()> {
    conn.execute(
        "INSERT INTO agent_groups (id, name, description, pattern_type, pattern_config, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             description = excluded.description,
             pattern_type = excluded.pattern_type,
             pattern_config = excluded.pattern_config,
             updated_at = excluded.updated_at",
        rusqlite::params![
            group.id,
            group.name,
            group.description,
            group.pattern_type,
            group.pattern_config,
            group.created_at,
            group.updated_at,
        ],
    )?;
    Ok(())
}

/// Get members of a group.
pub fn get_group_members(
    conn: &rusqlite::Connection,
    group_id: &str,
) -> DbResult<Vec<GroupMember>> {
    let mut stmt = conn.prepare(
        "SELECT group_id, agent_id, role, capabilities, joined_at
         FROM group_members WHERE group_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![group_id], GroupMember::from_row)?;
    let mut members = Vec::new();
    for row in rows {
        members.push(row?);
    }
    Ok(members)
}

/// Add an agent to a group.
pub fn add_group_member(conn: &rusqlite::Connection, member: &GroupMember) -> DbResult<()> {
    conn.execute(
        "INSERT INTO group_members (group_id, agent_id, role, capabilities, joined_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            member.group_id,
            member.agent_id,
            member.role,
            member.capabilities,
            member.joined_at,
        ],
    )?;
    Ok(())
}

/// Add or update an agent in a group (upsert).
///
/// If the membership already exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
pub fn upsert_group_member(conn: &rusqlite::Connection, member: &GroupMember) -> DbResult<()> {
    conn.execute(
        "INSERT INTO group_members (group_id, agent_id, role, capabilities, joined_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(group_id, agent_id) DO UPDATE SET
             role = excluded.role,
             capabilities = excluded.capabilities",
        rusqlite::params![
            member.group_id,
            member.agent_id,
            member.role,
            member.capabilities,
            member.joined_at,
        ],
    )?;
    Ok(())
}

/// Remove an agent from a group.
pub fn remove_group_member(
    conn: &rusqlite::Connection,
    group_id: &str,
    agent_id: &str,
) -> DbResult<()> {
    conn.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND agent_id = ?2",
        rusqlite::params![group_id, agent_id],
    )?;
    Ok(())
}

/// Update a group member's role.
pub fn update_group_member_role(
    conn: &rusqlite::Connection,
    group_id: &str,
    agent_id: &str,
    role: Option<&Json<GroupMemberRole>>,
) -> DbResult<()> {
    conn.execute(
        "UPDATE group_members SET role = ?1 WHERE group_id = ?2 AND agent_id = ?3",
        rusqlite::params![role, group_id, agent_id],
    )?;
    Ok(())
}

/// Update a group member's capabilities.
pub fn update_group_member_capabilities(
    conn: &rusqlite::Connection,
    group_id: &str,
    agent_id: &str,
    capabilities: &Json<Vec<String>>,
) -> DbResult<()> {
    conn.execute(
        "UPDATE group_members SET capabilities = ?1 WHERE group_id = ?2 AND agent_id = ?3",
        rusqlite::params![capabilities, group_id, agent_id],
    )?;
    Ok(())
}

/// Update a group member's role and capabilities.
pub fn update_group_member(
    conn: &rusqlite::Connection,
    group_id: &str,
    agent_id: &str,
    role: Option<&Json<GroupMemberRole>>,
    capabilities: &Json<Vec<String>>,
) -> DbResult<()> {
    conn.execute(
        "UPDATE group_members SET role = ?1, capabilities = ?2 WHERE group_id = ?3 AND agent_id = ?4",
        rusqlite::params![role, capabilities, group_id, agent_id],
    )?;
    Ok(())
}

/// Get all groups an agent belongs to.
pub fn get_agent_groups(conn: &rusqlite::Connection, agent_id: &str) -> DbResult<Vec<AgentGroup>> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.description, g.pattern_type, g.pattern_config,
                g.created_at, g.updated_at
         FROM agent_groups g
         INNER JOIN group_members m ON g.id = m.group_id
         WHERE m.agent_id = ?1
         ORDER BY g.name",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], AgentGroup::from_row)?;
    let mut groups = Vec::new();
    for row in rows {
        groups.push(row?);
    }
    Ok(groups)
}

/// Update an agent group.
pub fn update_group(conn: &rusqlite::Connection, group: &AgentGroup) -> DbResult<()> {
    conn.execute(
        "UPDATE agent_groups SET
             name = ?1, description = ?2, pattern_type = ?3,
             pattern_config = ?4, updated_at = datetime('now')
         WHERE id = ?5",
        rusqlite::params![
            group.name,
            group.description,
            group.pattern_type,
            group.pattern_config,
            group.id,
        ],
    )?;
    Ok(())
}

/// Delete an agent group and its members.
pub fn delete_group(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    // Delete members first (foreign key constraint).
    conn.execute(
        "DELETE FROM group_members WHERE group_id = ?1",
        rusqlite::params![id],
    )?;
    conn.execute(
        "DELETE FROM agent_groups WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Check if an agent has a specific capability in any of their group memberships.
///
/// Returns true if the agent has the capability with specialist role in any group.
/// This is used for permission checks on cross-agent operations like constellation-wide search.
pub fn agent_has_capability(
    conn: &rusqlite::Connection,
    agent_id: &str,
    capability: &str,
) -> DbResult<bool> {
    let result: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM group_members
             WHERE agent_id = ?1
               AND json_extract(role, '$.type') = 'specialist'
               AND EXISTS (
                   SELECT 1 FROM json_each(capabilities)
                   WHERE json_each.value = ?2
               )
         )",
        rusqlite::params![agent_id, capability],
        |r| r.get(0),
    )?;
    Ok(result)
}

/// Check if two agents share any group membership.
///
/// Returns true if both agents are members of at least one common group.
/// This is used for permission checks on cross-agent search operations.
pub fn agents_share_group(
    conn: &rusqlite::Connection,
    agent_id_1: &str,
    agent_id_2: &str,
) -> DbResult<bool> {
    let result: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM group_members m1
             INNER JOIN group_members m2 ON m1.group_id = m2.group_id
             WHERE m1.agent_id = ?1 AND m2.agent_id = ?2
         )",
        rusqlite::params![agent_id_1, agent_id_2],
        |r| r.get(0),
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AgentStatus, PatternType};
    use crate::ConstellationDb;
    use chrono::Utc;

    fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().unwrap()
    }

    fn make_test_agent(conn: &rusqlite::Connection, id: &str, name: &str) {
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
        create_agent(conn, &agent).unwrap();
    }

    fn make_test_group(conn: &rusqlite::Connection, id: &str, name: &str) {
        let group = AgentGroup {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            pattern_type: PatternType::RoundRobin,
            pattern_config: Json(serde_json::json!({})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        create_group(conn, &group).unwrap();
    }

    #[test]
    fn test_agent_has_capability_specialist_with_matching_capability() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        make_test_agent(&conn, "agent1", "Agent 1");
        make_test_group(&conn, "group1", "Group 1");

        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Specialist {
                domain: "memory-management".to_string(),
            })),
            capabilities: Json(vec!["memory".to_string(), "search".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(&conn, &member).unwrap();

        assert!(agent_has_capability(&conn, "agent1", "memory").unwrap());
        assert!(agent_has_capability(&conn, "agent1", "search").unwrap());
    }

    #[test]
    fn test_agent_has_capability_specialist_without_matching_capability() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        make_test_agent(&conn, "agent1", "Agent 1");
        make_test_group(&conn, "group1", "Group 1");

        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Specialist {
                domain: "search".to_string(),
            })),
            capabilities: Json(vec!["search".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(&conn, &member).unwrap();

        assert!(!agent_has_capability(&conn, "agent1", "memory").unwrap());
    }

    #[test]
    fn test_agent_has_capability_non_specialist_role() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        make_test_agent(&conn, "agent1", "Agent 1");
        make_test_group(&conn, "group1", "Group 1");

        let member = GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec!["memory".to_string()]),
            joined_at: Utc::now(),
        };
        add_group_member(&conn, &member).unwrap();

        assert!(!agent_has_capability(&conn, "agent1", "memory").unwrap());
    }

    #[test]
    fn test_agents_share_group_in_same_group() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        make_test_agent(&conn, "agent1", "Agent 1");
        make_test_agent(&conn, "agent2", "Agent 2");
        make_test_group(&conn, "group1", "Group 1");

        for agent_id in ["agent1", "agent2"] {
            let member = GroupMember {
                group_id: "group1".to_string(),
                agent_id: agent_id.to_string(),
                role: Some(Json(GroupMemberRole::Regular)),
                capabilities: Json(vec![]),
                joined_at: Utc::now(),
            };
            add_group_member(&conn, &member).unwrap();
        }

        assert!(agents_share_group(&conn, "agent1", "agent2").unwrap());
        assert!(agents_share_group(&conn, "agent2", "agent1").unwrap());
    }

    #[test]
    fn test_agents_share_group_in_different_groups() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        make_test_agent(&conn, "agent1", "Agent 1");
        make_test_agent(&conn, "agent2", "Agent 2");
        make_test_group(&conn, "group1", "Group 1");
        make_test_group(&conn, "group2", "Group 2");

        add_group_member(&conn, &GroupMember {
            group_id: "group1".to_string(),
            agent_id: "agent1".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        }).unwrap();

        add_group_member(&conn, &GroupMember {
            group_id: "group2".to_string(),
            agent_id: "agent2".to_string(),
            role: Some(Json(GroupMemberRole::Regular)),
            capabilities: Json(vec![]),
            joined_at: Utc::now(),
        }).unwrap();

        assert!(!agents_share_group(&conn, "agent1", "agent2").unwrap());
    }
}
