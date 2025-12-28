//! BlockEdit tool for editing memory block contents
//!
//! This tool provides operations to edit block content:
//! - `append` - Append content to a text block
//! - `replace` - Find and replace text in a text block
//! - `patch` - Apply diff/patch (not yet implemented)
//! - `set_field` - Set a field value in a Map/Composite block

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::{
    CoreError, Result,
    memory::BlockSchema,
    runtime::ToolContext,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

use super::types::{BlockEditInput, BlockEditOp, ToolOutput};

/// Tool for editing memory block contents
#[derive(Clone)]
pub struct BlockEditTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for BlockEditTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockEditTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl BlockEditTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    /// Handle the append operation
    async fn handle_append(
        &self,
        label: &str,
        content: Option<String>,
    ) -> crate::Result<ToolOutput> {
        let content = content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "append", "label": label}),
                "append requires 'content' parameter",
            )
        })?;
        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "append", "label": label}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "append", "label": label}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // is_system = false since this is an agent operation
        doc.append(&content, false).map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "append", "label": label}),
                format!("Failed to append: {}", e),
            )
        })?;

        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "append", "label": label}),
                format!("Failed to persist block: {:?}", e),
            )
        })?;

        Ok(ToolOutput::success(format!(
            "Appended to block '{}'",
            label
        )))
    }

    /// Handle the replace operation
    async fn handle_replace(
        &self,
        label: &str,
        old: Option<String>,
        new: Option<String>,
    ) -> crate::Result<ToolOutput> {
        let old = old.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                "old is required for replace operation",
            )
        })?;
        let new = new.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                "new is required for replace operation",
            )
        })?;

        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        // Get the block document (single fetch instead of metadata + block)
        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "replace", "label": label}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "replace", "label": label}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // Check that the block has Text schema
        if doc.schema() != &BlockSchema::Text {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                format!(
                    "replace operation requires Text schema, but block '{}' has {:?} schema",
                    label,
                    doc.schema()
                ),
            ));
        }

        // Replace text in the document (is_system = false since this is an agent operation)
        let replaced = doc.replace_text(&old, &new, false).map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                format!("Failed to replace text in block '{}': {:?}", label, e),
            )
        })?;

        if !replaced {
            return Err(CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label, "old": old}),
                format!("Text '{}' not found in block '{}'", old, label),
            ));
        }

        // Persist the changes
        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "replace", "label": label}),
                format!("Failed to persist block '{}': {:?}", label, e),
            )
        })?;

        Ok(ToolOutput::success(format!(
            "Replaced '{}' with '{}' in block '{}'",
            old, new, label
        )))
    }

    /// Handle the patch operation (not yet implemented)
    async fn handle_patch(&self, label: &str, _patch: Option<String>) -> crate::Result<ToolOutput> {
        Err(CoreError::tool_exec_msg(
            "block_edit",
            json!({"op": "patch", "label": label}),
            "patch operation is not yet implemented",
        ))
    }

    /// Handle the set_field operation
    async fn handle_set_field(
        &self,
        label: &str,
        field: Option<String>,
        value: Option<serde_json::Value>,
    ) -> crate::Result<ToolOutput> {
        let field = field.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label}),
                "field is required for set_field operation",
            )
        })?;
        let value = value.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label, "field": field}),
                "value is required for set_field operation",
            )
        })?;

        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        // Get the block document (single fetch instead of metadata + block)
        let doc = memory
            .get_block(agent_id, label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "set_field", "label": label, "field": field}),
                    format!("Failed to get block '{}': {:?}", label, e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "set_field", "label": label, "field": field}),
                    format!("Block '{}' not found", label),
                )
            })?;

        // Check that the block has Map or Composite schema
        match doc.schema() {
            BlockSchema::Map { .. } | BlockSchema::Composite { .. } => {}
            _ => {
                return Err(CoreError::tool_exec_msg(
                    "block_edit",
                    json!({"op": "set_field", "label": label, "field": field}),
                    format!(
                        "set_field operation requires Map or Composite schema, but block '{}' has {:?} schema",
                        label,
                        doc.schema()
                    ),
                ));
            }
        }

        // Set the field (is_system = false since this is an agent operation)
        doc.set_field(&field, value.clone(), false).map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label, "field": field}),
                format!(
                    "Failed to set field '{}' in block '{}': {}",
                    field, label, e
                ),
            )
        })?;

        // Persist the changes
        memory.persist_block(agent_id, label).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "block_edit",
                json!({"op": "set_field", "label": label, "field": field}),
                format!("Failed to persist block '{}': {:?}", label, e),
            )
        })?;

        Ok(ToolOutput::success_with_data(
            format!("Set field '{}' in block '{}'", field, label),
            json!({
                "field": field,
                "value": value,
            }),
        ))
    }
}

