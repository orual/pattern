//! Event and reminder queries.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{Event, EventOccurrence, OccurrenceStatus};

// ============================================================================
// Event CRUD
// ============================================================================

/// Create a new event.
pub async fn create_event(pool: &SqlitePool, event: &Event) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO events (id, agent_id, title, description, starts_at, ends_at, rrule, reminder_minutes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        event.id,
        event.agent_id,
        event.title,
        event.description,
        event.starts_at,
        event.ends_at,
        event.rrule,
        event.reminder_minutes,
        event.created_at,
        event.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get an event by ID.
pub async fn get_event(pool: &SqlitePool, id: &str) -> DbResult<Option<Event>> {
    let event = sqlx::query_as!(
        Event,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            starts_at as "starts_at!: _",
            ends_at as "ends_at: _",
            rrule,
            reminder_minutes,
            all_day as "all_day!: bool",
            location,
            external_id,
            external_source,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM events WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(event)
}

/// List events for an agent (or constellation-level).
pub async fn list_events(pool: &SqlitePool, agent_id: Option<&str>) -> DbResult<Vec<Event>> {
    let events = match agent_id {
        Some(aid) => {
            sqlx::query_as!(
                Event,
                r#"
                SELECT
                    id as "id!",
                    agent_id,
                    title as "title!",
                    description,
                    starts_at as "starts_at!: _",
                    ends_at as "ends_at: _",
                    rrule,
                    reminder_minutes,
                    all_day as "all_day!: bool",
                    location,
                    external_id,
                    external_source,
                    created_at as "created_at!: _",
                    updated_at as "updated_at!: _"
                FROM events WHERE agent_id = ? ORDER BY starts_at ASC
                "#,
                aid
            )
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as!(
                Event,
                r#"
                SELECT
                    id as "id!",
                    agent_id,
                    title as "title!",
                    description,
                    starts_at as "starts_at!: _",
                    ends_at as "ends_at: _",
                    rrule,
                    reminder_minutes,
                    all_day as "all_day!: bool",
                    location,
                    external_id,
                    external_source,
                    created_at as "created_at!: _",
                    updated_at as "updated_at!: _"
                FROM events WHERE agent_id IS NULL ORDER BY starts_at ASC
                "#
            )
            .fetch_all(pool)
            .await?
        }
    };
    Ok(events)
}

/// Get events in a time range.
pub async fn get_events_in_range(
    pool: &SqlitePool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> DbResult<Vec<Event>> {
    let events = sqlx::query_as!(
        Event,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            starts_at as "starts_at!: _",
            ends_at as "ends_at: _",
            rrule,
            reminder_minutes,
            all_day as "all_day!: bool",
            location,
            external_id,
            external_source,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM events
        WHERE starts_at >= ? AND starts_at <= ?
        ORDER BY starts_at ASC
        "#,
        start,
        end
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Get upcoming events (starting within N hours).
pub async fn get_upcoming_events(pool: &SqlitePool, hours: i64) -> DbResult<Vec<Event>> {
    let now = Utc::now();
    let deadline = now + chrono::Duration::hours(hours);
    let events = sqlx::query_as!(
        Event,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            starts_at as "starts_at!: _",
            ends_at as "ends_at: _",
            rrule,
            reminder_minutes,
            all_day as "all_day!: bool",
            location,
            external_id,
            external_source,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM events
        WHERE starts_at >= ? AND starts_at <= ?
        ORDER BY starts_at ASC
        "#,
        now,
        deadline
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Get events needing reminders (reminder time is now or past, but event hasn't started).
pub async fn get_events_needing_reminders(pool: &SqlitePool) -> DbResult<Vec<Event>> {
    let now = Utc::now();
    // This query finds events where: starts_at - reminder_minutes <= now < starts_at
    let events = sqlx::query_as!(
        Event,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            starts_at as "starts_at!: _",
            ends_at as "ends_at: _",
            rrule,
            reminder_minutes,
            all_day as "all_day!: bool",
            location,
            external_id,
            external_source,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM events
        WHERE reminder_minutes IS NOT NULL
          AND starts_at > ?
          AND datetime(starts_at, '-' || reminder_minutes || ' minutes') <= ?
        ORDER BY starts_at ASC
        "#,
        now,
        now
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Update an event.
pub async fn update_event(pool: &SqlitePool, event: &Event) -> DbResult<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE events
        SET title = ?, description = ?, starts_at = ?, ends_at = ?,
            rrule = ?, reminder_minutes = ?, updated_at = ?
        WHERE id = ?
        "#,
        event.title,
        event.description,
        event.starts_at,
        event.ends_at,
        event.rrule,
        event.reminder_minutes,
        event.updated_at,
        event.id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete an event.
pub async fn delete_event(pool: &SqlitePool, id: &str) -> DbResult<bool> {
    let result = sqlx::query!("DELETE FROM events WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

// ============================================================================
// EventOccurrence (for recurring events)
// ============================================================================

/// Create an event occurrence.
pub async fn create_occurrence(pool: &SqlitePool, occurrence: &EventOccurrence) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO event_occurrences (id, event_id, starts_at, ends_at, status, notes, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        occurrence.id,
        occurrence.event_id,
        occurrence.starts_at,
        occurrence.ends_at,
        occurrence.status,
        occurrence.notes,
        occurrence.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get occurrences for an event.
pub async fn get_event_occurrences(
    pool: &SqlitePool,
    event_id: &str,
) -> DbResult<Vec<EventOccurrence>> {
    let occurrences = sqlx::query_as!(
        EventOccurrence,
        r#"
        SELECT
            id as "id!",
            event_id as "event_id!",
            starts_at as "starts_at!: _",
            ends_at as "ends_at: _",
            status as "status!: OccurrenceStatus",
            notes,
            created_at as "created_at!: _"
        FROM event_occurrences WHERE event_id = ? ORDER BY starts_at ASC
        "#,
        event_id
    )
    .fetch_all(pool)
    .await?;
    Ok(occurrences)
}

/// Update occurrence status.
pub async fn update_occurrence_status(
    pool: &SqlitePool,
    id: &str,
    status: OccurrenceStatus,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE event_occurrences SET status = ? WHERE id = ?",
        status,
        id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
