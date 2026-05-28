# Data Source v2 Design

## Overview

This design describes a restructured data source system that fully leverages the v2 memory system's structured blocks, Loro CRDT capabilities, and permission model. The key insight is splitting data sources into two distinct trait families based on their fundamental interaction patterns.

## Core Concepts

### Two Source Types

**DataBlock** - Persistent, document-oriented sources (files, structured configs)
- Loro-backed with full versioning, rollback, and history
- Bidirectional sync with external storage (disk, remote)
- Agent works with these like documents

**DataStream** - Event-driven, push-based sources (Bluesky, Discord, sensors, LSP)
- Creates blocks from events/state
- Mix of passive (pinned block updates) and active (notifications) patterns
- Source subscribes to agent edits on writable fields

Both produce memory blocks, but lifecycles and coordination are fundamentally different.

---

## DataBlock Trait

> **Full details:** [2025-12-27-datablock-trait-design.md](./2025-12-27-datablock-trait-design.md)

Document-oriented sources (files, configs) with Loro-backed versioning and permission-gated disk sync.

### Key Points

- **Loro as working state** - Agent's view with full version history and rollback
- **Disk as canonical** - External changes (shell, editor) win via reconcile
- **Permission-gated writes** - Glob patterns determine access levels
- **Shell/ACP integration hooks** - Reconcile after external modifications

### Sync Model

```
Agent tools ←→ Loro ←→ Disk ←→ Editor (ACP)
                 ↑
            Shell side effects
```

### Core Types

```rust
pub struct PermissionRule {
    pub pattern: String,  // Glob: "src/**/*.rs"
    pub permission: MemoryPermission,
    pub operations_requiring_escalation: Vec<String>,
}

pub struct FileChange {
    pub path: String,
    pub change_type: FileChangeType,
    pub block_id: Option<String>,
}
```

### Trait Summary

```rust
#[async_trait]
pub trait DataBlock: Send + Sync {
    fn source_id(&self) -> &str;
    fn name(&self) -> &str;
    fn schema(&self) -> BlockSchema;
    fn permission_rules(&self) -> &[PermissionRule];
    fn required_tools(&self) -> Vec<ToolRule>;
    fn matches(&self, path: &str) -> bool;
    fn permission_for(&self, path: &str) -> MemoryPermission;

    async fn load(&self, path: &str, memory: &dyn MemoryStore, owner: AgentId) -> Result<BlockRef>;
    async fn create(&self, path: &str, content: Option<&str>, memory: &dyn MemoryStore, owner: AgentId) -> Result<BlockRef>;
    async fn save(&self, block_ref: &BlockRef, memory: &dyn MemoryStore) -> Result<()>;
    async fn delete(&self, path: &str, memory: &dyn MemoryStore) -> Result<()>;

    fn start_watch(&mut self) -> Option<broadcast::Receiver<FileChange>>;
    fn stop_watch(&mut self);
    async fn reconcile(&self, paths: &[String], memory: &dyn MemoryStore) -> Result<Vec<FileChange>>;

    async fn history(&self, block_ref: &BlockRef, memory: &dyn MemoryStore) -> Result<Vec<VersionInfo>>;
    async fn rollback(&self, block_ref: &BlockRef, version: &str, memory: &dyn MemoryStore) -> Result<()>;
    async fn diff(&self, block_ref: &BlockRef, from: Option<&str>, to: Option<&str>, memory: &dyn MemoryStore) -> Result<String>;
}
```

---

## DataStream Trait

> **Full details:** [2025-12-27-datastream-trait-design.md](./2025-12-27-datastream-trait-design.md)

Event-driven sources (Bluesky, Discord, LSP, sensors) that produce notifications and manage state blocks.

### Key Points

- **No generics** - Type safety at source boundary, flexible routing at trait level
- **Channel-based** - Source emits `Notification` on broadcast channel
- **Blocks persist, context is filtered** - Ephemeral blocks drop out of context, not deleted
- **Source manages blocks** - Gets `Arc<dyn MemoryStore>` on `start()`, creates/updates blocks directly

### Block Lifecycle

