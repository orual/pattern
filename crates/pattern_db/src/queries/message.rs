//! Message-related database queries.

use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{ArchiveSummary, Message, MessageRole, MessageSummary};

/// Get a message by ID.
pub async fn get_message(pool: &SqlitePool, id: &str) -> DbResult<Option<Message>> {
    let msg = sqlx::query_as!(
        Message,
        r#"
        SELECT 
            id as "id!",
            agent_id as "agent_id!",
            position as "position!",
            batch_id,
            sequence_in_batch,
            role as "role!: MessageRole",
            content,
            tool_call_id,
            tool_name,
            tool_args as "tool_args: _",
            tool_result as "tool_result: _",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            created_at as "created_at!: _"
        FROM messages WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}

/// Get messages for an agent, ordered by position (not archived).
pub async fn get_messages(pool: &SqlitePool, agent_id: &str, limit: i64) -> DbResult<Vec<Message>> {
    let messages = sqlx::query_as!(
        Message,
        r#"
        SELECT 
            id as "id!",
            agent_id as "agent_id!",
            position as "position!",
            batch_id,
            sequence_in_batch,
            role as "role!: MessageRole",
            content,
            tool_call_id,
            tool_name,
            tool_args as "tool_args: _",
            tool_result as "tool_result: _",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            created_at as "created_at!: _"
        FROM messages 
        WHERE agent_id = ? AND is_archived = 0 
        ORDER BY position DESC 
        LIMIT ?
        "#,
        agent_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

/// Get messages for an agent including archived.
pub async fn get_messages_with_archived(
    pool: &SqlitePool,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<Message>> {
    let messages = sqlx::query_as!(
        Message,
        r#"
        SELECT 
            id as "id!",
            agent_id as "agent_id!",
            position as "position!",
            batch_id,
            sequence_in_batch,
            role as "role!: MessageRole",
            content,
            tool_call_id,
            tool_name,
            tool_args as "tool_args: _",
            tool_result as "tool_result: _",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            created_at as "created_at!: _"
        FROM messages 
        WHERE agent_id = ? 
        ORDER BY position DESC 
        LIMIT ?
        "#,
        agent_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

/// Get messages after a specific position.
pub async fn get_messages_after(
    pool: &SqlitePool,
    agent_id: &str,
    after_position: &str,
    limit: i64,
) -> DbResult<Vec<Message>> {
    let messages = sqlx::query_as!(
        Message,
        r#"
        SELECT 
            id as "id!",
            agent_id as "agent_id!",
            position as "position!",
            batch_id,
            sequence_in_batch,
            role as "role!: MessageRole",
            content,
            tool_call_id,
            tool_name,
            tool_args as "tool_args: _",
            tool_result as "tool_result: _",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            created_at as "created_at!: _"
        FROM messages 
        WHERE agent_id = ? AND position > ? 
        ORDER BY position ASC 
        LIMIT ?
        "#,
        agent_id,
        after_position,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

/// Get messages in a specific batch.
pub async fn get_batch_messages(pool: &SqlitePool, batch_id: &str) -> DbResult<Vec<Message>> {
    let messages = sqlx::query_as!(
        Message,
        r#"
        SELECT 
            id as "id!",
            agent_id as "agent_id!",
            position as "position!",
            batch_id,
            sequence_in_batch,
            role as "role!: MessageRole",
            content,
            tool_call_id,
            tool_name,
            tool_args as "tool_args: _",
            tool_result as "tool_result: _",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            created_at as "created_at!: _"
        FROM messages 
        WHERE batch_id = ? 
        ORDER BY sequence_in_batch
        "#,
        batch_id
    )
    .fetch_all(pool)
    .await?;
    Ok(messages)
}

/// Create a new message.
pub async fn create_message(pool: &SqlitePool, msg: &Message) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO messages (id, agent_id, position, batch_id, sequence_in_batch,
                             role, content, tool_call_id, tool_name, tool_args, tool_result,
                             source, source_metadata, is_archived, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        msg.id,
        msg.agent_id,
        msg.position,
        msg.batch_id,
        msg.sequence_in_batch,
        msg.role,
        msg.content,
        msg.tool_call_id,
        msg.tool_name,
        msg.tool_args,
        msg.tool_result,
        msg.source,
        msg.source_metadata,
        msg.is_archived,
        msg.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark messages as archived.
pub async fn archive_messages(
    pool: &SqlitePool,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE messages SET is_archived = 1 WHERE agent_id = ? AND position < ? AND is_archived = 0",
        agent_id,
        before_position
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete messages (hard delete, use with caution).
pub async fn delete_messages(
    pool: &SqlitePool,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "DELETE FROM messages WHERE agent_id = ? AND position < ?",
        agent_id,
        before_position
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Get archive summary by ID.
pub async fn get_archive_summary(pool: &SqlitePool, id: &str) -> DbResult<Option<ArchiveSummary>> {
    let summary = sqlx::query_as!(
        ArchiveSummary,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            summary as "summary!",
            start_position as "start_position!",
            end_position as "end_position!",
            message_count as "message_count!",
            previous_summary_id,
            depth as "depth!",
            created_at as "created_at!: _"
        FROM archive_summaries WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(summary)
}

/// Get archive summaries for an agent.
pub async fn get_archive_summaries(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<ArchiveSummary>> {
    let summaries = sqlx::query_as!(
        ArchiveSummary,
        r#"
        SELECT
            id as "id!",
            agent_id as "agent_id!",
            summary as "summary!",
            start_position as "start_position!",
            end_position as "end_position!",
            message_count as "message_count!",
            previous_summary_id,
            depth as "depth!",
            created_at as "created_at!: _"
        FROM archive_summaries WHERE agent_id = ? ORDER BY start_position
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(summaries)
}

/// Create an archive summary.
pub async fn create_archive_summary(pool: &SqlitePool, summary: &ArchiveSummary) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO archive_summaries (id, agent_id, summary, start_position, end_position, message_count, previous_summary_id, depth, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        summary.id,
        summary.agent_id,
        summary.summary,
        summary.start_position,
        summary.end_position,
        summary.message_count,
        summary.previous_summary_id,
        summary.depth,
        summary.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Count messages for an agent (excluding archived).
pub async fn count_messages(pool: &SqlitePool, agent_id: &str) -> DbResult<i64> {
    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM messages WHERE agent_id = ? AND is_archived = 0",
        agent_id
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

/// Count all messages for an agent (including archived).
pub async fn count_all_messages(pool: &SqlitePool, agent_id: &str) -> DbResult<i64> {
    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM messages WHERE agent_id = ?",
        agent_id
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

/// Get message summaries (lightweight projection for listing).
pub async fn get_message_summaries(
    pool: &SqlitePool,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<MessageSummary>> {
    let summaries = sqlx::query_as!(
        MessageSummary,
        r#"
        SELECT 
            id as "id!",
            position as "position!",
            role as "role!: MessageRole",
            CAST(CASE WHEN LENGTH(content) > 100 THEN SUBSTR(content, 1, 100) || '...' ELSE content END AS TEXT) as "content_preview: _",
            source,
            created_at as "created_at!: _"
        FROM messages 
        WHERE agent_id = ? AND is_archived = 0 
        ORDER BY position DESC 
        LIMIT ?
        "#,
        agent_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(summaries)
}
