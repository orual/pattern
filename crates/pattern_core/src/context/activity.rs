//! Activity logging and rendering for agents
//!
//! This module provides:
//! - `ActivityLogger`: Thin wrapper for logging activity events to the database
//! - `ActivityRenderer`: Renders recent activity as a system prompt section with attribution

use std::collections::HashMap;
use std::sync::Arc;

use crate::db::ConstellationDatabases;
use chrono::{Duration, Utc};
use pattern_db::queries::{
    create_activity_event, get_agent, get_agent_activity, get_recent_activity_since,
};
use pattern_db::{ActivityEvent, ActivityEventType, EventImportance};
use serde_json::json;

/// Error type for activity operations
#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("Database error: {0}")]
    Database(#[from] pattern_db::DbError),
}

pub type ActivityResult<T> = Result<T, ActivityError>;

/// Activity logger for an agent
pub struct ActivityLogger {
    dbs: Arc<ConstellationDatabases>,
    agent_id: String,
}

impl ActivityLogger {
    pub fn new(dbs: Arc<ConstellationDatabases>, agent_id: impl Into<String>) -> Self {
        Self {
            dbs,
            agent_id: agent_id.into(),
        }
    }

    /// Log an activity event
    pub async fn log(
        &self,
        event_type: ActivityEventType,
        details: serde_json::Value,
        importance: EventImportance,
    ) -> ActivityResult<String> {
        let id = format!("evt_{}", uuid::Uuid::new_v4());

        let event = ActivityEvent {
            id: id.clone(),
            timestamp: Utc::now(),
            agent_id: Some(self.agent_id.clone()),
            event_type,
            details: sqlx::types::Json(details),
            importance: Some(importance),
        };

        create_activity_event(self.dbs.constellation.pool(), &event).await?;
        Ok(id)
    }

    /// Get recent activity for this agent
    pub async fn recent(&self, limit: i64) -> ActivityResult<Vec<ActivityEvent>> {
        let events =
            get_agent_activity(self.dbs.constellation.pool(), &self.agent_id, limit).await?;
        Ok(events)
    }

    /// Render recent activity as text for context inclusion
    pub async fn render_recent(&self, limit: i64) -> ActivityResult<String> {
        let events = self.recent(limit).await?;

        let lines: Vec<String> = events
            .iter()
            .map(|e| {
                let ts = e.timestamp.format("%Y-%m-%d %H:%M");
                let agent = e.agent_id.as_deref().unwrap_or("system");
                format!("[{}] {:?} by {}", ts, e.event_type, agent)
            })
            .collect();

        Ok(lines.join("\n"))
    }
}

// Convenience methods
impl ActivityLogger {
    pub async fn log_message_sent(&self, preview: &str) -> ActivityResult<String> {
        self.log(
            ActivityEventType::MessageSent,
            json!({"preview": preview}),
            EventImportance::Medium,
        )
        .await
    }

    pub async fn log_tool_used(&self, tool_name: &str, success: bool) -> ActivityResult<String> {
        self.log(
            ActivityEventType::ToolUsed,
            json!({"tool": tool_name, "success": success}),
            EventImportance::Low,
        )
        .await
    }

    pub async fn log_memory_updated(&self, label: &str, operation: &str) -> ActivityResult<String> {
        self.log(
            ActivityEventType::MemoryUpdated,
            json!({"label": label, "operation": operation}),
            EventImportance::Medium,
        )
        .await
    }
}

// ============================================================================
// Activity Renderer
// ============================================================================

/// Configuration for activity rendering
#[derive(Debug, Clone)]
pub struct ActivityConfig {
    /// Maximum number of events to include
    pub max_events: usize,
    /// Maximum number of the agent's OWN events to include (deprioritizes self)
    pub max_self_events: usize,
    /// Minimum importance level to include (currently unused, for future filtering)
    pub min_importance: EventImportance,
    /// How far back to look for events (in hours)
    pub lookback_hours: u32,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            max_events: 20,
            max_self_events: 3,
            min_importance: EventImportance::Low,
            lookback_hours: 24,
        }
    }
}

