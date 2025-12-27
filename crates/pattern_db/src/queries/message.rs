//! Message-related database queries.

use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{ArchiveSummary, BatchType, Message, MessageRole, MessageSummary};

/// Get a message by ID (excludes tombstoned messages).
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
            content_json as "content_json: _",
            content_preview,
            batch_type as "batch_type: BatchType",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            is_deleted as "is_deleted!: bool",
            created_at as "created_at!: _"
        FROM messages WHERE id = ? AND is_deleted = 0
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(msg)
}

/// Get messages for an agent, ordered by position (excludes archived and tombstoned).
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
            content_json as "content_json: _",
            content_preview,
            batch_type as "batch_type: BatchType",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            is_deleted as "is_deleted!: bool",
            created_at as "created_at!: _"
        FROM messages
        WHERE agent_id = ? AND is_archived = 0 AND is_deleted = 0
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

/// Get messages for an agent including archived (excludes tombstoned).
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
            content_json as "content_json: _",
            content_preview,
            batch_type as "batch_type: BatchType",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            is_deleted as "is_deleted!: bool",
            created_at as "created_at!: _"
        FROM messages
        WHERE agent_id = ? AND is_deleted = 0
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

/// Get messages after a specific position (excludes archived and tombstoned).
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
            content_json as "content_json: _",
            content_preview,
            batch_type as "batch_type: BatchType",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            is_deleted as "is_deleted!: bool",
            created_at as "created_at!: _"
        FROM messages
        WHERE agent_id = ? AND position > ? AND is_archived = 0 AND is_deleted = 0
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

/// Get messages in a specific batch (excludes tombstoned).
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
            content_json as "content_json: _",
            content_preview,
            batch_type as "batch_type: BatchType",
            source,
            source_metadata as "source_metadata: _",
            is_archived as "is_archived!: bool",
            is_deleted as "is_deleted!: bool",
            created_at as "created_at!: _"
        FROM messages
        WHERE batch_id = ? AND is_deleted = 0
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
                             role, content_json, content_preview, batch_type,
                             source, source_metadata, is_archived, is_deleted, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        msg.id,
        msg.agent_id,
        msg.position,
        msg.batch_id,
        msg.sequence_in_batch,
        msg.role,
        msg.content_json,
        msg.content_preview,
        msg.batch_type,
        msg.source,
        msg.source_metadata,
        msg.is_archived,
        msg.is_deleted,
        msg.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark messages as archived (excludes already-deleted messages).
pub async fn archive_messages(
    pool: &SqlitePool,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE messages SET is_archived = 1 WHERE agent_id = ? AND position < ? AND is_archived = 0 AND is_deleted = 0",
        agent_id,
        before_position
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Tombstone messages before a position (soft delete).
/// Use this instead of hard deletes to preserve data integrity.
pub async fn delete_messages(
    pool: &SqlitePool,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE messages SET is_deleted = 1 WHERE agent_id = ? AND position < ? AND is_deleted = 0",
        agent_id,
        before_position
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Tombstone a single message by ID (soft delete).
///
/// Sets is_deleted = 1 instead of hard deleting. This preserves the message
/// for audit purposes while making it invisible to normal queries.
///
/// Returns Ok(()) if the message was tombstoned, or if it didn't exist/was already deleted.
pub async fn delete_message(pool: &SqlitePool, id: &str) -> DbResult<()> {
    sqlx::query!(
        "UPDATE messages SET is_deleted = 1 WHERE id = ? AND is_deleted = 0",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update message content and preview (for cleanup operations).
///
/// This is used when finalize() modifies message content to remove unpaired tool calls.
pub async fn update_message_content(
    pool: &SqlitePool,
    id: &str,
    content_json: &sqlx::types::Json<serde_json::Value>,
    content_preview: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE messages SET content_json = ?, content_preview = ? WHERE id = ? AND is_deleted = 0",
        content_json,
        content_preview,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
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

/// Count messages for an agent (excluding archived and tombstoned).
pub async fn count_messages(pool: &SqlitePool, agent_id: &str) -> DbResult<i64> {
    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM messages WHERE agent_id = ? AND is_archived = 0 AND is_deleted = 0",
        agent_id
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

/// Count all messages for an agent (including archived, excluding tombstoned).
pub async fn count_all_messages(pool: &SqlitePool, agent_id: &str) -> DbResult<i64> {
    let result = sqlx::query!(
        "SELECT COUNT(*) as count FROM messages WHERE agent_id = ? AND is_deleted = 0",
        agent_id
    )
    .fetch_one(pool)
    .await?;
    Ok(result.count)
}

/// Get message summaries (lightweight projection for listing, excludes archived and tombstoned).
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
            content_preview as "content_preview: _",
            source,
            created_at as "created_at!: _"
        FROM messages
        WHERE agent_id = ? AND is_archived = 0 AND is_deleted = 0
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
