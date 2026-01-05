//! Agent ATProto endpoint queries.
//!
//! These queries manage the mapping between agents and their ATProto identities
//! (DIDs) for different endpoint types like Bluesky posting.

use sqlx::SqlitePool;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::DbResult;
use crate::models::AgentAtprotoEndpoint;

/// Get the current Unix timestamp in seconds.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64
}

/// Get an agent's ATProto endpoint configuration for a specific endpoint type.
pub async fn get_agent_atproto_endpoint(
    pool: &SqlitePool,
    agent_id: &str,
    endpoint_type: &str,
) -> DbResult<Option<AgentAtprotoEndpoint>> {
    let endpoint = sqlx::query_as!(
        AgentAtprotoEndpoint,
        r#"
        SELECT
            agent_id as "agent_id!",
            did as "did!",
            endpoint_type as "endpoint_type!",
            session_id,
            config,
            created_at as "created_at!",
            updated_at as "updated_at!"
        FROM agent_atproto_endpoints
        WHERE agent_id = ? AND endpoint_type = ?
        "#,
        agent_id,
        endpoint_type
    )
    .fetch_optional(pool)
    .await?;
    Ok(endpoint)
}

/// Get all ATProto endpoint configurations for an agent.
pub async fn get_agent_atproto_endpoints(
    pool: &SqlitePool,
    agent_id: &str,
) -> DbResult<Vec<AgentAtprotoEndpoint>> {
    let endpoints = sqlx::query_as!(
        AgentAtprotoEndpoint,
        r#"
        SELECT
            agent_id as "agent_id!",
            did as "did!",
            endpoint_type as "endpoint_type!",
            session_id,
            config,
            created_at as "created_at!",
            updated_at as "updated_at!"
        FROM agent_atproto_endpoints
        WHERE agent_id = ?
        ORDER BY endpoint_type
        "#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(endpoints)
}

/// Set (upsert) an agent's ATProto endpoint configuration.
///
/// If an endpoint configuration already exists for this agent and endpoint type,
/// it will be updated. Otherwise, a new configuration will be created.
pub async fn set_agent_atproto_endpoint(
    pool: &SqlitePool,
    endpoint: &AgentAtprotoEndpoint,
) -> DbResult<()> {
    let now = unix_now();
    sqlx::query!(
        r#"
        INSERT INTO agent_atproto_endpoints (agent_id, did, endpoint_type, session_id, config, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(agent_id, endpoint_type) DO UPDATE SET
            did = excluded.did,
            session_id = excluded.session_id,
            config = excluded.config,
            updated_at = excluded.updated_at
        "#,
        endpoint.agent_id,
        endpoint.did,
        endpoint.endpoint_type,
        endpoint.session_id,
        endpoint.config,
        now,
        now
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete an agent's ATProto endpoint configuration.
pub async fn delete_agent_atproto_endpoint(
    pool: &SqlitePool,
    agent_id: &str,
    endpoint_type: &str,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "DELETE FROM agent_atproto_endpoints WHERE agent_id = ? AND endpoint_type = ?",
        agent_id,
        endpoint_type
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// List all ATProto endpoint configurations across all agents.
pub async fn list_all_agent_atproto_endpoints(
    pool: &SqlitePool,
) -> DbResult<Vec<AgentAtprotoEndpoint>> {
    let endpoints = sqlx::query_as!(
        AgentAtprotoEndpoint,
        r#"
        SELECT
            agent_id as "agent_id!",
            did as "did!",
            endpoint_type as "endpoint_type!",
            session_id,
            config,
            created_at as "created_at!",
            updated_at as "updated_at!"
        FROM agent_atproto_endpoints
        ORDER BY did, agent_id
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(endpoints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConstellationDb;
    use tempfile::TempDir;

    async fn setup_test_db() -> (ConstellationDb, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = ConstellationDb::open(&db_path).await.unwrap();

        (db, temp_dir)
    }

    #[tokio::test]
    async fn test_roundtrip_endpoint() {
        let (db, _temp) = setup_test_db().await;
        let pool = db.pool();

        // Create an endpoint
        let endpoint = AgentAtprotoEndpoint {
            agent_id: "test-agent".to_string(),
            did: "did:plc:testuser123".to_string(),
            endpoint_type: "bluesky_post".to_string(),
            session_id: Some("_constellation_".to_string()),
            config: Some(r#"{"auto_reply": true}"#.to_string()),
            created_at: 0, // Will be set by the query
            updated_at: 0, // Will be set by the query
        };

        // Set the endpoint
        set_agent_atproto_endpoint(pool, &endpoint).await.unwrap();

        // Get the endpoint
        let retrieved = get_agent_atproto_endpoint(pool, "test-agent", "bluesky_post")
            .await
            .unwrap()
            .expect("endpoint should exist");

        assert_eq!(retrieved.agent_id, "test-agent");
        assert_eq!(retrieved.did, "did:plc:testuser123");
        assert_eq!(retrieved.endpoint_type, "bluesky_post");
        assert_eq!(
            retrieved.config,
            Some(r#"{"auto_reply": true}"#.to_string())
        );
        assert!(retrieved.created_at > 0);
        assert!(retrieved.updated_at > 0);

        // Update the endpoint (upsert)
        let updated_endpoint = AgentAtprotoEndpoint {
            agent_id: "test-agent".to_string(),
            did: "did:plc:newuser456".to_string(),
            endpoint_type: "bluesky_post".to_string(),
            session_id: None,
            config: None,
            created_at: 0,
            updated_at: 0,
        };
        set_agent_atproto_endpoint(pool, &updated_endpoint)
            .await
            .unwrap();

        // Verify update
        let after_update = get_agent_atproto_endpoint(pool, "test-agent", "bluesky_post")
            .await
            .unwrap()
            .expect("endpoint should exist");
        assert_eq!(after_update.did, "did:plc:newuser456");
        assert!(after_update.config.is_none());

        // Delete the endpoint
        let deleted = delete_agent_atproto_endpoint(pool, "test-agent", "bluesky_post")
            .await
            .unwrap();
        assert!(deleted);

        // Verify deletion
        let after_delete = get_agent_atproto_endpoint(pool, "test-agent", "bluesky_post")
            .await
            .unwrap();
        assert!(after_delete.is_none());

        // Delete again should return false
        let deleted_again = delete_agent_atproto_endpoint(pool, "test-agent", "bluesky_post")
            .await
            .unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn test_multiple_endpoints_per_agent() {
        let (db, _temp) = setup_test_db().await;
        let pool = db.pool();

        // Create multiple endpoints for the same agent
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

        set_agent_atproto_endpoint(pool, &endpoint1).await.unwrap();
        set_agent_atproto_endpoint(pool, &endpoint2).await.unwrap();

        // Get all endpoints for the agent
        let all_endpoints = get_agent_atproto_endpoints(pool, "test-agent")
            .await
            .unwrap();

        assert_eq!(all_endpoints.len(), 2);

        // Verify they're sorted by endpoint_type
        assert_eq!(all_endpoints[0].endpoint_type, "bluesky_firehose");
        assert_eq!(all_endpoints[1].endpoint_type, "bluesky_post");

        // Verify each can be retrieved individually
        let firehose = get_agent_atproto_endpoint(pool, "test-agent", "bluesky_firehose")
            .await
            .unwrap()
            .expect("firehose endpoint should exist");
        assert_eq!(
            firehose.config,
            Some(r#"{"filter": "mentions"}"#.to_string())
        );

        let post = get_agent_atproto_endpoint(pool, "test-agent", "bluesky_post")
            .await
            .unwrap()
            .expect("post endpoint should exist");
        assert!(post.config.is_none());
    }
}
