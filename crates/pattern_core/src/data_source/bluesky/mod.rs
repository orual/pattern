//! Bluesky DataStream implementation using Jacquard.
//!
//! Implements the DataStream trait for consuming Bluesky firehose events
//! via Jetstream and routing them as notifications to agents.

mod batch;
mod blocks;
mod embed;
mod firehose;
mod inner;
mod thread;

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::config::BlueskySourceConfig;
use crate::data_source::{BlockSchemaSpec, DataStream, Notification, StreamStatus};
use crate::error::{CoreError, Result};
use crate::id::AgentId;
use crate::memory::BlockSchema;
use crate::runtime::endpoints::BlueskyAgent;
use crate::runtime::{MessageOrigin, ToolContext};
use crate::tool::rules::ToolRule;

use batch::PendingBatch;
use inner::BlueskyStreamInner;

// Re-export public types
pub use firehose::FirehosePost;
pub use thread::{PostDisplay, ThreadContext};

/// Default batch window duration (seconds)
const DEFAULT_BATCH_WINDOW_SECS: u64 = 20;

/// Bluesky firehose data source using Jetstream
pub struct BlueskyStream {
    inner: Arc<BlueskyStreamInner>,
}

impl std::fmt::Debug for BlueskyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlueskyStream")
            .field("source_id", &self.inner.source_id)
            .field("name", &self.inner.name)
            .field("endpoint", &self.inner.endpoint)
            .field("config", &self.inner.config)
            .field("status", &*self.inner.status.read())
            .field("batch_window", &self.inner.batch_window)
            .finish()
    }
}

impl Clone for BlueskyStream {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl BlueskyStream {
    /// Create a new BlueskyStream from config
    pub fn from_config(config: BlueskySourceConfig, tool_context: Arc<dyn ToolContext>) -> Self {
        let source_id = config.name.clone();
        let endpoint = config.jetstream_endpoint.clone();
        // Use the first DID in mentions as the agent DID for self-detection
        let agent_did = config.mentions.first().cloned();
        Self {
            inner: Arc::new(BlueskyStreamInner {
                name: format!("Bluesky Firehose ({})", &source_id),
                source_id,
                endpoint,
                config,
                agent_did,
                authenticated_agent: None,
                batch_window: Duration::from_secs(DEFAULT_BATCH_WINDOW_SECS),
                status: RwLock::new(StreamStatus::Stopped),
                tx: RwLock::new(None),
                pending_batch: PendingBatch::new(),
                shutdown_tx: RwLock::new(None),
                last_message_time: RwLock::new(None),
                current_cursor: RwLock::new(None),
                recently_shown_threads: DashMap::new(),
                recently_shown_images: DashMap::new(),
                tool_context,
            }),
        }
    }

    /// Create a new BlueskyStream with default settings
    pub fn new(source_id: impl Into<String>, tool_context: Arc<dyn ToolContext>) -> Self {
        let source_id = source_id.into();
        let mut config = BlueskySourceConfig::default();
        config.name = source_id.clone();
        Self::from_config(config, tool_context)
    }

    // Helper to rebuild inner with new values
    fn rebuild(&self, modifier: impl FnOnce(&mut BlueskyStreamInner)) -> Self {
        let mut new_inner = BlueskyStreamInner {
            source_id: self.inner.source_id.clone(),
            name: self.inner.name.clone(),
            endpoint: self.inner.endpoint.clone(),
            config: self.inner.config.clone(),
            agent_did: self.inner.agent_did.clone(),
            authenticated_agent: self.inner.authenticated_agent.clone(),
            batch_window: self.inner.batch_window,
            status: RwLock::new(StreamStatus::Stopped),
            tx: RwLock::new(None),
            pending_batch: PendingBatch::new(),
            shutdown_tx: RwLock::new(None),
            last_message_time: RwLock::new(None),
            current_cursor: RwLock::new(None),
            recently_shown_threads: DashMap::new(),
            recently_shown_images: DashMap::new(),
            tool_context: self.inner.tool_context.clone(),
        };
        modifier(&mut new_inner);
        Self {
            inner: Arc::new(new_inner),
        }
    }

    /// Set the authenticated agent for API calls (hydration, respecting blocks)
    pub fn with_authenticated_agent(self, agent: Arc<BlueskyAgent>) -> Self {
        self.rebuild(|inner| inner.authenticated_agent = Some(agent))
    }

    /// Set the Jetstream endpoint
    pub fn with_endpoint(self, endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        self.rebuild(|inner| inner.endpoint = endpoint)
    }

    /// Set the config
    pub fn with_config(self, config: BlueskySourceConfig) -> Self {
        self.rebuild(|inner| inner.config = config)
    }

    /// Set the agent DID for self-detection (overrides mentions[0])
    pub fn with_agent_did(self, did: impl Into<String>) -> Self {
        let did = did.into();
        self.rebuild(|inner| inner.agent_did = Some(did))
    }

    /// Set the batch window duration
    pub fn with_batch_window(self, duration: Duration) -> Self {
        self.rebuild(|inner| inner.batch_window = duration)
    }

