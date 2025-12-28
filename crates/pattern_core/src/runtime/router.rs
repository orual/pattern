//! Message routing for agent-to-agent communication.
//!
//! The MessageRouter handles delivery of messages between agents, to users,
//! and to external platforms (Discord, Bluesky, etc.). It uses pattern_db
//! for queuing and provides anti-loop protection.

use crate::db::ConstellationDatabases;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::error::{CoreError, Result};
use crate::messages::Message;

/// Describes the origin of a message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageOrigin {
    /// Data source ingestion
    DataSource {
        source_id: String,
        source_type: String,
        item_id: Option<String>,
        cursor: Option<Value>,
    },

    /// Discord message
    Discord {
        server_id: String,
        channel_id: String,
        user_id: String,
        message_id: String,
    },

    /// CLI interaction
    Cli {
        session_id: String,
        command: Option<String>,
    },

    /// API request
    Api {
        client_id: String,
        request_id: String,
        endpoint: String,
    },

    /// Bluesky/ATProto
    Bluesky {
        handle: String,
        did: String,
        post_uri: Option<String>,
        is_mention: bool,
        is_reply: bool,
    },

    /// Agent-initiated (no external origin)
    Agent {
        agent_id: String,
        name: String,
        reason: String,
    },

    /// Other origin types
    Other {
        origin_type: String,
        source_id: String,
        metadata: Value,
    },
}

impl MessageOrigin {
    /// Get a human-readable description of the origin
    pub fn description(&self) -> String {
        match self {
            Self::DataSource {
                source_id,
                source_type,
                ..
            } => format!("Data from {} ({})", source_id, source_type),
            Self::Discord {
                server_id,
                channel_id,
                user_id,
                ..
            } => format!(
                "Discord message from user {} in {}/{}",
                user_id, server_id, channel_id
            ),
            Self::Cli {
                session_id,
                command,
            } => format!(
                "CLI session {} - {}",
                session_id,
                command.as_deref().unwrap_or("interactive")
            ),
            Self::Api {
                client_id,
                endpoint,
                ..
            } => format!("API request from {} to {}", client_id, endpoint),
            Self::Bluesky {
                handle,
                is_mention,
                is_reply,
                post_uri,
                ..
            } => {
                let mut post_framing = if *is_mention {
                    format!("Mentioned by @{}", handle)
                } else if *is_reply {
                    format!("Reply from @{}", handle)
                } else {
                    format!("Post from @{}", handle)
                };

                if let Some(post_uri) = post_uri {
                    post_framing.push_str(&format!(" aturi: {}", post_uri));
                }
                post_framing
            }
            Self::Agent { name, reason, .. } => format!("{} ({})", name, reason),
            Self::Other {
                origin_type,
                source_id,
                ..
            } => format!("{} from {}", origin_type, source_id),
        }
    }
}

/// Trait for message delivery endpoints
#[async_trait::async_trait]
pub trait MessageEndpoint: Send + Sync {
    /// Send a message to this endpoint
    async fn send(
        &self,
        message: Message,
        metadata: Option<Value>,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>>;

    /// Get the endpoint type name
    fn endpoint_type(&self) -> &'static str;
}

/// Routes messages from agents to their destinations
#[derive(Clone)]
pub struct AgentMessageRouter {
    /// The agent this router belongs to
    agent_id: String,

    /// Agent name
    name: String,

    /// Combined database connections (constellation + auth)
    dbs: ConstellationDatabases,

    /// Map of endpoint types to their implementations
    endpoints: Arc<RwLock<HashMap<String, Arc<dyn MessageEndpoint>>>>,

    /// Recent message pairs to prevent rapid loops (key: sorted agent pair, value: last message time)
    recent_messages: Arc<RwLock<HashMap<String, Instant>>>,

