//! FileTool - Agent-facing interface to FileSource operations.
//!
//! This tool provides file operations for agents:
//! - `load` - Load file from disk into a memory block
//! - `save` - Save block content back to disk
//! - `create` - Create a new file
//! - `delete` - Delete a file (requires escalation)
//! - `append` - Append content to a file
//! - `replace` - Find and replace text in a file

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::data_source::{DataBlock, FileSource};
use crate::id::AgentId;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};
use crate::{CoreError, Result};

use super::types::{FileInput, FileOp, ToolOutput};

/// Tool for file operations via FileSource.
#[derive(Clone)]
pub struct FileTool {
    ctx: Arc<dyn ToolContext>,
    source: Arc<FileSource>,
}

impl std::fmt::Debug for FileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTool")
            .field("agent_id", &self.ctx.agent_id())
            .field("source_id", &self.source.source_id())
            .finish()
    }
}

impl FileTool {
    /// Create a new FileTool with the given context and file source.
    pub fn new(ctx: Arc<dyn ToolContext>, source: Arc<FileSource>) -> Self {
        Self { ctx, source }
    }

    /// Get the agent ID from context.
    fn agent_id(&self) -> AgentId {
        AgentId::new(self.ctx.agent_id())
    }

    /// Handle load operation - load file from disk into block.
    async fn handle_load(&self, path: Option<&str>) -> Result<ToolOutput> {
        let path = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "load"}),
                "load requires 'path' parameter",
            )
        })?;

        let block_ref = self
            .source
            .load(
                Path::new(path),
                Arc::clone(&self.ctx) as Arc<dyn ToolContext>,
                self.agent_id(),
            )
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "load", "path": path}),
                    format!("Failed to load file '{}': {}", path, e),
                )
            })?;

        Ok(ToolOutput::success_with_data(
            format!("Loaded file '{}' into block '{}'", path, block_ref.label),
            json!({
                "label": block_ref.label,
                "block_id": block_ref.block_id,
                "path": path,
            }),
        ))
    }

    /// Handle save operation - save block content to disk.
    async fn handle_save(&self, path: Option<&str>, label: Option<&str>) -> Result<ToolOutput> {
        // Need either path or label to identify the block
        let (block_label, file_path) = match (path, label) {
            (Some(p), _) => {
                // Generate the label without reloading from disk (to preserve in-memory edits)
                let generated_label = self.source.make_label(Path::new(p)).map_err(|e| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "save", "path": p}),
                        format!("Failed to generate label for '{}': {}", p, e),
                    )
                })?;
                (generated_label, p.to_string())
            }
            (None, Some(l)) => {
                // Use label directly
                (l.to_string(), l.to_string())
            }
            (None, None) => {
                return Err(CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save"}),
                    "save requires either 'path' or 'label' parameter",
                ));
            }
        };

        // Get the block_id from memory store
        let memory = self.ctx.memory();
        let agent_id = self.ctx.agent_id();
        let metadata = memory
            .get_block_metadata(agent_id, &block_label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save", "label": &block_label}),
                    format!(
                        "Failed to get block metadata for '{}': {:?}",
                        block_label, e
                    ),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save", "label": &block_label}),
                    format!(
                        "Block '{}' not found in memory. Load the file first.",
                        block_label
                    ),
                )
            })?;

        // Create BlockRef with the actual block_id from memory
        let block_ref =
            crate::data_source::BlockRef::new(&block_label, &metadata.id).owned_by(agent_id);

        self.source
            .save(&block_ref, Arc::clone(&self.ctx) as Arc<dyn ToolContext>)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save", "label": block_label}),
                    format!("Failed to save block '{}': {}", block_label, e),
                )
            })?;

        Ok(ToolOutput::success(format!(
            "Saved block '{}' to disk",
            file_path
        )))
    }

    /// Handle create operation - create a new file.
    async fn handle_create(&self, path: Option<&str>, content: Option<&str>) -> Result<ToolOutput> {
        let path = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "create"}),
                "create requires 'path' parameter",
            )
        })?;

        let block_ref = self
            .source
            .create(
                Path::new(path),
                content,
                Arc::clone(&self.ctx) as Arc<dyn ToolContext>,
                self.agent_id(),
            )
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "create", "path": path}),
                    format!("Failed to create file '{}': {}", path, e),
                )
            })?;

        Ok(ToolOutput::success_with_data(
            format!("Created file '{}' with block '{}'", path, block_ref.label),
            json!({
                "label": block_ref.label,
                "block_id": block_ref.block_id,
                "path": path,
            }),
        ))
    }

    /// Handle delete operation - delete a file.
    async fn handle_delete(&self, path: Option<&str>) -> Result<ToolOutput> {
        let path = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "delete"}),
                "delete requires 'path' parameter",
            )
        })?;

        self.source
            .delete(
                Path::new(path),
                Arc::clone(&self.ctx) as Arc<dyn ToolContext>,
            )
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "delete", "path": path}),
                    format!("Failed to delete file '{}': {}", path, e),
                )
            })?;

        Ok(ToolOutput::success(format!("Deleted file '{}'", path)))
    }

    /// Handle append operation - append content to a file.
    /// Auto-loads the file if not already loaded.
    async fn handle_append(&self, path: Option<&str>, content: Option<&str>) -> Result<ToolOutput> {
        let path = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "append"}),
                "append requires 'path' parameter",
            )
        })?;
        let content = content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "append", "path": path}),
                "append requires 'content' parameter",
            )
        })?;

        // Auto-load the file if not already loaded
        let block_ref = self
            .source
            .load(
                Path::new(path),
                Arc::clone(&self.ctx) as Arc<dyn ToolContext>,
                self.agent_id(),
            )
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "append", "path": path}),
                    format!("Failed to load file for append '{}': {}", path, e),
                )
            })?;

        // Append to the block
        let memory = self.ctx.memory();
        memory
            .append_to_block(&block_ref.agent_id, &block_ref.label, content)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "append", "path": path}),
                    format!("Failed to append to block '{}': {:?}", block_ref.label, e),
                )
            })?;

        Ok(ToolOutput::success(format!(
            "Appended content to file '{}' (block '{}'). Use 'save' to write to disk.",
            path, block_ref.label
        )))
    }

    /// Handle replace operation - find and replace text in a file.
    /// Auto-loads the file if not already loaded.
    async fn handle_replace(
        &self,
        path: Option<&str>,
        old: Option<&str>,
        new: Option<&str>,
    ) -> Result<ToolOutput> {
        let path = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace"}),
                "replace requires 'path' parameter",
            )
        })?;
        let old = old.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path}),
                "replace requires 'old' parameter",
            )
        })?;
        let new = new.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path}),
                "replace requires 'new' parameter",
            )
        })?;

        // Auto-load the file if not already loaded
        let block_ref = self
            .source
            .load(
                Path::new(path),
                Arc::clone(&self.ctx) as Arc<dyn ToolContext>,
                self.agent_id(),
            )
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "replace", "path": path}),
                    format!("Failed to load file for replace '{}': {}", path, e),
                )
            })?;

        // Replace in the block
        let memory = self.ctx.memory();
        let replaced = memory
            .replace_in_block(&block_ref.agent_id, &block_ref.label, old, new)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "replace", "path": path}),
                    format!("Failed to replace in block '{}': {:?}", block_ref.label, e),
                )
            })?;

        if replaced {
            Ok(ToolOutput::success(format!(
                "Replaced '{}' with '{}' in file '{}' (block '{}'). Use 'save' to write to disk.",
                old, new, path, block_ref.label
            )))
        } else {
            Err(CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path, "old": old}),
                format!("Text '{}' not found in file '{}'", old, path),
            ))
        }
    }
}