/// Renders activity events for system prompt inclusion.
///
/// Unlike `ActivityLogger` which writes events, `ActivityRenderer` reads and
/// formats events for display in an agent's system prompt, with clear attribution
/// of who did what.
///
/// Events are kept in chronological order. The agent's own activity is deprioritized
/// by limiting how many self-events are included (controlled by `max_self_events`),
/// while other agents' events are not limited. This keeps the timeline coherent
/// while still reducing the visibility of the agent's own actions.
pub struct ActivityRenderer {
    dbs: Arc<ConstellationDatabases>,
    config: ActivityConfig,
}

impl ActivityRenderer {
    /// Create a new ActivityRenderer with the given database and configuration.
    pub fn new(dbs: Arc<ConstellationDatabases>, config: ActivityConfig) -> Self {
        Self { dbs, config }
    }

    /// Render recent activity as a system prompt section.
    ///
    /// Returns a formatted string showing recent constellation activity with
    /// attribution markers like [AGENT:Name], [YOU], or [SYSTEM].
    ///
    /// Events are kept in chronological order. The agent's own events are
    /// limited to `max_self_events` to deprioritize self-activity while
    /// maintaining a coherent timeline.
    pub async fn render_for_agent(&self, agent_id: &str) -> ActivityResult<String> {
        let since = Utc::now() - Duration::hours(self.config.lookback_hours as i64);

        let events = get_recent_activity_since(
            self.dbs.constellation.pool(),
            since,
            self.config.max_events as i64,
        )
        .await?;

        if events.is_empty() {
            return Ok(String::new());
        }

        // Build agent name cache for all unique agent IDs
        let agent_names = self.build_agent_name_cache(&events).await;

        let mut output = String::from("<constellation_activity>\n");
        output.push_str("The following events occurred recently in the constellation:\n\n");

        for event in &events {
            let attribution = self.format_attribution(event, agent_id, &agent_names);
            let description = self.format_event(event);
            let timestamp = event.timestamp.format("%H:%M");

            output.push_str(&format!(
                "[{}] {}: {}\n",
                timestamp, attribution, description
            ));
        }
        output.push_str("\n</constellation_activity>");

        Ok(output)
    }

    /// Build a cache of agent ID -> agent name mappings.
    async fn build_agent_name_cache(&self, events: &[ActivityEvent]) -> HashMap<String, String> {
        let mut name_cache = HashMap::new();

        // Collect all unique agent IDs
        let agent_ids: std::collections::HashSet<_> =
            events.iter().filter_map(|e| e.agent_id.as_ref()).collect();

        // Look up each agent's name
        for agent_id in agent_ids {
            if let Ok(Some(agent)) = get_agent(self.dbs.constellation.pool(), agent_id).await {
                name_cache.insert(agent_id.clone(), agent.name);
            }
        }

        name_cache
    }

    /// Format attribution for an event.
    fn format_attribution(
        &self,
        event: &ActivityEvent,
        current_agent_id: &str,
        agent_names: &HashMap<String, String>,
    ) -> String {
        match &event.agent_id {
            Some(aid) if aid == current_agent_id => "[YOU]".to_string(),
            Some(aid) => {
                // Try to get the agent name, fall back to ID
                let display_name = agent_names.get(aid).map(|s| s.as_str()).unwrap_or(aid);
                format!("[AGENT:{}]", display_name)
            }
            None => "[SYSTEM]".to_string(),
        }
    }

    /// Format an event into a human-readable description.
    fn format_event(&self, event: &ActivityEvent) -> String {
        match event.event_type {
            ActivityEventType::MessageSent => "sent a message".to_string(),
            ActivityEventType::ToolUsed => {
                let tool = event
                    .details
                    .get("tool")
                    .or_else(|| event.details.get("tool_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("used tool '{}'", tool)
            }
            ActivityEventType::MemoryUpdated => {
                let label = event
                    .details
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("updated memory '{}'", label)
            }
            ActivityEventType::TaskChanged => "task status changed".to_string(),
            ActivityEventType::AgentStatusChanged => "status changed".to_string(),
            ActivityEventType::ExternalEvent => {
                let source = event
                    .details
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("external");
                format!("external event from {}", source)
            }
            ActivityEventType::Coordination => "coordination event".to_string(),
            ActivityEventType::System => "system event".to_string(),
        }
    }
}
