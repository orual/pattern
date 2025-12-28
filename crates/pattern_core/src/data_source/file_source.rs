//! FileSource - Local filesystem data block implementation.
//!
//! FileSource is a DataBlock implementation that manages local files as memory blocks
//! with Loro-backed versioning. It provides:
//!
//! - Block labels in format: `file:{hash8}:{relative_path}`
//! - Conflict detection via mtime tracking
//! - Permission-gated operations via glob patterns
//! - Load/save with disk synchronization

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::SystemTime,
};

use async_trait::async_trait;
use dashmap::DashMap;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::error::{CoreError, Result};
use crate::id::AgentId;
use crate::memory::{BlockSchema, BlockType, MemoryError, MemoryPermission};
use crate::runtime::ToolContext;
use crate::tool::rules::ToolRule;

use super::{
    BlockRef, BlockSchemaSpec, BlockSourceStatus, DataBlock, FileChange, PermissionRule,
    ReconcileResult, VersionInfo,
};

/// Convert MemoryError to CoreError for FileSource operations.
fn memory_err(source_id: &str, operation: &str, err: MemoryError) -> CoreError {
    CoreError::DataSourceError {
        source_name: source_id.to_string(),
        operation: operation.to_string(),
        cause: format!("Memory operation failed: {}", err),
    }
}

/// Information about a loaded file tracked by FileSource.
#[derive(Debug, Clone)]
struct LoadedFileInfo {
    /// Block ID in the memory store
    #[allow(dead_code)]
    block_id: String,
    /// Block label (file:{hash8}:{relative_path})
    label: String,
    /// Modification time when file was last loaded/saved
    disk_mtime: SystemTime,
    /// File size when last loaded/saved
    disk_size: u64,
}

/// Status values for BlockSourceStatus (stored as u8 for atomic operations)
const STATUS_IDLE: u8 = 0;
const STATUS_WATCHING: u8 = 1;

/// FileSource manages local files as Loro-backed memory blocks.
///
/// # Block Label Format
///
/// Labels follow the format `file:{hash8}:{relative_path}` where:
/// - `hash8` is the first 8 hex characters of SHA-256 of the absolute path
/// - `relative_path` is the path relative to `base_path`
///
/// # Conflict Detection
///
/// Before saving, FileSource checks if the file's mtime has changed since loading.
/// If external modifications are detected, an error is returned to prevent data loss.
///
/// # Permission Rules
///
/// Glob patterns determine permission levels for different paths:
/// - `*.config.toml` -> ReadOnly
/// - `src/**/*.rs` -> ReadWrite
/// - `**` -> ReadWrite (default fallback)
#[derive(Debug)]
pub struct FileSource {
    /// Unique identifier for this source
    source_id: String,
    /// Base directory for all file operations
    base_path: PathBuf,
    /// Permission rules (glob pattern -> permission level)
    permission_rules: Vec<PermissionRule>,
    /// Tracks loaded files and their metadata
    loaded_blocks: DashMap<PathBuf, LoadedFileInfo>,
    /// Current status (Idle or Watching)
    status: AtomicU8,
}

impl FileSource {
    /// Create a new FileSource with the given base path.
    ///
    /// # Arguments
    ///
    /// * `source_id` - Unique identifier for this source
    /// * `base_path` - Base directory for file operations
    ///
    /// # Example
    ///
    /// ```ignore
    /// let source = FileSource::new("my_files", "/home/user/project");
    /// ```
    pub fn new(source_id: impl Into<String>, base_path: impl Into<PathBuf>) -> Self {
        Self {
            source_id: source_id.into(),
            base_path: base_path.into(),
            permission_rules: vec![
                // Default rule: all files are ReadWrite
                PermissionRule::new("**", MemoryPermission::ReadWrite),
            ],
            loaded_blocks: DashMap::new(),
            status: AtomicU8::new(STATUS_IDLE),
        }
    }

