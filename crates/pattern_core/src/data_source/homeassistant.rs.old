use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use compact_str::CompactString;
use futures::{SinkExt, Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

use crate::error::Result;
use crate::memory::{MemoryBlock, MemoryPermission, MemoryType};
use crate::{MemoryId, UserId};

use super::BufferConfig;
use super::traits::{DataSource, DataSourceMetadata, DataSourceStatus, StreamEvent};

/// HomeAssistant data source for real-time entity state tracking
pub struct HomeAssistantSource {
    /// Base URL of HomeAssistant instance (e.g., http://homeassistant.local:8123)
    base_url: Url,
    /// Long-lived access token for authentication
    access_token: String,
    /// Unique identifier for this source
    source_id: String,
    /// Current cursor position
    current_cursor: Option<HomeAssistantCursor>,
    /// Filter configuration
    filter: HomeAssistantFilter,
    /// Source metadata
    metadata: RwLock<DataSourceMetadata>,
    /// Whether notifications are enabled
    notifications_enabled: bool,
    /// WebSocket connection (when subscribed)
    ws_connection: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
}

/// Cursor for tracking position in HomeAssistant event stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAssistantCursor {
    /// Last event timestamp
    pub timestamp: DateTime<Utc>,
    /// Last event ID (if available)
    pub event_id: Option<String>,
}

/// Filter for HomeAssistant entities and events
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HomeAssistantFilter {
    /// Entity domains to include (e.g., "light", "sensor", "switch")
    pub domains: Option<Vec<String>>,
    /// Specific entity IDs to track
    pub entity_ids: Option<Vec<String>>,
    /// Event types to subscribe to (e.g., "state_changed", "call_service")
    pub event_types: Option<Vec<String>>,
    /// Areas/rooms to include
    pub areas: Option<Vec<String>>,
    /// Minimum time between updates for the same entity (rate limiting)
    pub min_update_interval: Option<Duration>,
}

/// HomeAssistant entity state or event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAssistantItem {
    /// Entity ID (e.g., "light.living_room")
    pub entity_id: String,
    /// Current state value
    pub state: String,
    /// Entity attributes
    pub attributes: HashMap<String, Value>,
    /// Last changed timestamp
    pub last_changed: DateTime<Utc>,
    /// Last updated timestamp
    pub last_updated: DateTime<Utc>,
    /// Friendly name
    pub friendly_name: Option<String>,
    /// Entity domain (extracted from entity_id)
    pub domain: String,
    /// Area/room assignment
    pub area: Option<String>,
    /// Event type if this is from an event
    pub event_type: Option<String>,
}

impl HomeAssistantSource {
    pub fn new(base_url: Url, access_token: String) -> Self {
        let source_id = format!("homeassistant:{}", base_url.host_str().unwrap_or("unknown"));

        let metadata = DataSourceMetadata {
            source_type: "homeassistant".to_string(),
            status: DataSourceStatus::Disconnected,
            items_processed: 0,
            last_item_time: None,
            error_count: 0,
            custom: HashMap::new(),
        };

        Self {
            base_url,
            access_token,
            source_id,
            current_cursor: None,
            filter: HomeAssistantFilter::default(),
            metadata: RwLock::new(metadata),
            notifications_enabled: true,
            ws_connection: None,
        }
    }

    /// Fetch all current entity states via REST API
    async fn fetch_states(&self) -> Result<Vec<HomeAssistantItem>> {
        let client = Client::new();
        let url = format!("{}/api/states", self.base_url);

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| {
                crate::CoreError::tool_exec_error("homeassistant_fetch", json!({ "url": url }), e)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(crate::CoreError::tool_exec_msg(
                "homeassistant_fetch",
                json!({ "url": url, "status": status.as_u16() }),
                format!("API request failed: {} - {}", status, text),
            ));
        }

        let states: Vec<Value> = response.json().await.map_err(|e| {
            crate::CoreError::tool_exec_error("homeassistant_fetch", json!({ "url": url }), e)
        })?;

        let mut items = Vec::new();
        for state in states {
            if let Some(item) = self.parse_state_object(state) {
                // Apply filters
                if self.should_include_item(&item) {
                    items.push(item);
                }
            }
        }

        Ok(items)
    }