| Type | Behavior |
|------|----------|
| Pinned (`pinned=true`) | Always in context while subscribed |
| Ephemeral (`pinned=false`) | Loaded for batch that references them, then drops out |

Agent can `load` (temporary peek), `pin` (persistent), or `unpin` (make ephemeral).

### Core Types

```rust
pub struct Notification {
    pub message: Message,              // Full Message (multi-modal)
    pub block_refs: Vec<BlockRef>,     // Blocks to load for this batch
    pub batch_id: SnowflakePosition,
}

pub struct BlockRef {
    pub label: String,
    pub block_id: String,
    pub agent_id: String,  // Owner, defaults to "_constellation_"
}
```

### Trait Summary

```rust
#[async_trait]
pub trait DataStream: Send + Sync {
    fn source_id(&self) -> &str;
    fn name(&self) -> &str;
    fn block_schemas(&self) -> Vec<BlockSchemaSpec>;
    fn required_tools(&self) -> Vec<ToolRule>;

    async fn start(&mut self, memory: Arc<dyn MemoryStore>, owner: AgentId)
        -> Result<broadcast::Receiver<Notification>>;
    async fn stop(&mut self) -> Result<()>;

    fn pause(&mut self);
    fn resume(&mut self);
    fn is_paused(&self) -> bool;

    fn supports_pull(&self) -> bool { false }
    async fn pull(&self, limit: usize, cursor: Option<StreamCursor>)
        -> Result<Vec<Notification>> { Ok(vec![]) }
}
```

---

## Block Editing Tools

### Tool Taxonomy (Consolidated Modal Design)

**`block_edit` tool** - Edit block content (capability-gated operations)
```
block_edit(block_label, op: append | replace | patch | set_field, content?, field?, position?)
```

| Operation | Description | Capability Level |
|-----------|-------------|------------------|
| `append` | Append to text/list content | Basic |
| `replace` | Basic text replacement | Basic |
| `patch` | Apply diff/patch operations | Advanced |
| `set_field` | Schema-aware field edit (Map/Composite) | Medium |

**`block` tool** - Block lifecycle management
```
block(label, op: load | pin | unpin | archive | info)
```

| Operation | Description |
|-----------|-------------|
| `load` | Load a block into working memory |
| `pin` | Pin block to retain in context |
| `unpin` | Unpin block (will fade after request) |
| `archive` | Move to archival storage |
| `info` | Get block metadata/schema info |

**`recall` tool** - Archival memory (simplified)
```
recall(op: search | get, query?, entry_id?)
```
Works with immutable archival entries only. Search and retrieve, no edit operations.

**`source` tool** - Data source control
```
source(source_id, op: pause | resume | status)
```

| Operation | Description |
|-----------|-------------|
| `pause` | Pause notifications from a source |
| `resume` | Resume notifications |
| `status` | Check source status |

### Dynamic Tool Registration

When a block is loaded, check its schema and enable appropriate operations:

```rust
impl ContextBuilder {
    fn load_block(&mut self, block: &CachedBlock) {
        // Enable block_edit tool with operations based on schema
        let allowed_ops = match block.schema() {
            BlockSchema::Map { .. } | BlockSchema::Composite { .. } => {
                // Structured blocks get set_field operation
                vec![Append, Replace, SetField]
            }
            BlockSchema::Text => {
                vec![Append, Replace]
            }
            BlockSchema::List { .. } => {
                vec![Append]
            }
            // etc.
        };

        // Add patch if agent has advanced capability
        if self.agent_capabilities.contains(Capability::AdvancedEdit) {
            allowed_ops.push(Patch);
        }

        self.tool_registry.register(block_edit_tool(ops: allowed_ops));
    }
}
```

Block rendering includes hints for agent:
- Schema type and structure
- Available operations based on permissions
- Read-only field indicators

---

## Coordinator Changes

The `DataIngestionCoordinator` manages both source types:

