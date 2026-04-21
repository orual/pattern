//! Message-related database queries.
//!
//! Message tables live in the `msg` schema (attached via `ATTACH DATABASE`).
//! Query functions use unqualified table names; SQLite's schema search order
//! resolves them to `msg.messages` etc. automatically.

use rusqlite::OptionalExtension;
use rusqlite::types::FromSqlError;

use crate::Json;
use crate::error::DbResult;
use crate::models::{ArchiveSummary, Message, MessageSummary};

// ============================================================================
// Timestamp helpers
// ============================================================================

/// Parse a TEXT column to `jiff::Timestamp`.
///
/// The column stores an RFC 3339 UTC string produced by `jiff::Timestamp`'s
/// `Display` impl (e.g. `"2026-04-19T12:00:00.000000000Z"`). The rusqlite
/// orphan rule prevents implementing `FromSql` for `jiff::Timestamp` directly,
/// so the conversion is done explicitly here.
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

// ============================================================================
// from_row implementations
// ============================================================================

impl Message {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            position: row.get("position")?,
            batch_id: row.get("batch_id")?,
            sequence_in_batch: row.get("sequence_in_batch")?,
            role: row.get("role")?,
            content_json: row.get("content_json")?,
            content_preview: row.get("content_preview")?,
            batch_type: row.get("batch_type")?,
            source: row.get("source")?,
            source_metadata: row.get("source_metadata")?,
            is_archived: row.get("is_archived")?,
            is_deleted: row.get("is_deleted")?,
            created_at: parse_timestamp(row, "created_at")?,
        })
    }
}

impl ArchiveSummary {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            summary: row.get("summary")?,
            start_position: row.get("start_position")?,
            end_position: row.get("end_position")?,
            message_count: row.get("message_count")?,
            previous_summary_id: row.get("previous_summary_id")?,
            depth: row.get("depth")?,
            created_at: parse_timestamp(row, "created_at")?,
        })
    }
}

impl MessageSummary {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            position: row.get("position")?,
            role: row.get("role")?,
            content_preview: row.get("content_preview")?,
            source: row.get("source")?,
            created_at: parse_timestamp(row, "created_at")?,
        })
    }
}

// ============================================================================
// Message queries
// ============================================================================

/// Get a message by ID (excludes tombstoned messages).
pub fn get_message(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, position, batch_id, sequence_in_batch,
                role, content_json, content_preview, batch_type,
                source, source_metadata, is_archived, is_deleted, created_at
         FROM messages WHERE id = ?1 AND is_deleted = 0",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], Message::from_row)
        .optional()?;
    Ok(result)
}

/// Get messages for an agent, ordered by position (excludes archived and tombstoned).
pub fn get_messages(
    conn: &rusqlite::Connection,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, position, batch_id, sequence_in_batch,
                role, content_json, content_preview, batch_type,
                source, source_metadata, is_archived, is_deleted, created_at
         FROM messages
         WHERE agent_id = ?1 AND is_archived = 0 AND is_deleted = 0
         ORDER BY position DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id, limit], Message::from_row)?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

/// Get messages for an agent including archived (excludes tombstoned).
pub fn get_messages_with_archived(
    conn: &rusqlite::Connection,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, position, batch_id, sequence_in_batch,
                role, content_json, content_preview, batch_type,
                source, source_metadata, is_archived, is_deleted, created_at
         FROM messages
         WHERE agent_id = ?1 AND is_deleted = 0
         ORDER BY position DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id, limit], Message::from_row)?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

/// Get messages after a specific position (excludes archived and tombstoned).
pub fn get_messages_after(
    conn: &rusqlite::Connection,
    agent_id: &str,
    after_position: &str,
    limit: i64,
) -> DbResult<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, position, batch_id, sequence_in_batch,
                role, content_json, content_preview, batch_type,
                source, source_metadata, is_archived, is_deleted, created_at
         FROM messages
         WHERE agent_id = ?1 AND position > ?2 AND is_archived = 0 AND is_deleted = 0
         ORDER BY position ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![agent_id, after_position, limit],
        Message::from_row,
    )?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

