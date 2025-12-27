//! Queue processor for polling and dispatching messages to agents.

use crate::db::ConstellationDatabases;
use dashmap::DashMap;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, error};

use crate::agent::{Agent, ResponseEvent};
use crate::error::Result;
use crate::message::{Message, MessageMetadata};
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
                debug!(
                    "Processing queued message {} for agent {}",
                    queued.id, agent_id
                );

                // Build metadata from QueuedMessage fields
                let mut metadata = MessageMetadata::default();
                metadata.user_id = queued.source_agent_id.clone();

                // Parse origin_json if present and merge into custom
                if let Some(ref origin_json) = queued.origin_json {
                    if let Ok(origin) = serde_json::from_str::<serde_json::Value>(origin_json) {
                        metadata.custom = serde_json::json!({
                            "origin": origin,
                            "queue_metadata": queued.metadata_json.as_ref()
                                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                        });
                    }
                } else if let Some(ref meta_json) = queued.metadata_json {
                    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_json) {
                        metadata.custom = meta;
                    }
                }

                // Convert to Message with metadata
                let mut message = Message::user(queued.content.clone());
                message.metadata = metadata;

                // Create event context for sinks
                let ctx = AgentEventContext {
                    source_tag: Some("Queue".to_string()),
                    agent_name: Some(agent.name().to_string()),
                };
                let agent = agent.clone();
                let queued_id = queued.id.clone();
                let pool = self.dbs.constellation.pool().clone();
                let sinks = self.sinks.clone();

                tokio::spawn(async move {
                    let ctx = ctx.clone();
                    // Process through agent
                    match agent.process(message).await {
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
                            error!("Failed to process queued message {}: {:?}", queued.id, e);
                            // DON'T mark as processed - message will be retried
                        }
                    }
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