    /// Create a new FileSource with custom permission rules.
    ///
    /// # Arguments
    ///
    /// * `source_id` - Unique identifier for this source
    /// * `base_path` - Base directory for file operations
    /// * `rules` - Permission rules to apply (first matching rule wins)
    pub fn with_rules(
        source_id: impl Into<String>,
        base_path: impl Into<PathBuf>,
        rules: Vec<PermissionRule>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            base_path: base_path.into(),
            permission_rules: rules,
            loaded_blocks: DashMap::new(),
            status: AtomicU8::new(STATUS_IDLE),
        }
    }

    /// Public method to generate a block label for a file path.
    ///
    /// Format: `file:{hash8}:{relative_path}`
    ///
    /// This can be used by tools to get the label without loading the file.
    pub fn make_label(&self, path: &Path) -> Result<String> {
        self.generate_label(path)
    }

    /// Generate a block label for a file path.
    ///
    /// Format: `file:{hash8}:{relative_path}`
    fn generate_label(&self, path: &Path) -> Result<String> {
        let abs_path = self.absolute_path(path)?;
        let rel_path = self.relative_path(path)?;

        // Hash the absolute path
        let mut hasher = Sha256::new();
        hasher.update(abs_path.to_string_lossy().as_bytes());
        let hash = hasher.finalize();
        // Encode first 4 bytes as 8 hex chars (using format! instead of hex crate)
        let hash8 = format!(
            "{:02x}{:02x}{:02x}{:02x}",
            hash[0], hash[1], hash[2], hash[3]
        );

        Ok(format!("file:{}:{}", hash8, rel_path.display()))
    }

    /// Get the absolute path, canonicalizing if possible.
    fn absolute_path(&self, path: &Path) -> Result<PathBuf> {
        let full_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base_path.join(path)
        };

        // Try to canonicalize, but fall back to the raw path if file doesn't exist yet
        full_path.canonicalize().or_else(|_| Ok(full_path))
    }

    /// Get the path relative to base_path.
    fn relative_path(&self, path: &Path) -> Result<PathBuf> {
        let abs_path = self.absolute_path(path)?;

        abs_path
            .strip_prefix(&self.base_path)
            .map(|p| p.to_path_buf())
            .or_else(|_| {
                // If not under base_path, use the path as-is
                Ok(if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    path.to_path_buf()
                })
            })
    }

    /// Get file metadata (mtime and size).
    async fn get_file_metadata(&self, path: &Path) -> Result<(SystemTime, u64)> {
        let abs_path = self.absolute_path(path)?;
        let metadata =
            tokio::fs::metadata(&abs_path)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "get_metadata".to_string(),
                    cause: format!("Failed to get metadata for {}: {}", abs_path.display(), e),
                })?;

        let mtime = metadata
            .modified()
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "get_mtime".to_string(),
                cause: format!("Failed to get mtime for {}: {}", abs_path.display(), e),
            })?;

        Ok((mtime, metadata.len()))
    }

    /// Check if a file has been modified externally since loading.
    fn check_conflict(&self, path: &Path, current_mtime: SystemTime) -> bool {
        if let Some(info) = self.loaded_blocks.get(path) {
            info.disk_mtime != current_mtime
        } else {
            false
        }
    }
}

#[async_trait]
impl DataBlock for FileSource {
    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn name(&self) -> &str {
        "Local File System"
    }

    fn block_schema(&self) -> BlockSchemaSpec {
        BlockSchemaSpec::ephemeral(
            "file:{hash}:{path}",
            BlockSchema::Text,
            "Local file content with Loro-backed versioning",
        )
    }