/// Get messages in a specific batch (excludes tombstoned).
pub fn get_batch_messages(conn: &rusqlite::Connection, batch_id: &str) -> DbResult<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, position, batch_id, sequence_in_batch,
                role, content_json, content_preview, batch_type,
                source, source_metadata, is_archived, is_deleted, created_at
         FROM messages
         WHERE batch_id = ?1 AND is_deleted = 0
         ORDER BY sequence_in_batch",
    )?;
    let rows = stmt.query_map(rusqlite::params![batch_id], Message::from_row)?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row?);
    }
    Ok(messages)
}

/// Create a new message.
pub fn create_message(conn: &rusqlite::Connection, msg: &Message) -> DbResult<()> {
    // jiff::Timestamp does not implement rusqlite's ToSql (orphan rule), so
    // convert to RFC 3339 string explicitly. The stored format is
    // "YYYY-MM-DDTHH:MM:SS.NNNNNNNNNZ" which sorts correctly as TEXT.
    let created_at = msg.created_at.to_string();
    conn.execute(
        "INSERT INTO messages (id, agent_id, position, batch_id, sequence_in_batch,
                              role, content_json, content_preview, batch_type,
                              source, source_metadata, is_archived, is_deleted, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
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
            created_at,
        ],
    )?;
    Ok(())
}

/// Create or update a message (upsert).
///
/// If a message with the same ID exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
pub fn upsert_message(conn: &rusqlite::Connection, msg: &Message) -> DbResult<()> {
    // jiff::Timestamp does not implement rusqlite's ToSql (orphan rule), so
    // convert to RFC 3339 string explicitly.
    let created_at = msg.created_at.to_string();
    conn.execute(
        "INSERT INTO messages (id, agent_id, position, batch_id, sequence_in_batch,
                              role, content_json, content_preview, batch_type,
                              source, source_metadata, is_archived, is_deleted, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
             agent_id = excluded.agent_id,
             position = excluded.position,
             batch_id = excluded.batch_id,
             sequence_in_batch = excluded.sequence_in_batch,
             role = excluded.role,
             content_json = excluded.content_json,
             content_preview = excluded.content_preview,
             batch_type = excluded.batch_type,
             source = excluded.source,
             source_metadata = excluded.source_metadata,
             is_archived = excluded.is_archived,
             is_deleted = excluded.is_deleted",
        rusqlite::params![
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
            created_at,
        ],
    )?;
    Ok(())
}

/// Mark messages as archived (excludes already-deleted messages).
pub fn archive_messages(
    conn: &rusqlite::Connection,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    let count = conn.execute(
        "UPDATE messages SET is_archived = 1 WHERE agent_id = ?1 AND position < ?2 AND is_archived = 0 AND is_deleted = 0",
        rusqlite::params![agent_id, before_position],
    )?;
    Ok(count as u64)
}

/// Tombstone messages before a position (soft delete).
/// Use this instead of hard deletes to preserve data integrity.
pub fn delete_messages(
    conn: &rusqlite::Connection,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    let count = conn.execute(
        "UPDATE messages SET is_deleted = 1 WHERE agent_id = ?1 AND position < ?2 AND is_deleted = 0",
        rusqlite::params![agent_id, before_position],
    )?;
    Ok(count as u64)
}

/// Tombstone a single message by ID (soft delete).
///
/// Sets is_deleted = 1 instead of hard deleting. This preserves the message
/// for audit purposes while making it invisible to normal queries.
///
/// Returns Ok(()) if the message was tombstoned, or if it didn't exist/was already deleted.
pub fn delete_message(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE messages SET is_deleted = 1 WHERE id = ?1 AND is_deleted = 0",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Update message content and preview (for cleanup operations).
///
/// This is used when finalize() modifies message content to remove unpaired tool calls.
pub fn update_message_content(
    conn: &rusqlite::Connection,
    id: &str,
    content_json: &Json<serde_json::Value>,
    content_preview: Option<&str>,
) -> DbResult<()> {
    conn.execute(
        "UPDATE messages SET content_json = ?1, content_preview = ?2 WHERE id = ?3 AND is_deleted = 0",
        rusqlite::params![content_json, content_preview, id],
    )?;
    Ok(())
}

/// Get archive summary by ID.
pub fn get_archive_summary(
    conn: &rusqlite::Connection,
    id: &str,
) -> DbResult<Option<ArchiveSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, summary, start_position, end_position,
                message_count, previous_summary_id, depth, created_at
         FROM archive_summaries WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], ArchiveSummary::from_row)
        .optional()?;
    Ok(result)
}

