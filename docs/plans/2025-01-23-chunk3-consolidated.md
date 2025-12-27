# Chunk 3: Agent Architecture Rework (Consolidated)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Rework the agent architecture with clean separation of concerns: slim Agent trait, Runtime for actions, ContextBuilder for assembly, separate stores for memory and messages.

**Depends On:** Chunk 1 (pattern_db), Chunk 2 (memory_v2)

**Incorporates:** Remaining work from Chunk 2.5 (context_v2)

---

## Architecture Overview

```
                    ┌───────────────────┐
                    │      Agent        │
                    │  (slim trait)     │
                    │                   │
                    │ • id, name, state │
                    │ • process() loop  │
                    └─────────┬─────────┘
                              │
              ┌───────────────┴───────────────┐
              │                               │
              ▼                               ▼
    ┌──────────────────┐            ┌──────────────────┐
    │     Runtime      │            │  ContextBuilder  │
    │     (doing)      │            │    (reading)     │
    │                  │            │                  │
    │ • execute_tool() │            │ • build_context()│
    │ • send_message() │◄───reads───│ • reads stores   │
    │ • store_message()│            │ • model-aware    │
    │ • permissions    │            │ • cache-aware    │
    │ • router         │            │ → Request        │
    └────────┬─────────┘            └──────────────────┘
             │
             ▼
    ┌──────────────────┐
    │     Stores       │
    │                  │
    │ • MemoryStore    │ ← memory_v2 (blocks, archival)
    │ • MessageStore   │ ← pattern_db (history, batches)
    └──────────────────┘
             │
             ▼
    ┌──────────────────┐
    │  Search/Embed    │
    │    (db_v2)       │
    │                  │
    │ • embed_and_store│ ← async background
    │ • semantic_search│ ← embeds query, searches
    └──────────────────┘
```

### Key Principles

1. **Agent is slim** - just identity + process loop + state
2. **Runtime does things** - tool execution, message sending, permission checks
3. **ContextBuilder reads things** - assembles model requests from current state
4. **Stores are separate** - MemoryStore (blocks) vs MessageStore (history)
5. **Memory access via tools** - agents don't directly touch memory, tools do
6. **Embeddings at db layer** - async background jobs, not blocking writes
7. **Stream-first processing** - `process()` returns Stream, collector helper for Response

### Store Scoping

- **MemoryStore** - **Shared** across agents. Agent-agnostic, `agent_id` is just a parameter.
  Multiple agents can use the same MemoryStore instance.

- **MessageStore** - **Per-agent**. Messages are provider-specific in practice (thinking blocks,
  tool call formats, etc.). Each agent gets its own MessageStore instance configured for its
  model provider.

---

## Component Specifications

### 1. Agent Trait (Slim)

```rust
#[async_trait]
pub trait Agent: Send + Sync + Debug {
    fn id(&self) -> AgentId;
    fn name(&self) -> &str;
    fn runtime(&self) -> &AgentRuntime;

    /// Process a message, streaming response events
    async fn process(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>>;

    /// Current agent state
    async fn state(&self) -> AgentState;
    async fn set_state(&self, state: AgentState) -> Result<()>;
}

// Collector helper
pub async fn collect_response(
    stream: impl Stream<Item = ResponseEvent>,
) -> Result<Response> {
    // Collects stream into final Response
}
```

**What's removed from old Agent trait:**
- `get_memory()`, `update_memory()`, `set_memory()` → via tools through Runtime
- `share_memory_with()`, `get_shared_memories()` → via MemoryStore
- `execute_tool()` → moved to Runtime
- `available_tools()` → in ToolRegistry
- `system_prompt()` → handled by ContextBuilder
- `register_endpoint()`, `set_default_user_endpoint()` → on Runtime
- `handle()` → replaced by `runtime()`

### 2. AgentRuntime (Doing)

```rust
pub struct AgentRuntime {
    agent_id: AgentId,
    agent_name: String,

    // Stores
    memory: Arc<dyn MemoryStore>,
    messages: Arc<MessageStore>,

    // Execution
    tools: Arc<ToolRegistry>,
    permissions: PermissionBroker,
    router: AgentMessageRouter,

    // Model
    model: Arc<dyn ModelProvider>,

    // Database
    pool: SqlitePool,
}

impl AgentRuntime {
    // Tool execution with permission arbitration
    pub async fn execute_tool(&self, call: &ToolCall) -> Result<ToolResponse>;

    // Message operations
    pub async fn store_message(&self, message: &Message) -> Result<()>;
    pub async fn send_message(&self, target: MessageTarget, content: &str) -> Result<()>;

    // Endpoint management
    pub async fn register_endpoint(&self, name: &str, endpoint: Arc<dyn MessageEndpoint>);
    pub async fn set_default_endpoint(&self, endpoint: Arc<dyn MessageEndpoint>);

    // Context building (delegates to ContextBuilder)
    pub async fn build_context(&self, config: &ContextConfig) -> Result<MemoryContext>;

    // Search (delegates to db_v2)
    pub async fn search(&self, query: &str, options: SearchOptions) -> Result<Vec<SearchResult>>;
}
```

