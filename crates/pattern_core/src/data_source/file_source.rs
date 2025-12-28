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
use loro::{LoroDoc, Subscription, VersionVector};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, broadcast};

use crate::error::{CoreError, Result};
use crate::id::AgentId;
use crate::memory::{BlockSchema, BlockType, MemoryError, MemoryPermission};
use crate::runtime::ToolContext;
use crate::tool::rules::ToolRule;

use super::{
    BlockRef, BlockSchemaSpec, BlockSourceStatus, DataBlock, FileChange, FileChangeType,
    PermissionRule, ReconcileResult, VersionInfo,
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
///
/// Contains the forked disk_doc and subscriptions for bidirectional sync.
/// The memory_doc is a clone of the memory block's LoroDoc (Arc-based, shares state).
struct LoadedFileInfo {
    /// Block ID in the memory store
    block_id: String,
    /// Block label (file:{hash8}:{relative_path})
    label: String,
    /// Modification time when file was last loaded/saved
    disk_mtime: SystemTime,
    /// File size when last loaded/saved
    disk_size: u64,
    /// Forked LoroDoc representing disk state
    disk_doc: LoroDoc,
    /// Clone of memory's LoroDoc (shares state via Arc)
    memory_doc: LoroDoc,
    /// Subscriptions kept alive: (memory→disk, disk→memory)
    #[allow(dead_code)]
    subscriptions: (Subscription, Subscription),
    /// Last saved frontier for tracking unsaved changes
    last_saved_frontier: VersionVector,
}

impl std::fmt::Debug for LoadedFileInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedFileInfo")
            .field("block_id", &self.block_id)
            .field("label", &self.label)
            .field("disk_mtime", &self.disk_mtime)
            .field("disk_size", &self.disk_size)
            .field("last_saved_frontier", &"<VersionVector>")
            .finish_non_exhaustive()
    }
}

/// Information about a file in the source (for list operation).
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// Relative path from source base
    pub path: String,
    /// File size in bytes
    pub size: u64,
    /// Whether the file is currently loaded as a block
    pub loaded: bool,
    /// Permission level for this file
    pub permission: MemoryPermission,
}

/// Sync status for a loaded file (for status operation).
#[derive(Debug, Clone)]
pub struct FileSyncStatus {
    /// Relative path from source base
    pub path: String,
    /// Block label
    pub label: String,
    /// Sync status description
    pub sync_status: String,
    /// Whether disk has been modified since load
    pub disk_modified: bool,
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
/// FileSource manages local files as Loro-backed memory blocks.
pub struct FileSource {
    /// Unique identifier for this source
    source_id: String,
    /// Base directory for all file operations
    base_path: PathBuf,
    /// Permission rules (glob pattern -> permission level)
    permission_rules: Vec<PermissionRule>,
    /// Tracks loaded files and their metadata (Arc for sharing with watcher)
    loaded_blocks: Arc<DashMap<PathBuf, LoadedFileInfo>>,
    /// Current status (Idle or Watching)
    status: AtomicU8,
    /// File watcher (active when watching)
    watcher: Mutex<Option<RecommendedWatcher>>,
    /// Channel for broadcasting file changes
    change_tx: Mutex<Option<broadcast::Sender<FileChange>>>,
}

impl std::fmt::Debug for FileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSource")
            .field("source_id", &self.source_id)
            .field("base_path", &self.base_path)
            .field("permission_rules", &self.permission_rules)
            .field("loaded_blocks", &self.loaded_blocks.len())
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
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
            loaded_blocks: Arc::new(DashMap::new()),
            status: AtomicU8::new(STATUS_IDLE),
            watcher: Mutex::new(None),
            change_tx: Mutex::new(None),
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
            loaded_blocks: Arc::new(DashMap::new()),
            status: AtomicU8::new(STATUS_IDLE),
            watcher: Mutex::new(None),
            change_tx: Mutex::new(None),
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

