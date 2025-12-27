//! Database operations for ATProto identity management

use crate::atproto_identity::{AtprotoAuthState, AtprotoIdentity};
use crate::db::{DatabaseError, entity::DbEntity};
use crate::error::CoreError;
use crate::id::{Did, IdType, UserId};
use surrealdb::{Connection, RecordId, Surreal};
use tracing::{debug, info};

type Result<T> = std::result::Result<T, CoreError>;

/// Create or update an ATProto identity for a user
pub async fn upsert_atproto_identity<C: Connection>(
    db: &Surreal<C>,
    identity: AtprotoIdentity,
) -> Result<AtprotoIdentity> {
    use super::{create_entity, get_entity, update_entity};

    debug!("Upserting ATProto identity for DID: {}", identity.id);

    // First, check if this DID is already linked to a different user
    let existing = get_entity::<AtprotoIdentity, _>(db, &identity.id)
        .await
        .map_err(|e| {
            CoreError::from(e).with_db_context(
                format!(
                    "SELECT * FROM atproto_identity WHERE id = '{}'",
                    identity.id
                ),
                "atproto_identity",
            )
        })?;

    if let Some(existing) = existing {
        if existing.user_id != identity.user_id {
            return Err(DatabaseError::Other(format!(
                "DID {} is already linked to a different user",
                identity.id
            ))
            .into());
        }

        // Update existing identity
        Ok(update_entity(db, &identity).await?)
    } else {
        // Create new identity and establish relationship
        let created = create_entity::<AtprotoIdentity, _>(db, &identity).await?;

        println!("result {:?}", created);
        // Create the relationship to the user
        let query = "RELATE $user->authenticates->$identity";
        db.query(query)
            .bind(("user", surrealdb::RecordId::from(created.user_id.clone())))
            .bind(("identity", surrealdb::RecordId::from(created.id.clone())))
            .await
            .map_err(DatabaseError::QueryFailed)?;

        info!("ATProto identity linked for user: {}", created.user_id);

        Ok(created)
    }
}

/// Get an ATProto identity by DID
pub async fn get_atproto_identity_by_did<C: Connection>(
    db: &Surreal<C>,
    did: &Did,
) -> Result<Option<AtprotoIdentity>> {
    debug!("Looking up ATProto identity for DID: {}", did);

    let select_by_did = "SELECT * FROM atproto_identity WHERE id = $did LIMIT 1";
    let mut result = db
        .query(select_by_did)
        .bind(("did", RecordId::from(did)))
        .await
        .map_err(|e| {
            crate::CoreError::database_query_error(select_by_did, "atproto_identity", e)
        })?;

    println!("result {:?}", result);
    // Query by DID field directly
    let identities: Vec<<AtprotoIdentity as DbEntity>::DbModel> =
        result.take(0).map_err(DatabaseError::QueryFailed)?;

    println!("identities {:?}", identities);

    Ok(identities
        .into_iter()
        .map(|e| AtprotoIdentity::from_db_model(e).unwrap())
        .next())
}

/// Get all ATProto identities for a user
pub async fn get_user_atproto_identities<C: Connection>(
    db: &Surreal<C>,
    user_id: &UserId,
) -> Result<Vec<AtprotoIdentity>> {
    debug!("Getting ATProto identities for user: {}", user_id);

    let select_by_user = "SELECT * FROM atproto_identity WHERE user_id = $user_id";
    let identities: Vec<<AtprotoIdentity as DbEntity>::DbModel> = db
        .query(select_by_user)
        .bind(("user_id", RecordId::from(user_id)))
        .await
        .map_err(|e| crate::CoreError::database_query_error(select_by_user, "atproto_identity", e))?
        .take(0)
        .map_err(|e| {
            crate::CoreError::database_query_error("take results", "atproto_identity", e)
        })?;

    Ok(identities
        .into_iter()
        .map(|e| AtprotoIdentity::from_db_model(e).unwrap())
        .collect())
}