```rust
pub struct DataIngestionCoordinator {
    block_sources: DashMap<String, Arc<dyn DataBlock>>,
    stream_sources: DashMap<String, Arc<dyn DataStream>>,

    // Subscription tracking
    agent_block_subscriptions: DashMap<AgentId, Vec<BlockSubscription>>,
    agent_stream_subscriptions: DashMap<AgentId, Vec<StreamSubscription>>,

    // Block edit subscribers (source -> block patterns it cares about)
    block_edit_subscribers: DashMap<String, Vec<BlockEditSubscriber>>,
}

impl DataIngestionCoordinator {
    /// Subscribe agent to a stream source
    pub async fn subscribe_to_stream(
        &self,
        agent_id: &AgentId,
        source_id: &str,
    ) -> Result<()> {
        let source = self.stream_sources.get(source_id)?;

        // Create pinned blocks
        let state = source.on_subscribe(agent_id).await?;

        // Register required tools
        for tool in source.required_tools() {
            self.tool_registry.register_for_agent(agent_id, tool);
        }

        // Subscribe source to block edits
        self.register_block_edit_subscriber(source_id, source.pinned_block_schemas());

        // Start monitoring if provides notifications
        if source.provides_notifications() {
            self.spawn_stream_monitor(agent_id, source_id);
        }

        Ok(())
    }

    /// Handle block edit - route to interested sources
    pub async fn handle_block_edit(&self, edit: &BlockEdit) -> Result<EditFeedback> {
        for subscriber in self.find_edit_subscribers(&edit.block_id) {
            let source = self.stream_sources.get(&subscriber.source_id)?;
            let feedback = source.handle_block_edit(
                &edit.block_id,
                &edit.field,
                edit.old_value.clone(),
                edit.new_value.clone(),
            ).await?;

            match &feedback {
                EditFeedback::Rejected { reason } => {
                    // Send message to agent
                    self.send_error_message(edit.agent_id, reason).await?;
                }
                EditFeedback::Applied { message } | EditFeedback::Pending { message } => {
                    // Log to activity stream
                    if let Some(msg) = message {
                        self.log_activity(edit.agent_id, msg).await?;
                    }
                }
            }

            return Ok(feedback);
        }

        // No subscriber - direct edit allowed
        Ok(EditFeedback::Applied { message: None })
    }
}
```

---

## Example Implementations

### File Source (DataBlock)

```rust
pub struct FileSource {
    base_path: PathBuf,
    permission_rules: Vec<PermissionRule>,
    watch_enabled: bool,
}

impl DataBlock for FileSource {
    fn schema(&self) -> BlockSchema {
        BlockSchema::Text  // Files are text blocks
    }

    fn memory_type(&self) -> MemoryType {
        MemoryType::Working
    }

    async fn load(&self, path: &str) -> Result<LoroDoc> {
        let full_path = self.base_path.join(path);
        let content = tokio::fs::read_to_string(&full_path).await?;

        let doc = LoroDoc::new();
        doc.get_text("content").insert(0, &content)?;
        Ok(doc)
    }

    async fn save(&self, path: &str, doc: &LoroDoc) -> Result<()> {
        let permission = self.permission_for(path);
        permission.check(MemoryOp::Overwrite)?;

        let content = doc.get_text("content").to_string();
        let full_path = self.base_path.join(path);
        tokio::fs::write(&full_path, content).await?;
        Ok(())
    }

    fn required_tools(&self) -> Vec<ToolRegistration> {
        vec![
            // Consolidated tools with allowed operations
            file_tool(ops: [Read, Append, Insert, Save]),
            file_history_tool(ops: [View, Diff]),
            file_search_tool(),
        ]
    }
}
```

### LSP Source (DataStream - Passive)