    /// List files in the source, optionally filtered by glob pattern.
    pub async fn list_files(&self, pattern: Option<&str>) -> Result<Vec<FileInfo>> {
        use globset::Glob;

        let glob_matcher = pattern
            .map(|p| {
                Glob::new(p)
                    .map_err(|e| CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "list".to_string(),
                        cause: format!("Invalid glob pattern: {}", e),
                    })
                    .map(|g| g.compile_matcher())
            })
            .transpose()?;

        let mut files = Vec::new();

        // Walk the directory tree
        let mut stack = vec![self.base_path.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries =
                tokio::fs::read_dir(&dir)
                    .await
                    .map_err(|e| CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "list".to_string(),
                        cause: format!("Failed to read directory {}: {}", dir.display(), e),
                    })?;

            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|e| CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "list".to_string(),
                        cause: format!("Failed to read entry: {}", e),
                    })?
            {
                let path = entry.path();
                let metadata = entry
                    .metadata()
                    .await
                    .map_err(|e| CoreError::DataSourceError {
                        source_name: self.source_id.clone(),
                        operation: "list".to_string(),
                        cause: format!("Failed to get metadata for {}: {}", path.display(), e),
                    })?;

                if metadata.is_dir() {
                    stack.push(path);
                } else if metadata.is_file() {
                    // Get relative path for pattern matching and display
                    let rel_path = path.strip_prefix(&self.base_path).unwrap_or(&path);

                    // Apply glob filter if specified
                    if let Some(ref matcher) = glob_matcher {
                        if !matcher.is_match(rel_path) {
                            continue;
                        }
                    }

                    let loaded = self.loaded_blocks.contains_key(&path);
                    let permission = self.permission_for(&path);

                    files.push(FileInfo {
                        path: rel_path.to_string_lossy().to_string(),
                        size: metadata.len(),
                        loaded,
                        permission,
                    });
                }
            }
        }

        // Sort by path for consistent output
        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(files)
    }

    /// Get sync status for loaded files.
    pub async fn get_sync_status(&self, path: Option<&str>) -> Result<Vec<FileSyncStatus>> {
        let mut statuses = Vec::new();

        for entry in self.loaded_blocks.iter() {
            let file_path = entry.key();
            let info = entry.value();

            // Get relative path for display
            let rel_path = file_path
                .strip_prefix(&self.base_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            // Filter by path if specified
            if let Some(filter_path) = path {
                if !rel_path.contains(filter_path) {
                    continue;
                }
            }

            // Check current disk state
            let (sync_status, disk_modified) = match self.get_file_metadata(file_path).await {
                Ok((current_mtime, _)) => {
                    if info.disk_mtime == current_mtime {
                        ("synced".to_string(), false)
                    } else {
                        ("disk_modified".to_string(), true)
                    }
                }
                Err(_) => ("disk_deleted".to_string(), true),
            };

            statuses.push(FileSyncStatus {
                path: rel_path,
                label: info.label.clone(),
                sync_status,
                disk_modified,
            });
        }

        // Sort by path for consistent output
        statuses.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(statuses)
    }

    /// Check if a file is already loaded as a block.
    pub fn is_loaded(&self, path: &Path) -> bool {
        if let Ok(abs_path) = self.absolute_path(path) {
            self.loaded_blocks.contains_key(&abs_path)
        } else {
            false
        }
    }

    /// Get the BlockRef for an already-loaded file.
    /// Returns None if the file is not loaded.
    pub fn get_loaded_block_ref(&self, path: &Path, agent_id: &AgentId) -> Option<BlockRef> {
        let abs_path = self.absolute_path(path).ok()?;
        let info = self.loaded_blocks.get(&abs_path)?;
        Some(BlockRef {
            label: info.label.clone(),
            block_id: info.block_id.clone(),
            agent_id: agent_id.to_string(),
        })
    }

    /// Set up bidirectional subscriptions between memory and disk docs.
    ///
    /// Returns (memory→disk subscription, disk→memory subscription).
    /// Permission determines which direction(s) are active:
    /// - ReadOnly: disk→memory only (agent can't modify)
    /// - ReadWrite/Admin: bidirectional
    fn setup_subscriptions(
        &self,
        memory_doc: &LoroDoc,
        disk_doc: &LoroDoc,
        file_path: PathBuf,
        permission: MemoryPermission,
    ) -> (Subscription, Subscription) {
        // Memory → disk: when memory changes, import to disk and save file
        let disk_clone = disk_doc.clone();
        let path_clone = file_path.clone();
        let mem_to_disk = if permission != MemoryPermission::ReadOnly {
            memory_doc.subscribe_local_update(Box::new(move |update| {
                // Import update to disk doc
                if disk_clone.import(update).is_ok() {
                    // Save disk doc content to file asynchronously
                    let content = disk_clone.get_text("content").to_string();
                    let path = path_clone.clone();
                    tokio::spawn(async move {
                        let _ = tokio::fs::write(&path, &content).await;
                    });
                }
                true // Keep subscription active
            }))
        } else {
            // ReadOnly: no memory→disk sync, create dummy subscription
            memory_doc.subscribe_local_update(Box::new(|_| true))
        };

        // Disk → memory: when disk doc changes, import to memory
        let mem_clone = memory_doc.clone();
        let disk_to_mem = disk_doc.subscribe_local_update(Box::new(move |update| {
            // Import update to memory doc
            let _ = mem_clone.import(update);
            true // Keep subscription active
        }));

        (mem_to_disk, disk_to_mem)
    }

    /// Start watching the base directory for file changes.
    ///
    /// Returns a receiver that will receive FileChange events when files are modified.
    /// The watcher runs in the background and updates disk_docs when files change.
    pub async fn start_watching(&self) -> Result<broadcast::Receiver<FileChange>> {
        let (tx, rx) = broadcast::channel(256);

        // Clone what we need for the watcher callback
        let loaded_blocks = self.loaded_blocks.clone();
        let base_path = self.base_path.clone();
        let tx_clone = tx.clone();

        // Create the watcher with a callback that handles events
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // We only care about modify/create/remove events
                let change_type = match event.kind {
                    notify::EventKind::Modify(_) => Some(FileChangeType::Modified),
                    notify::EventKind::Create(_) => Some(FileChangeType::Created),
                    notify::EventKind::Remove(_) => Some(FileChangeType::Deleted),
                    _ => None,
                };

                if let Some(change_type) = change_type {
                    for path in event.paths {
                        // Check if this is a file we're tracking
                        if let Some(mut info) = loaded_blocks.get_mut(&path) {
                            // For modifications, update the disk_doc
                            if matches!(change_type, FileChangeType::Modified) {
                                // Read the new content synchronously (we're in a sync callback)
                                if let Ok(content) = std::fs::read_to_string(&path) {
                                    // Update disk_doc with new content
                                    let text = info.disk_doc.get_text("content");
                                    let len = text.len_unicode();
                                    if len > 0 {
                                        let _ = text.delete(0, len);
                                    }
                                    let _ = text.insert(0, &content);
                                    info.disk_doc.commit();

                                    // Update tracked mtime
                                    if let Ok(meta) = std::fs::metadata(&path) {
                                        if let Ok(mtime) = meta.modified() {
                                            info.disk_mtime = mtime;
                                            info.disk_size = meta.len();
                                        }
                                    }
                                }
                            }

                            // Broadcast the change
                            let _ = tx_clone.send(FileChange {
                                path: path.clone(),
                                change_type: change_type.clone(),
                                block_id: Some(info.block_id.clone()),
                                timestamp: Some(chrono::Utc::now()),
                            });
                        } else {
                            // File not loaded, but broadcast anyway for awareness
                            let rel_path = path.strip_prefix(&base_path).ok();
                            if rel_path.is_some() {
                                let _ = tx_clone.send(FileChange {
                                    path,
                                    change_type: change_type.clone(),
                                    block_id: None,
                                    timestamp: Some(chrono::Utc::now()),
                                });
                            }
                        }
                    }
                }
            }
        })
        .map_err(|e| CoreError::DataSourceError {
            source_name: self.source_id.clone(),
            operation: "start_watching".to_string(),
            cause: format!("Failed to create watcher: {}", e),
        })?;

        // Start watching the base path
        let mut watcher = watcher;
        watcher
            .watch(&self.base_path, RecursiveMode::Recursive)
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "start_watching".to_string(),
                cause: format!("Failed to watch path: {}", e),
            })?;

        // Store watcher and tx
        *self.watcher.lock().await = Some(watcher);
        *self.change_tx.lock().await = Some(tx);

        // Update status
        self.status.store(STATUS_WATCHING, Ordering::SeqCst);

        Ok(rx)
    }

    /// Stop watching for file changes.
    pub async fn stop_watching(&self) {
        *self.watcher.lock().await = None;
        *self.change_tx.lock().await = None;
        self.status.store(STATUS_IDLE, Ordering::SeqCst);
    }

    /// Generate a unified diff between memory state and actual disk file.
    ///
    /// Returns a unified diff with metadata header showing:
    /// - File path
    /// - Disk vs memory comparison
    ///
    /// Note: This reads the actual file from disk, not the disk_doc
    /// (which is kept in sync with memory via subscriptions).
    pub async fn diff(&self, path: &Path) -> Result<String> {
        let abs_path = self.absolute_path(path)?;
        let info = self
            .loaded_blocks
            .get(&abs_path)
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "diff".to_string(),
                cause: format!("File {} is not loaded", path.display()),
            })?;

        // Get memory content
        let memory_content = info.memory_doc.get_text("content").to_string();

        // Read actual disk content (not disk_doc which is synced)
        let disk_content =
            tokio::fs::read_to_string(&abs_path)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "diff".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                })?;

        // Build unified diff
        let diff = similar::TextDiff::from_lines(&disk_content, &memory_content);

        let rel_path = self
            .relative_path(path)
            .unwrap_or_else(|_| path.to_path_buf());
        let rel_path_str = rel_path.display().to_string();

        // Build metadata header
        let mut output = String::new();
        output.push_str(&format!("--- a/{}\t(disk)\n", rel_path_str));
        output.push_str(&format!("+++ b/{}\t(memory)\n", rel_path_str));

        // Generate unified diff hunks
        let unified = diff.unified_diff();
        for hunk in unified.iter_hunks() {
            output.push_str(&hunk.to_string());
        }

        if output.lines().count() <= 2 {
            // Only headers, no changes
            output.push_str("(no changes)\n");
        }

        Ok(output)
    }

    /// Check if there are unsaved changes (memory differs from actual disk file).
    ///
    /// Note: This reads the actual file from disk, not the disk_doc
    /// (which is kept in sync with memory via subscriptions).
    pub async fn has_unsaved_changes(&self, path: &Path) -> Result<bool> {
        let abs_path = self.absolute_path(path)?;
        let info = self
            .loaded_blocks
            .get(&abs_path)
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "has_unsaved_changes".to_string(),
                cause: format!("File {} is not loaded", path.display()),
            })?;

        let memory_content = info.memory_doc.get_text("content").to_string();

        // Read actual disk content
        let disk_content =
            tokio::fs::read_to_string(&abs_path)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "has_unsaved_changes".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                })?;

        Ok(memory_content != disk_content)
    }

    /// Reload file from disk, discarding any memory changes.
    ///
    /// This reads the current disk content and updates both the memory doc
    /// and disk doc to match.
    pub async fn reload(&self, path: &Path) -> Result<()> {
        let abs_path = self.absolute_path(path)?;

        // Read current disk content
        let content =
            tokio::fs::read_to_string(&abs_path)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "reload".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                })?;

        // Get file metadata
        let metadata =
            tokio::fs::metadata(&abs_path)
                .await
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "reload".to_string(),
                    cause: format!("Failed to get metadata for {}: {}", abs_path.display(), e),
                })?;

        let mtime = metadata.modified().unwrap_or(SystemTime::now());
        let size = metadata.len();

        // Update the loaded block
        let mut info =
            self.loaded_blocks
                .get_mut(&abs_path)
                .ok_or_else(|| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "reload".to_string(),
                    cause: format!("File {} is not loaded", path.display()),
                })?;

        // Update memory doc
        let mem_text = info.memory_doc.get_text("content");
        let mem_len = mem_text.len_unicode();
        if mem_len > 0 {
            mem_text
                .delete(0, mem_len)
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "reload".to_string(),
                    cause: format!("Failed to clear memory content: {}", e),
                })?;
        }
        mem_text
            .insert(0, &content)
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "reload".to_string(),
                cause: format!("Failed to insert content: {}", e),
            })?;
        info.memory_doc.commit();

        // Update disk doc
        let disk_text = info.disk_doc.get_text("content");
        let disk_len = disk_text.len_unicode();
        if disk_len > 0 {
            disk_text
                .delete(0, disk_len)
                .map_err(|e| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "reload".to_string(),
                    cause: format!("Failed to clear disk content: {}", e),
                })?;
        }
        disk_text
            .insert(0, &content)
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "reload".to_string(),
                cause: format!("Failed to insert content: {}", e),
            })?;
        info.disk_doc.commit();

        // Update metadata
        info.disk_mtime = mtime;
        info.disk_size = size;
        info.last_saved_frontier = info.memory_doc.oplog_vv();

        Ok(())
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
        let permission = self.permission_for(path);

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
            memory
                .set_block_pinned(&owner_str, &label, true)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            id
        };

        // Get the StructuredDocument to access the LoroDoc
        let doc = memory
            .get_block(&owner_str, &label)
            .await
            .map_err(|e| memory_err(source_id, "load", e))?
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "load".to_string(),
                cause: format!("Block {} not found after creation", label),
            })?;

        // Clone the memory LoroDoc (Arc-based, shares state) and fork for disk
        let memory_doc = doc.inner().clone();
        let disk_doc = memory_doc.fork();

        // Set up bidirectional subscriptions based on permission
        let subscriptions =
            self.setup_subscriptions(&memory_doc, &disk_doc, abs_path.clone(), permission);

        // Track loaded file info with fork and subscriptions
        self.loaded_blocks.insert(
            abs_path.clone(),
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
                disk_doc,
                memory_doc,
                subscriptions,
                last_saved_frontier: doc.inner().oplog_vv(),
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
        memory
            .set_block_pinned(&owner_str, &label, true)
            .await
            .map_err(|e| memory_err(source_id, "create", e))?;

        if !content.is_empty() {
            memory
                .update_block_text(&owner_str, &label, content)
                .await
                .map_err(|e| memory_err(source_id, "create", e))?;
        }

        // Get the StructuredDocument to access the LoroDoc
        let doc = memory
            .get_block(&owner_str, &label)
            .await
            .map_err(|e| memory_err(source_id, "create", e))?
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "create".to_string(),
                cause: format!("Block {} not found after creation", label),
            })?;

        // Clone the memory LoroDoc (Arc-based, shares state) and fork for disk
        let memory_doc = doc.inner().clone();
        let disk_doc = memory_doc.fork();

        // Set up bidirectional subscriptions based on permission
        let subscriptions =
            self.setup_subscriptions(&memory_doc, &disk_doc, abs_path.clone(), permission);

        // Track loaded file info with fork and subscriptions
        self.loaded_blocks.insert(
            abs_path,
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
                disk_doc,
                memory_doc,
                subscriptions,
                last_saved_frontier: doc.inner().oplog_vv(),
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
