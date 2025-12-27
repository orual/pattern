//! Implementation of Jacquard's `SessionStore` trait for SQLite storage of app-password sessions.
//!
//! This provides persistent storage for app-password sessions (not OAuth/DPoP),
//! enabling Pattern agents to maintain simple JWT-based ATProto sessions across restarts.

use jacquard::CowStr;
use jacquard::IntoStatic;
use jacquard::client::AtpSession;
use jacquard::client::credential_session::SessionKey;
use jacquard::session::{SessionStore, SessionStoreError};
use jacquard::types::did::Did;
use jacquard::types::string::Handle;

use crate::atproto::models::AppPasswordSessionRow;
use crate::db::AuthDb;
use crate::error::AuthError;

impl SessionStore<SessionKey, AtpSession> for AuthDb {
    async fn get(&self, key: &SessionKey) -> Option<AtpSession> {
        let did_str = key.0.as_str();
        let session_id = key.1.as_ref();

        let row = sqlx::query_as!(
            AppPasswordSessionRow,
            r#"
            SELECT
                did as "did!",
                session_id as "session_id!",
                access_jwt as "access_jwt!",
                refresh_jwt as "refresh_jwt!",
                handle as "handle!"
            FROM app_password_sessions
            WHERE did = ? AND session_id = ?
            "#,
            did_str,
            session_id,
        )
        .fetch_optional(self.pool())
        .await
        .ok()?;

        let row = row?;

        // Convert row to AtpSession
        // Use new() which doesn't allocate for borrowed strings, then into_static()
        let did = Did::new(&row.did).ok()?.into_static();
        let handle = Handle::new(&row.handle).ok()?.into_static();

        Some(AtpSession {
            access_jwt: CowStr::from(row.access_jwt),
            refresh_jwt: CowStr::from(row.refresh_jwt),
            did,
            handle,
        })
    }