    /// Set the display name
    pub fn with_name(self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.rebuild(|inner| inner.name = name)
    }
}

#[async_trait]
impl DataStream for BlueskyStream {
    fn source_id(&self) -> &str {
        &self.inner.source_id
    }

    fn name(&self) -> &str {
        &self.inner.name
    }

    fn block_schemas(&self) -> Vec<BlockSchemaSpec> {
        vec![BlockSchemaSpec::ephemeral(
            "bluesky_user_{handle}",
            BlockSchema::text(),
            "Bluesky user profile and interaction history",
        )]
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]
    }

    async fn start(
        &self,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>> {
        if *self.inner.status.read() == StreamStatus::Running {
            return Err(CoreError::DataSourceError {
                source_name: self.inner.source_id.clone(),
                operation: "start".to_string(),
                cause: "Already running".to_string(),
            });
        }

        let (tx, rx) = broadcast::channel(256);
        *self.inner.tx.write() = Some(tx.clone());

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        *self.inner.shutdown_tx.write() = Some(shutdown_tx);

        // Clone inner Arc for the supervisor task
        let inner = Arc::clone(&self.inner);
        let ctx_for_supervisor = Arc::clone(&ctx);

        tokio::spawn(async move {
            inner.supervisor_loop(ctx_for_supervisor, shutdown_rx).await;
        });

        // Spawn routing task if target is configured
        let target = self.inner.config.target.clone();
        if !target.is_empty() {
            let source_id = self.inner.source_id.clone();
            let routing_rx = tx.subscribe();

            info!(
                "BlueskyStream {} starting notification routing to target '{}'",
                source_id, target
            );

            tokio::spawn(async move {
                route_notifications(routing_rx, target, source_id, ctx).await;
            });
        } else {
            let source_id = self.inner.source_id.clone();
            let routing_rx = tx.subscribe();

            info!(
                "BlueskyStream {} starting notification routing to target '{}'",
                source_id, owner
            );

            tokio::spawn(async move {
                route_notifications(routing_rx, owner.0, source_id, ctx).await;
            });
        }

        *self.inner.status.write() = StreamStatus::Running;
        Ok(rx)
    }

    async fn stop(&self) -> Result<()> {
        *self.inner.status.write() = StreamStatus::Stopped;

        if let Some(tx) = self.inner.shutdown_tx.write().take() {
            let _ = tx.send(());
        }

        Ok(())
    }

    fn pause(&self) {
        *self.inner.status.write() = StreamStatus::Paused;
    }

    fn resume(&self) {
        if *self.inner.status.read() == StreamStatus::Paused {
            *self.inner.status.write() = StreamStatus::Running;
        }
    }

    fn status(&self) -> StreamStatus {
        *self.inner.status.read()
    }

    fn supports_pull(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Route notifications from the stream to a target agent or group.
///
/// This runs as a background task, forwarding each notification to the
/// configured target using the router from ToolContext.
async fn route_notifications(
    mut rx: broadcast::Receiver<Notification>,
    target: String,
    source_id: String,
    ctx: Arc<dyn ToolContext>,
) {
    let router = ctx.router();

    loop {
        match rx.recv().await {
            Ok(notification) => {
                let mut message = notification.message;
                message.batch = Some(notification.batch_id);
                let origin = message.metadata.custom.as_object().and_then(|obj| {
                    // Try to extract MessageOrigin from custom metadata
                    serde_json::from_value::<MessageOrigin>(serde_json::Value::Object(obj.clone()))
                        .ok()
                });

                // Try routing to agent first, then group
                let result = router
                    .route_message_to_agent(&target, message.clone(), origin.clone())
                    .await;

                match result {
                    Ok(Some(_)) => {
                        debug!(
                            "BlueskyStream {} routed notification to agent '{}'",
                            source_id, target
                        );
                    }
                    Ok(None) => {
                        // Agent not found, try as group
                        match router
                            .route_message_to_group(&target, message, origin)
                            .await
                        {
                            Ok(_) => {
                                debug!(
                                    "BlueskyStream {} routed notification to group '{}'",
                                    source_id, target
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "BlueskyStream {} failed to route to target '{}': {}",
                                    source_id, target, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "BlueskyStream {} failed to route to agent '{}': {}",
                            source_id, target, e
                        );
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    "BlueskyStream {} routing task lagged {} messages",
                    source_id, n
                );
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!(
                    "BlueskyStream {} broadcast channel closed, stopping routing",
                    source_id
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let mut config = BlueskySourceConfig::default();
        config.friends.push("did:plc:abc123".to_string());
        config.keywords.push("rust".to_string());
        config.exclude_dids.push("did:plc:spam".to_string());

        assert!(config.friends.contains(&"did:plc:abc123".to_string()));
        assert!(config.keywords.contains(&"rust".to_string()));
        assert!(config.exclude_dids.contains(&"did:plc:spam".to_string()));
    }

    #[test]
    fn test_url_normalization() {
        assert!(BlueskyStreamInner::normalize_url("jetstream1.us-east.bsky.network").is_ok());
        assert!(BlueskyStreamInner::normalize_url("wss://jetstream1.us-east.bsky.network").is_ok());
        assert!(
            BlueskyStreamInner::normalize_url("https://jetstream1.us-east.bsky.network").is_ok()
        );
    }
}
