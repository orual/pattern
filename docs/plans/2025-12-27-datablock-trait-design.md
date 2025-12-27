# DataBlock Trait Design

## Purpose

Present files and persistent documents as Loro-backed memory blocks with gated edits, version history, and rollback capabilities. Agent works with these like documents, pulling content when needed.

## Design Principles

**No generics on the trait** - Same as DataStream, type safety at source boundary.

**Loro as working state** - Agent's view of the file, with full version history and rollback.

**Disk as canonical** - External changes (shell, editor, other processes) win. Watch detects changes and updates Loro overlay.

**Permission-gated writes** - Glob-based rules determine what agent can do with each path.

## Sync Model

```
Agent tools ←→ Loro ←→ Disk ←→ Editor (ACP)
                 ↑
            Shell side effects
```

Multiple writers:
- **Agent edits** via tools → Loro → disk (permission-gated)
- **Shell commands** → disk → reconcile to Loro
- **Editor (ACP)** → disk or direct → reconcile to Loro
- **External processes** → disk → watch triggers Loro update

Loro overlay can be ahead of disk (uncommitted changes) or behind (external changes detected).

## Types

```rust
pub struct PermissionRule {
    pub pattern: String,  // Glob pattern: "*.config.toml", "src/**/*.rs"
    pub permission: MemoryPermission,
    pub operations_requiring_escalation: Vec<String>,  // e.g., ["delete", "overwrite"]
}

pub struct FileChange {
    pub path: String,
    pub change_type: FileChangeType,
    pub block_id: Option<String>,  // If we have a loaded block for this path
}

pub enum FileChangeType {
    Modified,
    Created,
    Deleted,
}

/// Spec for documentation
pub struct DataBlockSpec {
    pub base_path: String,
    pub permission_rules: Vec<PermissionRule>,
    pub watch_enabled: bool,
    pub description: String,
}
```

## Trait Definition

```rust
#[async_trait]
pub trait DataBlock: Send + Sync {
    /// Unique identifier for this block source
    fn source_id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Schema for content (typically Text for files)
    fn schema(&self) -> BlockSchema;

    /// Permission rules (glob patterns → permission levels)
    fn permission_rules(&self) -> &[PermissionRule];

    /// Tools required when working with this source
    fn required_tools(&self) -> Vec<ToolRule>;

    /// Check if path matches this source's scope
    fn matches(&self, path: &str) -> bool;

    /// Get permission for a specific path
    fn permission_for(&self, path: &str) -> MemoryPermission;

    // === Load/Save Operations ===

    /// Load file content into memory store as a block
    async fn load(
        &self,
        path: &str,
        memory: &dyn MemoryStore,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Create a new file with optional initial content
    async fn create(
        &self,
        path: &str,
        initial_content: Option<&str>,
        memory: &dyn MemoryStore,
        owner: AgentId,
    ) -> Result<BlockRef>;

    /// Save block back to disk (permission-gated)
    async fn save(
        &self,
        block_ref: &BlockRef,
        memory: &dyn MemoryStore,
    ) -> Result<()>;

    /// Delete file (usually requires escalation)
    async fn delete(
        &self,
        path: &str,
        memory: &dyn MemoryStore,
    ) -> Result<()>;

    // === Watch/Reconcile ===

    /// Start watching for external changes (optional)
    fn start_watch(&mut self) -> Option<broadcast::Receiver<FileChange>>;

    /// Stop watching
    fn stop_watch(&mut self);

    /// Reconcile disk state with Loro overlay after external changes
    /// Called by shell tool after command execution, or by watch handler
    async fn reconcile(
        &self,
        paths: &[String],
        memory: &dyn MemoryStore,
    ) -> Result<Vec<FileChange>>;

    // === History Operations ===

    /// Get version history for a loaded block
    async fn history(
        &self,
        block_ref: &BlockRef,
        memory: &dyn MemoryStore,
    ) -> Result<Vec<VersionInfo>>;

    /// Rollback to a previous version
    async fn rollback(
        &self,
        block_ref: &BlockRef,
        version: &str,
        memory: &dyn MemoryStore,
    ) -> Result<()>;

    /// Diff between versions or current vs disk
    async fn diff(
        &self,
        block_ref: &BlockRef,
        from: Option<&str>,  // None = disk
        to: Option<&str>,    // None = current Loro state
        memory: &dyn MemoryStore,
    ) -> Result<String>;
}

pub struct VersionInfo {
    pub version_id: String,
    pub timestamp: DateTime<Utc>,
    pub description: Option<String>,
}
```

