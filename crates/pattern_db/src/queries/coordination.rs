//! Coordination-related database queries.

use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{
    ActivityEvent, ActivityEventType, AgentSummary, ConstellationSummary, CoordinationState,
    CoordinationTask, EventImportance, HandoffNote, NotableEvent, TaskPriority, TaskStatus,
};

// ============================================================================
// Activity Events
// ============================================================================

/// Get recent activity events.
pub async fn get_recent_activity(pool: &SqlitePool, limit: i64) -> DbResult<Vec<ActivityEvent>> {
    let events = sqlx::query_as!(
        ActivityEvent,
        r#"
        SELECT
            id as "id!",
            timestamp as "timestamp!: _",
            agent_id,
            event_type as "event_type!: ActivityEventType",
            details as "details!: _",
            importance as "importance: EventImportance"
        FROM activity_events
        ORDER BY timestamp DESC
        LIMIT ?
        "#,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Get recent activity events since a given timestamp.
pub async fn get_recent_activity_since(
    pool: &SqlitePool,
    since: chrono::DateTime<chrono::Utc>,
    limit: i64,
) -> DbResult<Vec<ActivityEvent>> {
    let events = sqlx::query_as!(
        ActivityEvent,
        r#"
        SELECT
            id as "id!",
            timestamp as "timestamp!: _",
            agent_id,
            event_type as "event_type!: ActivityEventType",
            details as "details!: _",
            importance as "importance: EventImportance"
        FROM activity_events
        WHERE timestamp >= ?
        ORDER BY timestamp DESC
        LIMIT ?
        "#,
        since,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Get recent activity events with minimum importance.
pub async fn get_recent_activity_by_importance(
    pool: &SqlitePool,
    limit: i64,
    min_importance: EventImportance,
) -> DbResult<Vec<ActivityEvent>> {
    let events = sqlx::query_as!(
        ActivityEvent,
        r#"
        SELECT
            id as "id!",
            timestamp as "timestamp!: _",
            agent_id,
            event_type as "event_type!: ActivityEventType",
            details as "details!: _",
            importance as "importance: EventImportance"
        FROM activity_events
        WHERE importance >= ?
        ORDER BY timestamp DESC
        LIMIT ?
        "#,
        min_importance,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Get activity events for a specific agent.
pub async fn get_agent_activity(
    pool: &SqlitePool,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<ActivityEvent>> {
    let events = sqlx::query_as!(
        ActivityEvent,
        r#"
        SELECT
            id as "id!",
            timestamp as "timestamp!: _",
            agent_id,
            event_type as "event_type!: ActivityEventType",
            details as "details!: _",
            importance as "importance: EventImportance"
        FROM activity_events
        WHERE agent_id = ?
        ORDER BY timestamp DESC
        LIMIT ?
        "#,
        agent_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Create an activity event.
pub async fn create_activity_event(pool: &SqlitePool, event: &ActivityEvent) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO activity_events (id, timestamp, agent_id, event_type, details, importance)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
        event.id,
        event.timestamp,
        event.agent_id,
        event.event_type,
        event.details,
        event.importance,
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ============================================================================
// Agent Summaries
// ============================================================================

/// Get an agent's summary.
pub async fn get_agent_summary(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Option<AgentSummary>> {
    let summary = sqlx::query_as!(
        AgentSummary,
        r#"
        SELECT
            agent_id as "agent_id!",
            summary as "summary!",
            messages_covered as "messages_covered!",
            generated_at as "generated_at!: _",
            last_active as "last_active!: _"
        FROM agent_summaries
        WHERE agent_id = ?
        "#,
        agent_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(summary)
}

/// Upsert an agent summary.
pub async fn upsert_agent_summary(pool: &SqlitePool, summary: &AgentSummary) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO agent_summaries (agent_id, summary, messages_covered, generated_at, last_active)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(agent_id) DO UPDATE SET
            summary = excluded.summary,
            messages_covered = excluded.messages_covered,
            generated_at = excluded.generated_at,
            last_active = excluded.last_active
        "#,
        summary.agent_id,
        summary.summary,
        summary.messages_covered,
        summary.generated_at,
        summary.last_active,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get all agent summaries.
pub async fn get_all_agent_summaries(pool: &SqlitePool) -> DbResult<Vec<AgentSummary>> {
    let summaries = sqlx::query_as!(
        AgentSummary,
        r#"
        SELECT
            agent_id as "agent_id!",
            summary as "summary!",
            messages_covered as "messages_covered!",
            generated_at as "generated_at!: _",
            last_active as "last_active!: _"
        FROM agent_summaries
        ORDER BY last_active DESC
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(summaries)
}

// ============================================================================
// Constellation Summaries
// ============================================================================

/// Get the latest constellation summary.
pub async fn get_latest_constellation_summary(
    pool: &SqlitePool,
) -> DbResult<Option<ConstellationSummary>> {
    let summary = sqlx::query_as!(
        ConstellationSummary,
        r#"
        SELECT
            id as "id!",
            period_start as "period_start!: _",
            period_end as "period_end!: _",
            summary as "summary!",
            key_decisions as "key_decisions: _",
            open_threads as "open_threads: _",
            created_at as "created_at!: _"
        FROM constellation_summaries
        ORDER BY period_end DESC
        LIMIT 1
        "#
    )
    .fetch_optional(pool)
    .await?;
    Ok(summary)
}

/// Create a constellation summary.
pub async fn create_constellation_summary(
    pool: &SqlitePool,
    summary: &ConstellationSummary,
) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO constellation_summaries (id, period_start, period_end, summary, key_decisions, open_threads, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        summary.id,
        summary.period_start,
        summary.period_end,
        summary.summary,
        summary.key_decisions,
        summary.open_threads,
        summary.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ============================================================================
// Notable Events
// ============================================================================

/// Get recent notable events.
pub async fn get_notable_events(pool: &SqlitePool, limit: i64) -> DbResult<Vec<NotableEvent>> {
    let events = sqlx::query_as!(
        NotableEvent,
        r#"
        SELECT
            id as "id!",
            timestamp as "timestamp!: _",
            event_type as "event_type!",
            description as "description!",
            agents_involved as "agents_involved: _",
            importance as "importance!: EventImportance",
            created_at as "created_at!: _"
        FROM notable_events
        ORDER BY timestamp DESC
        LIMIT ?
        "#,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(events)
}

/// Create a notable event.
pub async fn create_notable_event(pool: &SqlitePool, event: &NotableEvent) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO notable_events (id, timestamp, event_type, description, agents_involved, importance, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        event.id,
        event.timestamp,
        event.event_type,
        event.description,
        event.agents_involved,
        event.importance,
        event.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ============================================================================
// Coordination Tasks
// ============================================================================

/// Get a coordination task by ID.
pub async fn get_task(pool: &SqlitePool, id: &str) -> DbResult<Option<CoordinationTask>> {
    let task = sqlx::query_as!(
        CoordinationTask,
        r#"
        SELECT
            id as "id!",
            description as "description!",
            assigned_to,
            status as "status!: TaskStatus",
            priority as "priority!: TaskPriority",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM coordination_tasks
        WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(task)
}

/// Get tasks by status.
pub async fn get_tasks_by_status(
    pool: &SqlitePool,
    status: TaskStatus,
) -> DbResult<Vec<CoordinationTask>> {
    let tasks = sqlx::query_as!(
        CoordinationTask,
        r#"
        SELECT
            id as "id!",
            description as "description!",
            assigned_to,
            status as "status!: TaskStatus",
            priority as "priority!: TaskPriority",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM coordination_tasks
        WHERE status = ?
        ORDER BY priority DESC, created_at
        "#,
        status
    )
    .fetch_all(pool)
    .await?;
    Ok(tasks)
}

/// Get tasks assigned to an agent.
pub async fn get_tasks_for_agent(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<CoordinationTask>> {
    let tasks = sqlx::query_as!(
        CoordinationTask,
        r#"
        SELECT
            id as "id!",
            description as "description!",
            assigned_to,
            status as "status!: TaskStatus",
            priority as "priority!: TaskPriority",
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM coordination_tasks
        WHERE assigned_to = ?
        ORDER BY priority DESC, created_at
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(tasks)
}

/// Create a coordination task.
pub async fn create_task(pool: &SqlitePool, task: &CoordinationTask) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO coordination_tasks (id, description, assigned_to, status, priority, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
        task.id,
        task.description,
        task.assigned_to,
        task.status,
        task.priority,
        task.created_at,
        task.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update task status.
pub async fn update_task_status(pool: &SqlitePool, id: &str, status: TaskStatus) -> DbResult<()> {
    sqlx::query!(
        "UPDATE coordination_tasks SET status = ?, updated_at = datetime('now') WHERE id = ?",
        status,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Assign a task to an agent.
pub async fn assign_task(pool: &SqlitePool, id: &str, agent_id: Option<&str>) -> DbResult<()> {
    sqlx::query!(
        "UPDATE coordination_tasks SET assigned_to = ?, updated_at = datetime('now') WHERE id = ?",
        agent_id,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ============================================================================
// Handoff Notes
// ============================================================================

/// Get unread handoff notes for an agent.
pub async fn get_unread_handoffs(pool: &SqlitePool, agent_id: &str) -> DbResult<Vec<HandoffNote>> {
    let notes = sqlx::query_as!(
        HandoffNote,
        r#"
        SELECT
            id as "id!",
            from_agent as "from_agent!",
            to_agent,
            content as "content!",
            created_at as "created_at!: _",
            read_at as "read_at: _"
        FROM handoff_notes
        WHERE (to_agent = ? OR to_agent IS NULL) AND read_at IS NULL
        ORDER BY created_at
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(notes)
}

/// Create a handoff note.
pub async fn create_handoff(pool: &SqlitePool, note: &HandoffNote) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO handoff_notes (id, from_agent, to_agent, content, created_at, read_at)
        VALUES (?, ?, ?, ?, ?, ?)
        "#,
        note.id,
        note.from_agent,
        note.to_agent,
        note.content,
        note.created_at,
        note.read_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a handoff note as read.
pub async fn mark_handoff_read(pool: &SqlitePool, id: &str) -> DbResult<()> {
    sqlx::query!(
        "UPDATE handoff_notes SET read_at = datetime('now') WHERE id = ?",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ============================================================================
// Coordination State (Key-Value)
// ============================================================================

/// Get a coordination state value.
pub async fn get_state(pool: &SqlitePool, key: &str) -> DbResult<Option<CoordinationState>> {
    let state = sqlx::query_as!(
        CoordinationState,
        r#"
        SELECT
            key as "key!",
            value as "value!: _",
            updated_at as "updated_at!: _",
            updated_by
        FROM coordination_state
        WHERE key = ?
        "#,
        key
    )
    .fetch_optional(pool)
    .await?;
    Ok(state)
}

/// Set a coordination state value.
pub async fn set_state(pool: &SqlitePool, state: &CoordinationState) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO coordination_state (key, value, updated_at, updated_by)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at,
            updated_by = excluded.updated_by
        "#,
        state.key,
        state.value,
        state.updated_at,
        state.updated_by,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a coordination state value.
pub async fn delete_state(pool: &SqlitePool, key: &str) -> DbResult<()> {
    sqlx::query!("DELETE FROM coordination_state WHERE key = ?", key)
        .execute(pool)
        .await?;
    Ok(())
}
