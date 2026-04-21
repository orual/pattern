//! Message queue queries for agent-to-agent communication.
//!
//! Queue tables (queued_messages) live in the messages database,
//! attached as the `msg` schema.

use crate::error::DbResult;
use crate::models::QueuedMessage;

// ============================================================================
// from_row implementation
// ============================================================================

impl QueuedMessage {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            target_agent_id: row.get("target_agent_id")?,
            source_agent_id: row.get("source_agent_id")?,
            content: row.get("content")?,
            origin_json: row.get("origin_json")?,
            metadata_json: row.get("metadata_json")?,
            priority: row.get("priority")?,
            created_at: row.get("created_at")?,
            processed_at: row.get("processed_at")?,
            content_json: row.get("content_json")?,
            metadata_json_full: row.get("metadata_json_full")?,
            batch_id: row.get("batch_id")?,
            role: row.get("role")?,
        })
    }
}

/// Create a queued message.
pub fn create_queued_message(conn: &rusqlite::Connection, msg: &QueuedMessage) -> DbResult<()> {
    conn.execute(
        "INSERT INTO queued_messages (id, target_agent_id, source_agent_id, content,
                                      origin_json, metadata_json, priority, created_at,
                                      content_json, metadata_json_full, batch_id, role)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            msg.id,
            msg.target_agent_id,
            msg.source_agent_id,
            msg.content,
            msg.origin_json,
            msg.metadata_json,
            msg.priority,
            msg.created_at,
            msg.content_json,
            msg.metadata_json_full,
            msg.batch_id,
            msg.role,
        ],
    )?;
    Ok(())
}

/// Get pending messages for an agent.
pub fn get_pending_messages(
    conn: &rusqlite::Connection,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<QueuedMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, target_agent_id, source_agent_id, content,
                origin_json, metadata_json, priority, created_at,
                processed_at, content_json, metadata_json_full, batch_id, role
         FROM queued_messages
         WHERE target_agent_id = ?1 AND processed_at IS NULL
         ORDER BY priority DESC, created_at ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id, limit], QueuedMessage::from_row)?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

/// Mark a message as processed.
pub fn mark_message_processed(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE queued_messages SET processed_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Delete old processed messages (cleanup).
pub fn delete_old_processed(conn: &rusqlite::Connection, older_than_hours: i64) -> DbResult<u64> {
    let count = conn.execute(
        "DELETE FROM queued_messages
         WHERE processed_at IS NOT NULL
         AND processed_at < datetime('now', '-' || ?1 || ' hours')",
        rusqlite::params![older_than_hours],
    )?;
    Ok(count as u64)
}
