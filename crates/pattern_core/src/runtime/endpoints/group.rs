use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio_stream::StreamExt;

use crate::{
    Result,
    agent::Agent,
    coordination::groups::{AgentGroup, AgentWithMembership, GroupManager, GroupResponseEvent},
    message::Message,
};

use super::{MessageEndpoint, MessageOrigin};

/// Endpoint for routing messages through agent groups
pub struct GroupEndpoint {
    pub group: AgentGroup,
    pub agents: Vec<AgentWithMembership<Arc<dyn Agent>>>,
    pub manager: Arc<dyn GroupManager>,
}

#[async_trait]
impl MessageEndpoint for GroupEndpoint {
    async fn send(
        &self,
        mut message: Message,
        metadata: Option<Value>,
        _origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        // Merge any provided metadata into the message
        if let Some(meta) = metadata {
            if let Some(obj) = meta.as_object() {
                // Merge with existing custom metadata
                if let Some(existing_obj) = message.metadata.custom.as_object_mut() {
                    for (key, value) in obj {
                        existing_obj.insert(key.clone(), value.clone());
                    }
                } else {
                    message.metadata.custom = meta;
                }
            }
        }

        let mut stream = self
            .manager
            .route_message(&self.group, &self.agents, message)
            .await?;

        // Process to completion, logging key events
        while let Some(event) = stream.next().await {
            match event {
                GroupResponseEvent::Error {
                    message, agent_id, ..
                } => {
                    tracing::error!(
                        "Group {} routing error from {:?}: {}",
                        self.group.name,
                        agent_id,
                        message
                    );
                }
                GroupResponseEvent::Complete {
                    agent_responses, ..
                } => {
                    tracing::info!(
                        "Group {} processed message, {} agents responded",
                        self.group.name,
                        agent_responses.len()
                    );
                }
                _ => {} // Other events handled silently for now
            }
        }

        Ok(None)
    }

    fn endpoint_type(&self) -> &'static str {
        "group"
    }
}
