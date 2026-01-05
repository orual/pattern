//! ADHD task queries.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::DbResult;
use crate::models::{Task, TaskSummary, UserTaskPriority, UserTaskStatus};

// ============================================================================
// Task CRUD
// ============================================================================

/// Create a new user task.
pub async fn create_user_task(pool: &SqlitePool, task: &Task) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO tasks (id, agent_id, title, description, status, priority, due_at, scheduled_at, completed_at, parent_task_id, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        task.id,
        task.agent_id,
        task.title,
        task.description,
        task.status,
        task.priority,
        task.due_at,
        task.scheduled_at,
        task.completed_at,
        task.parent_task_id,
        task.created_at,
        task.updated_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Get a user task by ID.
pub async fn get_user_task(pool: &SqlitePool, id: &str) -> DbResult<Option<Task>> {
    let task = sqlx::query_as!(
        Task,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            status as "status!: UserTaskStatus",
            priority as "priority!: UserTaskPriority",
            due_at as "due_at: _",
            scheduled_at as "scheduled_at: _",
            completed_at as "completed_at: _",
            parent_task_id,
            tags as "tags: _",
            estimated_minutes,
            actual_minutes,
            notes,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM tasks WHERE id = ?
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(task)
}

/// List tasks for an agent (or constellation-level if agent_id is None).
pub async fn list_tasks(
    pool: &SqlitePool,
    agent_id: Option<&str>,
    include_completed: bool,
) -> DbResult<Vec<Task>> {
    let tasks = if include_completed {
        match agent_id {
            Some(aid) => {
                sqlx::query_as!(
                    Task,
                    r#"
                    SELECT
                        id as "id!",
                        agent_id,
                        title as "title!",
                        description,
                        status as "status!: UserTaskStatus",
                        priority as "priority!: UserTaskPriority",
                        due_at as "due_at: _",
                        scheduled_at as "scheduled_at: _",
                        completed_at as "completed_at: _",
                        parent_task_id,
                        tags as "tags: _",
                        estimated_minutes,
                        actual_minutes,
                        notes,
                        created_at as "created_at!: _",
                        updated_at as "updated_at!: _"
                    FROM tasks WHERE agent_id = ? ORDER BY priority DESC, due_at ASC NULLS LAST
                    "#,
                    aid
                )
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query_as!(
                    Task,
                    r#"
                    SELECT
                        id as "id!",
                        agent_id,
                        title as "title!",
                        description,
                        status as "status!: UserTaskStatus",
                        priority as "priority!: UserTaskPriority",
                        due_at as "due_at: _",
                        scheduled_at as "scheduled_at: _",
                        completed_at as "completed_at: _",
                        parent_task_id,
                        tags as "tags: _",
                        estimated_minutes,
                        actual_minutes,
                        notes,
                        created_at as "created_at!: _",
                        updated_at as "updated_at!: _"
                    FROM tasks WHERE agent_id IS NULL ORDER BY priority DESC, due_at ASC NULLS LAST
                    "#
                )
                .fetch_all(pool)
                .await?
            }
        }
    } else {
        match agent_id {
            Some(aid) => {
                sqlx::query_as!(
                    Task,
                    r#"
                    SELECT
                        id as "id!",
                        agent_id,
                        title as "title!",
                        description,
                        status as "status!: UserTaskStatus",
                        priority as "priority!: UserTaskPriority",
                        due_at as "due_at: _",
                        scheduled_at as "scheduled_at: _",
                        completed_at as "completed_at: _",
                        parent_task_id,
                        tags as "tags: _",
                        estimated_minutes,
                        actual_minutes,
                        notes,
                        created_at as "created_at!: _",
                        updated_at as "updated_at!: _"
                    FROM tasks WHERE agent_id = ? AND status NOT IN ('completed', 'cancelled')
                    ORDER BY priority DESC, due_at ASC NULLS LAST
                    "#,
                    aid
                )
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query_as!(
                    Task,
                    r#"
                    SELECT
                        id as "id!",
                        agent_id,
                        title as "title!",
                        description,
                        status as "status!: UserTaskStatus",
                        priority as "priority!: UserTaskPriority",
                        due_at as "due_at: _",
                        scheduled_at as "scheduled_at: _",
                        completed_at as "completed_at: _",
                        parent_task_id,
                        tags as "tags: _",
                        estimated_minutes,
                        actual_minutes,
                        notes,
                        created_at as "created_at!: _",
                        updated_at as "updated_at!: _"
                    FROM tasks WHERE agent_id IS NULL AND status NOT IN ('completed', 'cancelled')
                    ORDER BY priority DESC, due_at ASC NULLS LAST
                    "#
                )
                .fetch_all(pool)
                .await?
            }
        }
    };
    Ok(tasks)
}

/// Get subtasks of a parent task.
pub async fn get_subtasks(pool: &SqlitePool, parent_id: &str) -> DbResult<Vec<Task>> {
    let tasks = sqlx::query_as!(
        Task,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            status as "status!: UserTaskStatus",
            priority as "priority!: UserTaskPriority",
            due_at as "due_at: _",
            scheduled_at as "scheduled_at: _",
            completed_at as "completed_at: _",
            parent_task_id,
            tags as "tags: _",
            estimated_minutes,
            actual_minutes,
            notes,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM tasks WHERE parent_task_id = ? ORDER BY priority DESC, created_at ASC
        "#,
        parent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(tasks)
}

/// Get tasks due soon (within the next N hours).
pub async fn get_tasks_due_soon(pool: &SqlitePool, hours: i64) -> DbResult<Vec<Task>> {
    let now = Utc::now();
    let deadline = now + chrono::Duration::hours(hours);
    let tasks = sqlx::query_as!(
        Task,
        r#"
        SELECT
            id as "id!",
            agent_id,
            title as "title!",
            description,
            status as "status!: UserTaskStatus",
            priority as "priority!: UserTaskPriority",
            due_at as "due_at: _",
            scheduled_at as "scheduled_at: _",
            completed_at as "completed_at: _",
            parent_task_id,
            tags as "tags: _",
            estimated_minutes,
            actual_minutes,
            notes,
            created_at as "created_at!: _",
            updated_at as "updated_at!: _"
        FROM tasks
        WHERE due_at IS NOT NULL
          AND due_at <= ?
          AND status NOT IN ('completed', 'cancelled')
        ORDER BY due_at ASC
        "#,
        deadline
    )
    .fetch_all(pool)
    .await?;
    Ok(tasks)
}

/// Update user task status.
pub async fn update_user_task_status(
    pool: &SqlitePool,
    id: &str,
    status: UserTaskStatus,
) -> DbResult<bool> {
    let now = Utc::now();
    let completed_at = if status == UserTaskStatus::Completed {
        Some(now)
    } else {
        None
    };

    let result = sqlx::query!(
        r#"
        UPDATE tasks SET status = ?, completed_at = COALESCE(?, completed_at), updated_at = ? WHERE id = ?
        "#,
        status,
        completed_at,
        now,
        id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update user task priority.
pub async fn update_user_task_priority(
    pool: &SqlitePool,
    id: &str,
    priority: UserTaskPriority,
) -> DbResult<bool> {
    let now = Utc::now();
    let result = sqlx::query!(
        "UPDATE tasks SET priority = ?, updated_at = ? WHERE id = ?",
        priority,
        now,
        id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update a user task.
pub async fn update_user_task(pool: &SqlitePool, task: &Task) -> DbResult<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE tasks
        SET title = ?, description = ?, status = ?, priority = ?,
            due_at = ?, scheduled_at = ?, completed_at = ?,
            parent_task_id = ?, updated_at = ?
        WHERE id = ?
        "#,
        task.title,
        task.description,
        task.status,
        task.priority,
        task.due_at,
        task.scheduled_at,
        task.completed_at,
        task.parent_task_id,
        task.updated_at,
        task.id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete a user task (and its subtasks via CASCADE).
pub async fn delete_user_task(pool: &SqlitePool, id: &str) -> DbResult<bool> {
    let result = sqlx::query!("DELETE FROM tasks WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Get task summaries for quick listing.
pub async fn get_task_summaries(
    pool: &SqlitePool,
    agent_id: Option<&str>,
) -> DbResult<Vec<TaskSummary>> {
    let summaries = match agent_id {
        Some(aid) => {
            sqlx::query_as!(
                TaskSummary,
                r#"
                SELECT
                    t.id as "id!",
                    t.title as "title!",
                    t.status as "status!: UserTaskStatus",
                    t.priority as "priority!: UserTaskPriority",
                    t.due_at as "due_at: _",
                    t.parent_task_id,
                    (SELECT COUNT(*) FROM tasks WHERE parent_task_id = t.id) as "subtask_count: i64"
                FROM tasks t
                WHERE t.agent_id = ? AND t.status NOT IN ('completed', 'cancelled')
                ORDER BY t.priority DESC, t.due_at ASC NULLS LAST
                "#,
                aid
            )
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as!(
                TaskSummary,
                r#"
                SELECT
                    t.id as "id!",
                    t.title as "title!",
                    t.status as "status!: UserTaskStatus",
                    t.priority as "priority!: UserTaskPriority",
                    t.due_at as "due_at: _",
                    t.parent_task_id,
                    (SELECT COUNT(*) FROM tasks WHERE parent_task_id = t.id) as "subtask_count: i64"
                FROM tasks t
                WHERE t.agent_id IS NULL AND t.status NOT IN ('completed', 'cancelled')
                ORDER BY t.priority DESC, t.due_at ASC NULLS LAST
                "#
            )
            .fetch_all(pool)
            .await?
        }
    };
    Ok(summaries)
}
