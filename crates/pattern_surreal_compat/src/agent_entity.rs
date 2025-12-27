//! Agent entity definition for database persistence
//!
//! This module defines the Agent struct that represents the persistent state
//! of a DatabaseAgent. It includes all fields that need to be stored in the
//! database and can be used to reconstruct a DatabaseAgent instance.

use crate::config::ToolRuleConfig;
pub use crate::db::entity::AgentMemoryRelation;
use crate::groups::{AgentType, CompressionStrategy, SnowflakePosition, get_position_generator};
use crate::id::{AgentId, EventId, RelationId, TaskId, UserId};
use crate::memory::MemoryBlock;
use crate::message::{AgentMessageRelation, Message};
use chrono::{DateTime, Utc};
use pattern_macros::Entity;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Agent entity that persists to the database
///
/// This struct contains all the state needed to reconstruct a DatabaseAgent.
/// The runtime components (model provider, tools, etc.) are injected when
/// creating the DatabaseAgent from this persisted state.
#[derive(Debug, Clone, Entity, Serialize, Deserialize)]
#[entity(entity_type = "agent")]
pub struct AgentRecord {
    pub id: AgentId,
    pub name: String,
    pub agent_type: AgentType,

    // Model configuration
    pub model_id: Option<String>,
    pub model_config: HashMap<String, serde_json::Value>,

    // Context configuration that gets persisted
    pub base_instructions: String,
    pub max_messages: usize,
    pub max_message_age_hours: i64,
    pub compression_threshold: usize,
    pub memory_char_limit: usize,
    pub enable_thinking: bool,
    #[entity(db_type = "object")]
    pub compression_strategy: CompressionStrategy,

    // Tool execution rules for this agent (serialized as JSON)
    #[serde(default)]
    pub tool_rules: Vec<ToolRuleConfig>,

    // Runtime statistics
    pub total_messages: usize,
    pub total_tool_calls: usize,
    pub context_rebuilds: usize,
    pub compression_events: usize,

    // Timestamps
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,

    // Relations (using Entity macro features)
    #[entity(relation = "owns", reverse = true)]
    pub owner_id: UserId,

    #[entity(relation = "assigned")]
    pub assigned_task_ids: Vec<TaskId>,

    #[entity(edge_entity = "agent_memories")]
    pub memories: Vec<(MemoryBlock, AgentMemoryRelation)>,

    #[entity(edge_entity = "agent_messages")]
    pub messages: Vec<(Message, AgentMessageRelation)>,

    // Optional summary of archived messages for context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_summary: Option<String>,

    #[entity(relation = "scheduled")]
    pub scheduled_event_ids: Vec<EventId>,
}

impl Default for AgentRecord {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            id: AgentId::generate(),
            name: String::new(),
            agent_type: AgentType::Generic,
            model_id: None,
            model_config: HashMap::new(),
            base_instructions: String::new(),
            max_messages: 50,
            max_message_age_hours: 24,
            compression_threshold: 30,
            memory_char_limit: 5000,
            enable_thinking: true,
            compression_strategy: CompressionStrategy::Truncate { keep_recent: 100 },
            tool_rules: Vec::new(),
            total_messages: 0,
            total_tool_calls: 0,
            context_rebuilds: 0,
            compression_events: 0,
            created_at: now,
            updated_at: now,
            last_active: now,
            owner_id: UserId::nil(),
            assigned_task_ids: Vec::new(),
            memories: Vec::new(),
            messages: Vec::new(),
            message_summary: None,
            scheduled_event_ids: Vec::new(),
        }
    }
}

/// Extension methods for AgentRecord
impl AgentRecord {
    /// Store agent with relations individually to avoid payload size limits
    pub async fn store_with_relations_individually<C>(
        &self,
        db: &surrealdb::Surreal<C>,
    ) -> Result<Self, crate::db::DatabaseError>
    where
        C: surrealdb::Connection + Clone,
    {
        use crate::db::ops::create_relation_typed;

        // First store the agent itself without relations
        let mut agent_only = self.clone();
        agent_only.memories.clear();
        agent_only.messages.clear();
        agent_only.assigned_task_ids.clear();
        agent_only.scheduled_event_ids.clear();

        let stored_agent = agent_only.store_with_relations(db).await?;

        // Store each memory and its relation individually
        if !self.memories.is_empty() {
            tracing::info!("Storing {} memory blocks...", self.memories.len());
            for (i, (memory, relation)) in self.memories.iter().enumerate() {
                memory.store_with_relations(db).await?;
                create_relation_typed(db, relation).await?;

                let stored = i + 1;
                tracing::info!("  Stored {}/{} memories", stored, self.memories.len());
            }
            tracing::info!("Finished storing {} memories", self.memories.len());
        }

        // Store each message and its relation individually
        if !self.messages.is_empty() {
            tracing::info!("Storing {} messages...", self.messages.len());
            for (i, (message, relation)) in self.messages.iter().enumerate() {
                message.store_with_relations(db).await?;
                create_relation_typed(db, relation).await?;

                let stored = i + 1;
                if stored % 100 == 0 || stored == self.messages.len() {
                    tracing::info!("  Stored {}/{} messages", stored, self.messages.len());
                }
            }
            tracing::info!("Finished storing {} messages", self.messages.len());
        }

        Ok(stored_agent)
    }

