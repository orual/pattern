# Context Building System

## Overview

The context building system assembles model requests from memory blocks, message history, and tools. It's the bridge between Pattern's stateful agent infrastructure and stateless LLM APIs.

## Architecture

### ContextBuilder

The `ContextBuilder` assembles a `Request` ready for model calls:

```rust
pub struct ContextBuilder<'a> {
    memory: &'a dyn MemoryStore,      // Required: memory block access
    messages: Option<&'a MessageStore>, // Optional: conversation history
    tools: Option<&'a ToolRegistry>,    // Optional: available tools
    config: &'a ContextConfig,          // Required: limits and options
    // ... agent info, model info, tool rules, activity renderer
}
```

### Building a Request

```rust
let request = ContextBuilder::new(&memory, &config)
    .for_agent("entropy")
    .with_messages(&message_store)
    .with_tools(&tool_registry)
    .with_base_instructions("You are Entropy, a task breakdown specialist.")
    .with_tool_rules(rules)
    .with_model_info(&model_info)
    .build()
    .await?;
```

The `build()` method:
1. Builds system prompt from Core and Working memory blocks
2. Retrieves and compresses message history
3. Applies model-specific adjustments (e.g., Gemini first-message requirements)
4. Converts tools to genai format

### System Prompt Assembly

The system prompt is built in order:
1. Base instructions (custom or default)
2. Core blocks (always in context)
3. Working blocks (pinned or referenced by batch)
4. Activity section (if renderer provided)
5. Tool execution rules (if any)

Each block renders as:
```xml
<block:label permission="ReadWrite">
Block description (if include_descriptions enabled)

Rendered content from StructuredDocument
</block:label>
```

Shared blocks include attribution:
```xml
<block:shared_notes permission="Append" shared_from="Archive">
Content shared from another agent
</block:shared_notes>
```

## Memory System

### MemoryStore Trait

