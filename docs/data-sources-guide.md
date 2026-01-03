# Data Sources Guide

Pattern's data source system enables agents to consume external data through two complementary traits: `DataStream` for real-time events and `DataBlock` for documents with version control.

## Overview

Data sources bridge external systems and agent memory. They:
- Create and manage memory blocks for external content
- Route notifications to agents or groups
- Handle permission-gated edits with feedback
- Support version history and rollback (for DataBlock)

## Core Traits

### DataStream

For real-time event sources (Bluesky firehose, Discord, sensors):

```rust
#[async_trait]
pub trait DataStream: Send + Sync {
    fn source_id(&self) -> &str;
    fn name(&self) -> &str;
    fn block_schemas(&self) -> Vec<BlockSchemaSpec>;
    fn required_tools(&self) -> Vec<ToolRule>;

    async fn start(&self, ctx: Arc<dyn ToolContext>, owner: AgentId)
        -> Result<broadcast::Receiver<Notification>>;
    async fn stop(&self) -> Result<()>;

    fn pause(&self);
    fn resume(&self);
    fn status(&self) -> StreamStatus;

    fn supports_pull(&self) -> bool;
    async fn pull(&self, limit: usize, cursor: Option<StreamCursor>)
        -> Result<Vec<Notification>>;
}
```

### DataBlock

For document sources with versioning (files, configs):

```rust
#[async_trait]
pub trait DataBlock: Send + Sync {
    fn source_id(&self) -> &str;
    fn name(&self) -> &str;
    fn block_schema(&self) -> BlockSchemaSpec;
    fn permission_rules(&self) -> &[PermissionRule];
    fn permission_for(&self, path: &Path) -> MemoryPermission;
    fn matches(&self, path: &Path) -> bool;

    async fn load(&self, path: &Path, ctx: Arc<dyn ToolContext>, owner: AgentId)
        -> Result<BlockRef>;
    async fn create(&self, path: &Path, content: Option<&str>, ctx, owner)
        -> Result<BlockRef>;
    async fn save(&self, block_ref: &BlockRef, ctx: Arc<dyn ToolContext>) -> Result<()>;
    async fn delete(&self, path: &Path, ctx: Arc<dyn ToolContext>) -> Result<()>;

    async fn start_watch(&self) -> Option<broadcast::Receiver<FileChange>>;
    async fn stop_watch(&self) -> Result<()>;
    fn status(&self) -> BlockSourceStatus;

    async fn reconcile(&self, paths: &[PathBuf], ctx) -> Result<Vec<ReconcileResult>>;
    async fn history(&self, block_ref: &BlockRef, ctx) -> Result<Vec<VersionInfo>>;
    async fn rollback(&self, block_ref: &BlockRef, version: &str, ctx) -> Result<()>;
    async fn diff(&self, block_ref, from: Option<&str>, to: Option<&str>, ctx) -> Result<String>;
}
```

## Configuration

### Bluesky Stream

```toml
[agent.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "AgentName"  # or group name
jetstream_endpoint = "wss://jetstream1.us-east.fire.hose.cam/subscribe"

# Filtering
friends = ["did:plc:friend1", "did:plc:friend2"]  # Always see posts from these
keywords = ["adhd", "executive function"]          # Filter by keywords
languages = ["en"]                                 # Filter by language
exclude_dids = ["did:plc:spam"]                    # Block these users
exclude_keywords = ["spam"]                        # Exclude these terms

# Behavior
allow_any_mentions = false          # Only friends can mention
require_agent_participation = true  # Only show threads agent is in
```

### File Source

```toml
[agent.data_sources.notes]
type = "file"
name = "notes"
paths = ["./notes", "./documents"]
recursive = true
include_patterns = ["*.md", "*.txt"]
exclude_patterns = ["*.tmp", ".git/**"]

# Permission rules (evaluated in order)
[[agent.data_sources.notes.permission_rules]]
pattern = "*.config.toml"
permission = "read_only"

[[agent.data_sources.notes.permission_rules]]
pattern = "notes/**/*.md"
permission = "read_write"
```

## Notification Routing

Streams route notifications to agents or groups via the `target` field:

```rust
// Routing tries agent first, then group
match router.route_message_to_agent(&target, message, origin).await {
    Ok(Some(_)) => { /* routed to agent */ }
    Ok(None) => {
        // Agent not found, try as group
        router.route_message_to_group(&target, message, origin).await?;
    }
    Err(e) => { /* handle error */ }
}
```

## SourceManager

Access source operations through `ToolContext::sources()`:

```rust
#[async_trait]
pub trait SourceManager: Send + Sync {
    // Stream operations
    fn list_streams(&self) -> Vec<String>;
    fn get_stream_info(&self, source_id: &str) -> Option<StreamSourceInfo>;
    async fn pause_stream(&self, source_id: &str) -> Result<()>;
    async fn resume_stream(&self, source_id: &str, ctx) -> Result<()>;
    async fn subscribe_to_stream(&self, agent_id, source_id, ctx)
        -> Result<broadcast::Receiver<Notification>>;
    async fn pull_from_stream(&self, source_id, limit, cursor) -> Result<Vec<Notification>>;

    // Block operations
    fn list_block_sources(&self) -> Vec<String>;
    fn get_block_source_info(&self, source_id: &str) -> Option<BlockSourceInfo>;
    async fn load_block(&self, source_id, path, owner) -> Result<BlockRef>;
    async fn create_block(&self, source_id, path, content, owner) -> Result<BlockRef>;
    async fn save_block(&self, source_id, block_ref) -> Result<()>;
    async fn delete_block(&self, source_id, path) -> Result<()>;
    fn find_block_source_for_path(&self, path: &Path) -> Option<Arc<dyn DataBlock>>;

    // Version control
    async fn reconcile_blocks(&self, source_id, paths) -> Result<Vec<ReconcileResult>>;
    async fn block_history(&self, source_id, block_ref) -> Result<Vec<VersionInfo>>;
    async fn rollback_block(&self, source_id, block_ref, version) -> Result<()>;
    async fn diff_block(&self, source_id, block_ref, from, to) -> Result<String>;

    // Edit routing
    async fn handle_block_edit(&self, edit: &BlockEdit) -> Result<EditFeedback>;
}
```

## Helper Utilities

### BlockBuilder

Fluent block creation:

```rust
use pattern_core::data_source::BlockBuilder;

let block_ref = BlockBuilder::new(memory, owner, "user_profile")
    .description("User profile information")
    .schema(BlockSchema::Text { viewport: None })
    .pinned()  // Always in context
    .content("Initial content")
    .build()
    .await?;
```

### NotificationBuilder

Build notifications with blocks:

```rust
use pattern_core::data_source::NotificationBuilder;

let notification = NotificationBuilder::new("bluesky")
    .text("New post from @alice about ADHD strategies")
    .priority(NotificationPriority::Normal)
    .block(context_block_ref)
    .block(thread_block_ref)
    .build();
```

### EphemeralBlockCache

Get-or-create cache for ephemeral blocks:

```rust
let cache = EphemeralBlockCache::new();

// Returns existing block or creates new one
let block_ref = cache.get_or_create(
    "did:plc:user123",  // External ID
    || async {
        BlockBuilder::new(memory, owner, "user_context")
            .ephemeral()
            .build()
            .await
    }
).await?;
```

## Edit Feedback

Sources can approve, defer, or reject edits:

```rust
pub enum EditFeedback {
    Applied { message: Option<String> },  // Edit was applied
    Pending { message: Option<String> },  // Async operation pending
    Rejected { reason: String },          // Edit rejected
}
```

Example handler:

```rust
async fn handle_block_edit(&self, edit: &BlockEdit, ctx: Arc<dyn ToolContext>)
    -> Result<EditFeedback>
{
    let permission = self.permission_for(Path::new(&edit.block_label));

    match permission {
        MemoryPermission::ReadOnly => {
            Ok(EditFeedback::Rejected {
                reason: "File is read-only".to_string()
            })
        }
        MemoryPermission::ReadWrite => {
            // Apply edit to disk
            self.save_to_disk(edit).await?;
            Ok(EditFeedback::Applied { message: None })
        }
        _ => Ok(EditFeedback::Applied { message: None })
    }
}
```