#[async_trait]
impl AiTool for FileTool {
    type Input = FileInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "File operations for loading, saving, and editing local files. Operations:
- 'load': Load file from disk into a memory block (requires 'path')
- 'save': Save block content to disk (requires 'path' or 'label')
- 'create': Create a new file (requires 'path', optional 'content')
- 'delete': Delete a file (requires 'path', requires escalation)
- 'append': Append content to a file (requires 'path' and 'content', auto-loads if needed)
- 'replace': Find and replace text in a file (requires 'path', 'old', and 'new', auto-loads if needed)

Note: 'append' and 'replace' modify the in-memory block. Use 'save' to write changes to disk."
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
        &["load", "save", "create", "delete", "append", "replace"]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        match input.op {
            FileOp::Load => self.handle_load(input.path.as_deref()).await,
            FileOp::Save => {
                self.handle_save(input.path.as_deref(), input.label.as_deref())
                    .await
            }
            FileOp::Create => {
                self.handle_create(input.path.as_deref(), input.content.as_deref())
                    .await
            }
            FileOp::Delete => self.handle_delete(input.path.as_deref()).await,
            FileOp::Append => {
                self.handle_append(input.path.as_deref(), input.content.as_deref())
                    .await
            }
            FileOp::Replace => {
                self.handle_replace(
                    input.path.as_deref(),
                    input.old.as_deref(),
                    input.new.as_deref(),
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::builtin::create_test_context_with_agent;
    use tempfile::TempDir;

    /// Create a test file in the temp directory
    async fn create_test_file(
        dir: &std::path::Path,
        name: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let path = dir.join(name);
        tokio::fs::write(&path, content).await.unwrap();
        path
    }

    #[tokio::test]
    async fn test_file_tool_load() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let test_content = "Hello, World!\nThis is a test file.";
        create_test_file(&base_path, "test.txt", test_content).await;

        // Create FileSource and tool
        let source = Arc::new(FileSource::new("test_files", &base_path));
        let (_dbs, _memory, ctx) = create_test_context_with_agent("test_agent_file_load").await;
        let tool = FileTool::new(ctx, source);

        // Load the file
        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Load,
                    path: Some("test.txt".to_string()),
                    label: None,
                    content: None,
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success, "Load should succeed: {:?}", result.message);
        assert!(result.message.contains("Loaded"));
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        assert!(data["label"].as_str().unwrap().contains("test.txt"));
    }

    #[tokio::test]
    async fn test_file_tool_create_and_append() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create FileSource and tool
        let source = Arc::new(FileSource::new("test_files", &base_path));
        let (_dbs, _memory, ctx) = create_test_context_with_agent("test_agent_file_create").await;
        let tool = FileTool::new(ctx, source);

        // Create a new file
        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Create,
                    path: Some("new_file.txt".to_string()),
                    label: None,
                    content: Some("Initial content".to_string()),
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "Create should succeed: {:?}",
            result.message
        );
        assert!(result.message.contains("Created"));