## Watch Semantics

### Scope

- **Single file path** → watch that file only
- **Directory path** → watch recursively, including subdirs
- **Respect ignore files** → `.gitignore`, `.loroignore`, or similar to exclude paths

### Debouncing

Loro has built-in change merging via `set_change_merge_interval()`. For file watching:
- Use filesystem watcher's debounce (e.g., `notify` crate's debounced watcher)
- Recommended: 100-500ms debounce window for typical editing
- Loro's internal merge interval can be shorter for responsive collaboration

### Conflict Handling via Fork-and-Compare

Rather than blindly merging, we fork the doc to compare both sides independently:

```rust
async fn reconcile(&self, path: &str, memory: &dyn MemoryStore) -> Result<ReconcileResult> {
    let block = memory.get_block(&owner, &label).await?;
    let loro_doc = block.loro_doc();

    // Get the last known synced state (common ancestor)
    let last_synced = self.get_last_synced_frontiers(path)?;

    // Fork 1: Apply disk state
    let disk_fork = loro_doc.fork_at(&last_synced)?;
    let disk_content = fs::read_to_string(path).await?;
    {
        let text = disk_fork.get_text("content");
        text.delete(0, text.len_unicode())?;
        text.insert(0, &disk_content)?;
        disk_fork.commit();
    }

    // Fork 2: Current agent state (already in loro_doc)
    let agent_fork = loro_doc.fork_at(&last_synced)?;
    agent_fork.import(&loro_doc.export(ExportMode::updates(&last_synced)))?;

    // Diff both forks against common ancestor
    let disk_changes = disk_fork.diff(&last_synced, &disk_fork.state_frontiers());
    let agent_changes = agent_fork.diff(&last_synced, &agent_fork.state_frontiers());

    // Decide which wins based on source rules
    let resolution = self.resolve_conflict(path, &disk_changes, &agent_changes)?;

    match resolution {
        ConflictResolution::DiskWins => {
            // Replace agent state with disk state
            loro_doc.checkout(&last_synced)?;
            loro_doc.import(&disk_fork.export(ExportMode::all_updates()))?;
        }
        ConflictResolution::AgentWins => {
            // Keep agent state, ignore disk changes
            // (disk will be overwritten on next save)
        }
        ConflictResolution::Merge => {
            // Import both - CRDT merges, most recent ops win ties
            loro_doc.import(&disk_fork.export(ExportMode::all_updates()))?;
        }
        ConflictResolution::Conflict { disk_diff, agent_diff } => {
            // Can't auto-resolve - surface to agent for decision
            return Ok(ReconcileResult::NeedsResolution {
                path: path.to_string(),
                disk_changes: disk_diff,
                agent_changes: agent_diff,
            });
        }
    }

    // Update last synced frontier
    self.set_last_synced_frontiers(path, loro_doc.state_frontiers())?;

    Ok(ReconcileResult::Resolved {
        path: path.to_string(),
        resolution,
        final_frontiers: loro_doc.state_frontiers(),
    })
}

fn resolve_conflict(
    &self,
    path: &str,
    disk_changes: &Diff,
    agent_changes: &Diff,
) -> Result<ConflictResolution> {
    let permission = self.permission_for(path);

    // No changes on one side = easy
    if disk_changes.is_empty() {
        return Ok(ConflictResolution::AgentWins);
    }
    if agent_changes.is_empty() {
        return Ok(ConflictResolution::DiskWins);
    }

    // Both sides changed - use permission rules
    match permission {
        MemoryPermission::ReadOnly => {
            // Agent shouldn't have been able to edit, disk wins
            Ok(ConflictResolution::DiskWins)
        }
        MemoryPermission::Human => {
            // Requires human decision
            Ok(ConflictResolution::Conflict {
                disk_diff: disk_changes.clone(),
                agent_diff: agent_changes.clone(),
            })
        }
        MemoryPermission::ReadWrite => {
            // Agent has full access - merge, agent ops generally win ties
            Ok(ConflictResolution::Merge)
        }
    }
}

pub enum ConflictResolution {
    DiskWins,
    AgentWins,
    Merge,
    Conflict { disk_diff: Diff, agent_diff: Diff },
}

pub enum ReconcileResult {
    Resolved {
        path: String,
        resolution: ConflictResolution,
        final_frontiers: Frontiers,
    },
    NeedsResolution {
        path: String,
        disk_changes: Diff,
        agent_changes: Diff,
    },
}
```