```rust
pub struct LspSource {
    language_server: Arc<LanguageServer>,
}

impl DataStream for LspSource {
    fn pinned_block_schemas(&self) -> Vec<PinnedBlockSpec> {
        vec![PinnedBlockSpec {
            label: "lsp_diagnostics".into(),
            schema: BlockSchema::Map {
                fields: vec![
                    FieldDef {
                        name: "diagnostics".into(),
                        field_type: FieldType::List,
                        read_only: true,  // Source updates
                    },
                    FieldDef {
                        name: "severity_filter".into(),
                        field_type: FieldType::Text,
                        read_only: false,  // Agent can configure
                    },
                ],
            },
            memory_type: MemoryType::Working,
            description: "Language server diagnostics".into(),
        }]
    }

    fn provides_notifications(&self) -> bool {
        false  // Passive - updates block, no messages
    }

    async fn on_subscribe(&self, agent_id: &AgentId) -> Result<SubscriptionState> {
        // Create diagnostics block, start monitoring LSP
        let block = self.create_diagnostics_block(agent_id).await?;
        self.spawn_diagnostics_updater(agent_id, block.id);
        Ok(SubscriptionState { blocks: vec![block] })
    }

    async fn handle_block_edit(
        &self,
        block_id: &BlockId,
        field: &str,
        _old: Value,
        new: Value,
    ) -> Result<EditFeedback> {
        match field {
            "severity_filter" => {
                let filter: String = serde_json::from_value(new)?;
                self.language_server.set_severity_filter(&filter)?;
                Ok(EditFeedback::Applied {
                    message: Some(format!("Severity filter set to: {}", filter))
                })
            }
            _ => Ok(EditFeedback::Rejected {
                reason: format!("Field '{}' is read-only", field)
            })
        }
    }
}
```

### Bluesky Source (DataStream - Active)

```rust
pub struct BlueskySource {
    jetstream: JetstreamClient,
    filter: BlueskyFilter,
}

impl DataStream for BlueskySource {
    fn pinned_block_schemas(&self) -> Vec<PinnedBlockSpec> {
        vec![PinnedBlockSpec {
            label: "bluesky_config".into(),
            schema: bluesky_config_schema(),
            memory_type: MemoryType::Working,
            description: "Bluesky filter configuration".into(),
        }]
    }

    fn ephemeral_block_schemas(&self) -> Vec<EphemeralBlockSpec> {
        vec![EphemeralBlockSpec {
            label_pattern: "bluesky_user_{handle}".into(),
            schema: user_profile_schema(),
            memory_type: MemoryType::Working,
            description: "Bluesky user profile".into(),
        }]
    }

    fn provides_notifications(&self) -> bool {
        true  // Active - sends notification messages
    }

    fn format_notification(&self, event: &StreamEvent) -> Option<Notification> {
        let post = event.as_post()?;

        // Create ephemeral user profile block
        let user_block = self.create_user_profile_block(&post.author);

        Some(Notification {
            message: self.format_post_message(&post),
            ephemeral_blocks: vec![
                (format!("bluesky_user_{}", post.author.handle), user_block)
            ],
            metadata: post.metadata(),
        })
    }
}
```

---

## Migration Path

### Phase 1: Trait Definition
- Define `DataBlock` and `DataStream` traits
- Add schema-level `read_only` field flag
- Update `BlockSchema` with new field metadata

### Phase 2: File Source
- Implement `FileSource` as `DataBlock`
- Loro overlay with disk sync
- Glob-based permission rules
- Basic tools: read, append, history

### Phase 3: Stream Sources
- Migrate Bluesky to `DataStream` trait
- Implement pinned config block
- Implement ephemeral user profile blocks
- Wire up block edit handling

### Phase 4: Coordinator
- Update coordinator to manage both source types
- Implement block edit routing
- Implement dynamic tool registration

### Phase 5: Tools
- Implement new tool taxonomy
- Simplify recall tool
- Add source control tools (pause/resume)

---

## Future Considerations

### Plugin Model
Eventually support WASM plugins for custom sources. Plugins would:
- Declare their trait implementation (DataBlock or DataStream)
- Provide schemas
- Handle events and edits

### MCP Integration
Could present MCP tool resources as DataStream sources:
- MCP resources become pinned blocks
- Tool calls become block edits
- Responses update blocks

### Multi-Agent Block Collaboration
With Loro CRDT foundation, could support:
- Multiple agents editing same block
- Conflict-free merging
- Change attribution per agent

---

## Open Questions

1. **Snapshot consolidation strategy** - When to consolidate Loro updates? Time-based, change-count, or size-based?

2. **Block size limits** - How large can a file block get before we need chunking?

3. **Watch debouncing** - For file watching, what's the right debounce window?

4. **Tool capability verification** - How do we know an agent can handle patch operations before enabling the tool?