/// Get archive summaries for an agent.
pub fn get_archive_summaries(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<ArchiveSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, summary, start_position, end_position,
                message_count, previous_summary_id, depth, created_at
         FROM archive_summaries WHERE agent_id = ?1 ORDER BY start_position",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], ArchiveSummary::from_row)?;
    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row?);
    }
    Ok(summaries)
}

/// Create an archive summary.
pub fn create_archive_summary(
    conn: &rusqlite::Connection,
    summary: &ArchiveSummary,
) -> DbResult<()> {
    // jiff::Timestamp does not implement rusqlite's ToSql (orphan rule); convert explicitly.
    let created_at = summary.created_at.to_string();
    conn.execute(
        "INSERT INTO archive_summaries (id, agent_id, summary, start_position, end_position,
                                        message_count, previous_summary_id, depth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            summary.id,
            summary.agent_id,
            summary.summary,
            summary.start_position,
            summary.end_position,
            summary.message_count,
            summary.previous_summary_id,
            summary.depth,
            created_at,
        ],
    )?;
    Ok(())
}

/// Create or update an archive summary (upsert).
///
/// If a summary with the same ID exists, it will be updated in place.
/// Used by import to handle re-imports idempotently.
pub fn upsert_archive_summary(
    conn: &rusqlite::Connection,
    summary: &ArchiveSummary,
) -> DbResult<()> {
    // jiff::Timestamp does not implement rusqlite's ToSql (orphan rule); convert explicitly.
    let created_at = summary.created_at.to_string();
    conn.execute(
        "INSERT INTO archive_summaries (id, agent_id, summary, start_position, end_position,
                                        message_count, previous_summary_id, depth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
             agent_id = excluded.agent_id,
             summary = excluded.summary,
             start_position = excluded.start_position,
             end_position = excluded.end_position,
             message_count = excluded.message_count,
             previous_summary_id = excluded.previous_summary_id,
             depth = excluded.depth",
        rusqlite::params![
            summary.id,
            summary.agent_id,
            summary.summary,
            summary.start_position,
            summary.end_position,
            summary.message_count,
            summary.previous_summary_id,
            summary.depth,
            created_at,
        ],
    )?;
    Ok(())
}

/// Get the summary-head vector for an agent: one entry per depth level,
/// newest at each depth (by `start_position`), chronologically ordered
/// by `start_position` ascending.
///
/// This is the minimal context a composer needs to prepend "earlier
/// conversation" summaries to segment 2. Task 13's compaction layer
/// updates the underlying rows; this query reads the current state.
pub fn get_summary_head(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<ArchiveSummary>> {
    let mut stmt = conn.prepare(
        "WITH latest_per_depth AS (
             SELECT depth, MAX(start_position) AS latest_pos
             FROM archive_summaries
             WHERE agent_id = ?1
             GROUP BY depth
         )
         SELECT
             a.id, a.agent_id, a.summary, a.start_position, a.end_position,
             a.message_count, a.previous_summary_id, a.depth, a.created_at
         FROM archive_summaries a
         JOIN latest_per_depth ld ON a.depth = ld.depth AND a.start_position = ld.latest_pos
         WHERE a.agent_id = ?2
         ORDER BY a.start_position ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![agent_id, agent_id],
        ArchiveSummary::from_row,
    )?;
    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row?);
    }
    Ok(summaries)
}

/// Count messages for an agent (excluding archived and tombstoned).
pub fn count_messages(conn: &rusqlite::Connection, agent_id: &str) -> DbResult<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id = ?1 AND is_archived = 0 AND is_deleted = 0",
        rusqlite::params![agent_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Count all messages for an agent (including archived, excluding tombstoned).
pub fn count_all_messages(conn: &rusqlite::Connection, agent_id: &str) -> DbResult<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id = ?1 AND is_deleted = 0",
        rusqlite::params![agent_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Get message summaries (lightweight projection for listing, excludes archived and tombstoned).
pub fn get_message_summaries(
    conn: &rusqlite::Connection,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<MessageSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, position, role, content_preview, source, created_at
         FROM messages
         WHERE agent_id = ?1 AND is_archived = 0 AND is_deleted = 0
         ORDER BY position DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id, limit], MessageSummary::from_row)?;
    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row?);
    }
    Ok(summaries)
}
