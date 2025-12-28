//! Source tool for data source control
//!
//! This tool provides operations to control data sources:
//! - `list` - List all registered sources (streams and block sources)
//! - `status` - Get status of a specific source
//! - `pause` - Pause a stream source
//! - `resume` - Resume a stream source

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::CoreError;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};

use super::types::{SourceInput, SourceOp, ToolOutput};

/// Tool for controlling data sources
#[derive(Clone)]
pub struct SourceTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for SourceTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl SourceTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    /// Handle list operation - enumerate all registered sources
    fn handle_list(&self) -> ToolOutput {
        let sources = self.ctx.sources();

        match sources {
            Some(manager) => {
                let streams = manager.list_streams();
                let block_sources = manager.list_block_sources();

                let stream_info: Vec<serde_json::Value> = streams
                    .iter()
                    .filter_map(|id| {
                        manager.get_stream_info(id).map(|info| {
                            json!({
                                "source_id": info.source_id,
                                "name": info.name,
                                "type": "stream",
                                "status": format!("{:?}", info.status),
                                "supports_pull": info.supports_pull,
                            })
                        })
                    })
                    .collect();

                let block_info: Vec<serde_json::Value> = block_sources
                    .iter()
                    .filter_map(|id| {
                        manager.get_block_source_info(id).map(|info| {
                            json!({
                                "source_id": info.source_id,
                                "name": info.name,
                                "type": "block",
                                "status": format!("{:?}", info.status),
                            })
                        })
                    })
                    .collect();

                let total = stream_info.len() + block_info.len();
                let all_sources: Vec<serde_json::Value> =
                    stream_info.into_iter().chain(block_info).collect();

                ToolOutput::success_with_data(
                    format!(
                        "Found {} sources ({} streams, {} block sources)",
                        total,
                        streams.len(),
                        block_sources.len()
                    ),
                    json!({ "sources": all_sources }),
                )
            }
            None => {
                // No source manager available (e.g., in test context)
                ToolOutput::success_with_data(
                    "No sources registered (source manager not available)",
                    json!({ "sources": [] }),
                )
            }
        }
    }

    /// Handle status operation - get status of a specific source
    fn handle_status(&self, source_id: Option<String>) -> crate::Result<ToolOutput> {
        let source_id = source_id.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "source",
                json!({"op": "status"}),
                "status requires 'source_id' parameter",
            )
        })?;

        let sources = self.ctx.sources();

        match sources {
            Some(manager) => {
                // Try stream sources first
                if let Some(info) = manager.get_stream_info(&source_id) {
                    return Ok(ToolOutput::success_with_data(
                        format!("Status for stream source '{}'", source_id),
                        json!({
                            "source_id": info.source_id,
                            "name": info.name,
                            "type": "stream",
                            "status": format!("{:?}", info.status),
                            "supports_pull": info.supports_pull,
                            "block_schemas": info.block_schemas.len(),
                        }),
                    ));
                }

                // Try block sources
                if let Some(info) = manager.get_block_source_info(&source_id) {
                    return Ok(ToolOutput::success_with_data(
                        format!("Status for block source '{}'", source_id),
                        json!({
                            "source_id": info.source_id,
                            "name": info.name,
                            "type": "block",
                            "status": format!("{:?}", info.status),
                            "permission_rules": info.permission_rules.len(),
                        }),
                    ));
                }

                Err(CoreError::tool_exec_msg(
                    "source",
                    json!({"op": "status", "source_id": source_id}),
                    format!("Source '{}' not found", source_id),
                ))
            }
            None => Err(CoreError::tool_exec_msg(
                "source",
                json!({"op": "status", "source_id": source_id}),
                "Source manager not available",
            )),
        }
    }

    /// Handle pause operation - pause a stream source
    async fn handle_pause(&self, source_id: Option<String>) -> crate::Result<ToolOutput> {
        let source_id = source_id.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "source",
                json!({"op": "pause"}),
                "pause requires 'source_id' parameter",
            )
        })?;

        let sources = self.ctx.sources();

        match sources {
            Some(manager) => {
                // Check if it's a stream source (pause only works on streams)
                if manager.get_stream_info(&source_id).is_some() {
                    manager.pause_stream(&source_id).await.map_err(|e| {
                        CoreError::tool_exec_msg(
                            "source",
                            json!({"op": "pause", "source_id": source_id}),
                            format!("Failed to pause stream '{}': {:?}", source_id, e),
                        )
                    })?;

                    Ok(ToolOutput::success(format!(
                        "Stream source '{}' paused",
                        source_id
                    )))
                } else if manager.get_block_source_info(&source_id).is_some() {
                    // Block sources cannot be paused
                    Err(CoreError::tool_exec_msg(
                        "source",
                        json!({"op": "pause", "source_id": source_id}),
                        format!(
                            "Source '{}' is a block source - only stream sources can be paused",
                            source_id
                        ),
                    ))
                } else {
                    Err(CoreError::tool_exec_msg(
                        "source",
                        json!({"op": "pause", "source_id": source_id}),
                        format!("Source '{}' not found", source_id),
                    ))
                }
            }
            None => Err(CoreError::tool_exec_msg(
                "source",
                json!({"op": "pause", "source_id": source_id}),
                "Source manager not available",
            )),
        }
    }

    /// Handle resume operation - resume a stream source
    async fn handle_resume(&self, source_id: Option<String>) -> crate::Result<ToolOutput> {
        let source_id = source_id.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "source",
                json!({"op": "resume"}),
                "resume requires 'source_id' parameter",
            )
        })?;

        let sources = self.ctx.sources();

        match sources {
            Some(manager) => {
                // Check if it's a stream source (resume only works on streams)
                if manager.get_stream_info(&source_id).is_some() {
                    manager.resume_stream(&source_id).await.map_err(|e| {
                        CoreError::tool_exec_msg(
                            "source",
                            json!({"op": "resume", "source_id": source_id}),
                            format!("Failed to resume stream '{}': {:?}", source_id, e),
                        )
                    })?;

                    Ok(ToolOutput::success(format!(
                        "Stream source '{}' resumed",
                        source_id
                    )))
                } else if manager.get_block_source_info(&source_id).is_some() {
                    // Block sources cannot be resumed
                    Err(CoreError::tool_exec_msg(
                        "source",
                        json!({"op": "resume", "source_id": source_id}),
                        format!(
                            "Source '{}' is a block source - only stream sources can be resumed",
                            source_id
                        ),
                    ))
                } else {
                    Err(CoreError::tool_exec_msg(
                        "source",
                        json!({"op": "resume", "source_id": source_id}),
                        format!("Source '{}' not found", source_id),
                    ))
                }
            }
            None => Err(CoreError::tool_exec_msg(
                "source",
                json!({"op": "resume", "source_id": source_id}),
                "Source manager not available",
            )),
        }
    }
}

