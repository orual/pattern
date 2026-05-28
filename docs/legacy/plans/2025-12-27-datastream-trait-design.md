# DataStream Trait Design

## Purpose

Handle event-driven sources that produce notifications and/or maintain state blocks. Sources are autonomous - they have direct memory store access and manage their own blocks.

## Design Principles

**No generics on the trait** - Type safety lives at the source implementation boundary (Jacquard gives typed ATProto, LSP client gives typed diagnostics). By the time events hit the DataStream interface, they've been validated and we need flexible routing.

**Channel-based notifications** - Source emits `Notification` on a broadcast channel. Coordinator routes to subscribed agents. Natural backpressure, simple model.

**Blocks persist, context is filtered** - "Ephemeral" means blocks drop out of context after batch processing, not that they're deleted. They persist in memory store and can be brought back by another notification or explicit agent action.

## Block Lifecycle

**Pinned blocks** (`pinned=true`):
- Config blocks for the source
- Passive state (LSP diagnostics, sensor readings)
- Always loaded in agent context while subscribed
- Agent can unpin to make ephemeral

**Ephemeral blocks** (`pinned=false`):
- Created with notification, referenced by block_id
- Loaded for the batch that references them
- Drop out of context after batch completes
- Agent can `pin` to retain, or `load` for temporary peek

## Block Loading Rules

Context builder filters Working blocks:
1. `pinned == true` → always loaded
2. `block_id in batch_block_ids` → loaded for this batch
3. Otherwise → not loaded (but still exists in store)

Agent can use `block` tool:
- `load` → adds to current batch, ContinueLoop gives another turn with it visible
- `pin` → sets pinned=true, persistent in context
- `unpin` → sets pinned=false, becomes ephemeral

Tool execution results can include `keep_blocks: Vec<String>` that carries forward to next batch.

## Types

```rust
/// Event from any streaming source (internal to source)
pub struct StreamEvent {
    pub event_type: String,
    pub payload: serde_json::Value,  // Source-specific, validated at source
    pub cursor: Option<String>,       // Opaque resume token
    pub timestamp: DateTime<Utc>,
    pub source_id: String,
}

/// Notification delivered to agent via broadcast channel
pub struct Notification {
    /// Full Message type - supports text, images, multi-modal content
    pub message: Message,

    /// Blocks to load for this batch (already exist in memory store)
    pub block_refs: Vec<BlockRef>,

    /// Batch to associate these blocks with
    pub batch_id: SnowflakePosition,
}

/// Reference to a block in memory store
pub struct BlockRef {
    pub label: String,
    pub block_id: String,
    pub agent_id: String,  // Owner, defaults to "_constellation_"
}

impl BlockRef {
    pub fn new(label: impl Into<String>, block_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            block_id: block_id.into(),
            agent_id: "_constellation_".to_string(),
        }
    }

    pub fn owned_by(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }
}

/// Opaque cursor for pull-based access
pub struct StreamCursor(pub String);

/// Schema spec for documentation/validation
pub struct BlockSchemaSpec {
    pub label_pattern: String,  // "bluesky_user_{handle}" or exact "lsp_diagnostics"
    pub schema: BlockSchema,
    pub description: String,
    pub pinned: bool,           // Created pinned (config) vs ephemeral (per-event)
}
```

## Trait Definition

```rust
#[async_trait]
pub trait DataStream: Send + Sync {
    /// Unique identifier for this stream source
    fn source_id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    // === Schema Declarations ===

    /// Block schemas this source creates (for documentation/validation)
    fn block_schemas(&self) -> Vec<BlockSchemaSpec>;

    /// Tool rules required while subscribed
    fn required_tools(&self) -> Vec<ToolRule>;

    // === Lifecycle ===

    /// Start the source, returns broadcast receiver for notifications.
    /// Source receives memory store access here and manages its own blocks.
    async fn start(
        &mut self,
        memory: Arc<dyn MemoryStore>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>>;

    /// Stop the source, cleanup
    async fn stop(&mut self) -> Result<()>;

    // === Control ===

    fn pause(&mut self);
    fn resume(&mut self);
    fn is_paused(&self) -> bool;

    // === Optional Pull Support ===

    /// Whether this source supports on-demand pull
    fn supports_pull(&self) -> bool { false }

    /// Pull notifications on demand (for sources that support backfill)
    async fn pull(
        &self,
        limit: usize,
        cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>> {
        Ok(vec![])
    }
}
```

## Context Builder Integration

```rust
impl<'a> ContextBuilder<'a> {
    /// Set block IDs to keep loaded for this batch (even if unpinned)
    pub fn with_batch_blocks(mut self, block_ids: Vec<String>) -> Self {
        self.batch_block_ids = Some(block_ids);
        self
    }
}

// In build(), filter Working blocks:
let working_blocks: Vec<BlockMetadata> = owned_working_blocks
    .into_iter()
    .filter(|b| {
        b.pinned || self.batch_block_ids
            .as_ref()
            .map(|ids| ids.contains(&b.id))
            .unwrap_or(false)
    })
    .collect();
```

## Example Flow

1. BlueskySource receives post from Jetstream
2. Source creates user profile block in memory store (pinned=false)
3. Source sends `Notification { message: Message::user(...), block_refs: [BlockRef::new("bluesky_user_alice", block_id)], batch_id }`
4. Coordinator receives, routes to subscribed agent
5. Processing loop calls `ContextBuilder::with_batch_blocks([block_id]).with_active_batch(batch_id)`
6. Agent sees the post message + user profile block in context
7. Agent processes, maybe pins the block if user is interesting
8. Batch completes - unpinned block drops out of context (still in store)
9. Later notification can reference same block_id, or agent can `load` it explicitly

