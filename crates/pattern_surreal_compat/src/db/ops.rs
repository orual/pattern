//! Database operations - direct, simple, no unnecessary abstractions

use super::{
    DatabaseError, Result,
    entity::{AgentMemoryRelation, DbEntity},
};
use serde_json::json;
use surrealdb::{Connection, Surreal};

// Local imports
use crate::{
    Message, MessageRelationType,
    id::{AgentId, GroupId, IdType, MemoryId, MessageId, RelationId, UserId},
};

// TODO: restore after types are moved in later tasks
// Commented out imports for types not yet moved:
// - AgentRecord, get_next_message_position_sync (from agent module)
// - AgentGroup, GroupMembership (from coordination/groups)
// - EmbeddingProvider (from embeddings)
// - MemoryBlock (from memory)
// - Message (from message)
// - ResponseExt (from utils/debug)
use chrono::Utc;

use futures::{Stream, StreamExt};
use surrealdb::opt::PatchOp;

use surrealdb::{Action, Notification, RecordId};

// ============================================================================
// Generic Entity Operations
// ============================================================================

/// Create a new entity in the database
pub async fn create_entity<E: DbEntity, C: Connection>(
    conn: &Surreal<C>,
    entity: &E,
) -> Result<E::Domain> {
    let db_model = E::to_db_model(entity);

    // Debug output to see what we're trying to create
    tracing::debug!(
        "Creating entity in table '{}' with model: {:?}",
        E::table_name(),
        serde_json::to_string_pretty(&db_model)
            .unwrap_or_else(|_| "failed to serialize".to_string())
    );

    let created: E::DbModel = conn
        .create((E::table_name(), entity.record_key()))
        .content(db_model)
        .await
        .map_err(|e| {
            DatabaseError::QueryFailed(e).with_context(
                format!("CREATE {} [{}]", E::table_name(), entity.record_key()),
                E::table_name(),
            )
        })?
        .expect("SurrealDB should return created entity");

    E::from_db_model(created).map_err(DatabaseError::from)
}

/// Get an entity by ID
pub async fn get_entity<E: DbEntity, C: Connection>(
    conn: &Surreal<C>,
    id: &E::Id,
) -> Result<Option<E::Domain>> {
    let result: Option<E::DbModel> =
        conn.select((E::table_name(), id.to_key()))
            .await
            .map_err(|e| {
                DatabaseError::QueryFailed(e).with_context(
                    format!(
                        "SELECT * FROM {} WHERE id = '{}'",
                        E::table_name(),
                        id.to_key()
                    ),
                    E::table_name(),
                )
            })?;

    match result {
        Some(db_model) => Ok(Some(E::from_db_model(db_model)?)),
        None => Ok(None),
    }
}

/// Update an entity in the database
pub async fn update_entity<E: DbEntity, C: Connection>(
    conn: &Surreal<C>,
    entity: &E,
) -> Result<E::Domain> {
    let db_model = E::to_db_model(entity);
    let updated: Option<E::DbModel> = conn
        .update((E::table_name(), entity.id().to_key()))
        .merge(db_model)
        .await
        .map_err(|e| {
            DatabaseError::QueryFailed(e).with_context(
                format!(
                    "UPDATE {} WHERE id = '{}'",
                    E::table_name(),
                    entity.id().to_key()
                ),
                E::table_name(),
            )
        })?;

    match updated {
        Some(db_model) => E::from_db_model(db_model).map_err(DatabaseError::from),
        None => Err(DatabaseError::NotFound {
            entity_type: E::table_name().to_string(),
            id: format!("{:?}", entity.id()),
        }),
    }
}

/// Delete an entity from the database
pub async fn delete_entity<E: DbEntity, C: Connection, I>(
    conn: &Surreal<C>,
    id: &E::Id,
) -> Result<()> {
    let _deleted: Option<E::DbModel> =
        conn.delete((E::table_name(), id.to_key()))
            .await
            .map_err(|e| {
                DatabaseError::QueryFailed(e).with_context(
                    format!(
                        "DELETE FROM {} WHERE id = '{}'",
                        E::table_name(),
                        id.to_key()
                    ),
                    E::table_name(),
                )
            })?;

    Ok(())
}

/// List all entities of a given type
pub async fn list_entities<E: DbEntity, C: Connection>(
    conn: &Surreal<C>,
) -> Result<Vec<E::Domain>> {
    let results: Vec<E::DbModel> = conn.select(E::table_name()).await.map_err(|e| {
        DatabaseError::QueryFailed(e).with_context(
            format!("SELECT * FROM {}", E::table_name()),
            E::table_name(),
        )
    })?;

    results
        .into_iter()
        .map(|db_model| E::from_db_model(db_model).map_err(DatabaseError::from))
        .collect()
}

/// Query entities with a WHERE clause
/// Note: This follows SurrealDB's query pattern for proper result handling
pub async fn query_entities<E: DbEntity, C: Connection>(
    conn: &Surreal<C>,
    where_clause: &str,
) -> Result<Vec<E::Domain>> {
    let query = format!("SELECT * FROM {} WHERE {}", E::table_name(), where_clause);

    let mut response = conn
        .query(&query)
        .await
        .map_err(|e| DatabaseError::QueryFailed(e).with_context(query.clone(), E::table_name()))?;

    // SurrealDB returns results wrapped in a response structure
    let results: Vec<E::DbModel> = response.take::<Vec<E::DbModel>>(0).map_err(|e| {
        DatabaseError::QueryFailed(e.into()).with_context("take results", E::table_name())
    })?;

    results
        .into_iter()
        .map(|db_model| E::from_db_model(db_model).map_err(DatabaseError::from))
        .collect()
}

// ============================================================================
// Relationship Operations using RELATE
// ============================================================================

