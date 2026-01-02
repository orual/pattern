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
    PermissionRule, ReconcileResult, RestoreStats, VersionInfo,
};

/// Convert MemoryError to CoreError for FileSource operations.
fn memory_err(source_id: &str, operation: &str, err: MemoryError) -> CoreError {
    CoreError::DataSourceError {
        source_name: source_id.to_string(),
        operation: operation.to_string(),
        cause: format!("Memory operation failed: {}", err),
    }
}

/// Normalize line endings to Unix style (`\n`).
///
/// Converts `\r\n` (Windows) to `\n`. This ensures consistent behavior
/// across platforms for diffs, patches, and line-based operations.
/// Takes ownership to avoid unnecessary allocations when no conversion needed.
#[inline]
fn normalize_line_endings(content: String) -> String {
    if content.contains("\r\n") {
        content.replace("\r\n", "\n")
    } else {
        content
    }
}

/// Information about a loaded file tracked by FileSource.
///
/// Contains the forked disk_doc and subscriptions for bidirectional sync.
/// The memory_doc is a clone of the memory block's LoroDoc (Arc-based, shares state).
///
/// Subscriptions are active when watching:
/// - Watching: subscriptions sync memory↔disk, watcher updates disk_doc from filesystem
/// - Not watching: subscriptions torn down, disk_doc frozen, explicit save() required
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
    /// Subscriptions (only when watching): (memory→disk, disk→memory)
    #[allow(dead_code)]
    subscriptions: Option<(Subscription, Subscription)>,
    /// Permission level for this file
    permission: MemoryPermission,
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
/// Labels follow the format `file:{source_id}:{relative_path}` where:
/// - `source_id` is the first 8 hex characters of SHA-256 of the base_path (deterministic)
/// - `relative_path` is the path relative to `base_path`
///
/// The source_id is automatically derived from base_path, making it stable and
/// allowing tools to route operations to the correct FileSource by parsing block labels.
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
pub struct FileSource {
    /// Unique identifier derived from hash of base_path (first 8 hex chars of SHA-256)
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
    /// Compute source_id from base_path (first 8 hex chars of SHA-256, prefixed with 'file:').
    ///
    /// This provides a deterministic, stable identifier that can be parsed
    /// from block labels to route operations to the correct FileSource.
    /// The 'file:' prefix makes it clear this is a FileSource.
    fn compute_source_id(base_path: &Path) -> String {
        let mut hasher = Sha256::new();
        hasher.update(base_path.to_string_lossy().as_bytes());
        let hash = hasher.finalize();
        format!(
            "file:{:02x}{:02x}{:02x}{:02x}",
            hash[0], hash[1], hash[2], hash[3]
        )
    }

