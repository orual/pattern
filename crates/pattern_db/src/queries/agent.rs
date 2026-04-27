//! Agent-related database queries.

use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{Agent, AgentStatus};

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
    let result = stmt
        .query_row(rusqlite::params![id], Agent::from_row)
        .optional()?;
    Ok(result)
}

/// Get an agent by name.
pub fn get_agent_by_name(conn: &rusqlite::Connection, name: &str) -> DbResult<Option<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, description, model_provider, model_name, system_prompt,
                config, enabled_tools, tool_rules, status, created_at, updated_at
         FROM agents WHERE name = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![name], Agent::from_row)
        .optional()?;
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