/// Create a typed relationship between two entities using RELATE (idempotent)
pub async fn create_relation_typed<E: DbEntity, C: Connection>(
    conn: &Surreal<C>,
    edge_entity: &E,
) -> Result<E::Domain> {
    // Serialize the edge entity to get all its properties
    let db_model = E::to_db_model(edge_entity);
    let mut properties = serde_json::to_value(&db_model).map_err(DatabaseError::SerdeProblem)?;

    // Debug: print the serialized properties
    tracing::debug!(
        "create_relation_typed: table={}, properties={}",
        E::table_name(),
        serde_json::to_string_pretty(&properties)
            .unwrap_or_else(|_| "failed to serialize".to_string())
    );

    // Extract in and out from the entity
    // This assumes edge entities have in_id and out_id fields
    let obj = properties.as_object_mut().ok_or_else(|| {
        DatabaseError::QueryFailed(surrealdb::Error::Api(surrealdb::error::Api::Query(
            "Edge entity must serialize to an object".into(),
        )))
    })?;

    // Extract and remove in_id and out_id from the properties
    // These will be used in the RELATE clause, not in CONTENT
    let from_value = obj.remove("in").ok_or_else(|| {
        DatabaseError::QueryFailed(surrealdb::Error::Api(surrealdb::error::Api::Query(
            "Edge entity must have in_id field".into(),
        )))
    })?;

    let to_value = obj.remove("out").ok_or_else(|| {
        DatabaseError::QueryFailed(surrealdb::Error::Api(surrealdb::error::Api::Query(
            "Edge entity must have out_id field".into(),
        )))
    })?;

    // Remove the id field as well - SurrealDB generates this automatically for edge entities
    obj.remove("id");

    // Debug: print the raw values
    tracing::debug!("from_value: {:?}", from_value);
    tracing::debug!("to_value: {:?}", to_value);

    let from: surrealdb::RecordId = serde_json::from_value(from_value).unwrap();
    let to: surrealdb::RecordId = serde_json::from_value(to_value).unwrap();

    // Debug: print the extracted IDs
    tracing::debug!(
        "RELATE: from={}, to={}, table={}",
        from,
        to,
        E::table_name()
    );

    // Check if relation already exists
    let existing_query = format!(
        "SELECT * FROM {} WHERE in = $from AND out = $to",
        E::table_name()
    );

    let mut existing_result = conn
        .query(&existing_query)
        .bind(("from", from.clone()))
        .bind(("to", to.clone()))
        .await
        .map_err(|e| {
            DatabaseError::from(e).with_context(existing_query.clone(), E::table_name())
        })?;

    // Check if we already have this relation
    let existing: Vec<E::DbModel> = existing_result.take(0).unwrap_or_default();

    if !existing.is_empty() {
        tracing::debug!(
            "Relation already exists between {} and {} in table {}, returning existing",
            from,
            to,
            E::table_name()
        );

        // Return the existing relation
        return existing
            .into_iter()
            .next()
            .ok_or_else(|| {
                DatabaseError::QueryFailed(surrealdb::Error::Api(surrealdb::error::Api::Query(
                    "Failed to get existing relation".into(),
                )))
            })
            .and_then(|db_model| E::from_db_model(db_model).map_err(DatabaseError::from));
    }

    // Build the RELATE query
    let mut query = format!(
        "RELATE {from}->{relation_name}->{to}",
        from = from,
        relation_name = E::table_name(),
        to = to,
    );

    // Only add CONTENT if there are other properties besides in_id/out_id
    if !obj.is_empty() {
        query.push_str(" CONTENT ");
        query.push_str(&serde_json::to_string(&obj).map_err(DatabaseError::SerdeProblem)?);
    }

    let mut response = conn
        .query(query.clone())
        .await
        .map_err(|e| DatabaseError::from(e).with_context(query.clone(), E::table_name()))?;

    // Extract the created edge entity
    let created: Vec<E::DbModel> = response
        .take(0)
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    created
        .into_iter()
        .next()
        .ok_or_else(|| {
            DatabaseError::QueryFailed(surrealdb::Error::Api(surrealdb::error::Api::Query(
                "No edge entity returned from RELATE".into(),
            )))
        })
        .and_then(|db_model| E::from_db_model(db_model).map_err(DatabaseError::from))
}

/// Create a relationship between two entities using RELATE
pub async fn create_relation<C: Connection>(
    conn: &Surreal<C>,
    from: &RecordId,
    relation_name: &str,
    to: &RecordId,
    properties: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let mut query = format!(
        "RELATE {from}->{relation_name}->{to}",
        from = from,
        relation_name = relation_name,
        to = to,
    );

    let keys = properties
        .as_ref()
        .and_then(|v| v.as_object().map(|v| v.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default();

    if let Some(props) = properties {
        query.push_str(" CONTENT ");
        query.push_str(&serde_json::to_string(&props).map_err(DatabaseError::SerdeProblem)?);
    }

    let mut response = conn
        .query(query.clone())
        .await
        .map_err(|e| DatabaseError::from(e).with_context(query.clone(), relation_name))?;
    tracing::trace!("Query response: {:#?}", response);

    let mut output = json!({});

    for key in keys {
        let value: Option<serde_json::Value> = response
            .take(key.as_str())
            .map_err(|e| DatabaseError::QueryFailed(e))?;
        if let Some(value) = value {
            output[key] = value;
        }
    }

    Ok(output)
}

/// Query relationships from an entity
/// Returns the related entity IDs (not the full entities)
pub async fn query_relations_from<C: Connection>(
    conn: &Surreal<C>,
    from: &RecordId,
    relation_name: &str,
    to_table: &str,
) -> Result<Vec<RecordId>> {
    let query = format!(
        "SELECT id, ->{relation_name}->{to_table} AS related_entities FROM $parent ORDER BY id ASC",
        relation_name = relation_name,
        to_table = to_table,
    );

    let mut response = conn
        .query(query)
        .bind(("parent", from.clone()))
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    // Extract the response
    let result: Vec<serde_json::Value> = response
        .take(0)
        .map_err(|e| DatabaseError::QueryFailed(e.into()))?;

    // Parse the result structure
    if let Some(first) = result.first() {
        if let Some(related) = first.get("related_entities").and_then(|v| v.as_array()) {
            return Ok(related
                .iter()
                .filter_map(|v| serde_json::from_value::<RecordId>(v.clone()).ok())
                .collect());
        }
    }

    Ok(vec![])
}

/// Query relationships to an entity (reverse direction)
/// Returns the related entity IDs (not the full entities)
pub async fn query_relations_to<C: Connection>(
    conn: &Surreal<C>,
    to: &RecordId,
    relation_name: &str,
    from_table: &str,
) -> Result<Vec<RecordId>> {
    let query = format!(
        "SELECT id, <-{relation_name}<-{from_table} AS related_entities FROM $parent ORDER BY id ASC",
        relation_name = relation_name,
        from_table = from_table,
    );

    let mut response = conn
        .query(query)
        .bind(("parent", to.clone()))
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    // Extract the response
    let result: Vec<serde_json::Value> = response
        .take(0)
        .map_err(|e| DatabaseError::QueryFailed(e.into()))?;

    // Parse the result structure
    if let Some(first) = result.first() {
        if let Some(related) = first.get("related_entities").and_then(|v| v.as_array()) {
            return Ok(related
                .iter()
                .filter_map(|v| serde_json::from_value::<RecordId>(v.clone()).ok())
                .collect());
        }
    }

    Ok(vec![])
}

/// Delete a relationship between two entities
pub async fn delete_relation<C: Connection>(
    conn: &Surreal<C>,
    from: &RecordId,
    relation_name: &str,
    to: &RecordId,
) -> Result<()> {
    let query = format!(
        "DELETE FROM {relation_name} WHERE in = {from} AND out = {to}",
        relation_name = relation_name,
        from = from,
        to = to,
    );

    conn.query(query)
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    Ok(())
}

// ============================================================================
// Specialized Operations - Vector Search
// ============================================================================

use crate::memory::MemoryBlock;
use async_trait::async_trait;

/// Trait for embedding providers (minimal interface for vector search)
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate embedding for text
    async fn embed(
        &self,
        text: &str,
    ) -> std::result::Result<EmbeddingResult, crate::EmbeddingError>;

    /// Get the model name
    fn model_name(&self) -> &str;

    /// Embed a query (alias for embed for semantic search)
    async fn embed_query(&self, text: &str)
    -> std::result::Result<Vec<f32>, crate::EmbeddingError>;
}

/// Result of embedding generation
#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    pub vector: Vec<f32>,
}