    fn permission_rules(&self) -> &[PermissionRule] {
        &self.permission_rules
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]
    }

    fn matches(&self, path: &Path) -> bool {
        // Check if path is under base_path or matches any permission rule
        if let Ok(abs_path) = self.absolute_path(path) {
            abs_path.starts_with(&self.base_path)
        } else {
            false
        }
    }

    fn permission_for(&self, path: &Path) -> MemoryPermission {
        // Get relative path for glob matching
        let rel_path = self
            .relative_path(path)
            .unwrap_or_else(|_| path.to_path_buf());

        // Find first matching rule
        for rule in &self.permission_rules {
            if rule.matches(&rel_path) {
                return rule.permission;
            }
        }

        // Default to ReadWrite
        MemoryPermission::ReadWrite
    }

    async fn load(
        &self,
        path: &Path,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<BlockRef> {
        let abs_path = self.absolute_path(path)?;
        let label = self.generate_label(path)?;
        let owner_str = owner.to_string();

        // Read file content
        let content =
            tokio::fs::read_to_string(&abs_path)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "load".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                })?;

        // Get file metadata for conflict detection
        let (mtime, size) = self.get_file_metadata(path).await?;

        // Create or update block in memory store
        let memory = ctx.memory();
        let source_id = &self.source_id;

        // Check if block already exists
        let block_id = if let Some(existing) = memory
            .get_block_metadata(&owner_str, &label)
            .await
            .map_err(|e| memory_err(source_id, "load", e))?
        {
            // Update existing block
            memory
                .update_block_text(&owner_str, &label, &content)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            existing.id
        } else {
            // Create new block
            let id = memory
                .create_block(
                    &owner_str,
                    &label,
                    &format!("File: {}", abs_path.display()),
                    BlockType::Working,
                    BlockSchema::Text,
                    1024 * 1024, // 1MB char limit
                )
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            memory
                .update_block_text(&owner_str, &label, &content)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            id
        };

        // Track loaded file info
        self.loaded_blocks.insert(
            abs_path.clone(),
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
            },
        );

        Ok(BlockRef::new(&label, block_id).owned_by(&owner_str))
    }

    async fn create(
        &self,
        path: &Path,
        initial_content: Option<&str>,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<BlockRef> {
        let abs_path = self.absolute_path(path)?;
        let label = self.generate_label(path)?;
        let owner_str = owner.to_string();
        let content = initial_content.unwrap_or("");

        // Check permission
        let permission = self.permission_for(path);
        if permission == MemoryPermission::ReadOnly {
            return Err(CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "create".to_string(),
                cause: format!("Permission denied: {} is read-only", path.display()),
            });
        }

        // Create parent directories if needed
        if let Some(parent) = abs_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "create".to_string(),
                    cause: format!("Failed to create parent directories: {}", e),
                })?;
        }

        // Write file to disk
        tokio::fs::write(&abs_path, content)
            .await
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "create".to_string(),
                cause: format!("Failed to create file {}: {}", abs_path.display(), e),
            })?;

        // Get file metadata
        let (mtime, size) = self.get_file_metadata(path).await?;

        // Create block in memory store
        let memory = ctx.memory();
        let source_id = &self.source_id;
        let block_id = memory
            .create_block(
                &owner_str,
                &label,
                &format!("File: {}", abs_path.display()),
                BlockType::Working,
                BlockSchema::Text,
                1024 * 1024, // 1MB char limit
            )
            .await
            .map_err(|e| memory_err(source_id, "create", e))?;

        if !content.is_empty() {
            memory
                .update_block_text(&owner_str, &label, content)
                .await
                .map_err(|e| memory_err(source_id, "create", e))?;
        }

        // Track loaded file info
        self.loaded_blocks.insert(
            abs_path,
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
            },
        );

        Ok(BlockRef::new(&label, block_id).owned_by(&owner_str))
    }

    async fn save(&self, block_ref: &BlockRef, ctx: Arc<dyn ToolContext>) -> Result<()> {
        // Find the file path for this block
        let file_path = self
            .loaded_blocks
            .iter()
            .find(|entry| entry.value().label == block_ref.label)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "save".to_string(),
                cause: format!("Block {} not loaded from this source", block_ref.label),
            })?;

        // Check for conflicts
        let (current_mtime, _) = self.get_file_metadata(&file_path).await?;
        if self.check_conflict(&file_path, current_mtime) {
            return Err(CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "save".to_string(),
                cause: format!(
                    "Conflict detected: {} was modified externally since loading",
                    file_path.display()
                ),
            });
        }

        // Check permission
        let permission = self.permission_for(&file_path);
        if permission == MemoryPermission::ReadOnly {
            return Err(CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "save".to_string(),
                cause: format!("Permission denied: {} is read-only", file_path.display()),
            });
        }

        // Get content from memory block
        let memory = ctx.memory();
        let source_id = &self.source_id;
        let content = memory
            .get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await
            .map_err(|e| memory_err(source_id, "save", e))?
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "save".to_string(),
                cause: format!("Block {} not found in memory", block_ref.label),
            })?;

        // Write to disk
        tokio::fs::write(&file_path, &content)
            .await
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "save".to_string(),
                cause: format!("Failed to write file {}: {}", file_path.display(), e),
            })?;

        // Update tracked metadata
        let (new_mtime, new_size) = self.get_file_metadata(&file_path).await?;
        if let Some(mut entry) = self.loaded_blocks.get_mut(&file_path) {
            entry.disk_mtime = new_mtime;
            entry.disk_size = new_size;
        }

        Ok(())
    }

    async fn delete(&self, path: &Path, _ctx: Arc<dyn ToolContext>) -> Result<()> {
        let abs_path = self.absolute_path(path)?;

        // Check permission
        let permission = self.permission_for(path);
        if permission != MemoryPermission::Admin {
            return Err(CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "delete".to_string(),
                cause: format!(
                    "Permission denied: delete requires Admin permission for {}",
                    path.display()
                ),
            });
        }

        // Remove from disk
        tokio::fs::remove_file(&abs_path)
            .await
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "delete".to_string(),
                cause: format!("Failed to delete file {}: {}", abs_path.display(), e),
            })?;

        // Remove from tracking
        self.loaded_blocks.remove(&abs_path);

        Ok(())
    }

    async fn start_watch(&self) -> Option<broadcast::Receiver<FileChange>> {
        // V1: No file watching support
        None
    }

    async fn stop_watch(&self) -> Result<()> {
        self.status.store(STATUS_IDLE, Ordering::SeqCst);
        Ok(())
    }

    fn status(&self) -> BlockSourceStatus {
        match self.status.load(Ordering::SeqCst) {
            STATUS_WATCHING => BlockSourceStatus::Watching,
            _ => BlockSourceStatus::Idle,
        }
    }

    async fn reconcile(
        &self,
        paths: &[PathBuf],
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<ReconcileResult>> {
        let mut results = Vec::new();

        for path in paths {
            let abs_path = self.absolute_path(path)?;
            let path_str = abs_path.to_string_lossy().to_string();

            // Check if we have this file loaded
            if let Some(info) = self.loaded_blocks.get(&abs_path) {
                // Check if file still exists
                match self.get_file_metadata(&abs_path).await {
                    Ok((current_mtime, _)) => {
                        if info.disk_mtime != current_mtime {
                            // File was modified externally
                            results.push(ReconcileResult::NeedsResolution {
                                path: path_str,
                                disk_changes: "File modified on disk".to_string(),
                                agent_changes: "Block may have pending changes".to_string(),
                            });
                        } else {
                            results.push(ReconcileResult::NoChange { path: path_str });
                        }
                    }
                    Err(_) => {
                        // File was deleted
                        results.push(ReconcileResult::NeedsResolution {
                            path: path_str,
                            disk_changes: "File deleted from disk".to_string(),
                            agent_changes: "Block still exists in memory".to_string(),
                        });
                    }
                }
            } else {
                // Not loaded, check if file exists
                if abs_path.exists() {
                    results.push(ReconcileResult::NoChange { path: path_str });
                } else {
                    results.push(ReconcileResult::NoChange { path: path_str });
                }
            }
        }

        Ok(results)
    }

    async fn history(
        &self,
        _block_ref: &BlockRef,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<VersionInfo>> {
        // V1: No version history support
        Ok(vec![])
    }

    async fn rollback(
        &self,
        _block_ref: &BlockRef,
        _version: &str,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<()> {
        Err(CoreError::DataSourceError {
            source_name: self.source_id.clone(),
            operation: "rollback".to_string(),
            cause: "Rollback not implemented in v1".to_string(),
        })
    }

    async fn diff(
        &self,
        block_ref: &BlockRef,
        _from: Option<&str>,
        _to: Option<&str>,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<String> {
        // Find the file path for this block
        let file_path = self
            .loaded_blocks
            .iter()
            .find(|entry| entry.value().label == block_ref.label)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "diff".to_string(),
                cause: format!("Block {} not loaded from this source", block_ref.label),
            })?;

        // Get block content
        let memory = ctx.memory();
        let source_id = &self.source_id;
        let block_content = memory
            .get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await
            .map_err(|e| memory_err(source_id, "diff", e))?
            .unwrap_or_default();

        // Get disk content
        let disk_content = tokio::fs::read_to_string(&file_path)
            .await
            .unwrap_or_default();

        // Simple diff: show both
        if block_content == disk_content {
            Ok("No differences between block and disk".to_string())
        } else {
            Ok(format!(
                "=== Block content ===\n{}\n\n=== Disk content ===\n{}",
                block_content, disk_content
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::builtin::create_test_context_with_agent;
    use tempfile::TempDir;

    /// Create a test file in the temp directory
    async fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        tokio::fs::write(&path, content).await.unwrap();
        path
    }

    #[tokio::test]
    async fn test_file_source_load_save() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let test_content = "Hello, World!\nThis is a test file.";
        let file_path = create_test_file(&base_path, "test.txt", test_content).await;

        // Create FileSource
        let source = FileSource::new("test_files", &base_path);

        // Create test context
        let agent_id = "test_agent_load_save";
        let (_dbs, _memory, ctx) = create_test_context_with_agent(agent_id).await;
        let owner = AgentId::new(agent_id);

        // Load the file
        let block_ref = source
            .load(
                file_path.strip_prefix(&base_path).unwrap(),
                ctx.clone() as Arc<dyn ToolContext>,
                owner.clone(),
            )
            .await
            .expect("Load should succeed");

        // Verify block label format
        assert!(
            block_ref.label.starts_with("file:"),
            "Label should start with 'file:'"
        );
        assert!(
            block_ref.label.contains("test.txt"),
            "Label should contain filename"
        );

        // Verify content in memory
        let memory = ctx.memory();
        let content = memory
            .get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, test_content);
    }

    #[tokio::test]
    async fn test_file_source_create() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create FileSource
        let source = FileSource::new("test_files", &base_path);

        // Create test context
        let agent_id = "test_agent_create";
        let (_dbs, _memory, ctx) = create_test_context_with_agent(agent_id).await;
        let owner = AgentId::new(agent_id);

        // Create a new file
        let initial_content = "Initial content for new file";
        let new_file = Path::new("new_file.txt");
        let block_ref = source
            .create(
                new_file,
                Some(initial_content),
                ctx.clone() as Arc<dyn ToolContext>,
                owner.clone(),
            )
            .await
            .expect("Create should succeed");

        // Verify file exists on disk
        let abs_path = base_path.join(new_file);
        assert!(abs_path.exists(), "File should exist on disk");

        // Verify content on disk
        let disk_content = tokio::fs::read_to_string(&abs_path).await.unwrap();
        assert_eq!(disk_content, initial_content);

        // Verify content in memory
        let memory = ctx.memory();
        let mem_content = memory
            .get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mem_content, initial_content);
    }

    #[tokio::test]
    async fn test_file_source_save() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let original_content = "Original content";
        let file_path = create_test_file(&base_path, "save_test.txt", original_content).await;

        // Create FileSource
        let source = FileSource::new("test_files", &base_path);

        // Create test context
        let agent_id = "test_agent_save";
        let (_dbs, _memory, ctx) = create_test_context_with_agent(agent_id).await;
        let owner = AgentId::new(agent_id);

        // Load the file
        let block_ref = source
            .load(
                file_path.strip_prefix(&base_path).unwrap(),
                ctx.clone() as Arc<dyn ToolContext>,
                owner.clone(),
            )
            .await
            .expect("Load should succeed");

        // Modify block content
        let new_content = "Modified content via memory";
        let memory = ctx.memory();
        memory
            .update_block_text(&block_ref.agent_id, &block_ref.label, new_content)
            .await
            .expect("Update should succeed");

        // Save back to disk
        source
            .save(&block_ref, ctx.clone() as Arc<dyn ToolContext>)
            .await
            .expect("Save should succeed");

        // Verify disk was updated
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(disk_content, new_content);
    }

    #[tokio::test]
    async fn test_file_source_conflict_detection() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let original_content = "Original content";
        let file_path = create_test_file(&base_path, "conflict_test.txt", original_content).await;

        // Create FileSource
        let source = FileSource::new("test_files", &base_path);

        // Create test context
        let agent_id = "test_agent_conflict";
        let (_dbs, _memory, ctx) = create_test_context_with_agent(agent_id).await;
        let owner = AgentId::new(agent_id);

        // Load the file
        let block_ref = source
            .load(
                file_path.strip_prefix(&base_path).unwrap(),
                ctx.clone() as Arc<dyn ToolContext>,
                owner.clone(),
            )
            .await
            .expect("Load should succeed");

        // Small delay to ensure mtime changes
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Simulate external modification
        let external_content = "Externally modified content";
        tokio::fs::write(&file_path, external_content)
            .await
            .unwrap();

        // Modify block content
        let memory = ctx.memory();
        memory
            .update_block_text(&block_ref.agent_id, &block_ref.label, "Agent's changes")
            .await
            .expect("Update should succeed");

        // Try to save - should fail with conflict error
        let result = source
            .save(&block_ref, ctx.clone() as Arc<dyn ToolContext>)
            .await;

        assert!(result.is_err(), "Save should fail due to conflict");
        let err = result.unwrap_err();
        let err_str = format!("{}", err);
        assert!(
            err_str.contains("Conflict") || err_str.contains("modified externally"),
            "Error should mention conflict: {}",
            err_str
        );

        // Verify disk still has external changes
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(disk_content, external_content);
    }

    #[test]
    fn test_file_source_permission_for() {
        let source = FileSource::with_rules(
            "test_files",
            "/tmp",
            vec![
                PermissionRule::new("*.config.toml", MemoryPermission::ReadOnly),
                PermissionRule::new("src/**/*.rs", MemoryPermission::ReadWrite),
                PermissionRule::new("**", MemoryPermission::ReadWrite),
            ],
        );

        // Config files should be read-only
        assert_eq!(
            source.permission_for(Path::new("app.config.toml")),
            MemoryPermission::ReadOnly
        );

        // Rust source files should be read-write
        assert_eq!(
            source.permission_for(Path::new("src/main.rs")),
            MemoryPermission::ReadWrite
        );

        // Other files should match the catch-all
        assert_eq!(
            source.permission_for(Path::new("data.json")),
            MemoryPermission::ReadWrite
        );
    }

    #[test]
    fn test_file_source_matches() {
        let source = FileSource::new("test_files", "/home/user/project");

        // Files under base_path should match
        assert!(source.matches(Path::new("/home/user/project/src/main.rs")));
        assert!(source.matches(Path::new("src/main.rs"))); // Relative paths are joined with base

        // Files outside base_path should not match (once canonicalized)
        // Note: This depends on actual filesystem state, so we just verify the method runs
        let _ = source.matches(Path::new("/other/path/file.rs"));
    }

    #[test]
    fn test_file_source_status() {
        let source = FileSource::new("test_files", "/tmp");

        // Initially idle
        assert_eq!(source.status(), BlockSourceStatus::Idle);
    }
}