#[async_trait]
impl AiTool for BlockEditTool {
    type Input = BlockEditInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "block_edit"
    }

    fn description(&self) -> &str {
        "Edit memory block contents. Operations:
- 'append': Append content to a text block (requires 'content' param)
- 'replace': Find and replace text in a text block (requires 'old' and 'new' params)
- 'patch': Apply diff/patch - not yet implemented
- 'set_field': Set a field value in a Map/Composite block (requires 'field' and 'value' params)"
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
        &["append", "replace", "patch", "set_field"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        match input.op {
            BlockEditOp::Append => self.handle_append(&input.label, input.content).await,
            BlockEditOp::Replace => {
                self.handle_replace(&input.label, input.old, input.new)
                    .await
            }
            BlockEditOp::Patch => self.handle_patch(&input.label, input.patch).await,
            BlockEditOp::SetField => {
                self.handle_set_field(&input.label, input.field, input.value)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{BlockSchema, BlockType, FieldDef, FieldType, MemoryStore};
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

    #[tokio::test]
    async fn test_block_edit_append() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        memory
            .create_block(
                "test-agent",
                "test_block",
                "A test block for append operation",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        // Set initial content
        memory
            .update_block_text("test-agent", "test_block", "Hello")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Append to the block
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Append,
                    label: "test_block".to_string(),
                    content: Some(", world!".to_string()),
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Appended"));

        // Verify the content was updated
        let content = memory
            .get_rendered_content("test-agent", "test_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[tokio::test]
    async fn test_block_edit_replace() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        memory
            .create_block(
                "test-agent",
                "replace_block",
                "A test block for replace operation",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        // Set initial content
        memory
            .update_block_text("test-agent", "replace_block", "Hello, world!")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Replace text in the block
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "replace_block".to_string(),
                    content: None,
                    old: Some("world".to_string()),
                    new: Some("universe".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Replaced"));

        // Verify the content was updated
        let content = memory
            .get_rendered_content("test-agent", "replace_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "Hello, universe!");
    }

    #[tokio::test]
    async fn test_block_edit_set_field() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map block with fields
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Name field".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: false,
                },
                FieldDef {
                    name: "count".to_string(),
                    description: "Count field".to_string(),
                    field_type: FieldType::Number,
                    required: false,
                    default: Some(serde_json::json!(0)),
                    read_only: false,
                },
            ],
        };

        memory
            .create_block(
                "test-agent",
                "map_block",
                "A test Map block",
                BlockType::Working,
                schema,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Set a field
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::SetField,
                    label: "map_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: Some("name".to_string()),
                    value: Some(serde_json::json!("Alice")),
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.contains("Set field"));

        // Verify the field was set
        let doc = memory
            .get_block("test-agent", "map_block")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.get_field("name"), Some(serde_json::json!("Alice")));
    }

    #[tokio::test]
    async fn test_block_edit_rejects_readonly_field() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map block with a read-only field
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "status".to_string(),
                    description: "Status field".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: true, // Read-only!
                },
                FieldDef {
                    name: "notes".to_string(),
                    description: "Notes field".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        };

        memory
            .create_block(
                "test-agent",
                "readonly_block",
                "A block with read-only field",
                BlockType::Working,
                schema,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to set the read-only field - should fail
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::SetField,
                    label: "readonly_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: Some("status".to_string()),
                    value: Some(serde_json::json!("active")),
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        // Should fail with an error
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("read-only"),
                    "Expected error about read-only field, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_replace_text_not_found() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        memory
            .create_block(
                "test-agent",
                "notfound_block",
                "A test block",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        // Set initial content
        memory
            .update_block_text("test-agent", "notfound_block", "Hello, world!")
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to replace text that doesn't exist
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "notfound_block".to_string(),
                    content: None,
                    old: Some("goodbye".to_string()),
                    new: Some("hello".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        // Should fail with an error
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
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
    async fn test_block_edit_patch_not_implemented() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a text block
        memory
            .create_block(
                "test-agent",
                "patch_block",
                "A test block",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to use patch - should error
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Patch,
                    label: "patch_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: Some("some patch".to_string()),
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("not yet implemented"),
                    "Expected 'not yet implemented' error, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_replace_requires_text_schema() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Map block (not Text)
        let schema = BlockSchema::Map {
            fields: vec![FieldDef {
                name: "value".to_string(),
                description: "Value field".to_string(),
                field_type: FieldType::Text,
                required: true,
                default: None,
                read_only: false,
            }],
        };

        memory
            .create_block(
                "test-agent",
                "map_replace_block",
                "A Map block",
                BlockType::Working,
                schema,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to replace on a Map block - should fail
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Replace,
                    label: "map_replace_block".to_string(),
                    content: None,
                    old: Some("old".to_string()),
                    new: Some("new".to_string()),
                    field: None,
                    value: None,
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Text schema"),
                    "Expected error about Text schema, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_set_field_requires_map_or_composite() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a Text block (not Map or Composite)
        memory
            .create_block(
                "test-agent",
                "text_set_block",
                "A Text block",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        let tool = BlockEditTool::new(ctx);

        // Try to set_field on a Text block - should fail
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::SetField,
                    label: "text_set_block".to_string(),
                    content: None,
                    old: None,
                    new: None,
                    field: Some("field".to_string()),
                    value: Some(serde_json::json!("value")),
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("Map or Composite"),
                    "Expected error about Map or Composite schema, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_block_edit_block_not_found() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;

        let tool = BlockEditTool::new(ctx);

        // Try to append to non-existent block
        let result = tool
            .execute(
                BlockEditInput {
                    op: BlockEditOp::Append,
                    label: "nonexistent".to_string(),
                    content: Some("content".to_string()),
                    old: None,
                    new: None,
                    field: None,
                    value: None,
                    patch: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
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