## File Source Sync Model

```
Agent tools <-> Loro CRDT <-> Disk <-> External Editor
                    ^
               Version history
```

- **Loro as working state**: Agent's view with full version history
- **Disk as canonical**: External changes detected via watch/reconcile
- **Permission-gated writes**: Glob patterns determine access levels

### Reconciliation

After external file changes:

```rust
let results = source_manager.reconcile_blocks("files", &changed_paths).await?;

for result in results {
    match result {
        ReconcileResult::Resolved { path, resolution } => {
            println!("{}: {:?}", path, resolution);
        }
        ReconcileResult::NeedsResolution { path, disk_changes, agent_changes } => {
            // Present conflict to user
        }
        ReconcileResult::NoChange { path } => {}
    }
}
```

Resolution strategies:
- `DiskWins`: External changes overwrite
- `AgentWins`: Agent's Loro changes preserved
- `Merge`: CRDT merge applied
- `Conflict`: Needs human decision

## BlueskyStream Features

- **Jetstream consumption**: Real-time WebSocket firehose
- **DID/keyword/language filtering**: Reduce noise
- **Friend list**: Always see posts from specific DIDs
- **Thread context**: Fetches parent posts from constellation API
- **Post batching**: 20-second windows to reduce notification frequency
- **Agent participation**: Only show threads agent has engaged in
- **Rich text parsing**: Extracts mentions and links

## Programmatic Usage

### Creating Sources from Config

```rust
use pattern_core::config::DataSourceConfig;

// Create from configuration
let blocks = config.create_blocks(dbs.clone()).await?;
let streams = config.create_streams(dbs.clone(), tool_context.clone()).await?;

// Register with runtime
for block in blocks {
    runtime.register_block_source(block);
}
for stream in streams {
    runtime.register_stream_source(stream);
}
```

### Manual Source Creation

```rust
use pattern_core::data_source::{BlueskyStream, FileSource};

// Bluesky stream
let stream = BlueskyStream::new("bluesky", tool_context.clone())
    .with_agent_did(did.clone())
    .with_authenticated_agent(agent.clone())
    .with_config(config.clone());

let rx = stream.start(ctx.clone(), owner).await?;

// File source
let source = FileSource::from_config(base_path, &file_config);
let block_ref = source.load(&file_path, ctx.clone(), owner).await?;
```

## Implementation Patterns

### Accessing Sources from Tools (as_any downcast)

Tools that need typed access to specific data sources should use the `as_any()` downcast pattern. This allows tools to be created uniformly via `create_builtin_tool()` while still accessing type-specific source methods at runtime.

```rust
use std::any::Any;

// DataStream trait includes as_any() for downcasting
pub trait DataStream: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    // ... other methods
}

// Tool implementation
pub struct ShellTool {
    ctx: Arc<dyn ToolContext>,
    source_id: Option<String>,  // Optional explicit target
}

impl ShellTool {
    /// Get SourceManager from ToolContext.
    fn sources(&self) -> Result<Arc<dyn SourceManager>> {
        self.ctx.sources().ok_or_else(|| {
            CoreError::tool_exec_msg("shell", "no source manager available")
        })
    }

    /// Find ProcessSource by ID or default.
    fn find_process_source(&self, sources: &dyn SourceManager)
        -> Result<Arc<dyn DataStream>>
    {
        // Try explicit source_id first
        if let Some(id) = &self.source_id {
            if let Some(source) = sources.get_stream_source(id) {
                return Ok(source);
            }
        }

        // Try default ID
        if let Some(source) = sources.get_stream_source("process:shell") {
            return Ok(source);
        }

        // Find first ProcessSource
        for id in sources.list_streams() {
            if let Some(source) = sources.get_stream_source(&id) {
                if source.as_any().is::<ProcessSource>() {
                    return Ok(source);
                }
            }
        }

        Err(CoreError::tool_exec_msg("shell", "no process source found"))
    }

    /// Downcast to concrete ProcessSource.
    fn as_process_source(source: &dyn DataStream) -> Result<&ProcessSource> {
        source.as_any().downcast_ref::<ProcessSource>().ok_or_else(|| {
            CoreError::tool_exec_msg("shell", "source is not a ProcessSource")
        })
    }
}
```

