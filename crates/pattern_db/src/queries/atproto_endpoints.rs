//! Agent ATProto endpoint queries.
//!
//! These queries manage the mapping between agents and their ATProto identities
//! (DIDs) for different endpoint types like Bluesky posting.

use rusqlite::OptionalExtension;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::DbResult;
use crate::models::AgentAtprotoEndpoint;

// ============================================================================
// from_row implementation
// ============================================================================

impl AgentAtprotoEndpoint {
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            agent_id: row.get("agent_id")?,
            did: row.get("did")?,
            endpoint_type: row.get("endpoint_type")?,
            session_id: row.get("session_id")?,
            config: row.get("config")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Get the current Unix timestamp in seconds.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64
}

/// Get an agent's ATProto endpoint configuration for a specific endpoint type.
pub fn get_agent_atproto_endpoint(
    conn: &rusqlite::Connection,
    agent_id: &str,
    endpoint_type: &str,
) -> DbResult<Option<AgentAtprotoEndpoint>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, did, endpoint_type, session_id, config, created_at, updated_at
         FROM agent_atproto_endpoints WHERE agent_id = ?1 AND endpoint_type = ?2",
    )?;
    let result = stmt
        .query_row(
            rusqlite::params![agent_id, endpoint_type],
            AgentAtprotoEndpoint::from_row,
        )
        .optional()?;
    Ok(result)
}

/// Get all ATProto endpoint configurations for an agent.
pub fn get_agent_atproto_endpoints(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> DbResult<Vec<AgentAtprotoEndpoint>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, did, endpoint_type, session_id, config, created_at, updated_at
         FROM agent_atproto_endpoints WHERE agent_id = ?1 ORDER BY endpoint_type",
    )?;
    let rows = stmt.query_map(rusqlite::params![agent_id], AgentAtprotoEndpoint::from_row)?;
    let mut endpoints = Vec::new();
    for row in rows {
        endpoints.push(row?);
    }
    Ok(endpoints)
}

/// Set (upsert) an agent's ATProto endpoint configuration.
///
/// If an endpoint configuration already exists for this agent and endpoint type,
/// it will be updated. Otherwise, a new configuration will be created.
pub fn set_agent_atproto_endpoint(
    conn: &rusqlite::Connection,
    endpoint: &AgentAtprotoEndpoint,
) -> DbResult<()> {
    let now = unix_now();
    conn.execute(
        "INSERT INTO agent_atproto_endpoints (agent_id, did, endpoint_type, session_id, config, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(agent_id, endpoint_type) DO UPDATE SET
             did = excluded.did,
             session_id = excluded.session_id,
             config = excluded.config,
             updated_at = excluded.updated_at",
        rusqlite::params![
            endpoint.agent_id,
            endpoint.did,
            endpoint.endpoint_type,
            endpoint.session_id,
            endpoint.config,
            now,
            now,
        ],
    )?;
    Ok(())
}

/// Delete an agent's ATProto endpoint configuration.
pub fn delete_agent_atproto_endpoint(
    conn: &rusqlite::Connection,
    agent_id: &str,
    endpoint_type: &str,
) -> DbResult<bool> {
    let count = conn.execute(
        "DELETE FROM agent_atproto_endpoints WHERE agent_id = ?1 AND endpoint_type = ?2",
        rusqlite::params![agent_id, endpoint_type],
    )?;
    Ok(count > 0)
}

/// List all ATProto endpoint configurations across all agents.
pub fn list_all_agent_atproto_endpoints(
    conn: &rusqlite::Connection,
) -> DbResult<Vec<AgentAtprotoEndpoint>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, did, endpoint_type, session_id, config, created_at, updated_at
         FROM agent_atproto_endpoints ORDER BY did, agent_id",
    )?;
    let rows = stmt.query_map([], AgentAtprotoEndpoint::from_row)?;
    let mut endpoints = Vec::new();
    for row in rows {
        endpoints.push(row?);
    }
    Ok(endpoints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConstellationDb;

    fn setup_test_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().unwrap()
    }

    #[test]
    fn test_roundtrip_endpoint() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        let endpoint = AgentAtprotoEndpoint {
            agent_id: "test-agent".to_string(),
            did: "did:plc:testuser123".to_string(),
            endpoint_type: "bluesky_post".to_string(),
            session_id: Some("_constellation_".to_string()),
            config: Some(r#"{"auto_reply": true}"#.to_string()),
            created_at: 0,
            updated_at: 0,
        };

        set_agent_atproto_endpoint(&conn, &endpoint).unwrap();

        let retrieved = get_agent_atproto_endpoint(&conn, "test-agent", "bluesky_post")
            .unwrap()
            .expect("endpoint should exist");

        assert_eq!(retrieved.agent_id, "test-agent");
        assert_eq!(retrieved.did, "did:plc:testuser123");
        assert_eq!(retrieved.endpoint_type, "bluesky_post");
        assert_eq!(retrieved.config, Some(r#"{"auto_reply": true}"#.to_string()));
        assert!(retrieved.created_at > 0);

        // Update the endpoint (upsert).
        let updated_endpoint = AgentAtprotoEndpoint {
            agent_id: "test-agent".to_string(),
            did: "did:plc:newuser456".to_string(),
            endpoint_type: "bluesky_post".to_string(),
            session_id: None,
            config: None,
            created_at: 0,
            updated_at: 0,
        };
        set_agent_atproto_endpoint(&conn, &updated_endpoint).unwrap();

        let after_update = get_agent_atproto_endpoint(&conn, "test-agent", "bluesky_post")
            .unwrap()
            .expect("endpoint should exist");
        assert_eq!(after_update.did, "did:plc:newuser456");
        assert!(after_update.config.is_none());

        // Delete the endpoint.
        let deleted = delete_agent_atproto_endpoint(&conn, "test-agent", "bluesky_post").unwrap();
        assert!(deleted);

        let after_delete = get_agent_atproto_endpoint(&conn, "test-agent", "bluesky_post").unwrap();
        assert!(after_delete.is_none());

        let deleted_again =
            delete_agent_atproto_endpoint(&conn, "test-agent", "bluesky_post").unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_multiple_endpoints_per_agent() {
        let db = setup_test_db();
        let conn = db.get().unwrap();

        let endpoint1 = AgentAtprotoEndpoint {
            agent_id: "test-agent".to_string(),
            did: "did:plc:user123".to_string(),
            endpoint_type: "bluesky_post".to_string(),
            session_id: Some("_constellation_".to_string()),
            config: None,
            created_at: 0,
            updated_at: 0,
        };
        let endpoint2 = AgentAtprotoEndpoint {
            agent_id: "test-agent".to_string(),
            did: "did:plc:user123".to_string(),
            endpoint_type: "bluesky_firehose".to_string(),
            session_id: Some("_constellation_".to_string()),
            config: Some(r#"{"filter": "mentions"}"#.to_string()),
            created_at: 0,
            updated_at: 0,
        };

        set_agent_atproto_endpoint(&conn, &endpoint1).unwrap();
        set_agent_atproto_endpoint(&conn, &endpoint2).unwrap();

        let all_endpoints = get_agent_atproto_endpoints(&conn, "test-agent").unwrap();
        assert_eq!(all_endpoints.len(), 2);
        assert_eq!(all_endpoints[0].endpoint_type, "bluesky_firehose");
        assert_eq!(all_endpoints[1].endpoint_type, "bluesky_post");
    }
}