/// Update ATProto identity tokens after refresh
pub async fn update_atproto_tokens<C: Connection>(
    db: &Surreal<C>,
    did: &Did,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<AtprotoIdentity> {
    debug!("Updating tokens for DID: {}", did);

    // Get the existing identity
    let identity =
        get_atproto_identity_by_did(db, did)
            .await?
            .ok_or_else(|| DatabaseError::NotFound {
                entity_type: "atproto_identity".to_string(),
                id: did.to_string(),
            })?;

    // Update the tokens
    let mut updated = identity;
    updated.update_tokens(access_token, refresh_token, expires_at);

    println!("updated {:?}", updated);

    // Save to database
    let saved: Option<<AtprotoIdentity as DbEntity>::DbModel> = db
        .update(("atproto_identity", updated.id().to_record_id()))
        .content(updated.to_db_model())
        .await
        .map_err(|e| {
            crate::CoreError::database_query_error("update atproto_identity", "atproto_identity", e)
        })?;

    println!("saved {:?}", saved);

    let saved = saved.ok_or_else(|| DatabaseError::NotFound {
        entity_type: "atproto_identity".to_string(),
        id: did.to_string(),
    })?;

    Ok(AtprotoIdentity::from_db_model(saved).unwrap())
}

/// Delete an ATProto identity
pub async fn delete_atproto_identity<C: Connection>(
    db: &Surreal<C>,
    did: &Did,
    user_id: &UserId,
) -> Result<bool> {
    debug!(
        "Deleting ATProto identity for DID: {} and user: {}",
        did, user_id
    );

    // Get the identity to verify ownership
    let identity =
        get_atproto_identity_by_did(db, did)
            .await?
            .ok_or_else(|| DatabaseError::NotFound {
                entity_type: "atproto_identity".to_string(),
                id: did.to_string(),
            })?;

    // Verify the user owns this identity
    if identity.user_id != *user_id {
        return Err(DatabaseError::Other(
            "Cannot delete ATProto identity belonging to another user".to_string(),
        )
        .into());
    }

    // Delete the identity
    let _: Option<<AtprotoIdentity as DbEntity>::DbModel> = db
        .delete(("atproto_identity", identity.id.to_key()))
        .await
        .map_err(DatabaseError::QueryFailed)?;

    info!("Deleted ATProto identity for user: {}", user_id);
    Ok(true)
}

/// Store auth state during OAuth flow (in-memory cache in production)
pub async fn store_auth_state<C: Connection>(
    db: &Surreal<C>,
    state: &str,
    auth_state: AtprotoAuthState,
) -> Result<()> {
    debug!("Storing auth state: {}", state);

    // For now, store in a temporary table
    // In production, use Redis or in-memory cache
    let _: Vec<serde_json::Value> = db
        .query("INSERT INTO atproto_auth_state (id, state, data, expires_at) VALUES ($id, $state, $data, $expires)")
        .bind(("id", format!("atproto_auth_state:{}", state)))
        .bind(("state", state.to_string()))
        .bind(("data", serde_json::to_value(&auth_state).map_err(|e| CoreError::SerializationError {
            data_type: "AtprotoAuthState".to_string(),
            cause: e,
        })?))
        .bind(("expires", auth_state.created_at + chrono::Duration::minutes(15)))
        .await
        .map_err(DatabaseError::QueryFailed)?
        .take(0)
        .map_err(DatabaseError::QueryFailed)?;

    Ok(())
}

/// Retrieve and delete auth state during OAuth callback
pub async fn consume_auth_state<C: Connection>(
    db: &Surreal<C>,
    state: &str,
) -> Result<Option<AtprotoAuthState>> {
    debug!("Consuming auth state: {}", state);

    // Get the state
    let result: Vec<serde_json::Value> = db
        .query(
            "SELECT data FROM atproto_auth_state WHERE state = $state AND expires_at > time::now()",
        )
        .bind(("state", state.to_string()))
        .await
        .map_err(DatabaseError::QueryFailed)?
        .take(0)
        .map_err(DatabaseError::QueryFailed)?;

    let result = result.into_iter().next();

    if let Some(data) = result {
        // Delete it
        let _: Vec<serde_json::Value> = db
            .query("DELETE FROM atproto_auth_state WHERE state = $state")
            .bind(("state", state.to_string()))
            .await
            .map_err(DatabaseError::QueryFailed)?
            .take(0)
            .map_err(DatabaseError::QueryFailed)?;

        // Parse the auth state
        let auth_state: AtprotoAuthState =
            serde_json::from_value(data["data"].clone()).map_err(|e| {
                CoreError::SerializationError {
                    data_type: "AtprotoAuthState".to_string(),
                    cause: e,
                }
            })?;
        Ok(Some(auth_state))
    } else {
        Ok(None)
    }
}

/// Update last authentication time
pub async fn update_last_auth<C: Connection>(db: &Surreal<C>, did: &str) -> Result<()> {
    debug!("Updating last auth time for DID: {}", did);

    // Query by DID and update
    let _: Vec<serde_json::Value> = db
        .query("UPDATE atproto_identity SET last_auth_at = time::now() WHERE did = $did")
        .bind(("did", did.to_string()))
        .await
        .map_err(DatabaseError::QueryFailed)?
        .take(0)
        .map_err(DatabaseError::QueryFailed)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::client::create_test_db, id::Did};
    use atrium_api::types::string::Did as AtDid;

    #[tokio::test]
    async fn test_atproto_identity_crud() {
        let db = create_test_db().await.unwrap();
        let user_id = UserId::generate();

        // Create an identity
        let identity = AtprotoIdentity::new_oauth(
            AtDid::new("did:plc:abc123".to_string()).unwrap(),
            "test.bsky.social".to_string(),
            "https://bsky.social".to_string(),
            "access_token".to_string(),
            Some("refresh_token".to_string()),
            chrono::Utc::now() + chrono::Duration::hours(1),
            user_id.clone(),
        );

        let did = Did(AtDid::new("did:plc:abc123".to_string()).unwrap());

        let created = upsert_atproto_identity(&db, identity).await.unwrap();
        assert_eq!(
            created.id,
            Did(atrium_api::types::string::Did::new("did:plc:abc123".to_string()).unwrap())
        );

        // Get by DID
        let fetched = get_atproto_identity_by_did(&db, &did)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.handle, "test.bsky.social");

        // Get user's identities
        let identities = get_user_atproto_identities(&db, &user_id).await.unwrap();
        assert_eq!(identities.len(), 1);

        // Update tokens
        let updated = update_atproto_tokens(
            &db,
            &did,
            "new_access_token".to_string(),
            Some("new_refresh_token".to_string()),
            chrono::Utc::now() + chrono::Duration::hours(2),
        )
        .await
        .unwrap();
        assert_eq!(updated.access_token, Some("new_access_token".to_string()));

        // Delete
        let deleted = delete_atproto_identity(&db, &did, &user_id).await.unwrap();
        assert!(deleted);

        // Verify deletion
        let gone = get_atproto_identity_by_did(&db, &did).await.unwrap();
        assert!(gone.is_none());
    }
}
