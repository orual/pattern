# Data Sources

Pattern's data source system enables agents to consume data from external sources like files, social media, and APIs. It uses two core traits: `DataStream` for event-driven sources and `DataBlock` for document-oriented sources.

## Architecture

### Core Traits

#### DataStream

For real-time event sources (Bluesky firehose, Discord events):

```rust
#[async_trait]
pub trait DataStream: Send + Sync + std::fmt::Debug {
    /// Unique identifier for this source
    fn source_id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Memory block schemas this source may create
    fn block_schemas(&self) -> Vec<BlockSchemaSpec>;

    /// Tool rules required when this source is active
    fn required_tools(&self) -> Vec<ToolRule>;

    /// Start the stream and return a notification receiver
    async fn start(
        &self,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>>;

    /// Stop the stream
    async fn stop(&self) -> Result<()>;

    /// Pause notifications (source may continue internally)
    fn pause(&self);

    /// Resume notifications
    fn resume(&self);

    /// Current status
    fn status(&self) -> StreamStatus;

    /// Whether this source supports pull-based access
    fn supports_pull(&self) -> bool;

    /// Pull items if supported (optional)
    async fn pull(
        &self,
        _limit: usize,
        _cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>> {
        Ok(vec![])
    }
}
```

#### DataBlock

For document-oriented sources (files, external storage):

```rust
#[async_trait]
pub trait DataBlock: Send + Sync + std::fmt::Debug {
    /// Unique identifier for this source
    fn source_id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Schema for blocks created by this source
    fn block_schema(&self) -> BlockSchemaSpec;

    /// Permission rules for block access
    fn permission_rules(&self) -> Vec<PermissionRule>;

    /// Check if this source handles the given path
    fn matches(&self, path: &Path) -> bool;

    /// Current status
    fn status(&self) -> BlockSourceStatus;

    /// Load a document into a memory block
    async fn load(
        &self,
        path: &Path,
        owner: AgentId,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<BlockRef>;

    /// Create a new document
    async fn create(
        &self,
        path: &Path,
        content: Option<&str>,
        owner: AgentId,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<BlockRef>;

    /// Save block back to external storage
    async fn save(&self, block_ref: &BlockRef, ctx: Arc<dyn ToolContext>) -> Result<()>;

    /// Delete a document
    async fn delete(&self, path: &Path) -> Result<()>;

    /// Reconcile after external changes
    async fn reconcile(
        &self,
        paths: &[PathBuf],
        ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<ReconcileResult>>;

    /// Get version history
    async fn history(&self, block_ref: &BlockRef) -> Result<Vec<VersionInfo>>;

    /// Rollback to previous version
    async fn rollback(
        &self,
        block_ref: &BlockRef,
        version: &str,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<()>;

    /// Diff between versions
    async fn diff(
        &self,
        block_ref: &BlockRef,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<String>;
}
```

### Supporting Types

#### Notification

Event from a stream source:

```rust
pub struct Notification {
    pub id: String,
    pub batch_id: String,
    pub source_id: String,
    pub message: Message,
    pub block_ids: Vec<String>,
    pub priority: NotificationPriority,
    pub created_at: DateTime<Utc>,
}
```

#### BlockRef

Reference to a loaded block:

```rust
pub struct BlockRef {
    pub block_id: String,
    pub agent_id: AgentId,
    pub source_id: String,
    pub external_path: PathBuf,
    pub loaded_at: DateTime<Utc>,
    pub version: Option<String>,
}
```

#### BlockSchemaSpec

Schema specification for blocks a source creates:

```rust
pub struct BlockSchemaSpec {
    pub label_template: String,  // e.g., "bluesky_user_{handle}"
    pub schema: BlockSchema,
    pub description: String,
    pub ephemeral: bool,         // Working block vs Core block
}
```

## Source Manager

The `SourceManager` trait (implemented by `RuntimeContext`) coordinates source operations:

