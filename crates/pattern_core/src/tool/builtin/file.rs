//! FileTool - Agent-facing interface to FileSource operations.
//!
//! This tool provides file operations for agents:
//! - `load` - Load file from disk into a memory block
//! - `save` - Save block content back to disk
//! - `create` - Create a new file
//! - `delete` - Delete a file (requires escalation)
//! - `append` - Append content to a file
//! - `replace` - Find and replace text in a file
//! - `list` - List files in source
//! - `status` - Check sync status of loaded files
//! - `diff` - Show unified diff between memory and disk
//! - `reload` - Reload file from disk, discarding memory changes
//!
//! The tool uses SourceManager to route operations to the appropriate FileSource,
//! which is determined by:
//! 1. Explicit `source` parameter in the input
//! 2. Parsing the source_id from a block label
//! 3. Path-based routing (finding a source whose base_path contains the file path)

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::data_source::{FileSource, SourceManager, parse_file_label};
use crate::id::AgentId;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};
use crate::{CoreError, Result};

use super::types::{FileInput, FileOp, ToolOutput};

/// Tool for file operations via FileSource.
///
/// Unlike most tools, FileTool doesn't hold a reference to a specific source.
/// Instead, it uses SourceManager to find the appropriate FileSource at runtime
/// based on the operation's path or label.
#[derive(Clone)]
pub struct FileTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for FileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl FileTool {
    /// Create a new FileTool with the given context.
    ///
    /// The tool will use SourceManager to find appropriate FileSources at runtime.
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    /// Get the agent ID from context.
    fn agent_id(&self) -> AgentId {
        AgentId::new(self.ctx.agent_id())
    }

