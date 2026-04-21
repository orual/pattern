//! Data source queries.

use chrono::Utc;
use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{AgentDataSource, DataSource};

// ============================================================================
// from_row implementations
// ============================================================================

impl DataSource {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            source_type: row.get("source_type")?,
            config: row.get("config")?,
            last_sync_at: row.get("last_sync_at")?,
            sync_cursor: row.get("sync_cursor")?,
            enabled: row.get("enabled")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl AgentDataSource {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            agent_id: row.get("agent_id")?,
            source_id: row.get("source_id")?,
            notification_template: row.get("notification_template")?,
        })
    }
}

// ============================================================================
// DataSource CRUD
// ============================================================================

/// Create a new data source.
pub fn create_data_source(conn: &rusqlite::Connection, source: &DataSource) -> DbResult<()> {
    conn.execute(
        "INSERT INTO data_sources (id, name, source_type, config, last_sync_at, sync_cursor, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![source.id, source.name, source.source_type, source.config, source.last_sync_at, source.sync_cursor, source.enabled, source.created_at, source.updated_at],
    )?;
    Ok(())
}

/// Get a data source by ID.
pub fn get_data_source(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<DataSource>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, source_type, config, last_sync_at, sync_cursor, enabled, created_at, updated_at
         FROM data_sources WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], DataSource::from_row)
        .optional()?;
    Ok(result)
}

/// Get a data source by name.
pub fn get_data_source_by_name(
    conn: &rusqlite::Connection,
    name: &str,
) -> DbResult<Option<DataSource>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, source_type, config, last_sync_at, sync_cursor, enabled, created_at, updated_at
         FROM data_sources WHERE name = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![name], DataSource::from_row)
        .optional()?;
    Ok(result)
}

/// List all data sources.
pub fn list_data_sources(conn: &rusqlite::Connection) -> DbResult<Vec<DataSource>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, source_type, config, last_sync_at, sync_cursor, enabled, created_at, updated_at
         FROM data_sources ORDER BY name",
    )?;
    let rows = stmt.query_map([], DataSource::from_row)?;
    let mut sources = Vec::new();
    for row in rows {
        sources.push(row?);
    }
    Ok(sources)
}

/// List enabled data sources.
pub fn list_enabled_data_sources(conn: &rusqlite::Connection) -> DbResult<Vec<DataSource>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, source_type, config, last_sync_at, sync_cursor, enabled, created_at, updated_at
         FROM data_sources WHERE enabled = 1 ORDER BY name",
    )?;
    let rows = stmt.query_map([], DataSource::from_row)?;
    let mut sources = Vec::new();
    for row in rows {
        sources.push(row?);
    }
    Ok(sources)
}

/// Update a data source.
pub fn update_data_source(conn: &rusqlite::Connection, source: &DataSource) -> DbResult<bool> {
    let count = conn.execute(
        "UPDATE data_sources SET name = ?1, source_type = ?2, config = ?3, enabled = ?4, updated_at = ?5 WHERE id = ?6",
        rusqlite::params![source.name, source.source_type, source.config, source.enabled, source.updated_at, source.id],
    )?;
    Ok(count > 0)
}

/// Update sync state for a data source.
pub fn update_sync_state(
    conn: &rusqlite::Connection,
    id: &str,
    cursor: Option<&str>,
) -> DbResult<bool> {
    let now = Utc::now();
    let count = conn.execute(
        "UPDATE data_sources SET last_sync_at = ?1, sync_cursor = ?2, updated_at = ?3 WHERE id = ?4",
        rusqlite::params![now, cursor, now, id],
    )?;
    Ok(count > 0)
}

/// Enable or disable a data source.
pub fn set_data_source_enabled(
    conn: &rusqlite::Connection,
    id: &str,
    enabled: bool,
) -> DbResult<bool> {
    let now = Utc::now();
    let count = conn.execute(
        "UPDATE data_sources SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![enabled, now, id],
    )?;
    Ok(count > 0)
}

/// Delete a data source.
pub fn delete_data_source(conn: &rusqlite::Connection, id: &str) -> DbResult<bool> {
    let count = conn.execute(
        "DELETE FROM data_sources WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(count > 0)
}

// ============================================================================
// AgentDataSource (subscriptions)
// ============================================================================

/// Subscribe an agent to a data source.
pub fn subscribe_agent_to_source(
    conn: &rusqlite::Connection,
    agent_id: &str,
    source_id: &str,
    notification_template: Option<&str>,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO agent_data_sources (agent_id, source_id, notification_template)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(agent_id, source_id) DO UPDATE SET notification_template = excluded.notification_template",
        rusqlite::params![agent_id, source_id, notification_template],
    )?;
    Ok(())
}

/// Unsubscribe an agent from a data source.
pub fn unsubscribe_agent_from_source(
    conn: &rusqlite::Connection,
    agent_id: &str,
    source_id: &str,
) -> DbResult<bool> {
    let count = conn.execute(
        "DELETE FROM agent_data_sources WHERE agent_id = ?1 AND source_id = ?2",
        rusqlite::params![agent_id, source_id],
    )?;
    Ok(count > 0)
}

/// Get all subscriptions for an agent.
pub fn get_agent_subscriptions(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<AgentDataSource>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, source_id, notification_template FROM agent_data_sources WHERE agent_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], AgentDataSource::from_row)?;
    let mut subs = Vec::new();
    for row in rows {
        subs.push(row?);
    }
    Ok(subs)
}

/// Get all agents subscribed to a source.
pub fn get_source_subscribers(
    conn: &rusqlite::Connection,
    source_id: &str,
) -> DbResult<Vec<AgentDataSource>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, source_id, notification_template FROM agent_data_sources WHERE source_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id], AgentDataSource::from_row)?;
    let mut subs = Vec::new();
    for row in rows {
        subs.push(row?);
    }
    Ok(subs)
}
