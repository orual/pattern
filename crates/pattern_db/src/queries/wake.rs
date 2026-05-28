// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! CRUD queries for `wake_registrations` (migration 0018).
//!
//! Persists wake-condition registrations across daemon restarts so agents
//! don't have to re-register every restart. The session-open path calls
//! `list_wakes_for_agent` and replays each row through the in-memory
//! `WakeRegistry::register` — using the SAME wake_id so the IDs are stable
//! across restarts.
//!
//! `condition_json` is `serde_json::to_string(&WireWakeCondition)` — the
//! caller serializes/deserializes (this module doesn't depend on the
//! wire-type crate to avoid a cycle).

use jiff::Timestamp;
use rusqlite::{Connection, params};

use crate::error::DbResult;

/// One persisted wake registration row.
#[derive(Debug, Clone)]
pub struct WakeRegistrationRow {
    pub wake_id: String,
    pub agent_id: String,
    pub condition_json: String,
    pub created_at: Timestamp,
}

/// Insert a new wake registration. Errors on PK conflict (caller is
/// responsible for using a fresh wake_id).
pub fn insert_wake_registration(
    conn: &Connection,
    wake_id: &str,
    agent_id: &str,
    condition_json: &str,
) -> DbResult<()> {
    let created_at = Timestamp::now().to_string();
    conn.execute(
        "INSERT INTO wake_registrations (wake_id, agent_id, condition_json, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![wake_id, agent_id, condition_json, created_at],
    )?;
    Ok(())
}

/// Delete the registration row for `(agent_id, wake_id)`. Returns the
/// number of rows removed (0 if no such row, 1 on success). The composite
/// PK from migration 0019 means callers must supply both halves.
pub fn delete_wake_registration(
    conn: &Connection,
    agent_id: &str,
    wake_id: &str,
) -> DbResult<usize> {
    let n = conn.execute(
        "DELETE FROM wake_registrations WHERE agent_id = ?1 AND wake_id = ?2",
        params![agent_id, wake_id],
    )?;
    Ok(n)
}

/// List all wake registrations for the given agent_id, in insertion
/// order (by created_at).
pub fn list_wakes_for_agent(conn: &Connection, agent_id: &str) -> DbResult<Vec<WakeRegistrationRow>> {
    let mut stmt = conn.prepare(
        "SELECT wake_id, agent_id, condition_json, created_at
         FROM wake_registrations
         WHERE agent_id = ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![agent_id], |row| {
        let wake_id: String = row.get(0)?;
        let agent_id: String = row.get(1)?;
        let condition_json: String = row.get(2)?;
        let created_at_str: String = row.get(3)?;
        let created_at: Timestamp = created_at_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid jiff::Timestamp {created_at_str:?}: {e}"),
                )),
            )
        })?;
        Ok(WakeRegistrationRow { wake_id, agent_id, condition_json, created_at })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::run_memory_migrations;

    fn fresh_conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut c).unwrap();
        c
    }

    #[test]
    fn insert_list_delete_roundtrip() {
        let c = fresh_conn();
        insert_wake_registration(&c, "w1", "alice", "{\"Interval\":15}").unwrap();
        insert_wake_registration(&c, "w2", "alice", "{\"Interval\":30}").unwrap();
        insert_wake_registration(&c, "w3", "bob", "{\"Interval\":60}").unwrap();

        let alices = list_wakes_for_agent(&c, "alice").unwrap();
        assert_eq!(alices.len(), 2);
        assert_eq!(alices[0].wake_id, "w1");
        assert_eq!(alices[1].wake_id, "w2");

        let bobs = list_wakes_for_agent(&c, "bob").unwrap();
        assert_eq!(bobs.len(), 1);

        // composite key: must supply both halves
        assert_eq!(delete_wake_registration(&c, "alice", "w1").unwrap(), 1);
        assert_eq!(delete_wake_registration(&c, "alice", "nonexistent").unwrap(), 0);
        // wrong-agent delete is a no-op even if id exists
        assert_eq!(delete_wake_registration(&c, "alice", "w3").unwrap(), 0);

        let alices_after = list_wakes_for_agent(&c, "alice").unwrap();
        assert_eq!(alices_after.len(), 1);
        assert_eq!(alices_after[0].wake_id, "w2");
    }

    #[test]
    fn composite_key_allows_shared_names_across_agents() {
        // After migration 0019, two agents can both register a wake named
        // `social-check` without colliding — the PK is (agent_id, wake_id).
        let c = fresh_conn();
        insert_wake_registration(&c, "social-check", "alice", "{\"a\":1}").unwrap();
        insert_wake_registration(&c, "social-check", "bob", "{\"b\":2}").unwrap();

        let alices = list_wakes_for_agent(&c, "alice").unwrap();
        let bobs = list_wakes_for_agent(&c, "bob").unwrap();
        assert_eq!(alices.len(), 1);
        assert_eq!(bobs.len(), 1);
        assert_eq!(alices[0].wake_id, "social-check");
        assert_eq!(bobs[0].wake_id, "social-check");
        // condition_json is per-agent
        assert_eq!(alices[0].condition_json, "{\"a\":1}");
        assert_eq!(bobs[0].condition_json, "{\"b\":2}");

        // Deleting alice's shouldn't affect bob's.
        assert_eq!(delete_wake_registration(&c, "alice", "social-check").unwrap(), 1);
        assert_eq!(list_wakes_for_agent(&c, "alice").unwrap().len(), 0);
        assert_eq!(list_wakes_for_agent(&c, "bob").unwrap().len(), 1);
    }
}
