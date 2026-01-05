//! Event and reminder models.
//!
//! Calendar events with optional recurrence and reminder support.
//! Used for time-based triggers and ADHD-friendly scheduling.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// A calendar event or reminder.
///
/// Events can be one-time or recurring, and can trigger agent actions
/// via the Timer data source.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Event {
    /// Unique identifier
    pub id: String,

    /// Agent associated with this event (None = constellation-level)
    pub agent_id: Option<String>,

    /// Event title
    pub title: String,

    /// Event description
    pub description: Option<String>,

    /// When the event starts
    pub starts_at: DateTime<Utc>,

    /// When the event ends (None = point-in-time event)
    pub ends_at: Option<DateTime<Utc>>,

    /// Recurrence rule in iCal RRULE format
    /// Examples:
    /// - "FREQ=DAILY" (every day)
    /// - "FREQ=WEEKLY;BYDAY=MO,WE,FR" (Mon/Wed/Fri)
    /// - "FREQ=MONTHLY;BYMONTHDAY=1" (1st of each month)
    pub rrule: Option<String>,

    /// Minutes before event to trigger reminder (None = no reminder)
    pub reminder_minutes: Option<i64>,

    /// Whether the event is all-day (vs specific time)
    pub all_day: bool,

    /// Event location (physical or virtual)
    pub location: Option<String>,

    /// External calendar source ID (for sync)
    pub external_id: Option<String>,

    /// External calendar source type (google, ical, etc.)
    pub external_source: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
}

/// Event occurrence for recurring events.
///
/// When a recurring event fires, we may want to track individual occurrences
/// (e.g., for marking attendance, snoozing, or noting outcomes).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct EventOccurrence {
    /// Unique identifier
    pub id: String,

    /// Parent event
    pub event_id: String,

    /// When this occurrence starts
    pub starts_at: DateTime<Utc>,

    /// When this occurrence ends
    pub ends_at: Option<DateTime<Utc>>,

    /// Status of this occurrence
    pub status: OccurrenceStatus,

    /// Notes for this specific occurrence
    pub notes: Option<String>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// Status of an event occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceStatus {
    /// Upcoming, not yet happened
    Scheduled,
    /// Currently happening
    Active,
    /// Completed as planned
    Completed,
    /// Skipped this occurrence
    Skipped,
    /// Reminder was snoozed
    Snoozed,
    /// Cancelled this occurrence (but not the series)
    Cancelled,
}

impl Default for OccurrenceStatus {
    fn default() -> Self {
        Self::Scheduled
    }
}