    /// Update the agent from runtime state
    pub fn update_from_runtime(
        &mut self,
        total_messages: usize,
        total_tool_calls: usize,
        context_rebuilds: usize,
        compression_events: usize,
    ) {
        self.total_messages = total_messages;
        self.total_tool_calls = total_tool_calls;
        self.context_rebuilds = context_rebuilds;
        self.compression_events = compression_events;
        self.last_active = Utc::now();
        self.updated_at = Utc::now();
    }

    /// Load the agent's message history (active and/or archived)
    pub async fn load_message_history<C: surrealdb::Connection>(
        &self,
        db: &surrealdb::Surreal<C>,
        include_archived: bool,
    ) -> Result<Vec<(Message, AgentMessageRelation)>, crate::db::DatabaseError> {
        use crate::db::entity::DbEntity;

        let query = if include_archived {
            r#"SELECT *, out.position as snowflake, batch, sequence_num, out.created_at AS msg_created FROM agent_messages
               WHERE in = $agent_id AND batch IS NOT NULL
               ORDER BY batch NUMERIC ASC, sequence_num NUMERIC ASC, snowflake NUMERIC ASC, msg_created ASC"#
        } else {
            r#"SELECT *, out.position as snowflake, batch, sequence_num, message_type, out.created_at AS msg_created FROM agent_messages
               WHERE in = $agent_id AND message_type = 'active' AND batch IS NOT NULL
               ORDER BY batch NUMERIC ASC, sequence_num NUMERIC ASC, snowflake NUMERIC ASC, msg_created ASC"#
        };

        tracing::debug!(
            "Loading message history for agent {}: query={}",
            self.id,
            query
        );

        let mut result = db
            .query(query)
            .bind(("agent_id", surrealdb::RecordId::from(&self.id)))
            .await
            .map_err(crate::db::DatabaseError::QueryFailed)?;

        let relation_db_models: Vec<<AgentMessageRelation as DbEntity>::DbModel> =
            result
                .take(0)
                .map_err(crate::db::DatabaseError::QueryFailed)?;

        tracing::debug!(
            "Found {} agent_messages relations for agent {}",
            relation_db_models.len(),
            self.id
        );

        let relations: Vec<AgentMessageRelation> = relation_db_models
            .into_iter()
            .map(|db_model| {
                AgentMessageRelation::from_db_model(db_model)
                    .map_err(crate::db::DatabaseError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut messages_with_relations = Vec::new();

        for relation in relations {
            if let Some(message) = Message::load_with_relations(db, &relation.out_id).await? {
                messages_with_relations.push((message, relation));
            } else {
                tracing::warn!("Message {:?} not found in database", relation.out_id);
            }
        }

        tracing::debug!(
            "Total messages loaded for agent {}: {}",
            self.id,
            messages_with_relations.len()
        );

        Ok(messages_with_relations)
    }

    /// Attach a message to this agent with the specified relationship type
    pub async fn attach_message<C: surrealdb::Connection>(
        &self,
        db: &surrealdb::Surreal<C>,
        message_id: &crate::MessageId,
        message_type: crate::message::MessageRelationType,
    ) -> Result<(), crate::db::DatabaseError> {
        use crate::db::ops::create_relation_typed;
        use ferroid::SnowflakeGeneratorAsyncTokioExt;

        let position = get_position_generator()
            .try_next_id_async()
            .await
            .expect("snowflake generation should succeed");

        let (batch, sequence_num, batch_type) =
            if let Some(msg) = Message::load_with_relations(db, message_id).await? {
                (msg.batch, msg.sequence_num, msg.batch_type)
            } else {
                (None, None, None)
            };

        let relation = AgentMessageRelation {
            id: RelationId::nil(),
            in_id: self.id.clone(),
            out_id: message_id.clone(),
            message_type,
            position: Some(SnowflakePosition::new(position)),
            added_at: chrono::Utc::now(),
            batch,
            sequence_num,
            batch_type,
        };

        create_relation_typed(db, &relation).await?;

        Ok(())
    }
}
