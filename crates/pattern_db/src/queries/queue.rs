//! Message queue queries for agent-to-agent communication.

use crate::error::DbResult;
use crate::models::QueuedMessage;
use sqlx::SqlitePool;

/// Create a queued message.
pub async fn create_queued_message(pool: &SqlitePool, msg: &QueuedMessage) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO queued_messages (id, target_agent_id, source_agent_id, content,
                                     origin_json, metadata_json, priority, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        msg.id,
        msg.target_agent_id,
        msg.source_agent_id,
        msg.content,
        msg.origin_json,
        msg.metadata_json,
        msg.priority,
        msg.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get pending messages for an agent.
pub async fn get_pending_messages(
    pool: &SqlitePool,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<QueuedMessage>> {
    let messages = sqlx::query_as!(
        QueuedMessage,
        r#"
        SELECT
            id as "id!",
            target_agent_id as "target_agent_id!",
            source_agent_id,
            content as "content!",
            origin_json,
            metadata_json,
            priority as "priority!",
            created_at as "created_at!: _",
            processed_at as "processed_at: _"
        FROM queued_messages
        WHERE target_agent_id = ? AND processed_at IS NULL
        ORDER BY priority DESC, created_at ASC
        LIMIT ?
        "#,
        agent_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

/// Mark a message as processed.
pub async fn mark_message_processed(pool: &SqlitePool, id: &str) -> DbResult<()> {
    sqlx::query!(
        "UPDATE queued_messages SET processed_at = datetime('now') WHERE id = ?",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete old processed messages (cleanup).
pub async fn delete_old_processed(pool: &SqlitePool, older_than_hours: i64) -> DbResult<u64> {
    let result = sqlx::query!(
        r#"
        DELETE FROM queued_messages
        WHERE processed_at IS NOT NULL
        AND processed_at < datetime('now', '-' || ? || ' hours')
        "#,
        older_than_hours
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