    /// Get the SourceManager from context.
    fn sources(&self) -> Result<Arc<dyn SourceManager>> {
        self.ctx.sources().ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({}),
                "No SourceManager available - file operations require RuntimeContext",
            )
        })
    }

    /// Find a file source for the given path, with fallback to the only available file source.
    ///
    /// This enables operations on new files without requiring explicit source_id when there's
    /// only one FileSource registered.
    fn find_file_source_for_path(
        &self,
        sources: &Arc<dyn SourceManager>,
        path: &Path,
    ) -> Option<String> {
        // First try path-based routing
        if let Some(source) = sources.find_block_source_for_path(path) {
            return Some(source.source_id().to_string());
        }

        // Fallback: if there's only one file source, use it
        let all_sources = sources.list_block_sources();
        let file_sources: Vec<_> = all_sources
            .iter()
            .filter(|id| id.starts_with("file:"))
            .collect();

        if file_sources.len() == 1 {
            return Some(file_sources[0].clone());
        }

        None
    }

    /// Handle load operation - load file from disk into block.
    async fn handle_load(
        &self,
        path: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "load"}),
                "load requires 'path' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        // Find source by explicit ID, path routing, or fallback
        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "load", "path": path_str}),
                        format!(
                            "No file source found for path '{}'. Register a FileSource first.",
                            path_str
                        ),
                    )
                })?
        };

        let block_ref = sources
            .load_block(&source_id, Path::new(path_str), self.agent_id())
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "load", "path": path_str}),
                    format!("Failed to load file '{}': {}", path_str, e),
                )
            })?;

        Ok(ToolOutput::success_with_data(
            format!(
                "Loaded file '{}' into block '{}'",
                path_str, block_ref.label
            ),
            json!({
                "label": block_ref.label,
                "block_id": block_ref.block_id,
                "path": path_str,
                "source_id": source_id,
            }),
        ))
    }

    /// Handle save operation - save block content to disk.
    async fn handle_save(
        &self,
        path: Option<&str>,
        label: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let sources = self.sources()?;

        // Determine source_id and block_label
        let (source_id, block_label) = if let Some(l) = label {
            // Parse source_id from label
            if let Some(parsed) = parse_file_label(l) {
                (parsed.source_id, l.to_string())
            } else {
                return Err(CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save", "label": l}),
                    format!("Invalid file label format: '{}'", l),
                ));
            }
        } else if let Some(p) = path {
            // Find source by path and generate label
            let path_obj = Path::new(p);
            let source = sources
                .find_block_source_for_path(path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "save", "path": p}),
                        format!("No file source found for path '{}'", p),
                    )
                })?;

            let source_id = explicit_source
                .map(String::from)
                .unwrap_or_else(|| source.source_id().to_string());

            // Get the FileSource to generate label
            if let Some(file_source) = source.as_any().downcast_ref::<FileSource>() {
                let label = file_source.make_label(path_obj).map_err(|e| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "save", "path": p}),
                        format!("Failed to generate label: {}", e),
                    )
                })?;
                (source_id, label)
            } else {
                return Err(CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save", "path": p}),
                    "Source is not a FileSource",
                ));
            }
        } else {
            return Err(CoreError::tool_exec_msg(
                "file",
                json!({"op": "save"}),
                "save requires either 'path' or 'label' parameter",
            ));
        };

        // Get block metadata to create BlockRef
        let memory = self.ctx.memory();
        let agent_id = self.ctx.agent_id();
        let metadata = memory
            .get_block_metadata(agent_id, &block_label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "save", "label": &block_label}),
                    format!("Failed to get block metadata: {:?}", e),
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

        let block_ref =
            crate::data_source::BlockRef::new(&block_label, &metadata.id).owned_by(agent_id);

        sources
            .save_block(&source_id, &block_ref)
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
            block_label
        )))
    }

    /// Handle create operation - create a new file.
    async fn handle_create(
        &self,
        path: Option<&str>,
        content: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "create"}),
                "create requires 'path' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        // Find source by explicit ID, path routing, or fallback (important for new files)
        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "create", "path": path_str}),
                        format!(
                            "No file source found for path '{}'. For new files, provide explicit 'source' parameter or register exactly one FileSource.",
                            path_str
                        ),
                    )
                })?
        };

        let block_ref = sources
            .create_block(&source_id, path_obj, content, self.agent_id())
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "create", "path": path_str}),
                    format!("Failed to create file '{}': {}", path_str, e),
                )
            })?;

        Ok(ToolOutput::success_with_data(
            format!(
                "Created file '{}' with block '{}'",
                path_str, block_ref.label
            ),
            json!({
                "label": block_ref.label,
                "block_id": block_ref.block_id,
                "path": path_str,
                "source_id": source_id,
            }),
        ))
    }

    /// Handle delete operation - delete a file.
    async fn handle_delete(
        &self,
        path: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "delete"}),
                "delete requires 'path' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "delete", "path": path_str}),
                        format!("No file source found for path '{}'", path_str),
                    )
                })?
        };

        sources
            .delete_block(&source_id, path_obj)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "delete", "path": path_str}),
                    format!("Failed to delete file '{}': {}", path_str, e),
                )
            })?;

        Ok(ToolOutput::success(format!("Deleted file '{}'", path_str)))
    }

    /// Handle append operation - append content to a file.
    /// Auto-loads the file if not already loaded.
    async fn handle_append(
        &self,
        path: Option<&str>,
        content: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "append"}),
                "append requires 'path' parameter",
            )
        })?;
        let content = content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "append", "path": path_str}),
                "append requires 'content' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        // Find source_id using fallback
        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "append", "path": path_str}),
                        format!("No file source found for path '{}'", path_str),
                    )
                })?
        };

        // Get FileSource to check if already loaded
        let source = sources.get_block_source(&source_id).ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "append", "path": path_str}),
                format!("Source '{}' not found", source_id),
            )
        })?;

        let file_source = source
            .as_any()
            .downcast_ref::<FileSource>()
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "append", "path": path_str}),
                    "Source is not a FileSource",
                )
            })?;

        // Get or load the block
        let block_ref =
            if let Some(existing) = file_source.get_loaded_block_ref(path_obj, &self.agent_id()) {
                existing
            } else {
                sources
                    .load_block(&source_id, path_obj, self.agent_id())
                    .await
                    .map_err(|e| {
                        CoreError::tool_exec_msg(
                            "file",
                            json!({"op": "append", "path": path_str}),
                            format!("Failed to load file for append: {}", e),
                        )
                    })?
            };

        // Append to the block using get→mutate→persist pattern
        let memory = self.ctx.memory();
        let doc = memory
            .get_block(&block_ref.agent_id, &block_ref.label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "append", "path": path_str}),
                    format!("Failed to get block: {:?}", e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "append", "path": path_str}),
                    format!("Block not found: {}", block_ref.label),
                )
            })?;
        doc.append_text(content, false).map_err(|e| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "append", "path": path_str}),
                format!("Failed to append to block: {:?}", e),
            )
        })?;
        memory.mark_dirty(&block_ref.agent_id, &block_ref.label);
        memory
            .persist_block(&block_ref.agent_id, &block_ref.label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "append", "path": path_str}),
                    format!("Failed to persist block: {:?}", e),
                )
            })?;

        Ok(ToolOutput::success(format!(
            "Appended content to file '{}' (block '{}'). Use 'save' to write to disk.",
            path_str, block_ref.label
        )))
    }

    /// Handle list operation - list files in the source.
    async fn handle_list(
        &self,
        pattern: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let sources = self.sources()?;

        // If explicit source provided, use it; otherwise list all file sources
        let source_ids: Vec<String> = if let Some(id) = explicit_source {
            vec![id.to_string()]
        } else {
            sources.list_block_sources()
        };

        let mut all_files = Vec::new();

        for source_id in source_ids {
            if let Some(source) = sources.get_block_source(&source_id) {
                if let Some(file_source) = source.as_any().downcast_ref::<FileSource>() {
                    match file_source.list_files(pattern).await {
                        Ok(files) => {
                            for info in files {
                                all_files.push(json!({
                                    "source_id": source_id,
                                    "path": info.path,
                                    "size": info.size,
                                    "loaded": info.loaded,
                                    "permission": format!("{:?}", info.permission),
                                }));
                            }
                        }
                        Err(e) => {
                            // Log but continue with other sources
                            tracing::warn!("Failed to list files from source {}: {}", source_id, e);
                        }
                    }
                }
            }
        }

        Ok(ToolOutput::success_with_data(
            format!("Found {} files", all_files.len()),
            json!(all_files),
        ))
    }

    /// Handle status operation - check sync status of loaded files.
    async fn handle_status(
        &self,
        path: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let sources = self.sources()?;

        let source_ids: Vec<String> = if let Some(id) = explicit_source {
            vec![id.to_string()]
        } else {
            sources.list_block_sources()
        };

        let mut all_statuses = Vec::new();

        for source_id in source_ids {
            if let Some(source) = sources.get_block_source(&source_id) {
                if let Some(file_source) = source.as_any().downcast_ref::<FileSource>() {
                    match file_source.get_sync_status(path).await {
                        Ok(statuses) => {
                            for info in statuses {
                                all_statuses.push(json!({
                                    "source_id": source_id,
                                    "path": info.path,
                                    "label": info.label,
                                    "sync_status": info.sync_status,
                                    "disk_modified": info.disk_modified,
                                }));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get status from source {}: {}", source_id, e);
                        }
                    }
                }
            }
        }

        Ok(ToolOutput::success_with_data(
            format!("{} loaded files", all_statuses.len()),
            json!(all_statuses),
        ))
    }

    /// Handle diff operation - show unified diff between memory and disk.
    async fn handle_diff(
        &self,
        path: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "diff"}),
                "diff requires 'path' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        // Find source_id using fallback
        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "diff", "path": path_str}),
                        format!("No file source found for path '{}'", path_str),
                    )
                })?
        };

        let source = sources.get_block_source(&source_id).ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "diff", "path": path_str}),
                format!("Source '{}' not found", source_id),
            )
        })?;

        let file_source = source
            .as_any()
            .downcast_ref::<FileSource>()
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "diff", "path": path_str}),
                    "Source is not a FileSource",
                )
            })?;

        let diff_output = file_source.perform_diff(path_obj).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "diff", "path": path_str}),
                format!("Failed to generate diff: {}", e),
            )
        })?;

        Ok(ToolOutput::success_with_data(
            format!("Diff for '{}' (source: {})", path_str, source_id),
            json!({ "diff": diff_output }),
        ))
    }

    /// Handle reload operation - discard memory changes and reload from disk.
    async fn handle_reload(
        &self,
        path: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "reload"}),
                "reload requires 'path' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        // Find source_id using fallback
        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "reload", "path": path_str}),
                        format!("No file source found for path '{}'", path_str),
                    )
                })?
        };

        let source = sources.get_block_source(&source_id).ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "reload", "path": path_str}),
                format!("Source '{}' not found", source_id),
            )
        })?;

        let file_source = source
            .as_any()
            .downcast_ref::<FileSource>()
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "reload", "path": path_str}),
                    "Source is not a FileSource",
                )
            })?;

        file_source.reload(path_obj).await.map_err(|e| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "reload", "path": path_str}),
                format!("Failed to reload file: {}", e),
            )
        })?;

        Ok(ToolOutput::success(format!(
            "Reloaded '{}' from disk, discarding any memory changes",
            path_str
        )))
    }

    /// Handle replace operation - find and replace text in a file.
    /// Auto-loads the file if not already loaded.
    async fn handle_replace(
        &self,
        path: Option<&str>,
        old: Option<&str>,
        new: Option<&str>,
        explicit_source: Option<&str>,
    ) -> Result<ToolOutput> {
        let path_str = path.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace"}),
                "replace requires 'path' parameter",
            )
        })?;
        let old = old.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path_str}),
                "replace requires 'old' parameter",
            )
        })?;
        let new = new.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path_str}),
                "replace requires 'new' parameter",
            )
        })?;

        let sources = self.sources()?;
        let path_obj = Path::new(path_str);

        // Find source_id using fallback
        let source_id = if let Some(id) = explicit_source {
            id.to_string()
        } else {
            self.find_file_source_for_path(&sources, path_obj)
                .ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "replace", "path": path_str}),
                        format!("No file source found for path '{}'", path_str),
                    )
                })?
        };

        let source = sources.get_block_source(&source_id).ok_or_else(|| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path_str}),
                format!("Source '{}' not found", source_id),
            )
        })?;

        let file_source = source
            .as_any()
            .downcast_ref::<FileSource>()
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "replace", "path": path_str}),
                    "Source is not a FileSource",
                )
            })?;

        // Get or load the block
        let block_ref =
            if let Some(existing) = file_source.get_loaded_block_ref(path_obj, &self.agent_id()) {
                existing
            } else {
                sources
                    .load_block(&source_id, path_obj, self.agent_id())
                    .await
                    .map_err(|e| {
                        CoreError::tool_exec_msg(
                            "file",
                            json!({"op": "replace", "path": path_str}),
                            format!("Failed to load file for replace: {}", e),
                        )
                    })?
            };

        // Replace in the block using get→mutate→persist pattern
        let memory = self.ctx.memory();
        let doc = memory
            .get_block(&block_ref.agent_id, &block_ref.label)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "replace", "path": path_str}),
                    format!("Failed to get block: {:?}", e),
                )
            })?
            .ok_or_else(|| {
                CoreError::tool_exec_msg(
                    "file",
                    json!({"op": "replace", "path": path_str}),
                    format!("Block not found: {}", block_ref.label),
                )
            })?;
        let replaced = doc.replace_text(old, new, false).map_err(|e| {
            CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path_str}),
                format!("Failed to replace in block: {:?}", e),
            )
        })?;
        if replaced {
            memory.mark_dirty(&block_ref.agent_id, &block_ref.label);
            memory
                .persist_block(&block_ref.agent_id, &block_ref.label)
                .await
                .map_err(|e| {
                    CoreError::tool_exec_msg(
                        "file",
                        json!({"op": "replace", "path": path_str}),
                        format!("Failed to persist block: {:?}", e),
                    )
                })?;
        }

        if replaced {
            Ok(ToolOutput::success(format!(
                "Replaced '{}' with '{}' in file '{}' (block '{}'). Use 'save' to write to disk.",
                old, new, path_str, block_ref.label
            )))
        } else {
            Err(CoreError::tool_exec_msg(
                "file",
                json!({"op": "replace", "path": path_str, "old": old}),
                format!("Text '{}' not found in file '{}'", old, path_str),
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
- 'list': List files in source (optional 'pattern' for glob filtering, e.g. '**/*.rs')
- 'status': Check sync status of loaded files (optional 'path' to filter)
- 'diff': Show unified diff between memory and disk (requires 'path')
- 'reload': Discard memory changes and reload from disk (requires 'path')

Optional 'source' parameter specifies the file source ID. If omitted, source is inferred from path.

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
        &[
            "load", "save", "create", "delete", "append", "replace", "list", "status", "diff",
            "reload",
        ]
    }

    async fn execute(&self, input: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        let source = input.source.as_deref();

        match input.op {
            FileOp::Load => self.handle_load(input.path.as_deref(), source).await,
            FileOp::Save => {
                self.handle_save(input.path.as_deref(), input.label.as_deref(), source)
                    .await
            }
            FileOp::Create => {
                self.handle_create(input.path.as_deref(), input.content.as_deref(), source)
                    .await
            }
            FileOp::Delete => self.handle_delete(input.path.as_deref(), source).await,
            FileOp::Append => {
                self.handle_append(input.path.as_deref(), input.content.as_deref(), source)
                    .await
            }
            FileOp::Replace => {
                self.handle_replace(
                    input.path.as_deref(),
                    input.old.as_deref(),
                    input.new.as_deref(),
                    source,
                )
                .await
            }
            FileOp::List => self.handle_list(input.pattern.as_deref(), source).await,
            FileOp::Status => self.handle_status(input.path.as_deref(), source).await,
            FileOp::Diff => self.handle_diff(input.path.as_deref(), source).await,
            FileOp::Reload => self.handle_reload(input.path.as_deref(), source).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::AgentConfig;
    use crate::data_source::DataBlock;
    use crate::db::ConstellationDatabases;
    use crate::model::MockModelProvider;
    use crate::runtime::RuntimeContext;
    use crate::tool::{AiTool, ExecutionMeta};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Create a RuntimeContext for testing with in-memory databases
    async fn create_test_runtime() -> Arc<RuntimeContext> {
        let dbs = ConstellationDatabases::open_in_memory()
            .await
            .expect("Failed to create test dbs");
        let model = Arc::new(MockModelProvider {
            response: "test response".to_string(),
        });

        RuntimeContext::builder()
            .dbs_owned(dbs)
            .model_provider(model)
            .build()
            .await
            .expect("Failed to create RuntimeContext")
    }

    /// Create a test file in the temp directory
    async fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&path, content).await.unwrap();
        path
    }

    /// Set up test context, agent, and file tool with a FileSource registered
    async fn setup_test(base_path: &Path) -> (Arc<RuntimeContext>, Arc<dyn Agent>, FileTool) {
        let ctx = create_test_runtime().await;
        let file_source = Arc::new(FileSource::new(base_path));
        ctx.register_block_source(file_source).await;

        let agent_config = AgentConfig {
            name: "test_file_agent".to_string(),
            ..Default::default()
        };
        let agent = ctx
            .clone()
            .create_agent(&agent_config)
            .await
            .expect("Failed to create agent");

        let tool = FileTool::new(agent.runtime().clone());

        (ctx, agent, tool)
    }

    /// Helper to create FileInput for a given operation
    fn file_input(op: FileOp) -> FileInput {
        FileInput {
            op,
            path: None,
            label: None,
            content: None,
            old: None,
            new: None,
            pattern: None,
            source: None,
        }
    }

    #[tokio::test]
    async fn test_file_tool_load() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        let test_content = "Hello, FileTool!";
        create_test_file(base_path, "load_test.txt", test_content).await;

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Execute load operation
        let mut input = file_input(FileOp::Load);
        input.path = Some("load_test.txt".to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Load should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success, "Output should indicate success");
        assert!(
            output.message.contains("Loaded file"),
            "Message should mention loading: {}",
            output.message
        );

        // Verify data contains expected fields
        let data = output.data.unwrap();
        assert!(data.get("label").is_some(), "Should have label in data");
        assert!(
            data.get("block_id").is_some(),
            "Should have block_id in data"
        );
    }

    #[tokio::test]
    async fn test_file_tool_create() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Execute create operation
        let initial_content = "New file content";
        let mut input = file_input(FileOp::Create);
        input.path = Some("new_file.txt".to_string());
        input.content = Some(initial_content.to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Create should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success, "Output should indicate success");

        // Verify file exists on disk
        let file_path = base_path.join("new_file.txt");
        assert!(file_path.exists(), "File should exist on disk");

        // Verify content
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(disk_content, initial_content);
    }

    #[tokio::test]
    async fn test_file_tool_save() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        let original_content = "Original content";
        create_test_file(base_path, "save_test.txt", original_content).await;

        let (_ctx, agent, tool) = setup_test(base_path).await;

        // Load the file first
        let mut load_input = file_input(FileOp::Load);
        load_input.path = Some("save_test.txt".to_string());

        let load_result = tool
            .execute(load_input, &ExecutionMeta::default())
            .await
            .unwrap();
        let label = load_result.data.unwrap()["label"]
            .as_str()
            .unwrap()
            .to_string();

        // Modify content in memory
        let new_content = "Modified content via FileTool";
        let runtime = agent.runtime();
        let memory = runtime.memory();
        let doc = memory
            .get_block(agent.id().as_str(), &label)
            .await
            .unwrap()
            .unwrap();
        doc.set_text(new_content, true).unwrap();
        memory
            .persist_block(agent.id().as_str(), &label)
            .await
            .unwrap();

        // Allow auto-sync task to complete and update disk_mtime
        tokio::task::yield_now().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Execute save operation
        let mut save_input = file_input(FileOp::Save);
        save_input.label = Some(label.clone());

        let result = tool.execute(save_input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Save should succeed: {:?}", result.err());

        // Verify disk was updated
        let file_path = base_path.join("save_test.txt");
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(disk_content, new_content);
    }

    #[tokio::test]
    async fn test_file_tool_append() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        let original_content = "Line 1\n";
        create_test_file(base_path, "append_test.txt", original_content).await;

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Execute append operation (auto-loads the file)
        let append_content = "Line 2\n";
        let mut input = file_input(FileOp::Append);
        input.path = Some("append_test.txt".to_string());
        input.content = Some(append_content.to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Append should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success);
        assert!(
            output.message.contains("Appended"),
            "Message should mention appending"
        );
    }

    #[tokio::test]
    async fn test_file_tool_replace() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        create_test_file(base_path, "replace_test.txt", "Hello, World!").await;

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Execute replace operation (auto-loads the file)
        let mut input = file_input(FileOp::Replace);
        input.path = Some("replace_test.txt".to_string());
        input.old = Some("World".to_string());
        input.new = Some("Rust".to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Replace should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success);
        assert!(
            output.message.contains("Replaced"),
            "Message should mention replacing"
        );
    }

    #[tokio::test]
    async fn test_file_tool_list() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        create_test_file(base_path, "file1.txt", "content 1").await;
        create_test_file(base_path, "file2.rs", "fn main() {}").await;
        create_test_file(base_path, "subdir/file3.txt", "nested").await;

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Execute list operation
        let input = file_input(FileOp::List);

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "List should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success);
        assert!(
            output.message.contains("Found"),
            "Message should mention files found"
        );

        // Verify data contains file list
        let data = output.data.unwrap();
        let files = data.as_array().expect("Data should be array");
        assert!(files.len() >= 3, "Should find at least 3 files");
    }

    #[tokio::test]
    async fn test_file_tool_list_with_pattern() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        create_test_file(base_path, "file1.txt", "content 1").await;
        create_test_file(base_path, "file2.rs", "fn main() {}").await;
        create_test_file(base_path, "file3.txt", "content 3").await;

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Execute list with pattern
        let mut input = file_input(FileOp::List);
        input.pattern = Some("*.txt".to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "List should succeed: {:?}", result.err());

        let output = result.unwrap();
        let data = output.data.unwrap();
        let files = data.as_array().expect("Data should be array");

        // Should only find .txt files
        assert_eq!(files.len(), 2, "Should find exactly 2 .txt files");
        for file in files {
            let path = file["path"].as_str().unwrap();
            assert!(path.ends_with(".txt"), "File should be .txt: {}", path);
        }
    }

    #[tokio::test]
    async fn test_file_tool_status() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        create_test_file(base_path, "status_test.txt", "content").await;

        let (_ctx, _agent, tool) = setup_test(base_path).await;

        // Load file first
        let mut load_input = file_input(FileOp::Load);
        load_input.path = Some("status_test.txt".to_string());
        tool.execute(load_input, &ExecutionMeta::default())
            .await
            .unwrap();

        // Execute status operation
        let input = file_input(FileOp::Status);

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Status should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success);
        assert!(
            output.message.contains("loaded"),
            "Message should mention loaded files"
        );
    }

    #[tokio::test]
    async fn test_file_tool_diff() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        create_test_file(base_path, "diff_test.txt", "Original line\n").await;

        let (_ctx, agent, tool) = setup_test(base_path).await;

        // Load and modify
        let mut load_input = file_input(FileOp::Load);
        load_input.path = Some("diff_test.txt".to_string());

        let load_result = tool
            .execute(load_input, &ExecutionMeta::default())
            .await
            .unwrap();
        let label = load_result.data.unwrap()["label"]
            .as_str()
            .unwrap()
            .to_string();

        // Modify content in memory
        let runtime = agent.runtime();
        let memory = runtime.memory();
        let doc = memory
            .get_block(agent.id().as_str(), &label)
            .await
            .unwrap()
            .unwrap();
        doc.set_text("Modified line\n", true).unwrap();
        memory
            .persist_block(agent.id().as_str(), &label)
            .await
            .unwrap();

        // Execute diff operation
        let mut input = file_input(FileOp::Diff);
        input.path = Some("diff_test.txt".to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Diff should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success);

        // Verify diff contains expected headers
        let data = output.data.unwrap();
        let diff_text = data["diff"].as_str().unwrap();
        assert!(
            diff_text.contains("---") && diff_text.contains("+++"),
            "Diff should have unified diff headers"
        );
    }

    #[tokio::test]
    async fn test_file_tool_reload() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        let file_path = create_test_file(base_path, "reload_test.txt", "Original content").await;

        let (_ctx, agent, tool) = setup_test(base_path).await;

        // Load the file
        let mut load_input = file_input(FileOp::Load);
        load_input.path = Some("reload_test.txt".to_string());

        let load_result = tool
            .execute(load_input, &ExecutionMeta::default())
            .await
            .unwrap();
        let label = load_result.data.unwrap()["label"]
            .as_str()
            .unwrap()
            .to_string();

        // Modify content in memory
        let runtime = agent.runtime();
        let memory = runtime.memory();
        let doc = memory
            .get_block(agent.id().as_str(), &label)
            .await
            .unwrap()
            .unwrap();
        doc.set_text("Memory changes", true).unwrap();
        memory
            .persist_block(agent.id().as_str(), &label)
            .await
            .unwrap();

        // Update disk externally
        let new_disk_content = "New disk content";
        tokio::fs::write(&file_path, new_disk_content)
            .await
            .unwrap();

        // Execute reload operation
        let mut input = file_input(FileOp::Reload);
        input.path = Some("reload_test.txt".to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_ok(), "Reload should succeed: {:?}", result.err());

        let output = result.unwrap();
        assert!(output.success);
        assert!(
            output.message.contains("Reloaded"),
            "Message should mention reloading"
        );

        // Verify memory now has disk content
        let content = memory
            .get_rendered_content(agent.id().as_str(), &label)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, new_disk_content);
    }

    #[tokio::test]
    async fn test_file_tool_no_source_error() {
        // Set up RuntimeContext WITHOUT registering any FileSource
        let ctx = create_test_runtime().await;

        let agent_config = AgentConfig {
            name: "no_source_test_agent".to_string(),
            ..Default::default()
        };
        let agent = ctx.create_agent(&agent_config).await.unwrap();
        let tool = FileTool::new(agent.runtime().clone());

        // Try to load a file - should fail because no source registered
        let mut input = file_input(FileOp::Load);
        input.path = Some("nonexistent.txt".to_string());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(result.is_err(), "Should fail without registered source");

        let err = result.unwrap_err();
        let err_msg = format!("{:?}", err);
        assert!(
            err_msg.contains("No file source") || err_msg.contains("source"),
            "Error should mention missing source: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_file_tool_explicit_source() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test file
        create_test_file(base_path, "explicit_source.txt", "content").await;

        // Set up RuntimeContext - need to get source_id before setup_test
        let ctx = create_test_runtime().await;
        let file_source = Arc::new(FileSource::new(base_path));
        let source_id = file_source.source_id().to_string();
        ctx.register_block_source(file_source).await;

        let agent_config = AgentConfig {
            name: "explicit_source_test_agent".to_string(),
            ..Default::default()
        };
        let agent = ctx.create_agent(&agent_config).await.unwrap();
        let tool = FileTool::new(agent.runtime().clone());

        // Load with explicit source parameter
        let mut input = file_input(FileOp::Load);
        input.path = Some("explicit_source.txt".to_string());
        input.source = Some(source_id.clone());

        let result = tool.execute(input, &ExecutionMeta::default()).await;
        assert!(
            result.is_ok(),
            "Load with explicit source should succeed: {:?}",
            result.err()
        );

        let output = result.unwrap();
        let data = output.data.unwrap();
        assert_eq!(
            data["source_id"].as_str().unwrap(),
            source_id,
            "Should use the explicit source"
        );
    }

    #[tokio::test]
    async fn test_file_tool_multiple_sources() {
        let temp_dir1 = TempDir::new().unwrap();
        let temp_dir2 = TempDir::new().unwrap();
        let base_path1 = temp_dir1.path();
        let base_path2 = temp_dir2.path();

        // Create test files in different directories
        create_test_file(base_path1, "file_in_dir1.txt", "content 1").await;
        create_test_file(base_path2, "file_in_dir2.txt", "content 2").await;

        // Set up RuntimeContext with two FileSources
        let ctx = create_test_runtime().await;
        let file_source1 = Arc::new(FileSource::new(base_path1));
        let file_source2 = Arc::new(FileSource::new(base_path2));
        let source_id1 = file_source1.source_id().to_string();
        let source_id2 = file_source2.source_id().to_string();

        ctx.register_block_source(file_source1).await;
        ctx.register_block_source(file_source2).await;

        let agent_config = AgentConfig {
            name: "multi_source_test_agent".to_string(),
            ..Default::default()
        };
        let agent = ctx.create_agent(&agent_config).await.unwrap();
        let tool = FileTool::new(agent.runtime().clone());

        // Load from first source using explicit source
        let mut input1 = file_input(FileOp::Load);
        input1.path = Some("file_in_dir1.txt".to_string());
        input1.source = Some(source_id1.clone());

        let result1 = tool.execute(input1, &ExecutionMeta::default()).await;
        assert!(result1.is_ok(), "Load from source 1 should succeed");
        let data1 = result1.unwrap().data.unwrap();
        assert_eq!(data1["source_id"].as_str().unwrap(), source_id1);

        // Load from second source using explicit source
        let mut input2 = file_input(FileOp::Load);
        input2.path = Some("file_in_dir2.txt".to_string());
        input2.source = Some(source_id2.clone());

        let result2 = tool.execute(input2, &ExecutionMeta::default()).await;
        assert!(result2.is_ok(), "Load from source 2 should succeed");
        let data2 = result2.unwrap().data.unwrap();
        assert_eq!(data2["source_id"].as_str().unwrap(), source_id2);
    }

    #[tokio::test]
    async fn test_parse_file_label() {
        use crate::data_source::parse_file_label;

        // Valid file label - source_id now includes "file:" prefix
        let parsed = parse_file_label("file:a3f2b1c9:src/main.rs");
        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.source_id, "file:a3f2b1c9");
        assert_eq!(parsed.path, "src/main.rs");

        // Valid with nested path
        let parsed = parse_file_label("file:12345678:path/to/deep/file.txt");
        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.source_id, "file:12345678");
        assert_eq!(parsed.path, "path/to/deep/file.txt");

        // Invalid: wrong prefix
        assert!(parse_file_label("block:12345678:path").is_none());

        // Invalid: hash too short
        assert!(parse_file_label("file:1234567:path").is_none());

        // Invalid: hash too long
        assert!(parse_file_label("file:123456789:path").is_none());

        // Invalid: hash has non-hex chars
        assert!(parse_file_label("file:1234567g:path").is_none());

        // Invalid: no path
        assert!(parse_file_label("file:12345678").is_none());
    }
}