```rust
#[async_trait]
pub trait SourceManager: Send + Sync + std::fmt::Debug {
    // Stream operations
    fn list_streams(&self) -> Vec<String>;
    fn get_stream_info(&self, source_id: &str) -> Option<StreamSourceInfo>;
    async fn pause_stream(&self, source_id: &str) -> Result<()>;
    async fn resume_stream(&self, source_id: &str, ctx: Arc<dyn ToolContext>) -> Result<()>;
    async fn subscribe_to_stream(
        &self,
        agent_id: &AgentId,
        source_id: &str,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<broadcast::Receiver<Notification>>;
    async fn pull_from_stream(
        &self,
        source_id: &str,
        limit: usize,
        cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>>;

    // Block operations
    fn list_block_sources(&self) -> Vec<String>;
    fn get_block_source_info(&self, source_id: &str) -> Option<BlockSourceInfo>;
    async fn load_block(&self, source_id: &str, path: &Path, owner: AgentId) -> Result<BlockRef>;
    async fn create_block(
        &self,
        source_id: &str,
        path: &Path,
        content: Option<&str>,
        owner: AgentId,
    ) -> Result<BlockRef>;
    async fn save_block(&self, source_id: &str, block_ref: &BlockRef) -> Result<()>;
    async fn delete_block(&self, source_id: &str, path: &Path) -> Result<()>;
    fn find_block_source_for_path(&self, path: &Path) -> Option<Arc<dyn DataBlock>>;

    // Edit routing
    async fn handle_block_edit(&self, edit: &BlockEdit) -> Result<EditFeedback>;
}
```

## Implementations

### BlueskyStream

Consumes Bluesky firehose via Jetstream:

```rust
let stream = BlueskyStream::new("bluesky", tool_context.clone())
    .with_agent_did(did.clone())
    .with_authenticated_agent(agent.clone())
    .with_config(config.clone());

// Start consuming
let rx = stream.start(ctx.clone(), owner).await?;

// Notifications routed to target agent/group
while let Ok(notification) = rx.recv().await {
    // Process notification.message
}
```

Features:
- Jetstream WebSocket consumption
- DID/keyword/language filtering
- Friend list (bypass filters)
- Exclusion lists
- Thread context fetching
- Post batching (20-second windows)
- Agent participation filtering
- Rich text parsing (mentions, links)

### FileSource

Manages file-backed memory blocks:

```rust
let source = FileSource::from_config(path, &config);

// Load file into block
let block_ref = source.load(&file_path, owner, ctx.clone()).await?;

// Access via memory
let doc = memory.get_block(&owner.0, &block_ref.block_id).await?;
let content = doc.render();

// Modify and save
doc.set_text("new content", true)?;
source.save(&block_ref, ctx.clone()).await?;
```

Features:
- Path-based matching with globs
- Permission rules per pattern
- Reconciliation after external changes
- Version history via Loro CRDT
- Diff between versions

## Configuration

### Via TOML

```toml
[agent.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "Main Support"
jetstream_endpoint = "wss://jetstream1.us-east.fire.hose.cam/subscribe"
friends = ["did:plc:friend1"]
keywords = ["adhd"]
require_agent_participation = true

[agent.data_sources.notes]
type = "file"
name = "notes"
paths = ["./notes"]
recursive = true
include_patterns = ["*.md"]

[[agent.data_sources.notes.permission_rules]]
pattern = "*.md"
permission = "read_write"
```

### Programmatic

```rust
use pattern_core::config::DataSourceConfig;

// Create from config
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

## Notification Routing

Streams route notifications to agents or groups:

```rust
// BlueskyStream starts routing task when target is configured
let target = config.target.clone();
let routing_rx = tx.subscribe();

tokio::spawn(async move {
    route_notifications(routing_rx, target, source_id, ctx).await;
});

// route_notifications tries agent first, then group
async fn route_notifications(...) {
    match router.route_message_to_agent(&target, message.clone(), origin).await {
        Ok(Some(_)) => { /* routed to agent */ }
        Ok(None) => {
            // Agent not found, try as group
            router.route_message_to_group(&target, message, origin).await?;
        }
        Err(e) => { /* handle error */ }
    }
}
```

## Edit Feedback

Block sources can provide feedback on edits:

```rust
pub enum EditFeedback {
    Applied { message: Option<String> },
    Pending { message: Option<String> },
    Rejected { reason: String },
}
```

This enables:
- Validation before writing
- Async save operations
- Rejection with explanation

## Best Practices

1. **Use appropriate source type**: DataStream for events, DataBlock for documents
2. **Configure targets carefully**: Route to groups for coordination, agents for direct processing
3. **Set permission rules**: Control agent access to external data
4. **Handle reconciliation**: External changes should trigger reconcile
5. **Use batching**: BlueskyStream batches posts to reduce notification frequency
