//! Event and reminder queries.

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{Event, EventOccurrence, OccurrenceStatus};

// ============================================================================
// from_row implementations
// ============================================================================

impl Event {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            agent_id: row.get("agent_id")?,
            title: row.get("title")?,
            description: row.get("description")?,
            starts_at: row.get("starts_at")?,
            ends_at: row.get("ends_at")?,
            rrule: row.get("rrule")?,
            reminder_minutes: row.get("reminder_minutes")?,
            all_day: row.get("all_day")?,
            location: row.get("location")?,
            external_id: row.get("external_id")?,
            external_source: row.get("external_source")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl EventOccurrence {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            event_id: row.get("event_id")?,
            starts_at: row.get("starts_at")?,
            ends_at: row.get("ends_at")?,
            status: row.get("status")?,
            notes: row.get("notes")?,
            created_at: row.get("created_at")?,
        })
    }
}

// ============================================================================
// Event CRUD
// ============================================================================

/// Create a new event.
pub fn create_event(conn: &rusqlite::Connection, event: &Event) -> DbResult<()> {
    conn.execute(
        "INSERT INTO events (id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![event.id, event.agent_id, event.title, event.description, event.starts_at, event.ends_at, event.rrule, event.reminder_minutes, event.created_at, event.updated_at],
    )?;
    Ok(())
}

/// Get an event by ID.
pub fn get_event(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes,
                all_day, location, external_id, external_source, created_at, updated_at
         FROM events WHERE id = ?1",
    )?;
    let result = stmt.query_row(rusqlite::params![id], Event::from_row).optional()?;
    Ok(result)
}

/// List events for an agent (or constellation-level).
pub fn list_events(conn: &rusqlite::Connection, agent_id: Option<&str>) -> DbResult<Vec<Event>> {
    let mut events = Vec::new();
    match agent_id {
        Some(aid) => {
            let mut stmt = conn.prepare(
                "SELECT id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes,
                        all_day, location, external_id, external_source, created_at, updated_at
                 FROM events WHERE agent_id = ?1 ORDER BY starts_at ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![aid], Event::from_row)?;
            for row in rows { events.push(row?); }
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes,
                        all_day, location, external_id, external_source, created_at, updated_at
                 FROM events WHERE agent_id IS NULL ORDER BY starts_at ASC",
            )?;
            let rows = stmt.query_map([], Event::from_row)?;
            for row in rows { events.push(row?); }
        }
    }
    Ok(events)
}

/// Get events in a time range.
pub fn get_events_in_range(conn: &rusqlite::Connection, start: DateTime<Utc>, end: DateTime<Utc>) -> DbResult<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes,
                all_day, location, external_id, external_source, created_at, updated_at
         FROM events WHERE starts_at >= ?1 AND starts_at <= ?2 ORDER BY starts_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![start, end], Event::from_row)?;
    let mut events = Vec::new();
    for row in rows { events.push(row?); }
    Ok(events)
}

/// Get upcoming events (starting within N hours).
pub fn get_upcoming_events(conn: &rusqlite::Connection, hours: i64) -> DbResult<Vec<Event>> {
    let now = Utc::now();
    let deadline = now + chrono::Duration::hours(hours);
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes,
                all_day, location, external_id, external_source, created_at, updated_at
         FROM events WHERE starts_at >= ?1 AND starts_at <= ?2 ORDER BY starts_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![now, deadline], Event::from_row)?;
    let mut events = Vec::new();
    for row in rows { events.push(row?); }
    Ok(events)
}

/// Get events needing reminders (reminder time is now or past, but event hasn't started).
pub fn get_events_needing_reminders(conn: &rusqlite::Connection) -> DbResult<Vec<Event>> {
    let now = Utc::now();
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes,
                all_day, location, external_id, external_source, created_at, updated_at
         FROM events
         WHERE reminder_minutes IS NOT NULL
           AND starts_at > ?1
           AND datetime(starts_at, '-' || reminder_minutes || ' minutes') <= ?2
         ORDER BY starts_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![now, now], Event::from_row)?;
    let mut events = Vec::new();
    for row in rows { events.push(row?); }
    Ok(events)
}

/// Update an event.
pub fn update_event(conn: &rusqlite::Connection, event: &Event) -> DbResult<bool> {
    let count = conn.execute(
        "UPDATE events SET title = ?1, description = ?2, starts_at = ?3, ends_at = ?4,
             rrule = ?5, reminder_minutes = ?6, updated_at = ?7
         WHERE id = ?8",
        rusqlite::params![event.title, event.description, event.starts_at, event.ends_at, event.rrule, event.reminder_minutes, event.updated_at, event.id],
    )?;
    Ok(count > 0)
}

/// Delete an event.
pub fn delete_event(conn: &rusqlite::Connection, id: &str) -> DbResult<bool> {
    let count = conn.execute("DELETE FROM events WHERE id = ?1", rusqlite::params![id])?;
    Ok(count > 0)
}

// ============================================================================
// EventOccurrence (for recurring events)
// ============================================================================

/// Create an event occurrence.
pub fn create_occurrence(conn: &rusqlite::Connection, occurrence: &EventOccurrence) -> DbResult<()> {
    conn.execute(
        "INSERT INTO event_occurrences (id, event_id, starts_at, ends_at, status, notes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![occurrence.id, occurrence.event_id, occurrence.starts_at, occurrence.ends_at, occurrence.status, occurrence.notes, occurrence.created_at],
    )?;
    Ok(())
}

/// Get occurrences for an event.
pub fn get_event_occurrences(conn: &rusqlite::Connection, event_id: &str) -> DbResult<Vec<EventOccurrence>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_id, starts_at, ends_at, status, notes, created_at
         FROM event_occurrences WHERE event_id = ?1 ORDER BY starts_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![event_id], EventOccurrence::from_row)?;
    let mut occurrences = Vec::new();
    for row in rows { occurrences.push(row?); }
    Ok(occurrences)
}

/// Update occurrence status.
pub fn update_occurrence_status(conn: &rusqlite::Connection, id: &str, status: OccurrenceStatus) -> DbResult<bool> {
    let count = conn.execute(
        "UPDATE event_occurrences SET status = ?1 WHERE id = ?2",
        rusqlite::params![status, id],
    )?;
    Ok(count > 0)
}
