//! Coordination-related database queries.

use rusqlite::OptionalExtension;

use crate::error::DbResult;
use crate::models::{
    ActivityEvent, AgentSummary, ConstellationSummary, CoordinationState, CoordinationTask,
    EventImportance, HandoffNote, NotableEvent, TaskStatus,
};

// ============================================================================
// from_row implementations
// ============================================================================

impl ActivityEvent {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            timestamp: row.get("timestamp")?,
            agent_id: row.get("agent_id")?,
            event_type: row.get("event_type")?,
            details: row.get("details")?,
            importance: row.get("importance")?,
        })
    }
}

impl AgentSummary {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            agent_id: row.get("agent_id")?,
            summary: row.get("summary")?,
            messages_covered: row.get("messages_covered")?,
            generated_at: row.get("generated_at")?,
            last_active: row.get("last_active")?,
        })
    }
}

impl ConstellationSummary {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            period_start: row.get("period_start")?,
            period_end: row.get("period_end")?,
            summary: row.get("summary")?,
            key_decisions: row.get("key_decisions")?,
            open_threads: row.get("open_threads")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl NotableEvent {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            timestamp: row.get("timestamp")?,
            event_type: row.get("event_type")?,
            description: row.get("description")?,
            agents_involved: row.get("agents_involved")?,
            importance: row.get("importance")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl CoordinationTask {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            description: row.get("description")?,
            assigned_to: row.get("assigned_to")?,
            status: row.get("status")?,
            priority: row.get("priority")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl HandoffNote {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            from_agent: row.get("from_agent")?,
            to_agent: row.get("to_agent")?,
            content: row.get("content")?,
            created_at: row.get("created_at")?,
            read_at: row.get("read_at")?,
        })
    }
}

impl CoordinationState {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            key: row.get("key")?,
            value: row.get("value")?,
            updated_at: row.get("updated_at")?,
            updated_by: row.get("updated_by")?,
        })
    }
}

// ============================================================================
// Activity Events
// ============================================================================

/// Get recent activity events.
pub fn get_recent_activity(
    conn: &rusqlite::Connection,
    limit: i64,
) -> DbResult<Vec<ActivityEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, agent_id, event_type, details, importance
         FROM activity_events ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], ActivityEvent::from_row)?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get recent activity events since a given timestamp.
pub fn get_recent_activity_since(
    conn: &rusqlite::Connection,
    since: chrono::DateTime<chrono::Utc>,
    limit: i64,
) -> DbResult<Vec<ActivityEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, agent_id, event_type, details, importance
         FROM activity_events WHERE timestamp >= ?1
         ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![since, limit], ActivityEvent::from_row)?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get recent activity events with minimum importance.
pub fn get_recent_activity_by_importance(
    conn: &rusqlite::Connection,
    limit: i64,
    min_importance: EventImportance,
) -> DbResult<Vec<ActivityEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, agent_id, event_type, details, importance
         FROM activity_events WHERE importance >= ?1
         ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![min_importance, limit],
        ActivityEvent::from_row,
    )?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Get activity events for a specific agent.
pub fn get_agent_activity(
    conn: &rusqlite::Connection,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<ActivityEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, agent_id, event_type, details, importance
         FROM activity_events WHERE agent_id = ?1
         ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id, limit], ActivityEvent::from_row)?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Create an activity event.
pub fn create_activity_event(conn: &rusqlite::Connection, event: &ActivityEvent) -> DbResult<()> {
    conn.execute(
        "INSERT INTO activity_events (id, timestamp, agent_id, event_type, details, importance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            event.id,
            event.timestamp,
            event.agent_id,
            event.event_type,
            event.details,
            event.importance,
        ],
    )?;
    Ok(())
}

// ============================================================================
// Agent Summaries
// ============================================================================

/// Get an agent's summary.
pub fn get_agent_summary(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Option<AgentSummary>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, summary, messages_covered, generated_at, last_active
         FROM agent_summaries WHERE agent_id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![agent_id], AgentSummary::from_row)
        .optional()?;
    Ok(result)
}

/// Upsert an agent summary.
pub fn upsert_agent_summary(conn: &rusqlite::Connection, summary: &AgentSummary) -> DbResult<()> {
    conn.execute(
        "INSERT INTO agent_summaries (agent_id, summary, messages_covered, generated_at, last_active)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(agent_id) DO UPDATE SET
             summary = excluded.summary,
             messages_covered = excluded.messages_covered,
             generated_at = excluded.generated_at,
             last_active = excluded.last_active",
        rusqlite::params![
            summary.agent_id,
            summary.summary,
            summary.messages_covered,
            summary.generated_at,
            summary.last_active,
        ],
    )?;
    Ok(())
}

/// Get all agent summaries.
pub fn get_all_agent_summaries(conn: &rusqlite::Connection) -> DbResult<Vec<AgentSummary>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, summary, messages_covered, generated_at, last_active
         FROM agent_summaries ORDER BY last_active DESC",
    )?;
    let rows = stmt.query_map([], AgentSummary::from_row)?;
    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row?);
    }
    Ok(summaries)
}

// ============================================================================
// Constellation Summaries
// ============================================================================

/// Get the latest constellation summary.
pub fn get_latest_constellation_summary(
    conn: &rusqlite::Connection,
) -> DbResult<Option<ConstellationSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, period_start, period_end, summary, key_decisions, open_threads, created_at
         FROM constellation_summaries ORDER BY period_end DESC LIMIT 1",
    )?;
    let result = stmt
        .query_row([], ConstellationSummary::from_row)
        .optional()?;
    Ok(result)
}

