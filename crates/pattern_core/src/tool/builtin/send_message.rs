//! Message sending tool for agents

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

use super::{MessageTarget, TargetType};

/// Input parameters for sending a message
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SendMessageInput {
    /// The target to send the message to
    pub target: MessageTarget,

    /// The message content
    pub content: String,

    /// Optional metadata for the message
    #[schemars(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    // request_heartbeat handled via ExecutionMeta injection; field removed
}

/// Output from send message operation
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SendMessageOutput {
    /// Whether the message was sent successfully
    pub success: bool,

    /// Unique identifier for the sent message
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,

    /// Any additional information about the send operation
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

// ============================================================================
// Implementation using ToolContext
// ============================================================================

use crate::runtime::ToolContext;
use std::sync::Arc;

/// Tool for sending messages to various targets using ToolContext
#[derive(Clone)]
pub struct SendMessageTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for SendMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendMessageTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl SendMessageTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl AiTool for SendMessageTool {
    type Input = SendMessageInput;
    type Output = SendMessageOutput;

    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to the user, another agent, a group, or a specific channel, or as a post on bluesky. This is the primary way to communicate."
    }

    async fn execute(&self, params: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        // Get the message router from the context
        let router = self.ctx.router();

        // Handle agent name resolution if target is agent type
        let (reason, content) = if matches!(params.target.target_type, TargetType::Agent) {
            let split: Vec<_> = params.content.splitn(2, &['\n', '|', '-']).collect();

            let reason = if split.len() == 1 {
                split.first().unwrap_or(&"")
            } else {
                "send_message_invocation"
            };
            (reason, split.last().unwrap_or(&"").to_string())
        } else {
            ("send_message_invocation", params.content.clone())
        };

        // When agent uses send_message tool, origin is the agent itself
        let origin = crate::runtime::MessageOrigin::Agent {
            agent_id: router.agent_id().to_string(),
            name: router.agent_name().to_string(),
            reason: reason.to_string(),
        };

        // Route based on target type (the new router has specific methods)
        let result = match params.target.target_type {
            TargetType::User => {
                router
                    .send_to_user(content.clone(), params.metadata.clone(), Some(origin))
                    .await
            }
            TargetType::Agent => {
                let agent_id = params.target.target_id.as_deref().unwrap_or("unknown");
                router
                    .send_to_agent(
                        agent_id,
                        content.clone(),
                        params.metadata.clone(),
                        Some(origin),
                    )
                    .await
            }
            TargetType::Group => {
                let group_id = params.target.target_id.as_deref().unwrap_or("unknown");
                router
                    .send_to_group(
                        group_id,
                        content.clone(),
                        params.metadata.clone(),
                        Some(origin),
                    )
                    .await
            }
            TargetType::Channel => {
                let channel_type = params.target.target_id.as_deref().unwrap_or("cli");
                router
                    .send_to_channel(
                        channel_type,
                        content.clone(),
                        params.metadata.clone(),
                        Some(origin),
                    )
                    .await
            }
            TargetType::Bluesky => {
                router
                    .send_to_bluesky(
                        params.target.target_id.clone(),
                        content.clone(),
                        params.metadata.clone(),
                        Some(origin),
                    )
                    .await
            }
        };

        // Handle the result
        match result {
            Ok(created_uri) => {
                // Generate a message ID for tracking
                let message_id = format!("msg_{}", chrono::Utc::now().timestamp_millis());

                // Build details based on target type and whether it was a like
                let details = match params.target.target_type {
                    TargetType::User => {
                        if let Some(id) = &params.target.target_id {
                            format!("Message sent to user {}", id)
                        } else {
                            "Message sent to user".to_string()
                        }
                    }
                    TargetType::Agent => {
                        format!(
                            "Message queued for agent {}",
                            params.target.target_id.as_deref().unwrap_or("unknown")
                        )
                    }
                    TargetType::Group => {
                        format!(
                            "Message sent to group {}",
                            params.target.target_id.as_deref().unwrap_or("unknown")
                        )
                    }
                    TargetType::Channel => {
                        format!(
                            "Message sent to channel {}",
                            params.target.target_id.as_deref().unwrap_or("default")
                        )
                    }
                    TargetType::Bluesky => {
                        // Check if this was a "like" action
                        let is_like = content.trim().eq_ignore_ascii_case("like");

                        if let Some(uri) = created_uri.as_ref().or(params.target.target_id.as_ref())
                        {
                            if is_like {
                                // Check if the URI indicates this was a like (contains "app.bsky.feed.like")
                                if uri.contains("app.bsky.feed.like") {
                                    format!("Liked Bluesky post: {}", uri)
                                } else {
                                    // Fallback if we sent "like" but didn't get a like URI back
                                    format!(
                                        "Like action on Bluesky post: {}",
                                        params.target.target_id.as_deref().unwrap_or("unknown")
                                    )
                                }
                            } else {
                                format!("Reply sent to Bluesky post: {}", uri)
                            }
                        } else {
                            "Message posted to Bluesky".to_string()
                        }
                    }
                };

                Ok(SendMessageOutput {
                    success: true,
                    message_id: Some(message_id),
                    details: Some(details),
                })
            }
            Err(e) => {
                // Log the error for debugging
                tracing::error!("Failed to send message: {:?}", e);

                Ok(SendMessageOutput {
                    success: false,
                    message_id: None,
                    details: Some(format!("Failed to send message: {:?}", e)),
                })
            }
        }
    }

    fn examples(&self) -> Vec<crate::tool::ToolExample<Self::Input, Self::Output>> {
        vec![
            crate::tool::ToolExample {
                description: "Send a message to the user".to_string(),
                parameters: SendMessageInput {
                    target: MessageTarget {
                        target_type: TargetType::User,
                        target_id: None,
                    },
                    content: "Hello! How can I help you today?".to_string(),
                    metadata: None,
                },
                expected_output: Some(SendMessageOutput {
                    success: true,
                    message_id: Some("msg_1234567890".to_string()),
                    details: Some("Message sent to user".to_string()),
                }),
            },
            crate::tool::ToolExample {
                description: "Send a message to another agent".to_string(),
                parameters: SendMessageInput {
                    target: MessageTarget {
                        target_type: TargetType::Agent,
                        target_id: Some("entropy_123".to_string()),
                    },
                    content: "Can you help break down this task?".to_string(),
                    metadata: Some(serde_json::json!({
                        "priority": "high",
                        "context": "task_breakdown"
                    })),
                },
                expected_output: Some(SendMessageOutput {
                    success: true,
                    message_id: Some("msg_1234567891".to_string()),
                    details: Some("Message sent to agent entropy_123".to_string()),
                }),
            },
        ]
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("the conversation will end when called")
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule {
            tool_name: self.name().to_string(),
            rule_type: ToolRuleType::ExitLoop,
            conditions: vec![],
            priority: 0,
            metadata: None,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ConstellationDatabases;
    use crate::tool::builtin::MockToolContext;
    use std::sync::Arc;

    async fn create_test_context() -> Arc<MockToolContext> {
        let dbs = Arc::new(
            ConstellationDatabases::open_in_memory()
                .await
                .expect("Failed to create test dbs"),
        );
        let memory = Arc::new(crate::memory::MemoryCache::new(Arc::clone(&dbs)));
        Arc::new(MockToolContext::new("test-agent", memory, dbs))
    }

    #[tokio::test]
    async fn test_send_message_tool() {
        let ctx = create_test_context().await;
        let tool = SendMessageTool::new(ctx);

        // Test sending to user
        let result = tool
            .execute(
                SendMessageInput {
                    target: MessageTarget {
                        target_type: TargetType::User,
                        target_id: None,
                    },
                    content: "Test message".to_string(),
                    metadata: None,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message_id.is_some());
    }
}