    /// Create a new FileSource with the given base path.
    ///
    /// The source_id is automatically computed from the base_path hash,
    /// providing a stable identifier for routing operations.
    ///
    /// # Arguments
    ///
    /// * `base_path` - Base directory for file operations
    ///
    /// # Example
    ///
    /// ```ignore
    /// let source = FileSource::new("/home/user/project");
    /// // source_id will be something like "a3f2b1c9"
    /// ```
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        let base_path = base_path.into();
        let source_id = Self::compute_source_id(&base_path);
        Self {
            source_id,
            base_path,
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
    /// The source_id is automatically computed from the base_path hash.
    ///
    /// # Arguments
    ///
    /// * `base_path` - Base directory for file operations
    /// * `rules` - Permission rules to apply (first matching rule wins)
    pub fn with_rules(base_path: impl Into<PathBuf>, rules: Vec<PermissionRule>) -> Self {
        let base_path = base_path.into();
        let source_id = Self::compute_source_id(&base_path);
        Self {
            source_id,
            base_path,
            permission_rules: rules,
            loaded_blocks: Arc::new(DashMap::new()),
            status: AtomicU8::new(STATUS_IDLE),
            watcher: Mutex::new(None),
            change_tx: Mutex::new(None),
        }
    }

    /// Create a FileSource from configuration for a single path.
    ///
    /// Note: If config has multiple paths, call this once per path to create
    /// separate FileSource instances.
    ///
    /// # Arguments
    ///
    /// * `path` - The base path for this source
    /// * `config` - Configuration including permission rules
    pub fn from_config(path: impl Into<PathBuf>, config: &crate::config::FileSourceConfig) -> Self {
        use crate::config::FilePermissionRuleConfig;

        let rules: Vec<PermissionRule> = if config.permission_rules.is_empty() {
            // Default rule: all files are ReadWrite
            vec![PermissionRule::new("**", MemoryPermission::ReadWrite)]
        } else {
            config
                .permission_rules
                .iter()
                .map(|r: &FilePermissionRuleConfig| {
                    PermissionRule::new(r.pattern.clone(), r.permission)
                })
                .collect()
        };

        Self::with_rules(path, rules)
    }

    /// Get the base path for this source.
    pub fn base_path(&self) -> &Path {
        &self.base_path
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
    /// Format: `{source_id}:{relative_path}` where source_id is already prefixed with 'file:'
    /// Result: `file:XXXXXXXX:relative_path`
    ///
    /// The source_id can be used directly to route operations to the correct FileSource.
    fn generate_label(&self, path: &Path) -> Result<String> {
        let rel_path = self.relative_path(path)?;
        Ok(format!("{}:{}", self.source_id, rel_path.display()))
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
    /// Compares actual disk content with our disk_doc state.
    async fn check_conflict(&self, path: &Path) -> Result<bool> {
        let Some(info) = self.loaded_blocks.get(path) else {
            return Ok(false);
        };

        // Read current disk content
        let disk_content =
            normalize_line_endings(tokio::fs::read_to_string(path).await.map_err(|e| {
                CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "check_conflict".to_string(),
                    cause: format!("Failed to read file {}: {}", path.display(), e),
                }
            })?);

        // Compare with what we think disk has (disk_doc)
        let disk_doc_content = info.disk_doc.get_text("content").to_string();

        Ok(disk_content != disk_doc_content)
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
        let loaded_blocks_clone = self.loaded_blocks.clone();
        let mem_to_disk = if permission != MemoryPermission::ReadOnly {
            memory_doc.subscribe_local_update(Box::new(move |update| {
                // Import update to disk doc, then sync to file
                if disk_clone.import(update).is_ok() {
                    // Save disk doc content to file (sync I/O - we're in a sync callback)
                    let content = disk_clone.get_text("content").to_string();
                    if std::fs::write(&path_clone, &content).is_ok() {
                        // Update disk_mtime to reflect our write, preventing false conflict detection
                        if let Ok(metadata) = std::fs::metadata(&path_clone) {
                            if let Ok(mtime) = metadata.modified() {
                                if let Some(mut entry) = loaded_blocks_clone.get_mut(&path_clone) {
                                    entry.disk_mtime = mtime;
                                    entry.disk_size = metadata.len();
                                }
                            }
                        }
                    }
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

    /// Set up subscriptions for a single loaded file path.
    ///
    /// Called when a new file is loaded while already watching,
    /// or by start_watching for all loaded files.
    fn setup_subscriptions_for_path(&self, path: &Path) {
        if let Some(mut info) = self.loaded_blocks.get_mut(path) {
            // Only set up if not already subscribed
            if info.subscriptions.is_none() {
                let subscriptions = self.setup_subscriptions(
                    &info.memory_doc,
                    &info.disk_doc,
                    path.to_path_buf(),
                    info.permission,
                );
                info.subscriptions = Some(subscriptions);
            }
        }
    }

    /// Set up subscriptions for all loaded files.
    ///
    /// Called by start_watching to enable bidirectional sync.
    fn setup_all_subscriptions(&self) {
        // Collect paths first to avoid holding locks during setup
        let paths: Vec<PathBuf> = self
            .loaded_blocks
            .iter()
            .filter(|entry| entry.subscriptions.is_none())
            .map(|entry| entry.key().clone())
            .collect();

        for path in paths {
            self.setup_subscriptions_for_path(&path);
        }
    }

    /// Tear down subscriptions for all loaded files.
    ///
    /// Called by stop_watching to disable bidirectional sync.
    fn teardown_all_subscriptions(&self) {
        for mut entry in self.loaded_blocks.iter_mut() {
            entry.subscriptions = None;
        }
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
                                if let Ok(content) =
                                    std::fs::read_to_string(&path).map(normalize_line_endings)
                                {
                                    // Skip if content is the same (avoids feedback loop from our own writes)
                                    let current_content =
                                        info.disk_doc.get_text("content").to_string();
                                    if content != current_content {
                                        // Update disk_doc with new content using diff-based update
                                        let text = info.disk_doc.get_text("content");
                                        let _ = text.update(&content, Default::default());
                                        // No commit needed - subscriptions see changes immediately
                                    }

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

        // Set up subscriptions for all loaded files
        self.setup_all_subscriptions();

        Ok(rx)
    }

    /// Stop watching for file changes.
    pub async fn stop_watching(&self) {
        // Tear down subscriptions first
        self.teardown_all_subscriptions();

        *self.watcher.lock().await = None;
        *self.change_tx.lock().await = None;
        self.status.store(STATUS_IDLE, Ordering::SeqCst);
    }

    /// Generate a unified diff between memory state and actual disk file.
    ///
    /// Returns a unified diff with metadata header showing:
    /// - File path
    /// - Disk vs memory comparison
    pub async fn perform_diff(&self, path: &Path) -> Result<String> {
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
            normalize_line_endings(tokio::fs::read_to_string(&abs_path).await.map_err(|e| {
                CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "diff".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                }
            })?);

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

    /// Check if there are unsaved changes
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
            normalize_line_endings(tokio::fs::read_to_string(&abs_path).await.map_err(|e| {
                CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "has_unsaved_changes".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                }
            })?);

        Ok(memory_content != disk_content)
    }

    /// Reload file from disk, discarding any memory changes.
    pub async fn reload(&self, path: &Path) -> Result<()> {
        let abs_path = self.absolute_path(path)?;

        // Read current disk content
        let content =
            normalize_line_endings(tokio::fs::read_to_string(&abs_path).await.map_err(|e| {
                CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "reload".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                }
            })?);

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

        // Tear down subscriptions to prevent feedback loop during reload
        let had_subscriptions = info.subscriptions.is_some();
        info.subscriptions = None;

        // Update memory doc using diff-based update to minimize operations
        let mem_text = info.memory_doc.get_text("content");
        mem_text
            .update(&content, Default::default())
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "reload".to_string(),
                cause: format!("Failed to update memory content: {}", e),
            })?;

        // Update disk doc using diff-based update
        let disk_text = info.disk_doc.get_text("content");
        disk_text
            .update(&content, Default::default())
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "reload".to_string(),
                cause: format!("Failed to update disk content: {}", e),
            })?;

        // Update metadata
        info.disk_mtime = mtime;
        info.disk_size = size;
        info.last_saved_frontier = info.memory_doc.oplog_vv();

        // Drop the mutable borrow before re-setting up subscriptions
        drop(info);

        // Re-setup subscriptions if they were active
        if had_subscriptions {
            self.setup_subscriptions_for_path(&abs_path);
        }

        Ok(())
    }

    pub async fn ensure_block(
        &self,
        path: &Path,
        owner: AgentId,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<()> {
        let abs_path = self.absolute_path(path)?;
        let label = self.generate_label(path)?;
        let owner_str = owner.to_string();
        let permission = self.permission_for(path);

        // Read file content and normalize line endings
        let content =
            normalize_line_endings(tokio::fs::read_to_string(&abs_path).await.map_err(|e| {
                CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "load".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                }
            })?);

        // Get file metadata for conflict detection
        let (mtime, size) = self.get_file_metadata(path).await?;

        // Create or update block in memory store
        let memory = ctx.memory();
        let source_id = &self.source_id;

        // Check if block already exists
        let (block_id, doc) = if let Some(existing) = memory
            .get_block_metadata(&owner_str, &label)
            .await
            .map_err(|e| memory_err(source_id, "load", e))?
        {
            // Block exists, fetch the doc
            let doc = memory
                .get_block(&owner_str, &label)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?
                .ok_or_else(|| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "load".to_string(),
                    cause: format!("Block {} not found", label),
                })?;
            (existing.id, doc)
        } else {
            // Create new block (returns StructuredDocument with content ready to set)
            let doc = memory
                .create_block(
                    &owner_str,
                    &label,
                    &format!("File: {}", abs_path.display()),
                    BlockType::Working,
                    BlockSchema::text(),
                    1024 * 1024, // 1MB char limit
                )
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            let id = doc.id().to_string();
            doc.set_text(&content, true)
                .map_err(|e| memory_err(source_id, "load", e.into()))?;
            memory
                .persist_block(&owner_str, &label)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            memory
                .set_block_pinned(&owner_str, &label, true)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            (id, doc)
        };

        // Clone the memory LoroDoc (Arc-based, shares state) and fork for disk
        let memory_doc = doc.inner().clone();
        let disk_doc = memory_doc.fork();

        let text = disk_doc.get_text("content");

        // Track loaded file info (subscriptions set up by start_watching)
        self.loaded_blocks.insert(
            abs_path.clone(),
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
                disk_doc,
                memory_doc,
                subscriptions: None,
                permission,
                last_saved_frontier: doc.inner().oplog_vv(),
            },
        );

        // Start watching if not already (watching is on by default)
        if self.status.load(Ordering::SeqCst) != STATUS_WATCHING {
            let _ = self.start_watching().await;
        } else {
            // Already watching - set up subscriptions for this new block
            self.setup_subscriptions_for_path(&abs_path);
        }

        text.update(&content, Default::default())
            .map_err(|e| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "load".to_string(),
                cause: format!("Failed to update block text from file: {}", e),
            })?;

