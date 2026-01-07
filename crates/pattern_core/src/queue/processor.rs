//! Queue processor for polling and dispatching messages to agents.

use crate::db::ConstellationDatabases;
use dashmap::{DashMap, DashSet};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, error};

use crate::agent::{Agent, ResponseEvent};
use crate::error::Result;
use crate::messages::{Message, MessageContent, MessageMetadata};
use crate::realtime::{AgentEventContext, AgentEventSink};

/// Configuration for the queue processor
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// How often to poll for pending messages
    pub poll_interval: Duration,

    /// Maximum number of messages to fetch per poll per agent
    pub batch_size: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 10,
        }
    }
}

/// Processor that polls for queued messages and dispatches them to agents
pub struct QueueProcessor {
    dbs: ConstellationDatabases,
    /// DashMap-based agent registry for dynamic agent registration
    agents: Arc<DashMap<String, Arc<dyn Agent>>>,
    config: QueueConfig,
    /// Optional sinks for forwarding response events
    sinks: Vec<Arc<dyn AgentEventSink>>,
    /// Messages currently being processed (prevents duplicate activations)
    in_flight: Arc<DashSet<String>>,
}

impl QueueProcessor {
    /// Create a new queue processor with a DashMap agent registry
    pub fn new(
        dbs: ConstellationDatabases,
        agents: Arc<DashMap<String, Arc<dyn Agent>>>,
        config: QueueConfig,
    ) -> Self {
        Self {
            dbs,
            agents,
            config,
            sinks: Vec::new(),
            in_flight: Arc::new(DashSet::new()),
        }
    }

    /// Add an event sink to receive response events
    pub fn with_sink(mut self, sink: Arc<dyn AgentEventSink>) -> Self {
        self.sinks.push(sink);
        self
    }

    /// Add multiple event sinks
    pub fn with_sinks(mut self, sinks: Vec<Arc<dyn AgentEventSink>>) -> Self {
        self.sinks.extend(sinks);
        self
    }

    /// Start the queue processor, returning a join handle
    ///
    /// The processor will run in the background, polling for messages
    /// at the configured interval and dispatching them to agents.
    pub fn start(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    /// Main processing loop
    async fn run(self) {
        let mut poll_interval = tokio::time::interval(self.config.poll_interval);

        loop {
            poll_interval.tick().await;

            if let Err(e) = self.process_pending().await {
                error!("Queue processing error: {:?}", e);
            }
        }
    }

    /// Forward an event to all sinks

    /// Process all pending messages for all agents
    async fn process_pending(&self) -> Result<()> {
        // Collect agent IDs first to avoid holding DashMap refs across await
        let agent_ids: Vec<String> = self
            .agents
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for agent_id in agent_ids {
            // Look up agent - clone immediately to avoid holding ref
            let agent = match self.agents.get(&agent_id) {
                Some(entry) => entry.value().clone(),
                None => continue, // Agent was removed, skip
            };

            // Get pending messages for this agent
            let pending = match pattern_db::queries::get_pending_messages(
                self.dbs.constellation.pool(),
                &agent_id,
                self.config.batch_size as i64,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to fetch messages for agent {}: {:?}", agent_id, e);
                    continue; // Skip to next agent
                }
            };

            for queued in pending {
                // Skip if already being processed (prevents duplicate activations)
                if self.in_flight.contains(&queued.id) {
                    debug!("Skipping queued message {} - already in flight", queued.id);
                    continue;
                }

                // Mark as in-flight before spawning
                self.in_flight.insert(queued.id.clone());

                debug!(
                    "Processing queued message {} for agent {}",
                    queued.id, agent_id
                );

                // Reconstruct full Message from new fields if available
                let message = reconstruct_message(&queued);

                // Create event context for sinks
                let ctx = AgentEventContext {
                    source_tag: Some("Queue".to_string()),
                    agent_name: Some(agent.name().to_string()),
                };
                let agent = agent.clone();
                let queued_id = queued.id.clone();
                let pool = self.dbs.constellation.pool().clone();
                let sinks = self.sinks.clone();
                let in_flight = Arc::clone(&self.in_flight);

                tokio::spawn(async move {
                    let ctx = ctx.clone();
                    // Process through agent
                    match agent.process(vec![message]).await {
                        Ok(mut stream) => {
                            while let Some(event) = stream.next().await {
                                forward_event(&sinks, event, &ctx).await;
                            }

                            // Only mark as processed on success
                            if let Err(e) =
                                pattern_db::queries::mark_message_processed(&pool, &queued_id).await
                            {
                                error!(
                                    "Failed to mark message {} as processed: {:?}",
                                    queued_id, e
                                );
                            }
                        }
                        Err(e) => {
                            error!("Failed to process queued message {}: {:?}", queued_id, e);
                            // DON'T mark as processed - message will be retried
                        }
                    }

                    // Always remove from in-flight when done
                    in_flight.remove(&queued_id);
                });
            }
        }

        Ok(())
    }
}

async fn forward_event(
    sinks: &[Arc<dyn AgentEventSink>],
    event: ResponseEvent,
    ctx: &AgentEventContext,
) {
    for sink in sinks {
        let event = event.clone();
        let ctx = ctx.clone();
        let sink = sink.clone();
        tokio::spawn(async move {
            sink.on_event(event, ctx).await;
        });
    }
}

/// Reconstruct a full Message from a QueuedMessage.
///
/// Tries to deserialize from the new content_json/metadata_json_full fields first,
/// falling back to legacy behavior for old messages.
fn reconstruct_message(queued: &pattern_db::models::QueuedMessage) -> Message {
    // Try to deserialize content from new field
    let content: MessageContent = queued
        .content_json
        .as_ref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_else(|| MessageContent::Text(queued.content.clone()));

    // Try to deserialize metadata from new field
    let metadata: MessageMetadata = queued
        .metadata_json_full
        .as_ref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_else(|| {
            // Legacy fallback: build metadata from old fields
            let mut meta = MessageMetadata::default();
            meta.user_id = queued.source_agent_id.clone();

            // Parse origin_json if present
            if let Some(ref origin_json) = queued.origin_json {
                if let Ok(origin) = serde_json::from_str::<serde_json::Value>(origin_json) {
                    meta.custom = serde_json::json!({
                        "origin": origin,
                        "queue_metadata": queued.metadata_json.as_ref()
                            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                    });
                }
            } else if let Some(ref meta_json) = queued.metadata_json {
                if let Ok(custom) = serde_json::from_str::<serde_json::Value>(meta_json) {
                    meta.custom = custom;
                }
            }

            meta
        });

    // Parse batch_id
    let batch = queued.batch_id.as_ref().and_then(|s| s.parse().ok());

    // All queued messages are user messages (architectural invariant)
    let mut message = Message::user(content);
    message.metadata = metadata;
    message.batch = batch;

    message
}
