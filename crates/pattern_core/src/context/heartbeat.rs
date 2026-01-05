//! Heartbeat handling for multi-step agent reasoning
//!
//! Based on Letta/MemGPT's heartbeat concept, this allows agents to request
//! additional turns without waiting for external input.

use serde_json::Value;
use tokio::sync::mpsc;

use crate::AgentId;

/// A heartbeat request from an agent
#[derive(Debug, Clone)]
pub struct HeartbeatRequest {
    pub agent_id: AgentId,
    pub tool_name: String,
    pub tool_call_id: String,
    pub batch_id: Option<crate::SnowflakePosition>,
    pub next_sequence_num: Option<u32>,
    pub model_vendor: Option<crate::model::ModelVendor>,
}

/// Channel for sending heartbeat requests
pub type HeartbeatSender = mpsc::Sender<HeartbeatRequest>;
pub type HeartbeatReceiver = mpsc::Receiver<HeartbeatRequest>;

/// Create a new heartbeat channel with reasonable buffer size
pub fn heartbeat_channel() -> (HeartbeatSender, HeartbeatReceiver) {
    mpsc::channel(100)
}

/// Check if a tool's arguments contain a heartbeat request
pub fn check_heartbeat_request(fn_arguments: &Value) -> bool {
    fn_arguments
        .get("request_heartbeat")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

use crate::{
    agent::{Agent, AgentState, ResponseEvent},
    context::NON_USER_MESSAGE_PREFIX,
    messages::{ChatRole, Message},
};
use futures::StreamExt;
use std::time::Duration;

/// Process heartbeat requests for one or more agents
///
/// This generic task handles heartbeat continuations, creating appropriate
/// messages based on model vendor and maintaining batch context.
pub async fn process_heartbeats<F, Fut>(
    mut heartbeat_rx: HeartbeatReceiver,
    agents: Vec<std::sync::Arc<dyn Agent>>,
    event_handler: F,
) where
    F: Fn(ResponseEvent, AgentId, String) -> Fut + Clone + Send + Sync + 'static, // Added String for agent name
    Fut: std::future::Future<Output = ()> + Send,
{
    while let Some(heartbeat) = heartbeat_rx.recv().await {
        tracing::debug!(
            "💓 Received heartbeat request from agent {}: tool {} (call_id: {})",
            heartbeat.agent_id,
            heartbeat.tool_name,
            heartbeat.tool_call_id
        );

        // Find the agent that sent the heartbeat
        let agent = agents
            .iter()
            .find(|a| a.id() == heartbeat.agent_id)
            .cloned();

        if let Some(agent) = agent {
            let handler = event_handler.clone();
            let agent_id = heartbeat.agent_id.clone();
            let agent_name = agent.name().to_string();

            // Spawn task to handle this heartbeat
            tokio::spawn(async move {
                // Wait for agent to be ready
                let (state, maybe_receiver) = agent.state().await;
                if state != AgentState::Ready {
                    if let Some(mut receiver) = maybe_receiver {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(200),
                            receiver.wait_for(|s| *s == AgentState::Ready),
                        )
                        .await;
                    }
                }

                tracing::info!("💓 Processing heartbeat from tool: {}", heartbeat.tool_name);

                // Determine role based on vendor
                let role = match heartbeat.model_vendor {
                    Some(vendor) if vendor.is_openai_compatible() => ChatRole::System,
                    Some(crate::model::ModelVendor::Gemini) => ChatRole::User,
                    _ => ChatRole::User, // Anthropic and default
                };

                // Create continuation message in same batch
                let content = format!(
                    "{}Function called using request_heartbeat=true, returning control {}",
                    NON_USER_MESSAGE_PREFIX, heartbeat.tool_name
                );
                let message = if let (Some(batch_id), Some(seq_num)) =
                    (heartbeat.batch_id, heartbeat.next_sequence_num)
                {
                    match role {
                        ChatRole::System => Message::system_in_batch(batch_id, seq_num, content),
                        ChatRole::Assistant => {
                            Message::assistant_in_batch(batch_id, seq_num, content)
                        }
                        _ => Message::user_in_batch(batch_id, seq_num, content),
                    }
                } else {
                    // Fallback for older code without batch info
                    tracing::warn!("Heartbeat without batch info - creating new batch");
                    Message::user(content)
                };

                // Process and handle events
                match agent.process(message).await {
                    Ok(mut stream) => {
                        while let Some(event) = stream.next().await {
                            handler(event, agent_id.clone(), agent_name.clone()).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error processing heartbeat: {:?}", e);
                        handler(
                            ResponseEvent::Error {
                                message: format!("Heartbeat processing failed: {:?}", e),
                                recoverable: true,
                            },
                            agent_id,
                            agent_name,
                        )
                        .await;
                    }
                }
            });
        } else {
            tracing::warn!("No agent found for heartbeat from {}", heartbeat.agent_id);
        }
    }

    tracing::debug!("Heartbeat processor task exiting");
}