/// Extension trait for vector search operations
pub trait VectorSearchExt<C: Connection> {
    /// Search memories by semantic similarity
    fn search_memories(
        &self,
        embeddings: &dyn EmbeddingProvider,
        agent_id: &AgentId,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<(MemoryBlock, f32)>>>;
}

impl<C: Connection> VectorSearchExt<C> for Surreal<C> {
    async fn search_memories(
        &self,
        embeddings: &dyn EmbeddingProvider,
        agent_id: &AgentId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(MemoryBlock, f32)>> {
        // Generate embedding for query
        let query_embedding = embeddings
            .embed(query)
            .await
            .map_err(|e| DatabaseError::Other(e.to_string()))?;

        // Use SurrealDB's vector search
        let query_str = format!(
            r#"
            SELECT *,
                vector::distance::knn(embedding, $query_embedding) as score
            FROM {}
            WHERE {} IN agents
            ORDER BY score DESC
            LIMIT {}
            "#,
            MemoryId::PREFIX,
            RecordId::from(agent_id),
            limit
        );

        let results: Vec<serde_json::Value> = self
            .query(&query_str)
            .bind(("query_embedding", query_embedding.vector))
            .await
            .map_err(|e| DatabaseError::QueryFailed(e))?
            .take(0)
            .map_err(|e| DatabaseError::QueryFailed(e))?;

        // Convert results
        let mut memories = Vec::new();
        for result in results {
            if let Ok(memory) = serde_json::from_value::<MemoryBlock>(result.clone()) {
                let score = result.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0) as f32;
                memories.push((memory, score));
            }
        }
        Ok(memories)
    }
}

use crate::agent_entity::AgentRecord;
use crate::groups::{
    AgentGroup, Constellation, GroupMembership, GroupState, get_next_message_position_sync,
};

/// Agent statistics for updates (simplified from pattern_core)
#[derive(Debug, Clone)]
pub struct AgentStats {
    pub total_messages: usize,
    pub total_tool_calls: usize,
    pub last_active: chrono::DateTime<chrono::Utc>,
}

// ============================================================================
// Live Query Operations - Free Functions
// ============================================================================

/// Subscribe to memory updates for a specific memory
pub async fn subscribe_to_memory_updates<C: Connection>(
    conn: &Surreal<C>,
    memory_id: &MemoryId,
) -> Result<impl Stream<Item = (Action, MemoryBlock)>> {
    let stream = conn
        .select((MemoryId::PREFIX, memory_id.to_key()))
        .live()
        .await?;

    Ok(stream.filter_map(
        |notif: surrealdb::Result<Notification<serde_json::Value>>| async move {
            match notif {
                Ok(Notification { action, data, .. }) => {
                    if let Ok(memory) = serde_json::from_value::<MemoryBlock>(data) {
                        Some((action, memory))
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        },
    ))
}

// NOTE: subscribe_to_agent_messages and mark_message_as_read removed
// They depend on message_queue::QueuedMessage which is not in this crate

/// Subscribe to all memory updates for an agent
pub async fn subscribe_to_agent_memory_updates<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
) -> Result<impl Stream<Item = (Action, MemoryBlock)>> {
    // For now, just watch all memory blocks and filter in the handler
    // TODO: Optimize this to only watch memories connected to the agent
    let query = "LIVE SELECT * FROM mem".to_string();
    let _ = agent_id;

    let mut result = conn.query(query).await?;

    let stream = result.stream::<Notification<<MemoryBlock as DbEntity>::DbModel>>(0)?;

    Ok(stream.filter_map(
        |notif: surrealdb::Result<Notification<<MemoryBlock as DbEntity>::DbModel>>| async move {
            match notif {
                Ok(Notification { action, data, .. }) => match MemoryBlock::from_db_model(data) {
                    Ok(memory) => Some((action, memory)),
                    Err(e) => {
                        tracing::error!("Failed to convert db model to MemoryBlock: {}", e);
                        None
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to receive notification: {}", e);
                    None
                }
            }
        },
    ))
}

// ============================================================================
// Specialized Operations - Memory Management
// ============================================================================

/// Attach a memory block to an agent with specific access level
pub async fn attach_memory_to_agent<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
    memory_id: &MemoryId,
    access_level: crate::memory::MemoryPermission,
) -> Result<()> {
    tracing::debug!("🔗 Attaching memory {} to agent {}", memory_id, agent_id);

    // Use RELATE to create the relationship
    let query = r#"
        RELATE $agent_id->agent_memories->$memory_id
        SET access_level = $access_level,
            created_at = time::now()
    "#;

    conn.query(query)
        .bind(("agent_id", RecordId::from(agent_id.clone())))
        .bind(("memory_id", RecordId::from(memory_id.clone())))
        .bind(("access_level", access_level))
        .await?;

    tracing::debug!("✅ Created agent-memory relation");

    // Verify the relation was created by querying for it
    let verify_query = r#"
        SELECT * FROM agent_memories
        WHERE in = $agent_id
    "#;

    let verify_result = conn
        .query(verify_query)
        .bind(("agent_id", RecordId::from(agent_id.clone())))
        .await?;

    // Just check if we got results, don't try to deserialize
    let _response = verify_result.check()?;
    tracing::debug!(
        "🔍 Verification: Agent-memory relation verified for agent {}",
        agent_id
    );

    Ok(())
}

/// Get all memories accessible to an agent
pub async fn get_agent_memories<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
) -> Result<Vec<(MemoryBlock, crate::memory::MemoryPermission)>> {
    use crate::memory::MemoryPermission;

    tracing::debug!("🔍 Looking for memories for agent {}", agent_id);

    // First, let's see ALL agent_memories relations for debugging
    let debug_query = "SELECT * FROM agent_memories";
    let debug_result = conn.query(debug_query).await?;
    tracing::debug!(
        "🔍 DEBUG: agent_memories query response: {:?}",
        debug_result
    );

    // Query the edge entities for this agent and fetch memory blocks in one query
    let query = r#"
        SELECT access_level, out.* as memory FROM agent_memories
        WHERE in = $agent_id AND out.*.is_active = true
        FETCH out
    "#;

    let mut result = conn
        .query(query)
        .bind(("agent_id", RecordId::from(agent_id)))
        .await?;

    // Define a struct to capture the query result shape
    #[derive(serde::Deserialize)]
    struct AgentMemoryRow {
        access_level: MemoryPermission,
        memory: <MemoryBlock as DbEntity>::DbModel,
    }

    // Take the results as our expected type
    let rows: Vec<AgentMemoryRow> = result.take(0)?;

    tracing::debug!(
        "Found {} agent_memories relations with fetched blocks",
        rows.len()
    );

    // Process the results
    let mut memories = Vec::new();
    let mut seen_labels = std::collections::HashSet::<compact_str::CompactString>::new();

    for row in rows {
        let memory = MemoryBlock::from_db_model(row.memory)?;

        // Deduplicate by label
        if seen_labels.insert(memory.label.clone()) {
            memories.push((memory, row.access_level));
        } else {
            tracing::debug!(
                "Skipping duplicate memory block with label: {}",
                memory.label
            );
        }
    }

    Ok(memories)
}

/// Get memory by label for an agent
pub async fn get_memory_by_label<C: Connection>(
    conn: &Surreal<C>,
    agent_id: AgentId,
    label: &str,
) -> Result<Option<MemoryBlock>> {
    // First find memory blocks accessible to this agent through the junction table
    let query = r#"
        SELECT *, out.* AS memory_data
        FROM agent_memories
        WHERE in = $agent_id
        AND out.*.label = $label
    "#;

    let mut result = conn
        .query(query)
        .bind(("agent_id", RecordId::from(agent_id)))
        .bind(("label", label.to_string()))
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    tracing::trace!("memory label query result: {:?}", result);

    let records: Vec<serde_json::Value> = result
        .take("memory_data")
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    // Extract the memory from the result
    if let Some(record) = records.into_iter().next() {
        tracing::trace!("record: {:?}", record);
        Ok(serde_json::from_value(record).ok())
    } else {
        Ok(None)
    }
}

/// Update memory content with optional re-embedding
pub async fn update_memory_content<C: Connection>(
    conn: &Surreal<C>,
    memory_id: MemoryId,
    content: String,
    embeddings: Option<&impl EmbeddingProvider>,
) -> Result<()> {
    // Generate embeddings if provider is available
    let (embedding, model_name) = if let Some(provider) = embeddings {
        let emb = provider.embed_query(&content).await.map_err(|e| {
            DatabaseError::QueryFailed(surrealdb::Error::Db(surrealdb::error::Db::Tx(format!(
                "Embedding generation failed: {}",
                e
            ))))
        })?;
        (Some(emb), Some(provider.model_name().to_string()))
    } else {
        (Some(vec![0.0; 384]), Some("none".to_string()))
    };

    let _: Option<serde_json::Value> = conn
        .update(RecordId::from(memory_id))
        .patch(PatchOp::replace("/content", content))
        .patch(PatchOp::replace("/embedding", embedding))
        .patch(PatchOp::replace("/embedding_model", model_name))
        .patch(PatchOp::replace(
            "/updated_at",
            surrealdb::Datetime::from(Utc::now()),
        ))
        .await?;

    Ok(())
}

// ============================================================================
// Agent-Message Persistence Operations
// ============================================================================

/// Persist a message and create agent-message relation
pub async fn persist_agent_message<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
    message: &Message,
    message_type: crate::message::MessageRelationType,
) -> Result<()> {
    use rand::Rng;
    use tokio::time::{Duration, sleep};

    // Check if this is a coordination message that should not be persisted
    if let Some(is_coordination) = message
        .metadata
        .custom
        .get("coordination_message")
        .and_then(|v| v.as_bool())
    {
        if is_coordination {
            tracing::debug!(
                "Skipping persistence of coordination message for agent {}: message_id={:?}",
                agent_id,
                message.id
            );
            return Ok(());
        }
    }

    tracing::debug!(
        "Persisting message for agent {}: message_id={:?}, type={:?}",
        agent_id,
        message.id,
        message_type
    );

    // Try the operation, with one retry on transaction conflict
    let mut attempt = 0;
    loop {
        attempt += 1;

        match persist_agent_message_inner(conn, &agent_id, &message, message_type).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Check if it's a transaction conflict
                if attempt < 4
                    && e.to_string()
                        .contains("Failed to commit transaction due to a read or write conflict")
                {
                    // Random backoff between 50-150ms
                    let backoff_ms = rand::thread_rng().gen_range(50..150);
                    tracing::warn!(
                        "Transaction conflict on message persist, retrying after {}ms",
                        backoff_ms
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Inner function that does the actual persistence
async fn persist_agent_message_inner<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
    message: &Message,
    message_type: crate::message::MessageRelationType,
) -> Result<()> {
    // First, store the message
    let stored_message = message.store_with_relations(conn).await?;
    tracing::debug!("Stored message with id: {:?}", stored_message.id);

    // Then create the agent-message relation with position
    let position = stored_message.position;

    let relation = crate::message::AgentMessageRelation {
        id: RelationId::nil(),
        in_id: agent_id.clone(),
        out_id: stored_message.id.clone(),
        message_type,
        position,
        added_at: Utc::now(),
        batch: message.batch.clone(),
        sequence_num: message.sequence_num,
        batch_type: message.batch_type,
    };

    tracing::debug!(
        "Creating agent_messages relation: agent={}, message={:?}, position={}",
        agent_id,
        stored_message.id,
        stored_message
            .position
            .unwrap_or_else(get_next_message_position_sync)
    );

    let created_relation = create_relation_typed(conn, &relation).await?;
    tracing::debug!("Created relation: {:?}", created_relation);

    Ok(())
}

/// Archive messages by updating their relation type to Archived
pub async fn archive_agent_messages<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
    message_ids: &[MessageId],
) -> Result<()> {
    // Update all relations for these messages to Archived type
    let query = r#"
        UPDATE agent_messages
        SET message_type = $message_type
        WHERE in = $agent_id
        AND out IN $message_ids
    "#;

    conn.query(query)
        .bind(("agent_id", RecordId::from(agent_id)))
        .bind((
            "message_ids",
            message_ids
                .iter()
                .map(|id| RecordId::from(id.clone()))
                .collect::<Vec<_>>(),
        ))
        .bind(("message_type", "archived"))
        .await?;

    Ok(())
}

/// Find a memory block by owner and label (for shared memory deduplication)
pub async fn find_memory_by_owner_and_label<C: Connection>(
    conn: &Surreal<C>,
    owner_id: &UserId,
    label: &str,
) -> Result<Option<MemoryBlock>> {
    let query = r#"
        SELECT * FROM mem
        WHERE owner_id = $owner_id
        AND label = $label
        LIMIT 1
    "#;

    let mut response = conn
        .query(query)
        .bind(("owner_id", RecordId::from(owner_id)))
        .bind(("label", label.to_string()))
        .await?;

    let memories: Vec<<MemoryBlock as DbEntity>::DbModel> = response.take(0)?;

    if let Some(db_model) = memories.into_iter().next() {
        Ok(Some(MemoryBlock::from_db_model(db_model)?))
    } else {
        Ok(None)
    }
}

/// Persist or update an agent's memory block with relation (with retry logic)
pub async fn persist_agent_memory<C: Connection>(
    conn: &Surreal<C>,
    agent_id: AgentId,
    memory: &MemoryBlock,
    access_level: crate::memory::MemoryPermission,
) -> Result<()> {
    use rand::Rng;
    use tokio::time::{Duration, sleep};

    // Try the operation, with retries on transaction conflict
    let mut attempt = 0;
    loop {
        attempt += 1;
        match persist_agent_memory_inner(conn, agent_id.clone(), memory, access_level).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Check if it's a transaction conflict
                if attempt < 4
                    && e.to_string()
                        .contains("Failed to commit transaction due to a read or write conflict")
                {
                    // Random backoff between 50-150ms
                    let backoff_ms = rand::thread_rng().gen_range(50..150);
                    tracing::warn!(
                        "Transaction conflict on memory persist for block {}, retrying after {}ms (attempt {})",
                        memory.label,
                        backoff_ms,
                        attempt
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Inner function that does the actual memory persistence
async fn persist_agent_memory_inner<C: Connection>(
    conn: &Surreal<C>,
    agent_id: AgentId,
    memory: &MemoryBlock,
    access_level: crate::memory::MemoryPermission,
) -> Result<()> {
    // Store or update the memory block
    let stored_memory = memory.store_with_relations(conn).await?;

    // Create the relation using create_relation_typed (now idempotent)
    let relation = AgentMemoryRelation {
        id: RelationId::nil(),
        in_id: agent_id,
        out_id: stored_memory.id,
        access_level,
        created_at: chrono::Utc::now(),
    };

    let _created_relation = create_relation_typed(conn, &relation).await?;
    tracing::debug!("Agent-memory relation created/confirmed");

    Ok(())
}

pub async fn update_agent_context_config<C: Connection>(
    conn: &Surreal<C>,
    agent_id: AgentId,
    config: &AgentRecord,
) -> Result<()> {
    let query = r#"
         UPDATE agent
         SET
             compression_strategy = $compression_strategy,
             compression_threshold = $compression_threshold,
             max_messages = $max_messages,
             max_message_age_hours = $max_message_age_hours,
             memory_char_limit = $memory_char_limit,
             enable_thinking = $enable_thinking,
             updated_at = $updated_at
         WHERE id = $id
     "#;

    let resp: surrealdb::Response = conn
        .query(query)
        .bind(("id", RecordId::from(agent_id)))
        .bind(("compression_strategy", config.compression_strategy.clone()))
        .bind(("compression_threshold", config.compression_threshold))
        .bind(("max_messages", config.max_messages))
        .bind(("max_message_age_hours", config.max_message_age_hours))
        .bind(("memory_char_limit", config.memory_char_limit))
        .bind(("enable_thinking", config.enable_thinking))
        .bind(("updated_at", surrealdb::Datetime::from(Utc::now())))
        .await?;

    tracing::debug!("context config updated {:?}", resp);

    Ok(())
}

/// Update only agent runtime statistics
pub async fn update_agent_stats<C: Connection>(
    conn: &Surreal<C>,
    agent_id: AgentId,
    stats: &AgentStats,
) -> Result<()> {
    // Use query builder to properly handle datetime types
    let query = r#"
        UPDATE agent
        SET
            total_messages = $total_messages,
            total_tool_calls = $total_tool_calls,
            last_active = $last_active,
            updated_at = $updated_at
        WHERE id = $id
    "#;

    let resp: surrealdb::Response = conn
        .query(query)
        .bind(("id", RecordId::from(agent_id)))
        .bind(("total_messages", stats.total_messages))
        .bind(("total_tool_calls", stats.total_tool_calls))
        .bind(("last_active", surrealdb::Datetime::from(stats.last_active)))
        .bind(("updated_at", surrealdb::Datetime::from(Utc::now())))
        .await?;

    tracing::debug!("stats updated {:?}", resp);

    Ok(())
}

/// Subscribe to agent stats updates
pub async fn subscribe_to_agent_stats<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
) -> Result<impl Stream<Item = (Action, AgentRecord)>> {
    let stream = conn
        .select((AgentId::PREFIX, agent_id.to_key()))
        .live()
        .await?;

    Ok(stream.filter_map(
        |notif: surrealdb::Result<Notification<serde_json::Value>>| async move {
            match notif {
                Ok(Notification { action, data, .. }) => {
                    if let Ok(agent) = serde_json::from_value::<AgentRecord>(data) {
                        Some((action, agent))
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        },
    ))
}

/// Load full agent state from database
pub async fn load_agent_state<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &AgentId,
) -> Result<(AgentRecord, Vec<Message>, Vec<MemoryBlock>)> {
    // Load the agent record with relations
    let agent = AgentRecord::load_with_relations(conn, agent_id)
        .await?
        .ok_or_else(|| DatabaseError::NotFound {
            entity_type: "agent".to_string(),
            id: agent_id.to_string(),
        })?;

    // Messages are loaded via the relation
    let messages: Vec<Message> = agent
        .messages
        .iter()
        .filter(|(_, rel)| rel.message_type == MessageRelationType::Active)
        .map(|(msg, _)| msg.clone())
        .collect();

    // Memory blocks are loaded via the relation
    let memories: Vec<MemoryBlock> = agent.memories.iter().map(|(mem, _)| mem.clone()).collect();

    Ok((agent, messages, memories))
}

// ============================================================================
// Message Query Operations
// ============================================================================

/// Query messages using a custom query builder function
///
/// This function executes a query and deserializes the results from the database
/// model format to the domain model format.
pub async fn query_messages<C, F>(db: &Surreal<C>, query_builder: F) -> Result<Vec<Message>>
where
    C: Connection,
    F: FnOnce(&Surreal<C>) -> String,
{
    let query = query_builder(db);
    let mut result = db.query(&query).await?;

    let db_messages: Vec<<Message as DbEntity>::DbModel> = result.take(0)?;

    let messages: Result<Vec<_>> = db_messages
        .into_iter()
        .map(|db_msg| <Message as DbEntity>::from_db_model(db_msg).map_err(|e| e.into()))
        .collect();

    messages
}

/// Query messages with a pre-built query string
///
/// This is a simpler version when you already have the query string.
pub async fn query_messages_raw<C: Connection>(
    db: &Surreal<C>,
    query: &str,
) -> Result<Vec<Message>> {
    query_messages(db, |_| query.to_string()).await
}

// ============================================================================
// Group Operations
// ============================================================================

// Constellation is imported from crate::groups above

/// Create a new agent group
pub async fn create_group<C: Connection>(
    conn: &Surreal<C>,
    group: &AgentGroup,
) -> Result<AgentGroup> {
    create_entity::<AgentGroup, _>(conn, group).await
}

/// Get a group by ID
pub async fn get_group<C: Connection>(
    conn: &Surreal<C>,
    group_id: &GroupId,
) -> Result<Option<AgentGroup>> {
    get_entity::<AgentGroup, _>(conn, group_id).await
}

/// Get a group by name for a specific constellation/user
pub async fn get_group_by_name<C: Connection>(
    conn: &Surreal<C>,
    _user_id: &UserId,
    group_name: &str,
) -> Result<Option<AgentGroup>> {
    tracing::info!("get_group_by_name: searching for group '{}'", group_name);

    // For now, just query by name directly
    // TODO: Add constellation filtering once we fix the relation queries
    let query = r#"
        SELECT * FROM group
        WHERE name = $name
        LIMIT 1
    "#;

    let mut result = conn
        .query(query)
        .bind(("name", group_name.to_string()))
        .await
        .map_err(|e| DatabaseError::from(e).with_context(query, "group"))?;

    let db_groups: Vec<<AgentGroup as DbEntity>::DbModel> =
        result.take(0).map_err(|e| DatabaseError::QueryFailed(e))?;

    tracing::info!("get_group_by_name: found {} groups", db_groups.len());

    if let Some(db_model) = db_groups.into_iter().next() {
        let mut group = AgentGroup::from_db_model(db_model)?;
        tracing::info!("Loading group with relations for group id: {:?}", group.id);

        // Manually load the group members following the pattern from get_agent_memories
        let query = r#"
            SELECT * FROM group_members
            WHERE out = $group_id
            ORDER BY joined_at ASC
        "#;

        let mut result = conn
            .query(query)
            .bind(("group_id", surrealdb::RecordId::from(&group.id)))
            .await
            .map_err(|e| DatabaseError::from(e).with_context(query, "group_members"))?;

        // Take the DB models for GroupMembership
        let membership_db_models: Vec<<GroupMembership as DbEntity>::DbModel> =
            result.take(0).map_err(DatabaseError::QueryFailed)?;

        tracing::info!("Found {} membership records", membership_db_models.len());

        // Convert DB models to domain types
        let memberships: Vec<GroupMembership> = membership_db_models
            .into_iter()
            .map(|db_model| GroupMembership::from_db_model(db_model).map_err(DatabaseError::from))
            .collect::<Result<Vec<_>>>()?;

        // Now load the agents for each membership
        let mut members = Vec::new();
        for membership in memberships {
            tracing::info!("Loading agent {:?} for group membership", membership.in_id);
            // Load the agent using the in_id (agent)
            if let Some(agent) = AgentRecord::load_with_relations(conn, &membership.in_id).await? {
                members.push((agent, membership));
            } else {
                tracing::warn!(
                    "Agent {:?} not found for group membership",
                    membership.in_id
                );
            }
        }

        group.members = members;
        tracing::info!("Group loaded with {} members", group.members.len());
        Ok(Some(group))
    } else {
        Ok(None)
    }
}

/// List all groups for a user (via their constellations)
pub async fn list_groups_for_user<C: Connection>(
    conn: &Surreal<C>,
    _user_id: &UserId,
) -> Result<Vec<AgentGroup>> {
    // For now, list all groups (user filtering TBD) and load members explicitly
    let query = r#"
        SELECT * FROM group
    "#;

    let mut result = conn
        .query(query)
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    let db_groups: Vec<<AgentGroup as DbEntity>::DbModel> =
        result.take(0).map_err(|e| DatabaseError::QueryFailed(e))?;

    // Convert DB models and then load members for each group
    let mut groups: Vec<AgentGroup> = Vec::new();
    for db_model in db_groups {
        let mut group = AgentGroup::from_db_model(db_model)?;

        // Load memberships for this group
        let query = r#"
            SELECT * FROM group_members
            WHERE out = $group_id
            ORDER BY joined_at ASC
        "#;

        let mut member_result = conn
            .query(query)
            .bind(("group_id", surrealdb::RecordId::from(&group.id)))
            .await
            .map_err(|e| DatabaseError::from(e).with_context(query, "group_members"))?;

        let membership_db_models: Vec<<GroupMembership as DbEntity>::DbModel> =
            member_result.take(0).map_err(DatabaseError::QueryFailed)?;

        let memberships: Vec<GroupMembership> = membership_db_models
            .into_iter()
            .map(|db_model| GroupMembership::from_db_model(db_model).map_err(DatabaseError::from))
            .collect::<Result<Vec<_>>>()?;

        // Load agents for each membership
        let mut members = Vec::new();
        for membership in memberships {
            if let Some(agent) = AgentRecord::load_with_relations(conn, &membership.in_id).await? {
                members.push((agent, membership));
            } else {
                tracing::warn!(
                    "Agent {:?} not found for group membership in group {:?}",
                    membership.in_id,
                    group.id
                );
            }
        }

        group.members = members;
        groups.push(group);
    }

    Ok(groups)
}

/// Add an agent to a group
pub async fn add_agent_to_group<C: Connection>(
    conn: &Surreal<C>,
    membership: &GroupMembership,
) -> Result<()> {
    create_relation_typed(conn, membership).await?;

    Ok(())
}

/// Remove an agent from a group
pub async fn remove_agent_from_group<C: Connection>(
    conn: &Surreal<C>,
    group_id: &GroupId,
    agent_id: &AgentId,
) -> Result<()> {
    let query = r#"
        DELETE $agent_id->group_members WHERE out = $group_id
    "#;

    conn.query(query)
        .bind(("agent_id", RecordId::from(agent_id)))
        .bind(("group_id", RecordId::from(group_id)))
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    Ok(())
}

/// Get all members of a group
pub async fn get_group_members<C: Connection>(
    conn: &Surreal<C>,
    group_id: &GroupId,
) -> Result<Vec<(AgentRecord, GroupMembership)>> {
    // Load the group with its relations
    let group = AgentGroup::load_with_relations(conn, &group_id)
        .await?
        .ok_or_else(|| DatabaseError::NotFound {
            entity_type: "group".to_string(),
            id: group_id.to_string(),
        })?;

    Ok(group.members)
}

/// Get all group memberships for a given agent
pub async fn get_agent_memberships<C: Connection>(
    conn: &Surreal<C>,
    agent_id: &crate::id::AgentId,
) -> Result<Vec<GroupMembership>> {
    // Query all memberships where this agent is the inbound node
    let query = r#"
        SELECT * FROM group_members
        WHERE `in` = $agent_id
        ORDER BY joined_at ASC
    "#;

    let mut result = conn
        .query(query)
        .bind(("agent_id", surrealdb::RecordId::from(agent_id)))
        .await
        .map_err(|e| DatabaseError::from(e).with_context(query, "group_members"))?;

    // Take the DB models for GroupMembership
    let membership_db_models: Vec<<GroupMembership as DbEntity>::DbModel> =
        result.take(0).map_err(DatabaseError::QueryFailed)?;

    // Convert DB models to domain types
    let memberships: Vec<GroupMembership> = membership_db_models
        .into_iter()
        .map(|db_model| GroupMembership::from_db_model(db_model).map_err(DatabaseError::from))
        .collect::<Result<Vec<_>>>()?;

    Ok(memberships)
}

/// Update group state
pub async fn update_group_state<C: Connection>(
    conn: &Surreal<C>,
    group_id: &GroupId,
    state: GroupState,
) -> Result<()> {
    let _: Option<serde_json::Value> = conn
        .update(RecordId::from(group_id))
        .patch(PatchOp::replace("/state", state))
        .patch(PatchOp::replace(
            "/updated_at",
            surrealdb::Datetime::from(Utc::now()),
        ))
        .await?;

    Ok(())
}

// ============================================================================
// Constellation Operations
// ============================================================================

use crate::id::ConstellationId;

/// Get or create a constellation for a user
pub async fn get_or_create_constellation<C: Connection>(
    conn: &Surreal<C>,
    user_id: &UserId,
) -> Result<Constellation> {
    // First try to find existing constellation
    let query = r#"
        SELECT * FROM constellation
        WHERE owner_id = $user_id
        LIMIT 1
    "#;

    let mut result = conn
        .query(query)
        .bind(("user_id", RecordId::from(user_id.clone())))
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    let constellations: Vec<<Constellation as DbEntity>::DbModel> =
        result.take(0).map_err(|e| DatabaseError::QueryFailed(e))?;

    if let Some(db_model) = constellations.into_iter().next() {
        Ok(Constellation::from_db_model(db_model)?)
    } else {
        // Create new constellation
        let constellation = Constellation {
            id: ConstellationId::generate(),
            owner_id: user_id.clone(),
            name: "Default Constellation".to_string(),
            description: Some("Primary agent constellation".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            agents: vec![],
            groups: vec![],
        };

        create_entity::<Constellation, _>(conn, &constellation).await
    }
}

/// Add a group to a constellation
pub async fn add_group_to_constellation<C: Connection>(
    conn: &Surreal<C>,
    constellation_id: &ConstellationId,
    group_id: &GroupId,
) -> Result<()> {
    // Create the RELATE edge
    let query = r#"
        RELATE $constellation_id->composed_of->$group_id
    "#;

    conn.query(query)
        .bind(("constellation_id", RecordId::from(constellation_id)))
        .bind(("group_id", RecordId::from(group_id)))
        .await
        .map_err(|e| DatabaseError::QueryFailed(e))?;

    Ok(())
}

/// Create a group for a user (handles constellation)
pub async fn create_group_for_user<C: Connection>(
    conn: &Surreal<C>,
    user_id: &UserId,
    group: &AgentGroup,
) -> Result<AgentGroup> {
    // Get or create constellation
    let constellation = get_or_create_constellation(conn, user_id).await?;

    // Create the group
    let created_group = create_group(conn, group).await?;

    // Add group to constellation
    add_group_to_constellation(conn, &constellation.id, &created_group.id).await?;

    Ok(created_group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_entity::AgentRecord;
    use crate::db::{
        client,
        entity::{BaseTask, BaseTaskPriority, BaseTaskStatus},
    };
    use crate::groups::AgentType;
    use crate::id::{TaskId, UserId};
    use crate::users::User;

    #[tokio::test]
    async fn test_create_relation_with_properties() {
        let db = client::create_test_db().await.unwrap();

        // Create a user and agent first
        let user = User {
            id: UserId::generate(),
            ..Default::default()
        };
        let agent = AgentRecord {
            id: AgentId::generate(),
            name: "Test Agent".to_string(),
            agent_type: AgentType::Generic,
            owner_id: user.id.clone(),
            ..Default::default()
        };

        let created_user = create_entity::<User, _>(&db, &user).await.unwrap();
        let created_agent = create_entity::<AgentRecord, _>(&db, &agent).await.unwrap();

        // Test creating a relation with properties
        // Using string for access_level since it's an enum that serializes to lowercase strings
        let properties = serde_json::json!({
            "access_level": "write",
            "created_at": chrono::Utc::now(),
            "priority": 5
        });

        let relation = create_relation(
            &db,
            &RecordId::from(&created_user.id),
            "manages",
            &RecordId::from(&created_agent.id),
            Some(properties.clone()),
        )
        .await
        .unwrap();

        println!("Created relation: {:?}", relation);

        // Verify the relation was created with properties
        let query = r#"
            SELECT * FROM manages
            WHERE in = $user_id AND out = $agent_id
        "#;

        let mut result = db
            .query(query)
            .bind(("user_id", RecordId::from(&created_user.id)))
            .bind(("agent_id", RecordId::from(&created_agent.id)))
            .await
            .unwrap();
        println!("result: {:?}", result);
        // Extract the fields we care about directly using take() with field paths
        let access_levels: Vec<String> = result.take("access_level").unwrap();
        let priorities: Vec<i64> = result.take("priority").unwrap();

        // Combine into relation objects
        let relations: Vec<serde_json::Value> = access_levels
            .into_iter()
            .zip(priorities.into_iter())
            .map(|(access_level, priority)| {
                serde_json::json!({
                    "access_level": access_level,
                    "priority": priority
                })
            })
            .collect();
        assert_eq!(relations.len(), 1);

        let rel = &relations[0];
        assert_eq!(rel["access_level"], "write");
        assert_eq!(rel["priority"], 5);
    }

    #[tokio::test]
    async fn test_entity_operations() {
        // Initialize the database
        let db = client::create_test_db().await.unwrap();

        // Create a user using generic operations
        let user = User {
            id: UserId::generate(),
            discord_id: Some("123456789".to_string()),
            ..Default::default()
        };

        let created_user = create_entity::<User, _>(&db, &user).await.unwrap();
        assert_eq!(created_user.id, user.id);

        // Get the user by ID
        let retrieved_user = get_entity::<User, _>(&db, &user.id).await.unwrap();
        assert!(retrieved_user.is_some());
        assert_eq!(retrieved_user.unwrap().id, user.id);

        // Create an agent
        let agent = AgentRecord {
            id: AgentId::generate(),
            name: "Test Agent".to_string(),
            agent_type: AgentType::Generic,
            model_id: None,
            owner_id: user.id.clone(),
            ..Default::default()
        };

        let _created_agent = create_entity::<AgentRecord, _>(&db, &agent).await.unwrap();

        // Create ownership relationship using entity system
        let mut user_with_agent = user.clone();
        user_with_agent.owned_agent_ids = vec![agent.id.clone()];
        user_with_agent.store_relations(&db).await.unwrap();

        // Get agents for the user using entity system
        let retrieved_user = User::load_with_relations(&db, &user.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!retrieved_user.owned_agent_ids.is_empty());

        // Create a task
        let task = BaseTask {
            id: TaskId::generate(),
            title: "Test Task".to_string(),
            description: Some("A test task".to_string()),
            status: BaseTaskStatus::Pending,
            priority: BaseTaskPriority::Medium,
            ..Default::default()
        };

        let _created_task = create_entity::<BaseTask, _>(&db, &task).await.unwrap();

        // Create relationships using entity system
        let mut user_with_task = retrieved_user;
        user_with_task.created_task_ids = vec![task.id.clone()];
        user_with_task.store_relations(&db).await.unwrap();

        let mut agent_with_task = agent.clone();
        agent_with_task.assigned_task_ids = vec![task.id.clone()];
        agent_with_task.store_relations(&db).await.unwrap();

        // Query relationships using entity system
        let final_user = User::load_with_relations(&db, &user.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!final_user.created_task_ids.is_empty());

        let final_agent = AgentRecord::load_with_relations(&db, &agent.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!final_agent.assigned_task_ids.is_empty());
    }

    #[tokio::test]
    async fn test_simple_user_creation() {
        // Initialize the database
        let db = client::create_test_db().await.unwrap();

        // Create a simple user
        let user = User {
            id: UserId::generate(),
            ..Default::default()
        };

        // Debug: print what we're about to create
        println!("Creating user: {:?}", user);

        // Try to create the user
        let created_user = create_entity::<User, _>(&db, &user).await.unwrap();
        assert_eq!(created_user.id, user.id);
    }
}