    async fn set(&self, key: SessionKey, session: AtpSession) -> Result<(), SessionStoreError> {
        let did_str = key.0.as_str();
        let session_id = key.1.as_ref();
        let access_jwt = session.access_jwt.as_ref();
        let refresh_jwt = session.refresh_jwt.as_ref();
        let handle = session.handle.as_str();
        let now = chrono::Utc::now().timestamp();

        sqlx::query!(
            r#"
            INSERT INTO app_password_sessions (
                did, session_id, access_jwt, refresh_jwt, handle, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (did, session_id) DO UPDATE SET
                access_jwt = excluded.access_jwt,
                refresh_jwt = excluded.refresh_jwt,
                handle = excluded.handle,
                updated_at = excluded.updated_at
            "#,
            did_str,
            session_id,
            access_jwt,
            refresh_jwt,
            handle,
            now,
            now,
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        Ok(())
    }

    async fn del(&self, key: &SessionKey) -> Result<(), SessionStoreError> {
        let did_str = key.0.as_str();
        let session_id = key.1.as_ref();

        sqlx::query!(
            "DELETE FROM app_password_sessions WHERE did = ? AND session_id = ?",
            did_str,
            session_id,
        )
        .execute(self.pool())
        .await
        .map_err(|e| SessionStoreError::from(AuthError::Database(e)))?;

        Ok(())
    }
}

// Additional list/query methods for CLI commands (not part of SessionStore trait)
impl AuthDb {
    /// List all stored app-password sessions.
    ///
    /// Returns a list of (did, session_id, handle) tuples for all stored sessions.
    pub async fn list_app_password_sessions(
        &self,
    ) -> crate::error::AuthResult<Vec<AppPasswordSessionRow>> {
        let rows = sqlx::query_as!(
            AppPasswordSessionRow,
            r#"
            SELECT
                did as "did!",
                session_id as "session_id!",
                access_jwt as "access_jwt!",
                refresh_jwt as "refresh_jwt!",
                handle as "handle!"
            FROM app_password_sessions
            ORDER BY did, session_id
            "#
        )
        .fetch_all(self.pool())
        .await?;

        Ok(rows)
    }

    /// Delete an app-password session by DID (and optionally session_id).
    ///
    /// If `session_id` is None, deletes all sessions for the DID.
    pub async fn delete_app_password_session(
        &self,
        did: &str,
        session_id: Option<&str>,
    ) -> crate::error::AuthResult<u64> {
        let result = if let Some(sid) = session_id {
            sqlx::query!(
                "DELETE FROM app_password_sessions WHERE did = ? AND session_id = ?",
                did,
                sid,
            )
            .execute(self.pool())
            .await?
        } else {
            sqlx::query!("DELETE FROM app_password_sessions WHERE did = ?", did,)
                .execute(self.pool())
                .await?
        };

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_app_password_session_roundtrip() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Create a test session key
        let did = Did::new("did:plc:testuser123").unwrap().into_static();
        let session_id = CowStr::from("test-session-id".to_string());
        let key = SessionKey(did.clone(), session_id);

        // Create a test session
        let session = AtpSession {
            access_jwt: CowStr::from("access-jwt-value"),
            refresh_jwt: CowStr::from("refresh-jwt-value"),
            did: did.clone(),
            handle: Handle::new("testuser.bsky.social").unwrap().into_static(),
        };

        // Save the session
        db.set(key.clone(), session.clone()).await.unwrap();

        // Retrieve the session
        let retrieved = db.get(&key).await.expect("session should exist");

        // Verify fields match
        assert_eq!(retrieved.did.as_str(), did.as_str());
        assert_eq!(retrieved.access_jwt.as_ref(), "access-jwt-value");
        assert_eq!(retrieved.refresh_jwt.as_ref(), "refresh-jwt-value");
        assert_eq!(retrieved.handle.as_str(), "testuser.bsky.social");

        // Delete the session
        db.del(&key).await.unwrap();

        // Verify it's gone
        let deleted = db.get(&key).await;
        assert!(deleted.is_none());
    }

    #[tokio::test]
    async fn test_app_password_session_update() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let did = Did::new("did:plc:testuser").unwrap().into_static();
        let session_id = CowStr::from("update-test".to_string());
        let key = SessionKey(did.clone(), session_id);

        // Create initial session
        let session = AtpSession {
            access_jwt: CowStr::from("token-1"),
            refresh_jwt: CowStr::from("refresh-1"),
            did: did.clone(),
            handle: Handle::new("user.bsky.social").unwrap().into_static(),
        };

        db.set(key.clone(), session).await.unwrap();

        // Update the session with new tokens
        let updated_session = AtpSession {
            access_jwt: CowStr::from("token-2"),
            refresh_jwt: CowStr::from("refresh-2"),
            did: did.clone(),
            handle: Handle::new("user.bsky.social").unwrap().into_static(),
        };

        db.set(key.clone(), updated_session).await.unwrap();

        // Verify update
        let retrieved = db.get(&key).await.expect("session should exist");

        assert_eq!(retrieved.access_jwt.as_ref(), "token-2");
        assert_eq!(retrieved.refresh_jwt.as_ref(), "refresh-2");
    }

    #[tokio::test]
    async fn test_app_password_session_multiple_sessions() {
        let db = AuthDb::open_in_memory().await.unwrap();

        let did = Did::new("did:plc:multi").unwrap().into_static();

        // Create multiple sessions for the same DID
        let key1 = SessionKey(did.clone(), CowStr::from("session-1".to_string()));
        let key2 = SessionKey(did.clone(), CowStr::from("session-2".to_string()));

        let session1 = AtpSession {
            access_jwt: CowStr::from("access-1"),
            refresh_jwt: CowStr::from("refresh-1"),
            did: did.clone(),
            handle: Handle::new("user.bsky.social").unwrap().into_static(),
        };

        let session2 = AtpSession {
            access_jwt: CowStr::from("access-2"),
            refresh_jwt: CowStr::from("refresh-2"),
            did: did.clone(),
            handle: Handle::new("user.bsky.social").unwrap().into_static(),
        };

        db.set(key1.clone(), session1).await.unwrap();
        db.set(key2.clone(), session2).await.unwrap();

        // Both sessions should exist independently
        let retrieved1 = db.get(&key1).await.expect("session 1 should exist");
        let retrieved2 = db.get(&key2).await.expect("session 2 should exist");

        assert_eq!(retrieved1.access_jwt.as_ref(), "access-1");
        assert_eq!(retrieved2.access_jwt.as_ref(), "access-2");

        // Delete one, verify other still exists
        db.del(&key1).await.unwrap();
        assert!(db.get(&key1).await.is_none());
        assert!(db.get(&key2).await.is_some());
    }
}