#[async_trait]
impl AiTool for SourceTool {
    type Input = SourceInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "source"
    }

    fn description(&self) -> &str {
        "Control data sources. Operations:
- 'list': List all registered sources (streams and block sources)
- 'status': Get detailed status of a specific source (requires source_id)
- 'pause': Pause a stream source (requires source_id, only works for streams)
- 'resume': Resume a paused stream source (requires source_id, only works for streams)"
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some(
            "Use to monitor and control data source activity. Pause streams when you need to focus without interruptions.",
        )
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(
            self.name().to_string(),
            ToolRuleType::ContinueLoop,
        )]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["list", "status", "pause", "resume"]
    }

    async fn execute(
        &self,
        input: Self::Input,
        _meta: &ExecutionMeta,
    ) -> crate::Result<Self::Output> {
        match input.op {
            SourceOp::List => Ok(self.handle_list()),
            SourceOp::Status => self.handle_status(input.source_id),
            SourceOp::Pause => self.handle_pause(input.source_id).await,
            SourceOp::Resume => self.handle_resume(input.source_id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

    #[tokio::test]
    async fn test_source_tool_list_no_sources() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::List,
                    source_id: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        // MockToolContext returns None for sources(), so we get the "no sources" message
        assert!(result.success);
        assert!(result.message.contains("sources"));
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        let sources = data["sources"].as_array().unwrap();
        assert!(sources.is_empty());
    }

    #[tokio::test]
    async fn test_source_tool_status_requires_source_id() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::Status,
                    source_id: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("source_id"),
                    "Expected error about source_id, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_source_tool_pause_requires_source_id() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::Pause,
                    source_id: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("source_id"),
                    "Expected error about source_id, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_source_tool_resume_requires_source_id() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::Resume,
                    source_id: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("source_id"),
                    "Expected error about source_id, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_source_tool_status_no_manager() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::Status,
                    source_id: Some("nonexistent".to_string()),
                },
                &ExecutionMeta::default(),
            )
            .await;

        // MockToolContext returns None for sources(), so we get "not available" error
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("not available"),
                    "Expected error about manager not available, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_source_tool_pause_no_manager() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::Pause,
                    source_id: Some("some_stream".to_string()),
                },
                &ExecutionMeta::default(),
            )
            .await;

        // MockToolContext returns None for sources(), so we get "not available" error
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("not available"),
                    "Expected error about manager not available, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }
}