**Key points:**
- Store `Arc<dyn ToolContext>`, not the concrete source
- Implement `as_any()` on all `DataStream` implementations
- Use fallback chain: explicit ID → default ID → first matching type
- Downcast at point of use, not at construction

### Notification Routing Task Pattern

DataStream implementations that emit notifications should spawn a routing task to forward them to the owner agent. This pattern is used by BlueskyStream and ProcessSource.

```rust
impl ProcessSource {
    async fn start(&self, ctx: Arc<dyn ToolContext>, owner: AgentId)
        -> Result<broadcast::Receiver<Notification>>
    {
        // Create broadcast channel
        let (tx, rx) = broadcast::channel(256);
        *self.tx.write() = Some(tx.clone());

        // Store context and owner for later use
        *self.ctx.write() = Some(ctx.clone());
        *self.owner.write() = Some(owner.to_string());

        // Spawn routing task
        let routing_rx = tx.subscribe();
        let owner_id = owner.to_string();
        let source_id = self.source_id().to_string();

        tokio::spawn(async move {
            route_notifications(routing_rx, owner_id, source_id, ctx).await;
        });

        *self.status.write() = StreamStatus::Running;
        Ok(rx)
    }
}

/// Route notifications from source to owner agent.
async fn route_notifications(
    mut rx: broadcast::Receiver<Notification>,
    owner_id: String,
    source_id: String,
    ctx: Arc<dyn ToolContext>,
) {
    let router = ctx.router();

    loop {
        match rx.recv().await {
            Ok(notification) => {
                let mut message = notification.message;
                message.batch = Some(notification.batch_id);

                // Extract origin from message metadata for routing context
                let origin = message.metadata.custom.as_object().and_then(|obj| {
                    serde_json::from_value::<MessageOrigin>(
                        serde_json::Value::Object(obj.clone())
                    ).ok()
                });

                // Route to owner agent
                match router.route_message_to_agent(&owner_id, message, origin).await {
                    Ok(Some(_)) => {
                        debug!(source_id = %source_id, "routed notification to owner");
                    }
                    Ok(None) => {
                        warn!(source_id = %source_id, "owner agent not found");
                    }
                    Err(e) => {
                        warn!(source_id = %source_id, error = %e, "routing failed");
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(source_id = %source_id, lagged = n, "notification routing lagged");
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!(source_id = %source_id, "channel closed, stopping routing");
                break;
            }
        }
    }
}
```

**Key points:**
- Spawn routing task in `start()`, not constructor
- Use `ctx.router()` for message routing
- Store `MessageOrigin` in message metadata for routing context
- Handle `Lagged` (log warning) and `Closed` (exit loop) errors
- Set `message.batch` from notification's `batch_id`

## Best Practices

### Source Type Selection

| Source Type | Use For |
|-------------|---------|
| DataStream | Real-time events, social media, sensors |
| DataBlock | Documents, configs, files needing version control |

### Performance

- **Batching**: BlueskyStream batches posts (20s windows) to reduce agent invocations
- **Ephemeral blocks**: Use for transient context, drops from memory after batch
- **Pinned blocks**: Reserve for always-needed state (user config, agent settings)

### Permission Rules

Define from most to least restrictive:

```toml
# Config files: read-only
[[agent.data_sources.files.permission_rules]]
pattern = "**/*.config.toml"
permission = "read_only"

# Sensitive files: require escalation
[[agent.data_sources.files.permission_rules]]
pattern = "**/secrets/**"
permission = "human"

# General notes: full access
[[agent.data_sources.files.permission_rules]]
pattern = "notes/**/*.md"
permission = "read_write"
```

### Error Handling

```rust
match source_manager.load_block("files", &path, owner).await {
    Ok(block_ref) => {
        // Use block_ref.block_id with memory operations
    }
    Err(e) if e.is_not_found() => {
        // File doesn't exist, offer to create
    }
    Err(e) if e.is_permission_denied() => {
        // Path not covered by permission rules
    }
    Err(e) => return Err(e),
}
```
