// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Database statistics queries.

use crate::error::DbResult;

/// Overall database statistics.
#[derive(Debug, Clone)]
pub struct DbStats {
    pub agent_count: i64,
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
///
/// Messages live in the attached `msg` schema; unqualified table names
/// resolve via SQLite's schema search order (temp -> main -> attached).
pub fn get_stats(conn: &rusqlite::Connection) -> DbResult<DbStats> {
    let agent_count: i64 = conn.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))?;

    let message_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE is_deleted = 0",
        [],
        |r| r.get(0),
    )?;

    let memory_block_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_blocks WHERE is_active = 1",
        [],
        |r| r.get(0),
    )?;

    let archival_entry_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM archival_entries", [], |r| r.get(0))?;

    Ok(DbStats {
        agent_count,
        message_count,
        memory_block_count,
        archival_entry_count,
    })
}

/// Get the most active agents by message count.
pub fn get_most_active_agents(
    conn: &rusqlite::Connection,
    limit: i64,
) -> DbResult<Vec<AgentActivity>> {
    let sql = "SELECT a.name, COUNT(m.id) as msg_count
         FROM agents a
         LEFT JOIN messages m ON a.id = m.agent_id AND m.is_deleted = 0
         GROUP BY a.id
         ORDER BY 2 DESC
         LIMIT ?1";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok(AgentActivity {
            name: row.get(0)?,
            message_count: row.get(1)?,
        })
    })?;
    let mut activities = Vec::new();
    for row in rows {
        activities.push(row?);
    }
    Ok(activities)
}