## Example: Bluesky Source

```rust
pub struct BlueskySource {
    jetstream: JetstreamClient,
    memory: Option<Arc<dyn MemoryStore>>,
    owner: Option<AgentId>,
    tx: Option<broadcast::Sender<Notification>>,
    paused: AtomicBool,
}

#[async_trait]
impl DataStream for BlueskySource {
    fn source_id(&self) -> &str { "bluesky" }
    fn name(&self) -> &str { "Bluesky Firehose" }

    fn block_schemas(&self) -> Vec<BlockSchemaSpec> {
        vec![
            BlockSchemaSpec {
                label_pattern: "bluesky_config".into(),
                schema: bluesky_config_schema(),
                description: "Bluesky filter configuration".into(),
                pinned: true,
            },
            BlockSchemaSpec {
                label_pattern: "bluesky_user_{handle}".into(),
                schema: user_profile_schema(),
                description: "Bluesky user profile".into(),
                pinned: false,
            },
        ]
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![/* bluesky-specific tool rules */]
    }

    async fn start(
        &mut self,
        memory: Arc<dyn MemoryStore>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>> {
        self.memory = Some(memory.clone());
        self.owner = Some(owner.clone());

        let (tx, rx) = broadcast::channel(256);
        self.tx = Some(tx.clone());

        // Create pinned config block
        let config_block_id = memory.create_block(
            &owner.to_string(),
            "bluesky_config",
            "Bluesky filter settings",
            BlockType::Working,
            bluesky_config_schema(),
            4096,
        ).await?;
        // Set pinned=true on the block...

        // Spawn event processing task
        let jetstream = self.jetstream.clone();
        let memory = memory.clone();
        let owner = owner.clone();
        tokio::spawn(async move {
            while let Some(event) = jetstream.next().await {
                // Create/update user block
                let user_block_id = ensure_user_block(&memory, &owner, &event.author).await;

                // Build notification with full Message
                let message = Message::user(format_post(&event));
                let notification = Notification {
                    message,
                    block_refs: vec![
                        BlockRef::new(
                            format!("bluesky_user_{}", event.author.handle),
                            user_block_id,
                        ).owned_by(&owner.to_string())
                    ],
                    batch_id: SnowflakePosition::generate(),
                };

                let _ = tx.send(notification);
            }
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> Result<()> {
        self.tx = None;
        Ok(())
    }

    fn pause(&mut self) { self.paused.store(true, Ordering::SeqCst); }
    fn resume(&mut self) { self.paused.store(false, Ordering::SeqCst); }
    fn is_paused(&self) -> bool { self.paused.load(Ordering::SeqCst) }
}
```

## Example: LSP Source (Passive)

```rust
pub struct LspSource {
    language_server: Arc<LanguageServer>,
    memory: Option<Arc<dyn MemoryStore>>,
    owner: Option<AgentId>,
    tx: Option<broadcast::Sender<Notification>>,
}

#[async_trait]
impl DataStream for LspSource {
    fn source_id(&self) -> &str { "lsp" }
    fn name(&self) -> &str { "Language Server" }

    fn block_schemas(&self) -> Vec<BlockSchemaSpec> {
        vec![BlockSchemaSpec {
            label_pattern: "lsp_diagnostics".into(),
            schema: BlockSchema::Map {
                fields: vec![
                    FieldDef {
                        name: "diagnostics".into(),
                        field_type: FieldType::List,
                        description: Some("Current diagnostics".into()),
                        read_only: true,  // Source updates this
                        ..Default::default()
                    },
                    FieldDef {
                        name: "severity_filter".into(),
                        field_type: FieldType::Text,
                        description: Some("Minimum severity to show".into()),
                        read_only: false,  // Agent can configure
                        ..Default::default()
                    },
                ],
            },
            description: "Language server diagnostics".into(),
            pinned: true,  // Always in context while subscribed
        }]
    }

    fn required_tools(&self) -> Vec<ToolRule> {
        vec![]  // No special tools needed
    }

    async fn start(
        &mut self,
        memory: Arc<dyn MemoryStore>,
        owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>> {
        self.memory = Some(memory.clone());
        self.owner = Some(owner.clone());

        let (tx, rx) = broadcast::channel(64);
        self.tx = Some(tx);

        // Create pinned diagnostics block
        let block_id = memory.create_block(
            &owner.to_string(),
            "lsp_diagnostics",
            "Language server diagnostics",
            BlockType::Working,
            self.block_schemas()[0].schema.clone(),
            8192,
        ).await?;

        // Spawn diagnostics updater - updates block in place, no notifications
        let ls = self.language_server.clone();
        let memory = memory.clone();
        let owner = owner.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                if let Ok(diags) = ls.get_diagnostics().await {
                    // Update the block directly, no notification needed
                    let _ = memory.update_block_field(
                        &owner.to_string(),
                        "lsp_diagnostics",
                        "diagnostics",
                        serde_json::to_value(&diags).unwrap(),
                    ).await;
                }
            }
        });

        Ok(rx)
    }

    async fn stop(&mut self) -> Result<()> {
        self.tx = None;
        Ok(())
    }

    fn pause(&mut self) {}
    fn resume(&mut self) {}
    fn is_paused(&self) -> bool { false }
}
```

The LSP source demonstrates the "passive" pattern - it maintains a pinned block that it updates periodically, but doesn't send notifications. The agent always sees current diagnostics in context without being interrupted.
