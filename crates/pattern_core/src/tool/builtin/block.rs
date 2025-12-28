//! Block tool for memory block lifecycle management
//!
//! This tool provides operations to manage block lifecycle:
//! - `load` - Load block into working context
//! - `pin` - Pin block to retain across batches
//! - `unpin` - Unpin block (becomes ephemeral)
//! - `archive` - Change block type to Archival
//! - `info` - Get block metadata

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::{
    Result,
    memory::BlockType,
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
- 'info': Get block metadata (type, pinned status, char limit, etc.)"
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
        &["load", "pin", "unpin", "archive", "info"]
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

                // Load block by label - just verify it exists
                match memory.get_block_metadata(agent_id, &input.label).await {
                    Ok(Some(metadata)) => Ok(ToolOutput::success_with_data(
                        format!("Block '{}' loaded into context", input.label),
                        json!({
                            "label": metadata.label,
                            "description": metadata.description,
                            "block_type": format!("{:?}", metadata.block_type),
                            "pinned": metadata.pinned,
                            "char_limit": metadata.char_limit,
                        }),
                    )),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{BlockSchema, BlockType, MemoryStore};
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

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
                BlockSchema::Text,
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
                BlockSchema::Text,
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
                BlockSchema::Text,
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
                BlockSchema::Text,
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
                BlockSchema::Text,
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
}