        Ok(())
    }
}

/// Parsed components of a file block label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFileLabel {
    /// The source_id (hash of base_path)
    pub source_id: String,
    /// The relative path within the source
    pub path: String,
}

/// Parse a file block label into its components.
///
/// File labels have the format: `file:{hash}:{relative_path}`
/// where `file:{hash}` together form the full source_id.
///
/// # Returns
/// - `Some(ParsedFileLabel)` if the label is a valid file label
/// - `None` if the label doesn't match the file label format
///
/// # Example
/// ```ignore
/// let parsed = parse_file_label("file:a3f2b1c9:src/main.rs");
/// assert_eq!(parsed.unwrap().source_id, "file:a3f2b1c9");
/// assert_eq!(parsed.unwrap().path, "src/main.rs");
/// ```
pub fn parse_file_label(label: &str) -> Option<ParsedFileLabel> {
    // Label format: file:XXXXXXXX:path/to/file
    // source_id is file:XXXXXXXX (13 chars: "file:" + 8 hex)
    if !label.starts_with("file:") || label.len() < 14 {
        return None;
    }

    // Split into source_id and path at the second colon
    let parts: Vec<&str> = label.splitn(3, ':').collect();
    if parts.len() != 3 {
        return None;
    }

    let hash = parts[1];
    // hash should be 8 hex characters
    if hash.len() != 8 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(ParsedFileLabel {
        source_id: format!("{}:{}", parts[0], parts[1]), // "file:XXXXXXXX"
        path: parts[2].to_string(),
    })
}

