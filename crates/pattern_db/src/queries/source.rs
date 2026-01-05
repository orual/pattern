//! Data source queries.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{AgentDataSource, DataSource, SourceType};

// ============================================================================
// DataSource CRUD
// ============================================================================

/// Create a new data source.
pub async fn create_data_source(pool: &SqlitePool, source: &DataSource) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO data_sources (id, name, source_type, config, last_sync_at, sync_cursor, enabled, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        source.id,
        source.name,
        source.source_type,
        source.config,
        source.last_sync_at,
        source.sync_cursor,
        source.enabled,
        source.created_at,
        source.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a data source by ID.
pub async fn get_data_source(pool: &SqlitePool, id: &str) -> DbResult<Option<DataSource>> {
    let source = sqlx::query_as!(
        DataSource,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            source_type as "source_type!: SourceType",
            config as "config!: _",
            last_sync_at as "last_sync_at: _",
            sync_cursor,
            enabled as "enabled!: bool",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM data_sources WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(source)
}

/// Get a data source by name.
pub async fn get_data_source_by_name(
    pool: &SqlitePool,
    name: &str,
) -> DbResult<Option<DataSource>> {
    let source = sqlx::query_as!(
        DataSource,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            source_type as "source_type!: SourceType",
            config as "config!: _",
            last_sync_at as "last_sync_at: _",
            sync_cursor,
            enabled as "enabled!: bool",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM data_sources WHERE name = ?
        "#,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(source)
}

/// List all data sources.
pub async fn list_data_sources(pool: &SqlitePool) -> DbResult<Vec<DataSource>> {
    let sources = sqlx::query_as!(
        DataSource,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            source_type as "source_type!: SourceType",
            config as "config!: _",
            last_sync_at as "last_sync_at: _",
            sync_cursor,
            enabled as "enabled!: bool",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM data_sources ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(sources)
}

/// List enabled data sources.
pub async fn list_enabled_data_sources(pool: &SqlitePool) -> DbResult<Vec<DataSource>> {
    let sources = sqlx::query_as!(
        DataSource,
        r#"
        SELECT
            id as "id!",
            name as "name!",
            source_type as "source_type!: SourceType",
            config as "config!: _",
            last_sync_at as "last_sync_at: _",
            sync_cursor,
            enabled as "enabled!: bool",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM data_sources WHERE enabled = 1 ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(sources)
}

/// Update a data source.
pub async fn update_data_source(pool: &SqlitePool, source: &DataSource) -> DbResult<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE data_sources
        SET name = ?, source_type = ?, config = ?, enabled = ?, updated_at = ?
        WHERE id = ?
        "#,
        source.name,
        source.source_type,
        source.config,
        source.enabled,
        source.updated_at,
        source.id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update sync state for a data source.
pub async fn update_sync_state(
    pool: &SqlitePool,
    id: &str,
    cursor: Option<&str>,
) -> DbResult<bool> {
    let now = Utc::now();
    let result = sqlx::query!(
        r#"
        UPDATE data_sources
        SET last_sync_at = ?, sync_cursor = ?, updated_at = ?
        WHERE id = ?
        "#,
        now,
        cursor,
        now,
        id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Enable or disable a data source.
pub async fn set_data_source_enabled(pool: &SqlitePool, id: &str, enabled: bool) -> DbResult<bool> {
    let now = Utc::now();
    let result = sqlx::query!(
        r#"
        UPDATE data_sources SET enabled = ?, updated_at = ? WHERE id = ?
        "#,
        enabled,
        now,
        id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete a data source.
pub async fn delete_data_source(pool: &SqlitePool, id: &str) -> DbResult<bool> {
    let result = sqlx::query!("DELETE FROM data_sources WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

// ============================================================================
// AgentDataSource (subscriptions)
// ============================================================================

/// Subscribe an agent to a data source.
pub async fn subscribe_agent_to_source(
    pool: &SqlitePool,
    agent_id: &str,
    source_id: &str,
    notification_template: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO agent_data_sources (agent_id, source_id, notification_template)
        VALUES (?, ?, ?)
        ON CONFLICT(agent_id, source_id) DO UPDATE SET notification_template = excluded.notification_template
        "#,
        agent_id,
        source_id,
        notification_template,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Unsubscribe an agent from a data source.
pub async fn unsubscribe_agent_from_source(
    pool: &SqlitePool,
    agent_id: &str,
    source_id: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM agent_data_sources WHERE agent_id = ? AND source_id = ?",
        agent_id,
        source_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Get all subscriptions for an agent.
pub async fn get_agent_subscriptions(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<AgentDataSource>> {
    let subs = sqlx::query_as!(
        AgentDataSource,
        r#"
        SELECT
            agent_id as "agent_id!",
            source_id as "source_id!",
            notification_template
        FROM agent_data_sources WHERE agent_id = ?
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(subs)
}

/// Get all agents subscribed to a source.
pub async fn get_source_subscribers(
    pool: &SqlitePool,
    source_id: &str,
) -> DbResult<Vec<AgentDataSource>> {
    let subs = sqlx::query_as!(
        AgentDataSource,
        r#"
        SELECT
            agent_id as "agent_id!",
            source_id as "source_id!",
            notification_template
        FROM agent_data_sources WHERE source_id = ?
        "#,
        source_id
    )
    .fetch_all(pool)
    .await?;
    Ok(subs)
}
