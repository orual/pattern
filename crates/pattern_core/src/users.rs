use crate::id::{AgentId, EventId, MemoryId, TaskId, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// User model with entity support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Unique identifier for this user
    pub id: UserId,

    /// Discord user ID if this user is linked to Discord
    pub discord_id: Option<String>,

    /// When this user was created
    pub created_at: DateTime<Utc>,

    /// When this user was last updated
    pub updated_at: DateTime<Utc>,

    /// User-specific settings (e.g., preferences, notification settings)
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,

    /// Additional metadata about the user (e.g., source, tags)
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,

    // Relations
    pub owned_agent_ids: Vec<AgentId>,

    pub created_task_ids: Vec<TaskId>,

    pub memory_ids: Vec<MemoryId>,

    pub scheduled_event_ids: Vec<EventId>,
}

impl Default for User {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            id: UserId::generate(),
            discord_id: None,
            created_at: now,
            updated_at: now,
            settings: HashMap::new(),
            metadata: HashMap::new(),
            owned_agent_ids: Vec::new(),
            created_task_ids: Vec::new(),
            memory_ids: Vec::new(),
            scheduled_event_ids: Vec::new(),
        }
    }
}
