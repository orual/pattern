//! Database statistics queries.

use sqlx::SqlitePool;

use crate::error::DbResult;

/// Overall database statistics.
#[derive(Debug, Clone)]
pub struct DbStats {
    pub agent_count: i64,
    pub group_count: i64,
    pub message_count: i64,
    pub memory_block_count: i64,
    pub archival_entry_count: i64,
}

/// Agent activity info for stats display.
#[derive(Debug, Clone)]
pub struct AgentActivity {
    pub name: String,
    pub message_count: i64,
}

/// Get overall database statistics.
pub async fn get_stats(pool: &SqlitePool) -> DbResult<DbStats> {
    let agent_count = sqlx::query_scalar!("SELECT COUNT(*) FROM agents")
        .fetch_one(pool)
        .await?;

    let group_count = sqlx::query_scalar!("SELECT COUNT(*) FROM agent_groups")
        .fetch_one(pool)
        .await?;

    let message_count = sqlx::query_scalar!("SELECT COUNT(*) FROM messages WHERE is_deleted = 0")
        .fetch_one(pool)
        .await?;

    let memory_block_count =
        sqlx::query_scalar!("SELECT COUNT(*) FROM memory_blocks WHERE is_active = 1")
            .fetch_one(pool)
            .await?;

    let archival_entry_count = sqlx::query_scalar!("SELECT COUNT(*) FROM archival_entries")
        .fetch_one(pool)
        .await?;

    Ok(DbStats {
        agent_count,
        group_count,
        message_count,
        memory_block_count,
        archival_entry_count,
    })
}

/// Get the most active agents by message count.
pub async fn get_most_active_agents(pool: &SqlitePool, limit: i64) -> DbResult<Vec<AgentActivity>> {
    let rows = sqlx::query!(
        r#"
        SELECT a.name as "name!", COUNT(m.id) as "msg_count!"
        FROM agents a
        LEFT JOIN messages m ON a.id = m.agent_id AND m.is_deleted = 0
        GROUP BY a.id
        ORDER BY 2 DESC
        LIMIT ?
        "#,
        limit
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AgentActivity {
            name: r.name,
            message_count: r.msg_count,
        })
        .collect())
}