    /// Default endpoint for user messages
    default_user_endpoint: Arc<RwLock<Option<Arc<dyn MessageEndpoint>>>>,
}

impl AgentMessageRouter {
    /// Create a new message router for an agent
    pub fn new(agent_id: String, name: String, dbs: ConstellationDatabases) -> Self {
        Self {
            agent_id,
            name,
            dbs,
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            recent_messages: Arc::new(RwLock::new(HashMap::new())),
            default_user_endpoint: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the agent ID
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Get the agent name
    pub fn agent_name(&self) -> &str {
        &self.name
    }

    /// Register an endpoint for a specific type
    pub async fn register_endpoint(
        &self,
        endpoint_type: String,
        endpoint: Arc<dyn MessageEndpoint>,
    ) {
        let mut endpoints = self.endpoints.write().await;
        endpoints.insert(endpoint_type, endpoint);
    }

    /// Set the default endpoint for user messages (builder pattern)
    pub fn with_default_user_endpoint(self, endpoint: Arc<dyn MessageEndpoint>) -> Self {
        *self.default_user_endpoint.blocking_write() = Some(endpoint);
        self
    }

    /// Set the default user endpoint at runtime
    pub async fn set_default_user_endpoint(&self, endpoint: Arc<dyn MessageEndpoint>) {
        let mut default_endpoint = self.default_user_endpoint.write().await;
        *default_endpoint = Some(endpoint);
    }

    /// Send a message to the user (uses default endpoint)
    pub async fn send_to_user(
        &self,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        debug!("Routing message from agent {} to user", self.agent_id);

        // If we have a default user endpoint, use it
        let default_endpoint = self.default_user_endpoint.read().await;
        if let Some(endpoint) = default_endpoint.as_ref() {
            let message = Message::user(content);
            return endpoint.send(message, metadata, origin.as_ref()).await;
        }

        // No endpoint configured - log warning
        warn!(
            "No user endpoint configured for agent {}, message not delivered",
            self.agent_id
        );
        Ok(None)
    }

    /// Send a message to Bluesky
    pub async fn send_to_bluesky(
        &self,
        target_uri: Option<String>,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        debug!("Routing message from agent {} to Bluesky", self.agent_id);

        // Look for Bluesky endpoint in registered endpoints
        let endpoints = self.endpoints.read().await;
        if let Some(endpoint) = endpoints.get("bluesky") {
            let message = Message::user(content);

            // Include the target URI in metadata if it's a reply
            let final_metadata = if let Some(uri) = target_uri {
                let mut meta = metadata.unwrap_or_else(|| Value::Object(Default::default()));
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("reply_to".to_string(), Value::String(uri));
                }
                Some(meta)
            } else {
                metadata
            };

            return endpoint
                .send(message, final_metadata, origin.as_ref())
                .await;
        }

        warn!("No Bluesky endpoint registered");
        Ok(None)
    }

    /// Send a message to an agent by name or ID
    pub async fn send_to_agent(
        &self,
        target_identifier: &str,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        debug!(
            "Routing message from agent {} to agent {}",
            self.agent_id, target_identifier
        );

        // Resolve the target agent (try ID first, then name)
        let target_agent = if let Some(agent) =
            pattern_db::queries::get_agent(self.dbs.constellation.pool(), target_identifier).await?
        {
            agent
        } else if let Some(agent) =
            pattern_db::queries::get_agent_by_name(self.dbs.constellation.pool(), target_identifier)
                .await?
        {
            agent
        } else {
            return Err(CoreError::AgentNotFound {
                identifier: target_identifier.to_string(),
            });
        };

        let target_agent_id = target_agent.id;

        // Check recent message cache to prevent rapid loops
        {
            let mut recent = self.recent_messages.write().await;

            // Create a consistent key for the agent pair (sorted to ensure consistency)
            let mut agents = vec![self.agent_id.clone(), target_agent_id.clone()];
            agents.sort();
            let pair_key = agents.join(":");

            // Check if we've sent a message to this pair recently
            if let Some(last_time) = recent.get(&pair_key) {
                if last_time.elapsed() < Duration::from_secs(30) {
                    return Err(CoreError::RateLimited {
                        target: target_agent_id,
                        cooldown_secs: 30 - last_time.elapsed().as_secs(),
                    });
                }
            }

            // Update the cache
            recent.insert(pair_key, Instant::now());

            // Clean up old entries (older than 5 minutes)
            recent.retain(|_, time| time.elapsed() < Duration::from_secs(300));
        }

        // Create the queued message
        let queued = pattern_db::models::QueuedMessage {
            id: crate::utils::get_next_message_position_sync().to_string(),
            target_agent_id: target_agent_id.clone(),
            source_agent_id: Some(self.agent_id.clone()),
            content,
            origin_json: origin.as_ref().and_then(|o| serde_json::to_string(o).ok()),
            metadata_json: metadata
                .as_ref()
                .and_then(|m| serde_json::to_string(m).ok()),
            priority: 0,
            created_at: chrono::Utc::now(),
            processed_at: None,
        };

        // Store the message in the database
        pattern_db::queries::create_queued_message(self.dbs.constellation.pool(), &queued).await?;

        info!(
            "Queued message from {} to {} (id: {})",
            self.agent_id, target_agent_id, queued.id
        );

        Ok(Some(queued.id))
    }

    /// Send a message to a group by name or ID
    pub async fn send_to_group(
        &self,
        group_identifier: &str,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        debug!(
            "Routing message from agent {} to group {}",
            self.agent_id, group_identifier
        );

        // Check if we have a registered group endpoint
        let endpoints = self.endpoints.read().await;
        if let Some(endpoint) = endpoints.get("group") {
            // Use the registered group endpoint
            let message = Message::user(content);
            return endpoint.send(message, metadata, origin.as_ref()).await;
        }

        // Otherwise, fall back to direct queuing to all members
        warn!(
            "No group endpoint registered. Falling back to basic routing for group {}",
            group_identifier
        );

        // Resolve the group
        let group = if let Some(g) =
            pattern_db::queries::get_group(self.dbs.constellation.pool(), group_identifier).await?
        {
            g
        } else if let Some(g) =
            pattern_db::queries::get_group_by_name(self.dbs.constellation.pool(), group_identifier)
                .await?
        {
            g
        } else {
            return Err(CoreError::GroupNotFound {
                identifier: group_identifier.to_string(),
            });
        };

        // Get group members
        let members =
            pattern_db::queries::get_group_members(self.dbs.constellation.pool(), &group.id)
                .await?;

        if members.is_empty() {
            warn!("Group {} has no members", group.id);
            return Ok(None);
        }

        info!(
            "Basic routing to group {} with {} members",
            group.id,
            members.len()
        );

        // Queue for all members (no is_active field in GroupMember)
        let mut sent_count = 0;
        for member in members {
            let queued = pattern_db::models::QueuedMessage {
                id: crate::utils::get_next_message_position_sync().to_string(),
                target_agent_id: member.agent_id.clone(),
                source_agent_id: Some(self.agent_id.clone()),
                content: content.clone(),
                origin_json: origin.as_ref().and_then(|o| serde_json::to_string(o).ok()),
                metadata_json: metadata
                    .as_ref()
                    .and_then(|m| serde_json::to_string(m).ok()),
                priority: 0,
                created_at: chrono::Utc::now(),
                processed_at: None,
            };

            if let Err(e) =
                pattern_db::queries::create_queued_message(self.dbs.constellation.pool(), &queued)
                    .await
            {
                warn!(
                    "Failed to queue message for group member {}: {:?}",
                    member.agent_id, e
                );
            } else {
                sent_count += 1;
            }
        }

        info!(
            "Basic broadcast message to {} active members of group {}",
            sent_count, group.id
        );

        Ok(None)
    }

    /// Send a message to a channel (Discord, etc)
    pub async fn send_to_channel(
        &self,
        channel_type: &str,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        debug!(
            "Routing message from agent {} to {} channel",
            self.agent_id, channel_type
        );

        // Look for appropriate endpoint
        let endpoints = self.endpoints.read().await;
        if let Some(endpoint) = endpoints.get(channel_type) {
            let message = Message::user(content);
            endpoint.send(message, metadata, origin.as_ref()).await
        } else {
            Err(CoreError::NoEndpointConfigured {
                target_type: channel_type.to_string(),
            })
        }
    }

    /// Get pending messages for this agent
    pub async fn get_pending_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<pattern_db::models::QueuedMessage>> {
        pattern_db::queries::get_pending_messages(
            self.dbs.constellation.pool(),
            &self.agent_id,
            limit as i64,
        )
        .await
        .map_err(Into::into)
    }

    /// Mark a queued message as processed
    pub async fn mark_processed(&self, message_id: &str) -> Result<()> {
        pattern_db::queries::mark_message_processed(self.dbs.constellation.pool(), message_id)
            .await
            .map_err(Into::into)
    }

    /// Clean up old processed messages
    pub async fn cleanup_old_messages(&self, older_than_hours: u64) -> Result<u64> {
        pattern_db::queries::delete_old_processed(
            self.dbs.constellation.pool(),
            older_than_hours as i64,
        )
        .await
        .map_err(Into::into)
    }
}

impl std::fmt::Debug for AgentMessageRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMessageRouter")
            .field("agent_id", &self.agent_id)
            .field("name", &self.name)
            .field("endpoints_count", &self.endpoints.blocking_read().len())
            .field(
                "has_default_endpoint",
                &self.default_user_endpoint.blocking_read().is_some(),
            )
            .finish()
    }
}