**Resolution rules by permission:**

| Permission | Both Sides Changed | Behavior |
|------------|-------------------|----------|
| ReadOnly | Disk wins | Agent edits discarded (shouldn't have happened) |
| Human | Surface conflict | Agent must explicitly choose |
| ReadWrite | Merge (agent priority) | CRDT merge, agent ops win ties |

### Watching Implementation

```rust
pub struct FileWatcher {
    watcher: RecommendedWatcher,
    tx: broadcast::Sender<FileChange>,
    ignore_patterns: Vec<GlobPattern>,
}

impl FileWatcher {
    pub fn new(base_path: &Path, ignore_file: Option<&Path>) -> Result<Self> {
        let (tx, _) = broadcast::channel(256);

        // Load ignore patterns
        let ignore_patterns = ignore_file
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|content| parse_gitignore(&content))
            .unwrap_or_default();

        // Create debounced watcher
        let watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                // Filter by ignore patterns
                for path in event.paths {
                    if !should_ignore(&path, &ignore_patterns) {
                        let change = FileChange {
                            path: path.to_string_lossy().to_string(),
                            change_type: event.kind.into(),
                            block_id: None,  // Filled in by reconcile
                        };
                        let _ = tx.send(change);
                    }
                }
            }
        })?;

        Ok(Self { watcher, tx, ignore_patterns })
    }

    pub fn watch(&mut self, path: &Path) -> Result<()> {
        self.watcher.watch(path, RecursiveMode::Recursive)?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FileChange> {
        self.tx.subscribe()
    }
}
```

### Watch Event Flow

1. Filesystem change detected by `notify` watcher
2. Debounce window elapses
3. `FileChange` emitted on broadcast channel
4. Coordinator receives, checks if path has loaded block
5. If loaded: call `reconcile()` to merge disk state into Loro
6. If significant changes: optionally notify agent via activity log

### Detached Mode for Staging

For complex operations, can use Loro's detached mode:

```rust
// Stage changes without applying
loro_doc.detach();

// Import external changes to oplog only
loro_doc.import(&update)?;

// Preview diff before applying
let diff = loro_doc.diff(&loro_doc.state_frontiers(), &loro_doc.oplog_frontiers());

// Apply when ready
loro_doc.checkout_to_latest();
// or revert:
// loro_doc.attach();  // Discard staged changes
```

This allows agent to review external changes before accepting them.

## Shell Tool Integration

The shell/bash tool needs pre/post hooks to:
1. **Enforce permissions** - prevent operations outside permitted boundaries
2. **Capture changes** - reconcile Loro state with what shell did
3. **Report violations** - log/warn if command exceeded permissions

### Pre-Execution Hook

Before running a shell command:

```rust
pub struct ShellPreHook {
    file_source: Arc<dyn DataBlock>,
    permission_rules: Vec<PermissionRule>,
}

impl ShellPreHook {
    /// Analyze command and check if it would violate permissions
    pub fn check(&self, command: &str, working_dir: &Path) -> PreHookResult {
        // Best-effort static analysis of command
        let predicted_paths = analyze_command_paths(command, working_dir);

        let mut violations = Vec::new();
        let mut warnings = Vec::new();

        for path in &predicted_paths {
            let permission = self.file_source.permission_for(path);
            let op = predict_operation(command, path);  // read, write, delete, etc.

            match (permission, op) {
                (MemoryPermission::ReadOnly, Operation::Write | Operation::Delete) => {
                    violations.push(PermissionViolation {
                        path: path.clone(),
                        attempted: op,
                        allowed: permission,
                    });
                }
                (MemoryPermission::Human, op) if op.is_destructive() => {
                    warnings.push(EscalationRequired {
                        path: path.clone(),
                        operation: op,
                        reason: "Requires human approval".into(),
                    });
                }
                _ => {}
            }
        }

        // Also check for unpredictable commands
        if is_unpredictable(command) {
            warnings.push(Warning::UnpredictableCommand(command.to_string()));
        }

        PreHookResult { violations, warnings, predicted_paths }
    }
}

pub enum PreHookDecision {
    Allow,
    AllowWithWarning(Vec<Warning>),
    Block(Vec<PermissionViolation>),
    RequireEscalation(Vec<EscalationRequired>),
}
```

**Command analysis heuristics:**
- Parse common patterns: `rm`, `mv`, `cp`, `echo >`, `cat >`, `sed -i`, etc.
- Extract path arguments
- Resolve relative paths against working directory
- Flag unpredictable commands: pipes to unknown programs, `eval`, `$(...)`, etc.

### Post-Execution Hook

After shell command completes:

```rust
pub struct ShellPostHook {
    file_source: Arc<dyn DataBlock>,
    memory: Arc<dyn MemoryStore>,
}

impl ShellPostHook {
    /// Reconcile filesystem state and report what changed
    pub async fn reconcile(
        &self,
        command: &str,
        predicted_paths: &[String],
        snapshot_before: &FilesystemSnapshot,
    ) -> PostHookResult {
        // Compare before/after for predicted paths
        let mut changes = Vec::new();
        let mut violations = Vec::new();

        for path in predicted_paths {
            if let Some(change) = detect_change(path, snapshot_before) {
                // Reconcile if we have a loaded block
                if self.has_loaded_block(path) {
                    let reconcile_result = self.file_source
                        .reconcile(&[path.clone()], &*self.memory)
                        .await?;
                    changes.push(reconcile_result);
                }

                // Check if change violated permissions
                let permission = self.file_source.permission_for(path);
                if !permission.allows(&change.operation) {
                    violations.push(PostViolation {
                        path: path.clone(),
                        operation: change.operation,
                        permission,
                        // Can't undo, but we log it
                    });
                }
            }
        }

        // Also scan for unexpected changes (paths we didn't predict)
        let unexpected = scan_for_unexpected_changes(
            &self.file_source.base_path(),
            snapshot_before,
            predicted_paths,
        );

        PostHookResult { changes, violations, unexpected }
    }
}

/// Lightweight snapshot of file metadata for comparison
pub struct FilesystemSnapshot {
    entries: HashMap<PathBuf, FileMetadata>,
}

impl FilesystemSnapshot {
    pub fn capture(paths: &[PathBuf]) -> Self {
        let entries = paths.iter()
            .filter_map(|p| {
                fs::metadata(p).ok().map(|m| (p.clone(), FileMetadata::from(m)))
            })
            .collect();
        Self { entries }
    }
}

pub struct FileMetadata {
    pub mtime: SystemTime,
    pub size: u64,
    pub exists: bool,
}
```

### Shell Tool Execution Flow

```rust
impl ShellTool {
    pub async fn execute(
        &self,
        command: &str,
        file_source: &dyn DataBlock,
        memory: &dyn MemoryStore,
    ) -> Result<ShellOutput> {
        let working_dir = self.working_dir();

        // 1. Pre-hook: analyze and check permissions
        let pre_result = self.pre_hook.check(command, &working_dir);

        match pre_result.decision() {
            PreHookDecision::Block(violations) => {
                return Err(ShellError::PermissionDenied(violations));
            }
            PreHookDecision::RequireEscalation(items) => {
                // Could prompt agent or queue for human approval
                return Err(ShellError::EscalationRequired(items));
            }
            PreHookDecision::AllowWithWarning(warnings) => {
                // Log warnings but proceed
                for w in warnings {
                    tracing::warn!(?w, "Shell command warning");
                }
            }
            PreHookDecision::Allow => {}
        }

        // 2. Snapshot filesystem state before
        let snapshot = FilesystemSnapshot::capture(&pre_result.predicted_paths);

        // 3. Execute command
        let output = run_command(command, &working_dir).await?;

        // 4. Post-hook: reconcile and report
        let post_result = self.post_hook
            .reconcile(command, &pre_result.predicted_paths, &snapshot)
            .await?;

        // Log any violations (can't prevent, but we track them)
        for v in &post_result.violations {
            tracing::error!(?v, "Shell command violated permissions");
        }

        // Report unexpected changes
        for u in &post_result.unexpected {
            tracing::warn!(?u, "Shell command made unexpected file change");
        }

        Ok(ShellOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.status.code(),
            file_changes: post_result.changes,
            violations: post_result.violations,
        })
    }
}
```

### Permission Enforcement Levels

| Level | Pre-Hook | Post-Hook | Use Case |
|-------|----------|-----------|----------|
| **Strict** | Block on predicted violation | Report violations | Production agents |
| **Warned** | Warn but allow | Report violations | Development/testing |
| **Audit** | Log only | Log only | Debugging, permissive agents |
| **None** | Skip | Skip | Trusted environments |

### Sandboxing (Future)

For stronger isolation, could integrate:
- **bubblewrap/firejail** - Linux sandboxing with path restrictions
- **chroot** - Restrict filesystem view
- **seccomp** - Syscall filtering

These would provide enforcement beyond best-effort analysis, but add complexity and platform dependencies.

## Editor Integration (ACP) - Future

Agent Client Protocol integration points:
- File open/close events → load/unload blocks
- Editor saves → treated like disk change, reconcile
- Editor buffer changes → could sync to Loro in real-time (collaborative editing)
- Agent saves → push to editor buffer and/or disk

## Example: FileSource Implementation

```rust
pub struct FileSource {
    base_path: PathBuf,
    permission_rules: Vec<PermissionRule>,
    watcher: Option<RecommendedWatcher>,
    watch_tx: Option<broadcast::Sender<FileChange>>,

    // Track which paths have loaded blocks
    loaded_blocks: DashMap<PathBuf, String>,  // path → block_id
}

#[async_trait]
impl DataBlock for FileSource {
    fn source_id(&self) -> &str { "file" }
    fn name(&self) -> &str { "Local Files" }

    fn schema(&self) -> BlockSchema {
        BlockSchema::Text  // Files are text blocks
    }

    fn permission_rules(&self) -> &[PermissionRule] {
        &self.permission_rules
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![
            // file tool with operations gated by permission
            ToolRule::new("file").with_allowed_operations(
                ["read", "append", "insert", "patch", "save"]
            ),
            ToolRule::new("file_history").with_allowed_operations(
                ["view", "diff", "rollback"]
            ),
        ]
    }

    fn matches(&self, path: &str) -> bool {
        Path::new(path).starts_with(&self.base_path)
    }

    fn permission_for(&self, path: &str) -> MemoryPermission {
        for rule in &self.permission_rules {
            if glob_match(&rule.pattern, path) {
                return rule.permission.clone();
            }
        }
        MemoryPermission::ReadOnly  // Default
    }

    async fn load(
        &self,
        path: &str,
        memory: &dyn MemoryStore,
        owner: AgentId,
    ) -> Result<BlockRef> {
        let full_path = self.base_path.join(path);
        let content = tokio::fs::read_to_string(&full_path).await?;
        let permission = self.permission_for(path);

        // Create block with Loro backing
        let block_id = memory.create_block(
            &owner.to_string(),
            &format!("file:{}", path),
            &format!("File: {}", path),
            BlockType::Working,
            BlockSchema::Text,
            content.len() * 2,  // Allow growth
        ).await?;

        // Initialize content
        memory.update_block_text(&owner.to_string(), &format!("file:{}", path), &content).await?;

        // Track loaded block
        self.loaded_blocks.insert(full_path, block_id.clone());

        Ok(BlockRef {
            label: format!("file:{}", path),
            block_id,
            agent_id: owner.to_string(),
        })
    }

    async fn save(
        &self,
        block_ref: &BlockRef,
        memory: &dyn MemoryStore,
    ) -> Result<()> {
        // Extract path from label
        let path = block_ref.label.strip_prefix("file:").ok_or(Error::InvalidLabel)?;
        let permission = self.permission_for(path);

        // Check permission
        if !permission.allows_write() {
            return Err(Error::PermissionDenied(path.to_string()));
        }

        // Get content from block
        let content = memory.get_rendered_content(&block_ref.agent_id, &block_ref.label)
            .await?
            .ok_or(Error::BlockNotFound)?;

        // Write to disk
        let full_path = self.base_path.join(path);
        tokio::fs::write(&full_path, content).await?;

        Ok(())
    }

    async fn reconcile(
        &self,
        paths: &[String],
        memory: &dyn MemoryStore,
    ) -> Result<Vec<FileChange>> {
        let mut changes = Vec::new();

        for path in paths {
            let full_path = self.base_path.join(path);

            if let Some(block_id) = self.loaded_blocks.get(&full_path) {
                // Compare disk to Loro
                let disk_content = tokio::fs::read_to_string(&full_path).await.ok();
                let label = format!("file:{}", path);

                // Get agent_id from somewhere (might need to track this)
                // For now, assume we can look it up
                if let Some(disk) = disk_content {
                    // TODO: Actually compare and update Loro if different
                    changes.push(FileChange {
                        path: path.clone(),
                        change_type: FileChangeType::Modified,
                        block_id: Some(block_id.clone()),
                    });
                }
            }
        }

        Ok(changes)
    }

    // ... history, rollback, diff implementations use Loro's built-in versioning
}
```

## Permission Examples

```rust
let rules = vec![
    PermissionRule {
        pattern: "*.config.toml".to_string(),
        permission: MemoryPermission::Human,  // Requires approval
        operations_requiring_escalation: vec!["delete".to_string()],
    },
    PermissionRule {
        pattern: "src/**/*.rs".to_string(),
        permission: MemoryPermission::ReadWrite,
        operations_requiring_escalation: vec!["delete".to_string()],
    },
    PermissionRule {
        pattern: "scratch/**".to_string(),
        permission: MemoryPermission::ReadWrite,
        operations_requiring_escalation: vec![],  // Full access including delete
    },
    PermissionRule {
        pattern: "**".to_string(),
        permission: MemoryPermission::ReadOnly,  // Default fallback
        operations_requiring_escalation: vec![],
    },
];
```

## Open Questions

1. **Conflict resolution** - What happens when agent has unsaved Loro edits and disk changes? Options:
   - Loro wins (lose external changes)
   - Disk wins (lose agent changes)
   - Merge (complex, may not always work)
   - Alert agent and let them decide

2. **Block labels** - Using `file:{path}` as label. Should path be relative or absolute? Relative to base_path seems cleaner.

3. **Multi-agent access** - If two agents load same file, separate blocks or shared? Probably separate (agent-owned), with potential for explicit sharing.