        // Verify file exists on disk
        let file_path = base_path.join("new_file.txt");
        assert!(file_path.exists(), "File should exist on disk");

        // Append to the file
        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Append,
                    path: Some("new_file.txt".to_string()),
                    label: None,
                    content: Some("\nAppended content".to_string()),
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "Append should succeed: {:?}",
            result.message
        );
        assert!(result.message.contains("Appended"));

        // Save the file
        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Save,
                    path: Some("new_file.txt".to_string()),
                    label: None,
                    content: None,
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success, "Save should succeed: {:?}", result.message);

        // Verify disk content
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert!(
            disk_content.contains("Initial content"),
            "Should contain initial content"
        );
        assert!(
            disk_content.contains("Appended content"),
            "Should contain appended content"
        );
    }

    #[tokio::test]
    async fn test_file_tool_replace() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let test_content = "Hello, World!";
        create_test_file(&base_path, "replace_test.txt", test_content).await;

        // Create FileSource and tool
        let source = Arc::new(FileSource::new("test_files", &base_path));
        let (_dbs, _memory, ctx) = create_test_context_with_agent("test_agent_file_replace").await;
        let tool = FileTool::new(ctx, source);

        // Replace text
        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Replace,
                    path: Some("replace_test.txt".to_string()),
                    label: None,
                    content: None,
                    old: Some("World".to_string()),
                    new: Some("Universe".to_string()),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "Replace should succeed: {:?}",
            result.message
        );
        assert!(result.message.contains("Replaced"));

        // Save to disk
        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Save,
                    path: Some("replace_test.txt".to_string()),
                    label: None,
                    content: None,
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success, "Save should succeed: {:?}", result.message);

        // Verify disk content
        let file_path = base_path.join("replace_test.txt");
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(disk_content, "Hello, Universe!");
    }

    #[tokio::test]
    async fn test_file_tool_load_requires_path() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        let source = Arc::new(FileSource::new("test_files", &base_path));
        let (_dbs, _memory, ctx) = create_test_context_with_agent("test_agent_file_err").await;
        let tool = FileTool::new(ctx, source);

        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Load,
                    path: None,
                    label: None,
                    content: None,
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("path"),
                    "Expected error about path, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_file_tool_append_requires_content() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        create_test_file(&base_path, "append_test.txt", "content").await;

        let source = Arc::new(FileSource::new("test_files", &base_path));
        let (_dbs, _memory, ctx) =
            create_test_context_with_agent("test_agent_file_append_err").await;
        let tool = FileTool::new(ctx, source);

        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Append,
                    path: Some("append_test.txt".to_string()),
                    label: None,
                    content: None, // Missing content
                    old: None,
                    new: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("content"),
                    "Expected error about content, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_file_tool_replace_text_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        create_test_file(&base_path, "notfound_test.txt", "Hello, World!").await;

        let source = Arc::new(FileSource::new("test_files", &base_path));
        let (_dbs, _memory, ctx) = create_test_context_with_agent("test_agent_file_notfound").await;
        let tool = FileTool::new(ctx, source);

        let result = tool
            .execute(
                FileInput {
                    op: FileOp::Replace,
                    path: Some("notfound_test.txt".to_string()),
                    label: None,
                    content: None,
                    old: Some("nonexistent".to_string()),
                    new: Some("replacement".to_string()),
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("not found"),
                    "Expected error about text not found, got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }
}
