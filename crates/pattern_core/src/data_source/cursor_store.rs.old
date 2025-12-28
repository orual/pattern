//! Database storage for data source cursors

use crate::Result;
use crate::id::AgentId;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;

/// Stored cursor for resuming data sources
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceCursorRecord {
    /// Agent this cursor belongs to
    pub agent_id: AgentId,

    /// Data source ID
    pub source_id: String,

    /// Serialized cursor data (format depends on source type)
    pub cursor_data: serde_json::Value,

    /// Source type (e.g., "bluesky_firehose", "file_watcher")
    pub source_type: String,

    /// Last update time
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl DataSourceCursorRecord {
    /// Create a new cursor entry
    pub fn new(
        agent_id: AgentId,
        source_id: String,
        source_type: String,
        cursor_data: serde_json::Value,
    ) -> Self {
        Self {
            agent_id,
            source_id,
            cursor_data,
            source_type,
            updated_at: chrono::Utc::now(),
        }
    }

    /// Create record ID for this cursor
    fn record_id(agent_id: &AgentId, source_id: &str) -> String {
        format!(
            "data_source_cursor:{}_{}",
            agent_id.0,
            source_id.replace(':', "_")
        )
    }

    /// Load cursor from database
    pub async fn load<C>(
        db: &Surreal<C>,
        agent_id: &AgentId,
        source_id: &str,
    ) -> Result<Option<Self>>
    where
        C: surrealdb::Connection,
    {
        let id = Self::record_id(agent_id, source_id);
        let id_part = id.split(':').nth(1).unwrap_or("");
        let result: Option<Self> = db
            .select(("data_source_cursor", id_part))
            .await
            .map_err(|e| crate::db_v1::DatabaseError::from(e))?;
        Ok(result)
    }

    /// Save or update cursor in database
    pub async fn save<C>(&self, db: &Surreal<C>) -> Result<()>
    where
        C: surrealdb::Connection,
    {
        let id = Self::record_id(&self.agent_id, &self.source_id);
        let id_part = id.split(':').nth(1).unwrap_or("");
        let _: Option<Self> = db
            .update(("data_source_cursor", id_part))
            .content(self.clone())
            .await
            .map_err(|e| crate::db_v1::DatabaseError::from(e))?;
        Ok(())
    }
}
