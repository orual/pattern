//! Block tool for memory block lifecycle management
//!
//! This tool provides operations to manage block lifecycle:
//! - `load` - Load block into working context
//! - `pin` - Pin block to retain across batches
//! - `unpin` - Unpin block (becomes ephemeral)
//! - `archive` - Change block type to Archival
//! - `info` - Get block metadata
//! - `viewport` - Set display window for Text blocks (start_line, display_lines)
//! - `share` - Share block with another agent by name (optional permission, default: Append)
//! - `unshare` - Remove sharing from another agent by name

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::{
    Result,
    memory::{BlockSchema, BlockType, TextViewport},
    runtime::ToolContext,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

use super::types::{BlockInput, BlockOp, ToolOutput};

/// Tool for managing memory block lifecycle
#[derive(Clone)]
pub struct BlockTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for BlockTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl BlockTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl AiTool for BlockTool {
    type Input = BlockInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "block"
    }

    fn description(&self) -> &str {
        "Manage memory block lifecycle. Operations:
- 'load': Load a block into working context by label
- 'pin': Pin block to retain across message batches (always in context)
- 'unpin': Unpin block (becomes ephemeral, only loads when referenced)
- 'archive': Change block type to Archival (cannot archive Core blocks)
- 'info': Get block metadata (type, pinned status, char limit, etc.)
- 'viewport': Set display window for Text blocks (requires 'start_line' and 'display_lines')
- 'share': Share block with another agent by name (requires 'target_agent', optional 'permission' defaults to Append)
- 'unshare': Remove sharing from another agent (requires 'target_agent')"
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("the conversation will be continued when called")
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(
            self.name().to_string(),
            ToolRuleType::ContinueLoop,
        )]
    }

    fn operations(&self) -> &'static [&'static str] {
        &[
            "load", "pin", "unpin", "archive", "info", "viewport", "share", "unshare",
        ]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        match input.op {
            BlockOp::Load => {
                // If source_id is provided, tell user to use source-specific tool
                if input.source_id.is_some() {
                    return Err(crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "load", "label": input.label}),
                        "Loading from a specific source requires the source-specific tool. \
                         Use 'block' tool with just 'label' to load by label.",
                    ));
                }

                // Load block by label and print it
                match memory.get_block_metadata(agent_id, &input.label).await {
                    Ok(Some(metadata)) => {
                        let block = memory
                            .get_rendered_content(agent_id, &input.label)
                            .await
                            .ok()
                            .flatten();
                        Ok(ToolOutput::success_with_data(
                            format!("Block '{}' loaded into context", input.label),
                            json!({
                                "label": metadata.label,
                                "description": metadata.description,
                                "block_type": format!("{:?}", metadata.block_type),
                                "pinned": metadata.pinned,
                                "char_limit": metadata.char_limit,
                                "content": block.unwrap_or_default()
                            }),
                        ))
                    }
                    Ok(None) => Err(crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "load", "label": input.label}),
                        format!("Block '{}' not found", input.label),
                    )),
                    Err(e) => Err(crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "load", "label": input.label}),
                        format!("Failed to load block '{}': {:?}", input.label, e),
                    )),
                }
            }

            BlockOp::Pin => match memory.set_block_pinned(agent_id, &input.label, true).await {
                Ok(()) => Ok(ToolOutput::success(format!(
                    "Block '{}' pinned - will be retained across message batches",
                    input.label
                ))),
                Err(e) => Err(crate::CoreError::tool_exec_msg(
                    "block",
                    serde_json::json!({"op": "pin", "label": input.label}),
                    format!("Failed to pin block '{}': {:?}", input.label, e),
                )),
            },

            BlockOp::Unpin => match memory.set_block_pinned(agent_id, &input.label, false).await {
                Ok(()) => Ok(ToolOutput::success(format!(
                    "Block '{}' unpinned - now ephemeral (loads only when referenced)",
                    input.label
                ))),
                Err(e) => Err(crate::CoreError::tool_exec_msg(
                    "block",
                    serde_json::json!({"op": "unpin", "label": input.label}),
                    format!("Failed to unpin block '{}': {:?}", input.label, e),
                )),
            },

            BlockOp::Archive => {
                // First check if block exists and is not Core type
                match memory.get_block_metadata(agent_id, &input.label).await {
                    Ok(Some(metadata)) => {
                        if metadata.block_type == BlockType::Core {
                            return Err(crate::CoreError::tool_exec_msg(
                                "block",
                                serde_json::json!({"op": "archive", "label": input.label}),
                                format!(
                                    "Cannot archive Core block '{}'. Core blocks are essential for agent identity.",
                                    input.label
                                ),
                            ));
                        }

                        // Change block type to Archival
                        match memory
                            .set_block_type(agent_id, &input.label, BlockType::Archival)
                            .await
                        {
                            Ok(()) => Ok(ToolOutput::success(format!(
                                "Block '{}' archived - now stored in archival memory",
                                input.label
                            ))),
                            Err(e) => Err(crate::CoreError::tool_exec_msg(
                                "block",
                                serde_json::json!({"op": "archive", "label": input.label}),
                                format!("Failed to archive block '{}': {:?}", input.label, e),
                            )),
                        }
                    }
                    Ok(None) => Err(crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "archive", "label": input.label}),
                        format!("Block '{}' not found", input.label),
                    )),
                    Err(e) => Err(crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "archive", "label": input.label}),
                        format!(
                            "Failed to get block metadata for '{}': {:?}",
                            input.label, e
                        ),
                    )),
                }
            }

            BlockOp::Info => match memory.get_block_metadata(agent_id, &input.label).await {
                Ok(Some(metadata)) => Ok(ToolOutput::success_with_data(
                    format!("Metadata for block '{}'", input.label),
                    json!({
                        "id": metadata.id,
                        "label": metadata.label,
                        "description": metadata.description,
                        "block_type": format!("{:?}", metadata.block_type),
                        "schema": format!("{:?}", metadata.schema),
                        "char_limit": metadata.char_limit,
                        "permission": format!("{:?}", metadata.permission),
                        "pinned": metadata.pinned,
                        "created_at": metadata.created_at.to_rfc3339(),
                        "updated_at": metadata.updated_at.to_rfc3339(),
                    }),
                )),
                Ok(None) => Err(crate::CoreError::tool_exec_msg(
                    "block",
                    serde_json::json!({"op": "info", "label": input.label}),
                    format!("Block '{}' not found", input.label),
                )),
                Err(e) => Err(crate::CoreError::tool_exec_msg(
                    "block",
                    serde_json::json!({"op": "info", "label": input.label}),
                    format!("Failed to get block info for '{}': {:?}", input.label, e),
                )),
            },

            BlockOp::Viewport => {
                // Get required parameters
                let start_line = input.start_line.unwrap_or(1);
                let display_lines = input.display_lines.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "viewport", "label": input.label}),
                        "viewport requires 'display_lines' parameter",
                    )
                })?;

                // Get current block metadata to check schema type
                let metadata = memory
                    .get_block_metadata(agent_id, &input.label)
                    .await
                    .map_err(|e| {
                        crate::CoreError::tool_exec_msg(
                            "block",
                            serde_json::json!({"op": "viewport", "label": input.label}),
                            format!("Failed to get block metadata: {:?}", e),
                        )
                    })?
                    .ok_or_else(|| {
                        crate::CoreError::tool_exec_msg(
                            "block",
                            serde_json::json!({"op": "viewport", "label": input.label}),
                            format!("Block '{}' not found", input.label),
                        )
                    })?;

                // Verify this is a Text block
                if !metadata.schema.is_text() {
                    return Err(crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "viewport", "label": input.label}),
                        format!(
                            "viewport only applies to Text blocks, but '{}' has schema {:?}",
                            input.label, metadata.schema
                        ),
                    ));
                }

                // Create new schema with viewport
                let new_schema = BlockSchema::Text {
                    viewport: Some(TextViewport {
                        start_line,
                        display_lines,
                    }),
                };

                // Update the schema
                memory
                    .update_block_schema(agent_id, &input.label, new_schema)
                    .await
                    .map_err(|e| {
                        crate::CoreError::tool_exec_msg(
                            "block",
                            serde_json::json!({"op": "viewport", "label": input.label}),
                            format!("Failed to update viewport: {:?}", e),
                        )
                    })?;

                Ok(ToolOutput::success(format!(
                    "Set viewport for block '{}': lines {}-{} (showing {} lines)",
                    input.label,
                    start_line,
                    start_line + display_lines - 1,
                    display_lines
                )))
            }

            BlockOp::Share => {
                // Get target agent name
                let target_agent = input.target_agent.as_ref().ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "share", "label": input.label}),
                        "target_agent is required for share operation",
                    )
                })?;

                // Get shared block manager
                let shared_blocks = self.ctx.shared_blocks().ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "share", "label": input.label}),
                        "Block sharing is not available in this context",
                    )
                })?;

                // Default to Append permission
                let permission = input
                    .permission
                    .unwrap_or(crate::memory::MemoryPermission::Append);

                // Share the block by name
                let target_id = shared_blocks
                    .share_block_by_name(agent_id, &input.label, target_agent, permission.into())
                    .await
                    .map_err(|e| {
                        crate::CoreError::tool_exec_msg(
                            "block",
                            serde_json::json!({"op": "share", "label": input.label, "target_agent": target_agent}),
                            format!("Failed to share block: {:?}", e),
                        )
                    })?;

                Ok(ToolOutput::success_with_data(
                    format!(
                        "Shared block '{}' with agent '{}' ({:?} permission)",
                        input.label, target_agent, permission
                    ),
                    serde_json::json!({
                        "target_agent_id": target_id,
                        "permission": format!("{:?}", permission)
                    }),
                ))
            }

            BlockOp::Unshare => {
                // Get target agent name
                let target_agent = input.target_agent.as_ref().ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "unshare", "label": input.label}),
                        "target_agent is required for unshare operation",
                    )
                })?;

                // Get shared block manager
                let shared_blocks = self.ctx.shared_blocks().ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "block",
                        serde_json::json!({"op": "unshare", "label": input.label}),
                        "Block sharing is not available in this context",
                    )
                })?;

                // Unshare the block by name
                let target_id = shared_blocks
                    .unshare_block_by_name(agent_id, &input.label, target_agent)
                    .await
                    .map_err(|e| {
                        crate::CoreError::tool_exec_msg(
                            "block",
                            serde_json::json!({"op": "unshare", "label": input.label, "target_agent": target_agent}),
                            format!("Failed to unshare block: {:?}", e),
                        )
                    })?;

                Ok(ToolOutput::success_with_data(
                    format!(
                        "Removed sharing of block '{}' from agent '{}'",
                        input.label, target_agent
                    ),
                    serde_json::json!({
                        "target_agent_id": target_id
                    }),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{BlockSchema, BlockType, MemoryStore};
    use crate::tool::builtin::test_utils::{
        create_test_agent_in_db, create_test_context_with_agent,
    };

    #[tokio::test]
    async fn test_block_tool_info_operation() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a block to get info on
        memory
            .create_block(
                "test-agent",
                "test_block",
                "A test block for info operation",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx);

        // Test info operation
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Info,
                    label: "test_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Metadata for block"));

        let data = result.data.unwrap();
        assert_eq!(data["label"], "test_block");
        assert_eq!(data["description"], "A test block for info operation");
        assert_eq!(data["block_type"], "Working");
        assert_eq!(data["char_limit"], 2000);
        assert!(!data["pinned"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_block_tool_pin_unpin() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a block
        memory
            .create_block(
                "test-agent",
                "pin_test",
                "Block for pin/unpin test",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx.clone());

        // Initially not pinned
        let metadata = memory
            .get_block_metadata("test-agent", "pin_test")
            .await
            .unwrap()
            .unwrap();
        assert!(!metadata.pinned);

        // Pin the block
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Pin,
                    label: "pin_test".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("pinned"));

        // Verify pinned
        let metadata = memory
            .get_block_metadata("test-agent", "pin_test")
            .await
            .unwrap()
            .unwrap();
        assert!(metadata.pinned);

        // Unpin the block
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Unpin,
                    label: "pin_test".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("unpinned"));

        // Verify not pinned
        let metadata = memory
            .get_block_metadata("test-agent", "pin_test")
            .await
            .unwrap()
            .unwrap();
        assert!(!metadata.pinned);
    }

    #[tokio::test]
    async fn test_block_tool_archive() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Working block
        memory
            .create_block(
                "test-agent",
                "archive_test",
                "Block for archive test",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx.clone());

        // Initially Working type
        let metadata = memory
            .get_block_metadata("test-agent", "archive_test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metadata.block_type, BlockType::Working);

        // Archive the block
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Archive,
                    label: "archive_test".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("archived"));

        // Verify type changed to Archival
        let metadata = memory
            .get_block_metadata("test-agent", "archive_test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metadata.block_type, BlockType::Archival);
    }

    #[tokio::test]
    async fn test_block_tool_cannot_archive_core() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Core block
        memory
            .create_block(
                "test-agent",
                "core_block",
                "A core block that cannot be archived",
                BlockType::Core,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx);

        // Try to archive Core block - should fail with Err
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Archive,
                    label: "core_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Cannot archive Core block"),
                    "Expected error about Core block, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }

        // Verify type unchanged
        let metadata = memory
            .get_block_metadata("test-agent", "core_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metadata.block_type, BlockType::Core);
    }

    #[tokio::test]
    async fn test_block_tool_load_operation() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a block
        memory
            .create_block(
                "test-agent",
                "load_test",
                "Block for load test",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx);

        // Load the block
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Load,
                    label: "load_test".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("loaded"));
    }

    #[tokio::test]
    async fn test_block_tool_load_with_source_id_error() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = BlockTool::new(ctx);

        // Try to load with source_id - should error
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Load,
                    label: "some_block".to_string(),
                    source_id: Some("source_123".to_string()),
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("source-specific tool"),
                    "Expected error about source-specific tool, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_tool_not_found() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = BlockTool::new(ctx);

        // Try to get info on non-existent block - should error
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Info,
                    label: "nonexistent".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("not found"),
                    "Expected error about not found, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_tool_viewport() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block with some content
        let doc = memory
            .create_block(
                "test-agent",
                "viewport_test",
                "Block for viewport test",
                BlockType::Working,
                BlockSchema::text(),
                5000,
            )
            .await
            .unwrap();

        // Add multi-line content
        doc.set_text(
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9\nLine 10",
            true,
        )
        .unwrap();
        memory
            .persist_block("test-agent", "viewport_test")
            .await
            .unwrap();

        let tool = BlockTool::new(ctx.clone());

        // Set viewport to show lines 3-5
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Viewport,
                    label: "viewport_test".to_string(),
                    source_id: None,
                    start_line: Some(3),
                    display_lines: Some(3),
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("lines 3-5"));

        // Verify the schema was updated
        let metadata = memory
            .get_block_metadata("test-agent", "viewport_test")
            .await
            .unwrap()
            .unwrap();
        match &metadata.schema {
            BlockSchema::Text { viewport } => {
                let vp = viewport.as_ref().unwrap();
                assert_eq!(vp.start_line, 3);
                assert_eq!(vp.display_lines, 3);
            }
            other => panic!("Expected Text schema, got: {:?}", other),
        }

        // Verify rendered content respects viewport
        let rendered = memory
            .get_rendered_content("test-agent", "viewport_test")
            .await
            .unwrap()
            .unwrap();
        assert!(rendered.contains("Line 3"), "Should contain Line 3");
        assert!(rendered.contains("Line 4"), "Should contain Line 4");
        assert!(rendered.contains("Line 5"), "Should contain Line 5");
        assert!(!rendered.contains("Line 1\n"), "Should not contain Line 1");
        assert!(!rendered.contains("Line 10"), "Should not contain Line 10");
    }

    #[tokio::test]
    async fn test_block_tool_viewport_requires_text_schema() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map schema block
        memory
            .create_block(
                "test-agent",
                "map_block",
                "A map block",
                BlockType::Working,
                BlockSchema::Map { fields: vec![] },
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx);

        // Try to set viewport on non-Text block - should fail
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Viewport,
                    label: "map_block".to_string(),
                    source_id: None,
                    start_line: Some(1),
                    display_lines: Some(10),
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("only applies to Text blocks"),
                    "Expected error about Text blocks, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_tool_share_operation() {
        // Create two agents
        let (dbs, memory, ctx1) = create_test_context_with_agent("agent-1").await;
        create_test_agent_in_db(&dbs, "agent-2").await;

        // Create a block for agent-1
        memory
            .create_block(
                "agent-1",
                "shared_block",
                "A block to share",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx1);

        // Share the block with agent-2 (default permission: Append)
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Share,
                    label: "shared_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: Some("Test Agent agent-2".to_string()),
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Shared block"));
        assert!(result.message.contains("Test Agent agent-2"));
        assert!(result.message.contains("Append"));

        let data = result.data.unwrap();
        assert_eq!(data["target_agent_id"], "agent-2");
    }

    #[tokio::test]
    async fn test_block_tool_share_with_explicit_permission() {
        use crate::memory::MemoryPermission;

        // Create two agents
        let (dbs, memory, ctx1) = create_test_context_with_agent("agent-1").await;
        create_test_agent_in_db(&dbs, "agent-2").await;

        // Create a block for agent-1
        memory
            .create_block(
                "agent-1",
                "rw_block",
                "A block to share with read-write",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx1);

        // Share with ReadWrite permission
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Share,
                    label: "rw_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: Some("Test Agent agent-2".to_string()),
                    permission: Some(MemoryPermission::ReadWrite),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("ReadWrite"));
    }

    #[tokio::test]
    async fn test_block_tool_unshare_operation() {
        // Create two agents
        let (dbs, memory, ctx1) = create_test_context_with_agent("agent-1").await;
        create_test_agent_in_db(&dbs, "agent-2").await;

        // Create a block for agent-1
        memory
            .create_block(
                "agent-1",
                "unshare_block",
                "A block to share then unshare",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx1);

        // First share the block
        tool.execute(
            BlockInput {
                op: BlockOp::Share,
                label: "unshare_block".to_string(),
                source_id: None,
                start_line: None,
                display_lines: None,
                target_agent: Some("Test Agent agent-2".to_string()),
                permission: None,
            },
            &ExecutionMeta::default(),
        )
        .await
        .unwrap();

        // Now unshare it
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Unshare,
                    label: "unshare_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: Some("Test Agent agent-2".to_string()),
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Removed sharing"));
        assert!(result.message.contains("Test Agent agent-2"));

        let data = result.data.unwrap();
        assert_eq!(data["target_agent_id"], "agent-2");
    }

    #[tokio::test]
    async fn test_block_tool_share_missing_target_agent() {
        let (_dbs, memory, ctx) = create_test_context_with_agent("agent-1").await;

        // Create a block
        memory
            .create_block(
                "agent-1",
                "some_block",
                "A block",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx);

        // Try to share without target_agent - should fail
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Share,
                    label: "some_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: None,
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("target_agent is required"),
                    "Expected error about target_agent, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_tool_share_agent_not_found() {
        let (_dbs, memory, ctx) = create_test_context_with_agent("agent-1").await;

        // Create a block
        memory
            .create_block(
                "agent-1",
                "some_block",
                "A block",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let tool = BlockTool::new(ctx);

        // Try to share with non-existent agent
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Share,
                    label: "some_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: Some("NonExistentAgent".to_string()),
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Agent not found"),
                    "Expected error about agent not found, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_tool_share_block_not_found() {
        let (dbs, _memory, ctx) = create_test_context_with_agent("agent-1").await;
        create_test_agent_in_db(&dbs, "agent-2").await;

        let tool = BlockTool::new(ctx);

        // Try to share non-existent block
        let result = tool
            .execute(
                BlockInput {
                    op: BlockOp::Share,
                    label: "nonexistent_block".to_string(),
                    source_id: None,
                    start_line: None,
                    display_lines: None,
                    target_agent: Some("Test Agent agent-2".to_string()),
                    permission: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Block not found"),
                    "Expected error about block not found, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }
}
