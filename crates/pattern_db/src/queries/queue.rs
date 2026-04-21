//! Message queue queries for agent-to-agent communication.
//!
//! Queue tables (queued_messages) live in the messages database,
//! attached as the `msg` schema.

use rusqlite::types::FromSqlError;

use crate::error::DbResult;
use crate::models::QueuedMessage;

// ============================================================================
// Timestamp helpers
// ============================================================================

/// Parse a required TEXT column to `jiff::Timestamp`.
///
/// The orphan rule prevents implementing `FromSql` for `jiff::Timestamp` on
/// `rusqlite`, so the conversion is done explicitly here.
fn parse_timestamp(row: &rusqlite::Row, col: &str) -> rusqlite::Result<jiff::Timestamp> {
    let s: String = row.get(col)?;
    s.parse::<jiff::Timestamp>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(FromSqlError::Other(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid jiff::Timestamp {s:?}: {e}"),
            )))),
        )
    })
}

/// Parse an optional TEXT column to `Option<jiff::Timestamp>`.
fn parse_timestamp_opt(
    row: &rusqlite::Row,
    col: &str,
) -> rusqlite::Result<Option<jiff::Timestamp>> {
    let s: Option<String> = row.get(col)?;
    match s {
        None => Ok(None),
        Some(ref s) => s.parse::<jiff::Timestamp>().map(Some).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(FromSqlError::Other(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid jiff::Timestamp {s:?}: {e}"),
                )))),
            )
        }),
    }
}

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
            created_at: parse_timestamp(row, "created_at")?,
            processed_at: parse_timestamp_opt(row, "processed_at")?,
            content_json: row.get("content_json")?,
            metadata_json_full: row.get("metadata_json_full")?,
            batch_id: row.get("batch_id")?,
            role: row.get("role")?,
        })
    }
}

/// Create a queued message.
pub fn create_queued_message(conn: &rusqlite::Connection, msg: &QueuedMessage) -> DbResult<()> {
    // jiff::Timestamp does not implement rusqlite's ToSql (orphan rule); convert explicitly.
    let created_at = msg.created_at.to_string();
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
            created_at,
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