### 3. ContextBuilder (Reading)

```rust
pub struct ContextBuilder<'a> {
    memory: &'a dyn MemoryStore,
    messages: &'a MessageStore,
    tools: &'a ToolRegistry,
    config: &'a ContextConfig,
}

impl<'a> ContextBuilder<'a> {
    pub fn new(
        memory: &'a dyn MemoryStore,
        messages: &'a MessageStore,
        tools: &'a ToolRegistry,
        config: &'a ContextConfig,
    ) -> Self;

    /// Build complete context for model request
    pub async fn build(&self) -> Result<MemoryContext>;

    /// Build with specific model adjustments
    pub async fn build_for_model(&self, model: &ModelInfo) -> Result<MemoryContext>;
}


```

**ContextBuilder responsibilities:**
- Read memory blocks (core, working) for system prompt
- Read message history, apply compression if needed
- Read tool definitions and rules
- Model-specific formatting (Anthropic vs Gemini vs OpenAI)
- Cache optimization (stable prefix, variable suffix)
- Include summaries at top of context window

### 4. MessageStore

```rust
pub struct MessageStore {
    pool: SqlitePool,
    agent_id: String,
}

impl MessageStore {
    pub async fn get_recent(&self, limit: usize) -> Result<Vec<Message>>;
    pub async fn get_all(&self, limit: usize) -> Result<Vec<Message>>;
    pub async fn store(&self, message: &Message) -> Result<()>;
    pub async fn archive_before(&self, position: SnowflakePosition) -> Result<u64>;

    // Batch operations
    pub async fn get_batch(&self, batch_id: &str) -> Result<Vec<Message>>;
    pub async fn current_batch(&self) -> Result<Option<String>>;

    // Summaries
    pub async fn get_summary(&self) -> Result<Option<AgentSummary>>;
    pub async fn upsert_summary(&self, summary: &AgentSummary) -> Result<()>;

    // Activity (uses existing coordination queries)
    pub async fn log_activity(&self, event: ActivityEvent) -> Result<()>;
    pub async fn recent_activity(&self, limit: usize) -> Result<Vec<ActivityEvent>>;
}
```

### 5. Search & Embedding Layer (db_v2)

db_v2 wraps pattern_db and adds embedding logic. Leverages `pattern_db::search::HybridSearchBuilder`.

```rust
// In crates/pattern_core/src/db_v2/mod.rs

/// Database layer with embedding support
pub struct ConstellationDb {
    pool: SqlitePool,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    embed_tx: Option<mpsc::Sender<EmbedJob>>,  // For background embedding
}

impl ConstellationDb {
    /// Create with embedding support
    pub fn with_embedder(pool: SqlitePool, embedder: Arc<dyn EmbeddingProvider>) -> Self;

    /// Unified search - uses pattern_db::search::HybridSearchBuilder
    /// If embedder configured and mode requires it, embeds query first
    pub async fn search(&self, query: &str, options: SearchOptions) -> DbResult<Vec<SearchResult>> {
        let mut builder = pattern_db::search::search(&self.pool)
            .text(query)
            .filter(options.filter)
            .limit(options.limit)
            .mode(options.mode);

        // If vector/hybrid requested and we have embedder, embed query
        if options.mode.needs_embedding() {
            if let Some(embedder) = &self.embedder {
                let embedding = embedder.embed_query(query).await?;
                builder = builder.embedding(embedding);
            }
            // No embedder: gracefully fall back to FTS
        }

        builder.execute().await
    }

    /// Queue content for background embedding (non-blocking)
    pub fn queue_embedding(&self, content_type: ContentType, content_id: &str, text: &str);
}
```

**Embedding flow:**
- **Search:** Single `search()` method, mode determines behavior (FTS/Vector/Hybrid/Auto)
- **Content writes:** `queue_embedding()` fires event, background task embeds and stores
- **No embedder:** All modes gracefully fall back to FTS

### 6. Memory Search in memory_v2

Expose pattern_db search capabilities:

```rust
// In memory_v2/store.rs

pub trait MemoryStore: Send + Sync {
    // ... existing block operations ...

    /// Search memory (FTS, vector, or hybrid)
    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<SearchResult>>;
}

pub struct SearchOptions {
    pub mode: SearchMode,  // Fts, Vector, Hybrid, Auto
    pub content_types: Vec<SearchContentType>,  // Blocks, Archival, Messages
    pub limit: usize,
}

pub enum SearchMode {
    Fts,
    Vector,  // Returns error if embeddings not configured
    Hybrid,
    Auto,    // FTS if no embedder, Hybrid if available
}
```

---

## Task Breakdown

### Phase A: Foundation Types

**Task A1: Add SearchOptions to memory_v2**
- Add `SearchOptions`, `SearchMode` types to `memory_v2/types.rs`
- Add `search()` method to `MemoryStore` trait
- Implement in `MemoryCache` - delegate to pattern_db::search
- Vector mode returns `MemoryError::VectorSearchNotConfigured` until db_v2 embeddings ready

**Task A2: Add MessageStore**
- Create `crates/pattern_core/src/messages/store.rs`
- Thin wrapper around `pattern_db::queries::messages`
- Include summary operations (uses coordination queries)
- Include activity logging