/// Create a constellation summary.
pub fn create_constellation_summary(
    conn: &rusqlite::Connection,
    summary: &ConstellationSummary,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO constellation_summaries (id, period_start, period_end, summary, key_decisions, open_threads, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            summary.id,
            summary.period_start,
            summary.period_end,
            summary.summary,
            summary.key_decisions,
            summary.open_threads,
            summary.created_at,
        ],
    )?;
    Ok(())
}

// ============================================================================
// Notable Events
// ============================================================================

/// Get recent notable events.
pub fn get_notable_events(conn: &rusqlite::Connection, limit: i64) -> DbResult<Vec<NotableEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, event_type, description, agents_involved, importance, created_at
         FROM notable_events ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], NotableEvent::from_row)?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Create a notable event.
pub fn create_notable_event(conn: &rusqlite::Connection, event: &NotableEvent) -> DbResult<()> {
    conn.execute(
        "INSERT INTO notable_events (id, timestamp, event_type, description, agents_involved, importance, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            event.id,
            event.timestamp,
            event.event_type,
            event.description,
            event.agents_involved,
            event.importance,
            event.created_at,
        ],
    )?;
    Ok(())
}

// ============================================================================
// Coordination Tasks
// ============================================================================

/// Get a coordination task by ID.
pub fn get_task(conn: &rusqlite::Connection, id: &str) -> DbResult<Option<CoordinationTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, description, assigned_to, status, priority, created_at, updated_at
         FROM coordination_tasks WHERE id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![id], CoordinationTask::from_row)
        .optional()?;
    Ok(result)
}

/// Get tasks by status.
pub fn get_tasks_by_status(
    conn: &rusqlite::Connection,
    status: TaskStatus,
) -> DbResult<Vec<CoordinationTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, description, assigned_to, status, priority, created_at, updated_at
         FROM coordination_tasks WHERE status = ?1
         ORDER BY priority DESC, created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![status], CoordinationTask::from_row)?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    Ok(tasks)
}

/// Get tasks assigned to an agent.
pub fn get_tasks_for_agent(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<CoordinationTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, description, assigned_to, status, priority, created_at, updated_at
         FROM coordination_tasks WHERE assigned_to = ?1
         ORDER BY priority DESC, created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], CoordinationTask::from_row)?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    Ok(tasks)
}

/// Create a coordination task.
pub fn create_task(conn: &rusqlite::Connection, task: &CoordinationTask) -> DbResult<()> {
    conn.execute(
        "INSERT INTO coordination_tasks (id, description, assigned_to, status, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            task.id,
            task.description,
            task.assigned_to,
            task.status,
            task.priority,
            task.created_at,
            task.updated_at,
        ],
    )?;
    Ok(())
}

/// Update task status.
pub fn update_task_status(
    conn: &rusqlite::Connection,
    id: &str,
    status: TaskStatus,
) -> DbResult<()> {
    conn.execute(
        "UPDATE coordination_tasks SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![status, id],
    )?;
    Ok(())
}

/// Assign a task to an agent.
pub fn assign_task(conn: &rusqlite::Connection, id: &str, agent_id: Option<&str>) -> DbResult<()> {
    conn.execute(
        "UPDATE coordination_tasks SET assigned_to = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![agent_id, id],
    )?;
    Ok(())
}

// ============================================================================
// Handoff Notes
// ============================================================================

/// Get unread handoff notes for an agent.
pub fn get_unread_handoffs(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<HandoffNote>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_agent, to_agent, content, created_at, read_at
         FROM handoff_notes
         WHERE (to_agent = ?1 OR to_agent IS NULL) AND read_at IS NULL
         ORDER BY created_at",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], HandoffNote::from_row)?;
    let mut notes = Vec::new();
    for row in rows {
        notes.push(row?);
    }
    Ok(notes)
}

/// Create a handoff note.
pub fn create_handoff(conn: &rusqlite::Connection, note: &HandoffNote) -> DbResult<()> {
    conn.execute(
        "INSERT INTO handoff_notes (id, from_agent, to_agent, content, created_at, read_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            note.id,
            note.from_agent,
            note.to_agent,
            note.content,
            note.created_at,
            note.read_at,
        ],
    )?;
    Ok(())
}

/// Mark a handoff note as read.
pub fn mark_handoff_read(conn: &rusqlite::Connection, id: &str) -> DbResult<()> {
    conn.execute(
        "UPDATE handoff_notes SET read_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// ============================================================================
// Coordination State (Key-Value)
// ============================================================================

/// Get a coordination state value.
pub fn get_state(conn: &rusqlite::Connection, key: &str) -> DbResult<Option<CoordinationState>> {
    let mut stmt = conn.prepare(
        "SELECT key, value, updated_at, updated_by
         FROM coordination_state WHERE key = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![key], CoordinationState::from_row)
        .optional()?;
    Ok(result)
}

/// Set a coordination state value.
pub fn set_state(conn: &rusqlite::Connection, state: &CoordinationState) -> DbResult<()> {
    conn.execute(
        "INSERT INTO coordination_state (key, value, updated_at, updated_by)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET
             value = excluded.value,
             updated_at = excluded.updated_at,
             updated_by = excluded.updated_by",
        rusqlite::params![state.key, state.value, state.updated_at, state.updated_by],
    )?;
    Ok(())
}

/// Delete a coordination state value.
pub fn delete_state(conn: &rusqlite::Connection, key: &str) -> DbResult<()> {
    conn.execute(
        "DELETE FROM coordination_state WHERE key = ?1",
        rusqlite::params![key],
    )?;
    Ok(())
}