All memory operations go through `MemoryStore`:

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync + fmt::Debug {
    // Block CRUD
    async fn create_block(&self, agent_id: &str, label: &str, ...) -> MemoryResult<String>;
    async fn get_block(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>>;
    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>>;
    async fn list_blocks_by_type(&self, agent_id: &str, block_type: BlockType) -> MemoryResult<Vec<BlockMetadata>>;
    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    // Content operations
    async fn get_rendered_content(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>>;
    async fn update_block_text(&self, agent_id: &str, label: &str, new_content: &str) -> MemoryResult<()>;
    async fn append_to_block(&self, agent_id: &str, label: &str, content: &str) -> MemoryResult<()>;
    async fn replace_in_block(&self, agent_id: &str, label: &str, old: &str, new: &str) -> MemoryResult<bool>;
    async fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    // Archival (separate from blocks)
    async fn insert_archival(&self, agent_id: &str, content: &str, metadata: Option<JsonValue>) -> MemoryResult<String>;
    async fn search_archival(&self, agent_id: &str, query: &str, limit: usize) -> MemoryResult<Vec<ArchivalEntry>>;
    async fn delete_archival(&self, id: &str) -> MemoryResult<()>;

    // Search
    async fn search(&self, agent_id: &str, query: &str, options: SearchOptions) -> MemoryResult<Vec<MemorySearchResult>>;
    async fn search_all(&self, query: &str, options: SearchOptions) -> MemoryResult<Vec<MemorySearchResult>>;

    // Shared blocks
    async fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>>;
    async fn get_shared_block(&self, requester: &str, owner: &str, label: &str) -> MemoryResult<Option<StructuredDocument>>;

    // Block configuration
    async fn set_block_pinned(&self, agent_id: &str, label: &str, pinned: bool) -> MemoryResult<()>;
    async fn set_block_type(&self, agent_id: &str, label: &str, block_type: BlockType) -> MemoryResult<()>;
    async fn update_block_schema(&self, agent_id: &str, label: &str, schema: BlockSchema) -> MemoryResult<()>;
}
```

### MemoryCache

The default `MemoryStore` implementation using Loro CRDT with write-through to SQLite:

```rust
let dbs = ConstellationDatabases::open("./constellation.db", "./auth.db").await?;
let memory = MemoryCache::new(dbs.clone());
```

Features:
- Lazy loading: blocks loaded on first access
- Write-through: changes persisted to SQLite
- Delta updates: exports only changes since last persist
- Eviction: LRU-based cache management

### Block Types

```rust
pub enum BlockType {
    Core,     // Always in context, cannot be swapped out
    Working,  // Active working memory, can be swapped
    Archival, // Long-term storage, searchable on demand
    Log,      // Append-only log entries
}
```

### Memory Permissions

```rust
pub enum MemoryPermission {
    ReadOnly,   // Can only read
    Partner,    // Requires partner (owner) permission to write
    Human,      // Requires human permission to write
    Append,     // Can append to existing content
    ReadWrite,  // Can modify freely (default)
    Admin,      // Total control, can delete
}
```

## Structured Documents (Loro CRDT)

### StructuredDocument

Each memory block is backed by a `StructuredDocument` wrapping a Loro CRDT document:

```rust
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,
    permission: MemoryPermission,
    label: String,
    accessor_agent_id: Option<String>,
}
```

Operations check permissions via `is_system` flag:
- System operations bypass permission checks
- Agent operations respect the document's permission level

### Block Schemas

```rust
pub enum BlockSchema {
    Text { viewport: Option<Viewport> },
    Map { fields: Vec<FieldDef> },
    List { item_schema: Option<Box<BlockSchema>>, max_items: Option<usize> },
    Log { display_limit: usize, entry_schema: LogEntrySchema },
    Composite { sections: Vec<CompositeSection> },
}
```

**Text**: Simple text content with optional viewport for large documents
```rust
doc.set_text("Hello, world!", is_system)?;
doc.append_text(" More text.", is_system)?;
doc.replace_text("Hello", "Hi", is_system)?;
```

**Map**: Key-value fields with typed entries
```rust
doc.set_field("name", json!("Alice"), is_system)?;
doc.set_text_field("status", "active", is_system)?;
doc.append_to_list_field("tags", json!("important"), is_system)?;
doc.increment_counter("score", 5, is_system)?;
```

**List**: Ordered collection
```rust
doc.push_item(json!({"task": "Review PR"}), is_system)?;
doc.insert_item(0, json!({"task": "Urgent"}), is_system)?;
doc.delete_item(1, is_system)?;
```

**Log**: Append-only with display limit
```rust
doc.append_log_entry(json!({
    "timestamp": "2025-01-01T00:00:00Z",
    "message": "User logged in"
}), is_system)?;

// Get most recent entries (respects display_limit)
let entries = doc.log_entries(None);
```

**Composite**: Multiple sections with independent schemas
```rust
doc.set_text_in_section("notes here", "notes", is_system)?;
doc.set_field_in_section("count", 42, "stats", is_system)?;
```

### Subscriptions

Documents support change subscriptions:
```rust
let _sub = doc.subscribe_root(Arc::new(|event| {
    println!("Document changed: {:?}", event.triggered_by);
}));

doc.increment_counter("counter", 1, true)?;
doc.commit(); // Triggers subscriptions
```

### Rendering

Documents render to string for LLM context:
```rust
let content = doc.render();
```

Rendering respects schema:
- Text: Plain content (with viewport windowing if configured)
- Map: `field_name: value` format
- List: Numbered or checkbox format
- Log: Timestamped entries (newest first)
- Composite: Sections with `=== section_name ===` headers

Read-only fields/sections are marked in rendered output:
```
status [read-only]: active
=== diagnostics [read-only] ===
```

## Context Configuration

```rust
pub struct ContextConfig {
    pub default_limits: ModelContextLimits,
    pub model_overrides: HashMap<String, ModelContextLimits>,
    pub include_descriptions: bool,   // Include block descriptions in prompt
    pub include_schemas: bool,        // Include schema definitions
    pub activity_entries_limit: usize, // Recent activity to show
    pub compression_strategy: CompressionStrategy,
    pub max_messages_cap: usize,      // Hard limit on messages
}
```

### Model Context Limits

```rust
pub struct ModelContextLimits {
    pub max_tokens: usize,           // Total context window
    pub memory_tokens: usize,        // Reserved for memory blocks
    pub history_tokens: usize,       // Reserved for message history
    pub reserved_response_tokens: usize,
}
```

## Message Compression

When history exceeds limits, compression strategies apply:

```rust
pub enum CompressionStrategy {
    Truncate { keep_recent: usize },
    RecursiveSummarization {
        chunk_size: usize,
        summarization_model: String,
    },
}
```

The `MessageCompressor` handles batch-aware compression:
- Complete batches are compressed together
- Active batch (currently processing) is never compressed
- Incomplete non-active batches are excluded

## Search

Unified search across memory content:

```rust
let options = SearchOptions::new()
    .mode(SearchMode::Fts)  // or Vector, Hybrid, Auto
    .content_types(vec![SearchContentType::Messages])
    .limit(20);

let results = memory.search(agent_id, "task management", options).await?;
```

Search modes:
- **Fts**: FTS5 keyword search with BM25 scoring
- **Vector**: sqlite-vec similarity search
- **Hybrid**: Combines both with fusion
- **Auto**: Chooses based on embedder availability

## Ephemeral Blocks

Working blocks can be pinned or ephemeral:

- **Pinned**: Always loaded into context
- **Ephemeral** (unpinned): Only loaded when referenced

DataStream notifications can reference blocks:
```rust
let builder = ContextBuilder::new(&memory, &config)
    .for_agent("entropy")
    .with_batch_blocks(vec!["ephemeral-block-id".to_string()]);
```

This loads the specified blocks even if unpinned, allowing data sources to inject temporary context.

## Tool Rules Integration

Tool rules control execution flow and are included in the system prompt:

```rust
pub enum ToolRuleType {
    ExitLoop,           // Tool call ends the agent's turn
    ContinueLoop,       // Tool call continues processing
    MaxCalls(u32),      // Maximum calls allowed per turn
    RequiresPrior(String), // Only callable after another tool
}
```

Example rules:
```rust
let rules = vec![
    ToolRule::exit_loop("send_message".to_string()),
    ToolRule::continue_loop("search".to_string()),
    ToolRule::max_calls("api_request".to_string(), 3),
    ToolRule::start_constraint("context".to_string()),
];

let request = ContextBuilder::new(&memory, &config)
    .for_agent("entropy")
    .with_tool_rules(rules)
    .build()
    .await?;
```

Generated system prompt includes:
```
# Tool Execution Rules

- Call `context` first before any other tools
- The conversation will end after calling `send_message`
- The conversation will be continued after calling `search`
- Call `api_request` at most 3 times
```

## Best Practices

1. **Use appropriate block types**: Core for identity, Working for active context, Log for audit trails
2. **Set permissions carefully**: ReadOnly for system-managed data, Append for logs, ReadWrite for agent-editable content
3. **Leverage schemas**: Map schema for structured data, Composite for multi-section documents
4. **Monitor token usage**: Check ContextMetadata after building
5. **Use ephemeral blocks**: For notification-specific context that shouldn't persist in every request