**Task A3: Update context_v2 types**
- Finalize `ContextConfig` in context_v2/types.rs
- Add `MemoryContext` output type
- Add `CachePoint` for prompt caching hints
- Remove unnecessary `ToolDescription` types (use ToolRegistry directly)

### Phase B: ContextBuilder

**Task B1: Create ContextBuilder**
- `crates/pattern_core/src/context_v2/builder.rs`
- Read-only access to MemoryStore, MessageStore, ToolRegistry
- Build system prompt from memory blocks
- Handle message history with compression
- Model-aware formatting

**Task B2: Add model-specific optimizations**
- Anthropic cache control points
- Gemini message requirements
- Token estimation per model

**Task B3: Compression integration**
- Port/adapt existing `MessageCompressor` logic
- Use ModelProvider for summarization strategies
- Integrate with ContextBuilder

### Phase C: AgentRuntime

**Task C1: Create AgentRuntime struct**
- `crates/pattern_core/src/agent/runtime.rs`
- Holds all dependencies (stores, tools, permissions, router, model)
- Core `execute_tool()` with permission checks

**Task C2: Port MessageRouter**
- Update to use pattern_db queries (not SurrealDB)
- Keep MessageEndpoint trait unchanged
- Move endpoint registration to Runtime

**Task C3: Port tool execution**
- Move from AgentContext to Runtime
- Permission broker integration
- Memory tools call MemoryStore via Runtime

### Phase D: Agent Trait Rework

**Task D1: Define slim Agent trait**
- `crates/pattern_core/src/agent/trait.rs` (or update mod.rs)
- Just id, name, state, process(), runtime()
- Stream-based processing

**Task D2: Add AgentExt and collector**
- `process_to_response()` helper
- Stream collecting utilities

**Task D3: Implement DatabaseAgentV2**
- `crates/pattern_core/src/agent/impls/db_agent_v2.rs`
- Uses new Runtime, ContextBuilder
- Implements slim Agent trait

### Phase E: Integration

**Task E1: Update heartbeat processor**
- Uses new Agent trait (just needs process())
- Should work with minimal changes

**Task E2: Port QueuedMessage/ScheduledWakeup**
- Remove Entity macro, use pattern_db models
- Update message_queue.rs

**Task E3: Update built-in tools**
- `context`, `recall`, `search` tools use Runtime
- Memory mutation tools go through Runtime

**Task E4: Integration tests**
- End-to-end agent processing
- Tool execution with permissions
- Search (FTS working, vector errors gracefully)

### Phase F: Cleanup (Later)

**Task F1: Remove old agent code**
- Delete `db_agent.rs` when v2 is stable
- Delete `context/state.rs` AgentContext/AgentHandle
- Remove SurrealDB dependencies

---

## Migration Mapping

| Old Location | New Location | Notes |
|-------------|--------------|-------|
| `Agent` trait (17+ methods) | `Agent` trait (5 methods) | Slim down |
| `AgentHandle` | `AgentRuntime` | Runtime does things |
| `AgentContext` | Split: Runtime + ContextBuilder | Separate concerns |
| `context/compression.rs` | `context_v2/builder.rs` | Integrated |
| `context/state.rs` search methods | `MemoryStore::search()` | Via trait |
| `message_router.rs` SurrealDB queries | pattern_db queries | DB swap |
| `message_queue.rs` Entity | pattern_db models | No macro |
| `db_agent.rs` | `db_agent_v2.rs` | New implementation |

---

## Execution Order

Recommended sequence:
1. **A1-A3** in parallel (foundation types)
2. **B1** (ContextBuilder core)
3. **B2-B3** (ContextBuilder model-aware + compression)
4. **C1** (AgentRuntime core)
5. **C2-C3** (Runtime: router + tools)
6. **D1-D2** (Agent trait)
7. **D3** (DatabaseAgentV2)
8. **E1-E4** (Integration)
9. **F1** (Cleanup - later, after testing)

---

## Success Criteria

- [ ] `cargo check -p pattern_core` passes
- [ ] `cargo test -p pattern_core` passes
- [ ] Agent processes messages with new architecture
- [ ] Tools execute through Runtime with permission checks
- [ ] Memory operations work via tools
- [ ] FTS search works, vector search returns graceful error
- [ ] Message history persists correctly
- [ ] Context building produces valid model requests
- [ ] Heartbeat processor works with new Agent trait
- [ ] Old code remains compilable (parallel implementation)

---

## Design Decisions

1. **Naming:** `AgentRuntime` ✓

2. **ToolRegistry scope:** Variable. Generally per-agent, but some tools need shared state
   (data sources, etc.). Option to create multiple agents with a totally shared ToolRegistry.

3. **Embedder injection:** At db_v2 layer. This is where we diverge from using pattern_db
   query methods directly - db_v2 wraps pattern_db and adds embedding logic:
   - **Search queries:** Blocking - need embedding before search can proceed
   - **Messages/memory blocks:** Non-blocking, event-triggered background jobs

4. **Stream type:** `Box<dyn Stream + Send + Unpin>` for trait object compatibility.