    /// Parse a state object from the API into our item format
    fn parse_state_object(&self, state: Value) -> Option<HomeAssistantItem> {
        let entity_id = state["entity_id"].as_str()?.to_string();
        let state_value = state["state"].as_str()?.to_string();

        // Extract domain from entity_id (e.g., "light" from "light.living_room")
        let domain = entity_id.split('.').next()?.to_string();

        let attributes: HashMap<String, Value> = state["attributes"]
            .as_object()
            .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let friendly_name = attributes
            .get("friendly_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let area = attributes
            .get("area")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let last_changed = state["last_changed"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let last_updated = state["last_updated"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        Some(HomeAssistantItem {
            entity_id,
            state: state_value,
            attributes,
            last_changed,
            last_updated,
            friendly_name,
            domain,
            area,
            event_type: None,
        })
    }

    /// Check if an item passes our filters
    fn should_include_item(&self, item: &HomeAssistantItem) -> bool {
        // Check domain filter
        if let Some(domains) = &self.filter.domains {
            if !domains.contains(&item.domain) {
                return false;
            }
        }

        // Check entity_id filter
        if let Some(entity_ids) = &self.filter.entity_ids {
            if !entity_ids.contains(&item.entity_id) {
                return false;
            }
        }

        // Check area filter
        if let Some(areas) = &self.filter.areas {
            if let Some(item_area) = &item.area {
                if !areas.contains(item_area) {
                    return false;
                }
            } else {
                return false; // No area set but filter requires one
            }
        }

        true
    }

    /// Connect to WebSocket API for real-time updates
    async fn connect_websocket(&mut self) -> Result<()> {
        let ws_url = self.base_url.clone();
        let ws_url = if ws_url.scheme() == "https" {
            ws_url.as_str().replace("https://", "wss://")
        } else {
            ws_url.as_str().replace("http://", "ws://")
        };
        let ws_url = format!("{}/api/websocket", ws_url);

        let (ws_stream, _) = connect_async(&ws_url).await.map_err(|e| {
            crate::CoreError::tool_exec_error(
                "homeassistant_websocket",
                json!({ "url": ws_url }),
                e,
            )
        })?;

        let (mut write, mut read) = ws_stream.split();

        // Wait for auth_required message
        if let Some(Ok(Message::Text(text))) = read.next().await {
            let msg: Value = serde_json::from_str(&text).unwrap_or_default();
            if msg["type"].as_str() != Some("auth_required") {
                return Err(crate::CoreError::tool_exec_msg(
                    "homeassistant_websocket",
                    json!({ "url": ws_url }),
                    format!("Expected auth_required, got: {}", msg["type"]),
                ));
            }
        }

        // Send authentication
        let auth_msg = json!({
            "type": "auth",
            "access_token": self.access_token
        });

        write
            .send(Message::Text(auth_msg.to_string()))
            .await
            .map_err(|e| {
                crate::CoreError::tool_exec_error(
                    "homeassistant_websocket",
                    json!({ "action": "send_auth" }),
                    e,
                )
            })?;

        // Wait for auth response
        if let Some(Ok(Message::Text(text))) = read.next().await {
            let msg: Value = serde_json::from_str(&text).unwrap_or_default();
            if msg["type"].as_str() == Some("auth_invalid") {
                return Err(crate::CoreError::tool_exec_msg(
                    "homeassistant_websocket",
                    json!({ "url": ws_url }),
                    format!(
                        "Authentication failed: {}",
                        msg["message"].as_str().unwrap_or("unknown")
                    ),
                ));
            } else if msg["type"].as_str() != Some("auth_ok") {
                return Err(crate::CoreError::tool_exec_msg(
                    "homeassistant_websocket",
                    json!({ "url": ws_url }),
                    format!("Expected auth_ok, got: {}", msg["type"]),
                ));
            }
        }

        // Subscribe to state changes
        let subscribe_msg = json!({
            "id": 1,
            "type": "subscribe_events",
            "event_type": "state_changed"
        });

        write
            .send(Message::Text(subscribe_msg.to_string()))
            .await
            .map_err(|e| {
                crate::CoreError::tool_exec_error(
                    "homeassistant_websocket",
                    json!({ "action": "subscribe" }),
                    e,
                )
            })?;

        // Rejoin the stream for storage
        let ws_stream = read.reunite(write).map_err(|_| {
            crate::CoreError::tool_exec_msg(
                "homeassistant_websocket",
                json!({ "url": ws_url }),
                "Failed to reunite WebSocket stream".to_string(),
            )
        })?;

        self.ws_connection = Some(ws_stream);

        // Update metadata
        {
            let mut metadata = self.metadata.write().await;
            metadata.status = DataSourceStatus::Active;
        }

        Ok(())
    }

    /// Process a state change event from WebSocket
    fn process_state_change(&self, event: Value) -> Option<HomeAssistantItem> {
        // Extract the new state from the event
        let new_state = event["event"]["data"]["new_state"].clone();
        if new_state.is_null() {
            return None;
        }

        let mut item = self.parse_state_object(new_state)?;
        item.event_type = Some("state_changed".to_string());

        // Apply filters
        if self.should_include_item(&item) {
            Some(item)
        } else {
            None
        }
    }
}

#[async_trait]
impl DataSource for HomeAssistantSource {
    type Item = HomeAssistantItem;
    type Filter = HomeAssistantFilter;
    type Cursor = HomeAssistantCursor;

    fn source_id(&self) -> &str {
        &self.source_id
    }

    async fn pull(&mut self, limit: usize, after: Option<Self::Cursor>) -> Result<Vec<Self::Item>> {
        // Fetch current states via REST API
        let mut states = self.fetch_states().await?;

        // Apply cursor filtering if provided
        if let Some(cursor) = after {
            states.retain(|item| item.last_updated > cursor.timestamp);
        }

        // Apply limit
        states.truncate(limit);

        // Update cursor
        if let Some(last) = states.last() {
            self.current_cursor = Some(HomeAssistantCursor {
                timestamp: last.last_updated,
                event_id: None,
            });
        }

        // Update metadata
        {
            let mut metadata = self.metadata.write().await;
            metadata.items_processed += states.len() as u64;
            metadata.last_item_time = states.last().map(|s| s.last_updated);
        }

        Ok(states)
    }

    async fn subscribe(
        &mut self,
        from: Option<Self::Cursor>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin>>
    {
        // Connect to WebSocket if not already connected
        if self.ws_connection.is_none() {
            self.connect_websocket().await?;
        }

        // Take ownership of the WebSocket connection
        let ws_stream = self.ws_connection.take().ok_or_else(|| {
            crate::CoreError::tool_exec_msg(
                "homeassistant_subscribe",
                json!({}),
                "WebSocket connection not available".to_string(),
            )
        })?;

        // Create a filter for processing events
        let filter = self.filter.clone();
        let min_update_interval = filter.min_update_interval.clone();
        let last_update_times = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::<
            String,
            std::time::Instant,
        >::new()));

        // Create the stream that processes WebSocket messages
        let stream = ws_stream
            .filter_map(move |msg| {
                let filter = filter.clone();
                let result = async move {
                    match msg {
                        Ok(Message::Text(text)) => {
                            // Parse the message
                            let json_msg: Value = serde_json::from_str(&text).ok()?;

                            // Check if it's an event message
                            if json_msg["type"].as_str() == Some("event") {
                                let event = json_msg["event"].clone();

                                // Check if it's a state_changed event
                                if event["event_type"].as_str() == Some("state_changed") {
                                    // Extract the new state
                                    let new_state = event["data"]["new_state"].clone();
                                    if new_state.is_null() {
                                        return None;
                                    }

                                    // Parse into our item format
                                    let item = Self::parse_state_from_json(new_state, &filter)?;

                                    // Create cursor
                                    let cursor = HomeAssistantCursor {
                                        timestamp: item.last_updated,
                                        event_id: event["id"].as_str().map(|s| s.to_string()),
                                    };

                                    // Create stream event
                                    Some(Ok(StreamEvent {
                                        item,
                                        cursor,
                                        timestamp: chrono::Utc::now(),
                                    }))
                                } else {
                                    None
                                }
                            } else if json_msg["type"].as_str() == Some("auth_invalid") {
                                // Authentication failed during stream
                                Some(Err(crate::CoreError::tool_exec_msg(
                                    "homeassistant_subscribe",
                                    json!({}),
                                    "Authentication invalidated during stream".to_string(),
                                )))
                            } else {
                                // Other message types we don't handle yet
                                None
                            }
                        }
                        Ok(Message::Close(_)) => {
                            // Connection closed
                            Some(Err(crate::CoreError::tool_exec_msg(
                                "homeassistant_subscribe",
                                json!({}),
                                "WebSocket connection closed".to_string(),
                            )))
                        }
                        Ok(_) => None, // Binary, Ping, Pong, etc.
                        Err(e) => Some(Err(crate::CoreError::tool_exec_error(
                            "homeassistant_subscribe",
                            json!({}),
                            e,
                        ))),
                    }
                };
                result
            })
            .filter_map(move |event| {
                let last_update_times = last_update_times.clone();
                // Apply rate limiting if configured
                let result = async move {
                    match event {
                        Ok(stream_event) => {
                            if let Some(interval) = min_update_interval {
                                let entity_id = stream_event.item.entity_id.clone();
                                let now = std::time::Instant::now();

                                let mut times = last_update_times.lock().await;
                                if let Some(last_time) = times.get(&entity_id) {
                                    if now.duration_since(*last_time) < interval {
                                        return None; // Skip due to rate limiting
                                    }
                                }

                                times.insert(entity_id, now);
                            }
                            Some(Ok(stream_event))
                        }
                        Err(e) => Some(Err(e)),
                    }
                };
                result
            })
            .filter(move |event| {
                // Apply cursor filtering if provided
                let keep = if let Some(ref cursor) = from {
                    if let Ok(stream_event) = event {
                        stream_event.cursor.timestamp > cursor.timestamp
                    } else {
                        true // Keep errors
                    }
                } else {
                    true
                };
                futures::future::ready(keep)
            })
            .boxed();

        Ok(Box::new(stream)
            as Box<
                dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin,
            >)
    }

    fn set_filter(&mut self, filter: HomeAssistantFilter) {
        self.filter = filter;
        // TODO: If connected, update WebSocket subscriptions
    }

    fn current_cursor(&self) -> Option<Self::Cursor> {
        self.current_cursor.clone()
    }

    fn metadata(&self) -> DataSourceMetadata {
        // Blocking read is okay for metadata
        futures::executor::block_on(async { self.metadata.read().await.clone() })
    }

    fn buffer_config(&self) -> BufferConfig {
        BufferConfig {
            max_items: 1000,
            max_age: Duration::from_secs(3600), // Keep states for 1 hour
            persist_to_db: true,
            index_content: false,
            notify_changes: true,
        }
    }

    async fn format_notification(
        &self,
        item: &Self::Item,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        // Format state change notification
        let notification = match &item.event_type {
            Some(event_type) => {
                format!(
                    "HomeAssistant Event: {} - {} changed to '{}'",
                    event_type,
                    item.friendly_name.as_deref().unwrap_or(&item.entity_id),
                    item.state
                )
            }
            None => {
                format!(
                    "HomeAssistant: {} is now '{}'",
                    item.friendly_name.as_deref().unwrap_or(&item.entity_id),
                    item.state
                )
            }
        };

        // Create memory block for entity context
        let mut memory_blocks = Vec::new();

        // Add entity state as a memory block
        let block_name = CompactString::new(format!("ha_{}", item.domain));
        let block_content = format!(
            "Entity: {}\nState: {}\nAttributes: {:?}\nLast Updated: {}",
            item.entity_id, item.state, item.attributes, item.last_updated
        );

        memory_blocks.push((
            block_name,
            MemoryBlock {
                id: MemoryId::generate(),
                owner_id: UserId::generate(), // TODO: Get from context
                label: CompactString::new(format!("ha_{}", item.domain)),
                value: block_content,
                memory_type: MemoryType::Working,
                description: Some(format!("HomeAssistant {} entities", item.domain)),
                pinned: false,
                permission: MemoryPermission::ReadOnly,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                metadata: serde_json::json!({}),
                embedding_model: None,
                embedding: None,
                is_active: true,
            },
        ));

        Some((notification, memory_blocks))
    }

    fn set_notifications_enabled(&mut self, enabled: bool) {
        self.notifications_enabled = enabled;
    }

    fn notifications_enabled(&self) -> bool {
        self.notifications_enabled
    }
}

impl HomeAssistantSource {
    /// Static method to parse state from JSON with filters
    fn parse_state_from_json(
        state: Value,
        filter: &HomeAssistantFilter,
    ) -> Option<HomeAssistantItem> {
        let entity_id = state["entity_id"].as_str()?.to_string();
        let state_value = state["state"].as_str()?.to_string();

        // Extract domain from entity_id
        let domain = entity_id.split('.').next()?.to_string();

        // Apply domain filter early
        if let Some(domains) = &filter.domains {
            if !domains.contains(&domain) {
                return None;
            }
        }

        // Apply entity_id filter early
        if let Some(entity_ids) = &filter.entity_ids {
            if !entity_ids.contains(&entity_id) {
                return None;
            }
        }

        let attributes: HashMap<String, Value> = state["attributes"]
            .as_object()
            .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let friendly_name = attributes
            .get("friendly_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let area = attributes
            .get("area")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Apply area filter
        if let Some(areas) = &filter.areas {
            if let Some(ref item_area) = area {
                if !areas.contains(item_area) {
                    return None;
                }
            } else {
                return None; // No area set but filter requires one
            }
        }

        let last_changed = state["last_changed"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let last_updated = state["last_updated"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        Some(HomeAssistantItem {
            entity_id,
            state: state_value,
            attributes,
            last_changed,
            last_updated,
            friendly_name,
            domain,
            area,
            event_type: Some("state_changed".to_string()),
        })
    }
}
