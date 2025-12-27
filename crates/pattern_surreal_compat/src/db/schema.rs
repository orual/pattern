//! Database schema definitions for Pattern

use pattern_macros::Entity;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, ToolCallId};

/// SQL schema definitions for the database
pub struct Schema;

impl Schema {
    /// Get all table definitions
    /// Note: Entity tables (user, agent, task, etc.) are defined by their Entity implementations
    /// This only includes auxiliary tables not managed by the entity system
    pub fn tables() -> Vec<TableDefinition> {
        vec![
            Self::system_metadata(),
            // Messages and ToolCalls now use Entity derive, so they're created via Entity::schema()
        ]
    }

    /// System metadata table
    pub fn system_metadata() -> TableDefinition {
        TableDefinition {
            name: "system_metadata".to_string(),
            schema: "
                DEFINE TABLE system_metadata SCHEMALESS;
                DEFINE FIELD embedding_model ON system_metadata TYPE string;
                DEFINE FIELD embedding_dimensions ON system_metadata TYPE int;
                DEFINE FIELD schema_version ON system_metadata TYPE int;
                DEFINE FIELD created_at ON system_metadata TYPE datetime;
                DEFINE FIELD updated_at ON system_metadata TYPE datetime;
            "
            .to_string(),
            indexes: vec![],
        }
    }

    /// Create vector index query for a table
    pub fn vector_index(table: &str, field: &str, dimensions: usize) -> String {
        format!(
            "DEFINE INDEX OVERWRITE {}_vector_idx ON {} FIELDS {} HNSW DIMENSION {} DIST COSINE",
            table, table, field, dimensions
        )
    }
}

/// Table definition with schema and indexes
#[derive(Debug, Clone)]
pub struct TableDefinition {
    /// Name of the database table
    pub name: String,
    /// SQL schema definition for creating the table
    pub schema: String,
    /// SQL statements for creating indexes on this table
    pub indexes: Vec<String>,
}

/// Database models
/// Note: Primary entities (User, Agent, Task, etc.) are defined in entity/base.rs
/// using the Entity derive macro system

/// Tool call model
#[derive(Debug, Clone, Entity, Serialize, Deserialize)]
#[entity(entity_type = "tool_call")]
pub struct ToolCall {
    /// Unique identifier for this tool call
    pub id: ToolCallId,

    /// The agent that executed this tool
    pub agent_id: AgentId,
    /// Name of the tool that was called
    pub tool_name: String,
    /// Parameters passed to the tool (as JSON)
    #[entity(db_type = "object")]
    pub parameters: serde_json::Value,
    /// Result returned by the tool (as JSON)
    #[entity(db_type = "object")]
    pub result: serde_json::Value,
    /// Error message if the tool call failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long the tool execution took in milliseconds
    pub duration_ms: i64,

    /// When this tool was called
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Energy level required for a task (used in pattern-nd)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EnergyLevel {
    Low,
    #[default]
    Medium,
    High,
}