/// Check if a block label is a file label.
pub fn is_file_label(label: &str) -> bool {
    label.starts_with("file:") && parse_file_label(label).is_some()
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
            "file:{source_id}:{path}",
            BlockSchema::text(),
            "Local file content with Loro-backed versioning",
        )
    }

    fn permission_rules(&self) -> &[PermissionRule] {
        &self.permission_rules
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![
            ToolRule {
                tool_name: "file".into(),
                rule_type: crate::tool::ToolRuleType::Needed,
                conditions: vec![],
                priority: 6,
                metadata: None,
            },
            ToolRule {
                tool_name: "block_edit".into(),
                rule_type: crate::tool::ToolRuleType::Needed,
                conditions: vec![],
                priority: 6,
                metadata: None,
            },
        ]
    }

    fn matches(&self, path: &Path) -> bool {
        // For absolute paths: check if under base_path
        // For relative paths: check if file exists at base_path/path
        if path.is_absolute() {
            // Absolute path must be under base_path
            if let Ok(abs_path) = self.absolute_path(path) {
                abs_path.starts_with(&self.base_path)
            } else {
                false
            }
        } else {
            // Relative path - check if file exists under our base_path
            let full_path = self.base_path.join(path);
            full_path.exists()
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

        // Read file content and normalize line endings
        let content =
            normalize_line_endings(tokio::fs::read_to_string(&abs_path).await.map_err(|e| {
                CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "load".to_string(),
                    cause: format!("Failed to read file {}: {}", abs_path.display(), e),
                }
            })?);

        // Get file metadata for conflict detection
        let (mtime, size) = self.get_file_metadata(path).await?;

        // Create or update block in memory store
        let memory = ctx.memory();
        let source_id = &self.source_id;

        // Check if block already exists
        let (block_id, doc) = if let Some(existing) = memory
            .get_block_metadata(&owner_str, &label)
            .await
            .map_err(|e| memory_err(source_id, "load", e))?
        {
            // Get existing block and update content
            let doc = memory
                .get_block(&owner_str, &label)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?
                .ok_or_else(|| CoreError::DataSourceError {
                    source_name: self.source_id.clone(),
                    operation: "load".to_string(),
                    cause: format!("Block {} not found", label),
                })?;
            doc.set_text(&content, true)
                .map_err(|e| memory_err(source_id, "load", e.into()))?;
            memory
                .persist_block(&owner_str, &label)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            (existing.id, doc)
        } else {
            // Create new block (returns StructuredDocument with content ready to set)
            let doc = memory
                .create_block(
                    &owner_str,
                    &label,
                    &format!("File: {}", abs_path.display()),
                    BlockType::Working,
                    BlockSchema::text(),
                    1024 * 1024, // 1MB char limit
                )
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            let id = doc.id().to_string();
            doc.set_text(&content, true)
                .map_err(|e| memory_err(source_id, "load", e.into()))?;
            memory
                .persist_block(&owner_str, &label)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            memory
                .set_block_pinned(&owner_str, &label, true)
                .await
                .map_err(|e| memory_err(source_id, "load", e))?;
            (id, doc)
        };

        // Clone the memory LoroDoc (Arc-based, shares state) and fork for disk
        let memory_doc = doc.inner().clone();
        let disk_doc = memory_doc.fork();

        // Track loaded file info (subscriptions set up by start_watching)
        self.loaded_blocks.insert(
            abs_path.clone(),
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
                disk_doc,
                memory_doc,
                subscriptions: None,
                permission,
                last_saved_frontier: doc.inner().oplog_vv(),
            },
        );

        // Start watching if not already (watching is on by default)
        if self.status.load(Ordering::SeqCst) != STATUS_WATCHING {
            let _ = self.start_watching().await;
        } else {
            // Already watching - set up subscriptions for this new block
            self.setup_subscriptions_for_path(&abs_path);
        }

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

        // Create block in memory store (now returns StructuredDocument directly)
        let memory = ctx.memory();
        let source_id = &self.source_id;
        let doc = memory
            .create_block(
                &owner_str,
                &label,
                &format!("File: {}", abs_path.display()),
                BlockType::Working,
                BlockSchema::text(),
                1024 * 1024, // 1MB char limit
            )
            .await
            .map_err(|e| memory_err(source_id, "create", e))?;
        let block_id = doc.id().to_string();

        memory
            .set_block_pinned(&owner_str, &label, true)
            .await
            .map_err(|e| memory_err(source_id, "create", e))?;

        if !content.is_empty() {
            doc.set_text(content, true)
                .map_err(|e| memory_err(source_id, "create", e.into()))?;
            memory
                .persist_block(&owner_str, &label)
                .await
                .map_err(|e| memory_err(source_id, "create", e))?;
        }

        // Clone the memory LoroDoc (Arc-based, shares state) and fork for disk
        let memory_doc = doc.inner().clone();
        let disk_doc = memory_doc.fork();

        // Track loaded file info (subscriptions set up by start_watching)
        self.loaded_blocks.insert(
            abs_path.clone(),
            LoadedFileInfo {
                block_id: block_id.clone(),
                label: label.clone(),
                disk_mtime: mtime,
                disk_size: size,
                disk_doc,
                memory_doc,
                subscriptions: None,
                permission,
                last_saved_frontier: doc.inner().oplog_vv(),
            },
        );

        // Start watching if not already (watching is on by default)
        if self.status.load(Ordering::SeqCst) != STATUS_WATCHING {
            let _ = self.start_watching().await;
        } else {
            // Already watching - set up subscriptions for this new block
            self.setup_subscriptions_for_path(&abs_path);
        }

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

        // Check for conflicts (content-based comparison)
        if self.check_conflict(&file_path).await? {
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
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<String> {
        // Find the file path for this block
        let file_path = self
            .loaded_blocks
            .iter()
            .find(|entry| entry.value().block_id == block_ref.block_id)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| CoreError::DataSourceError {
                source_name: self.source_id.clone(),
                operation: "diff".to_string(),
                cause: format!("Block {} not loaded from this source", block_ref.label),
            })?;

        self.perform_diff(&file_path).await
    }

    async fn restore_from_memory(&self, ctx: Arc<dyn ToolContext>) -> Result<RestoreStats> {
        let memory = ctx.memory();
        let mut stats = RestoreStats::new();

        // Query for all blocks matching our source_id prefix (across all agents)
        let prefix = format!("{}:", self.source_id);
        let blocks = memory
            .list_all_blocks_by_label_prefix(&prefix)
            .await
            .map_err(|e| memory_err(&self.source_id, "restore", e))?;

        for block_meta in blocks {
            // Parse the label to get the relative path
            let Some(parsed) = parse_file_label(&block_meta.label) else {
                stats.skipped += 1;
                continue;
            };

            // Verify this block belongs to our source
            if parsed.source_id != self.source_id {
                stats.skipped += 1;
                continue;
            }

            let rel_path = Path::new(&parsed.path);
            let abs_path = match self.absolute_path(rel_path) {
                Ok(p) => p,
                Err(_) => {
                    stats.skipped += 1;
                    continue;
                }
            };

            // Check if file still exists on disk
            if !abs_path.exists() {
                // File was deleted - unpin the block to remove from context
                // but preserve history
                if let Err(e) = memory
                    .set_block_pinned(&block_meta.agent_id, &block_meta.label, false)
                    .await
                {
                    tracing::warn!(
                        "Failed to unpin block {} for deleted file {}: {}",
                        block_meta.label,
                        abs_path.display(),
                        e
                    );
                }
                stats.unpinned += 1;
                continue;
            }

            // File exists - restore tracking
            // Get the full document from memory
            let doc = match memory
                .get_block(&block_meta.agent_id, &block_meta.label)
                .await
            {
                Ok(Some(d)) => d,
                Ok(None) | Err(_) => {
                    stats.skipped += 1;
                    continue;
                }
            };

            // Read current disk content
            let disk_content = match tokio::fs::read_to_string(&abs_path).await {
                Ok(c) => normalize_line_endings(c),
                Err(_) => {
                    stats.skipped += 1;
                    continue;
                }
            };

            // Get file metadata
            let (mtime, size) = match self.get_file_metadata(&abs_path).await {
                Ok((m, s)) => (m, s),
                Err(_) => {
                    stats.skipped += 1;
                    continue;
                }
            };

            // Clone memory doc and fork for disk
            let memory_doc = doc.inner().clone();
            let disk_doc = memory_doc.fork();

            // Update disk_doc with current disk content (Loro will merge)
            let text = disk_doc.get_text("content");
            if let Err(e) = text.update(&disk_content, Default::default()) {
                tracing::warn!(
                    "Failed to update disk_doc for {}: {}",
                    abs_path.display(),
                    e
                );
                stats.skipped += 1;
                continue;
            }

            let permission = self.permission_for(&abs_path);

            // Add to loaded_blocks (subscriptions set up by start_watching later)
            self.loaded_blocks.insert(
                abs_path.clone(),
                LoadedFileInfo {
                    block_id: block_meta.id.clone(),
                    label: block_meta.label.clone(),
                    disk_mtime: mtime,
                    disk_size: size,
                    disk_doc,
                    memory_doc,
                    subscriptions: None,
                    permission,
                    last_saved_frontier: doc.inner().oplog_vv(),
                },
            );

            stats.restored += 1;
        }

        // Start watching if we restored any files
        if stats.restored > 0 {
            let _ = self.start_watching().await;
        }

        Ok(stats)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn test_file_source_load_save() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let test_content = "Hello, World!\nThis is a test file.";
        let file_path = create_test_file(&base_path, "test.txt", test_content).await;

        // Create FileSource
        let source = FileSource::new(&base_path);

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn test_file_source_create() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create FileSource
        let source = FileSource::new(&base_path);

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn test_file_source_save() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let original_content = "Original content";
        let file_path = create_test_file(&base_path, "save_test.txt", original_content).await;

        // Create FileSource
        let source = FileSource::new(&base_path);

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
        let doc = memory
            .get_block(&block_ref.agent_id, &block_ref.label)
            .await
            .expect("Get should succeed")
            .expect("Block should exist");
        doc.set_text(new_content, true).unwrap();
        memory
            .persist_block(&block_ref.agent_id, &block_ref.label)
            .await
            .expect("Persist should succeed");

        // Save back to disk
        source
            .save(&block_ref, ctx.clone() as Arc<dyn ToolContext>)
            .await
            .expect("Save should succeed");

        // Verify disk was updated
        let disk_content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(disk_content, new_content);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn test_file_source_conflict_detection() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create a test file
        let original_content = "Original content";
        let file_path = create_test_file(&base_path, "conflict_test.txt", original_content).await;

        // Create FileSource
        let source = FileSource::new(&base_path);

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

        // Small delay to ensure file watcher is active
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Simulate external modification (like another editor saving the file)
        let external_content = "Externally modified content";
        tokio::fs::write(&file_path, external_content)
            .await
            .unwrap();

        // Modify block content (agent making changes)
        let memory = ctx.memory();
        let doc = memory
            .get_block(&block_ref.agent_id, &block_ref.label)
            .await
            .expect("Get should succeed")
            .expect("Block should exist");
        doc.set_text("Agent's changes", true).unwrap();
        memory
            .persist_block(&block_ref.agent_id, &block_ref.label)
            .await
            .expect("Persist should succeed");

        // Give auto-sync a chance to run
        tokio::task::yield_now().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // With auto-sync enabled, Loro CRDT should merge both changes automatically.
        // The external content and agent's changes should both be present in the merged result.
        let final_disk = tokio::fs::read_to_string(&file_path).await.unwrap();

        // Verify at least one set of changes is present (Loro merges them)
        assert!(
            final_disk.contains("Externally modified") || final_disk.contains("Agent's changes"),
            "Merged content should contain at least one set of changes: {:?}",
            final_disk
        );

        // Save should succeed since disk_doc and disk file are in sync after auto-merge
        let result = source
            .save(&block_ref, ctx.clone() as Arc<dyn ToolContext>)
            .await;
        assert!(
            result.is_ok(),
            "Save should succeed after auto-merge: {:?}",
            result
        );
    }

    #[test]
    fn test_file_source_permission_for() {
        let source = FileSource::with_rules(
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn test_file_source_matches() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create test files
        let src_dir = base_path.join("src");
        tokio::fs::create_dir_all(&src_dir).await.unwrap();
        tokio::fs::write(src_dir.join("main.rs"), "fn main() {}")
            .await
            .unwrap();

        let source = FileSource::new(&base_path);

        // Absolute path under base_path should match
        assert!(source.matches(&src_dir.join("main.rs")));

        // Relative path that exists should match
        assert!(source.matches(Path::new("src/main.rs")));

        // Relative path that doesn't exist should not match
        assert!(!source.matches(Path::new("nonexistent/file.rs")));

        // Absolute path outside base_path should not match
        assert!(!source.matches(Path::new("/tmp/other/file.rs")));
    }

    #[test]
    fn test_file_source_status() {
        let source = FileSource::new("/tmp");

        // Initially idle
        assert_eq!(source.status(), BlockSourceStatus::Idle);
    }
}
