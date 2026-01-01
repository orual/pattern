# CLAUDE.md - Pattern Core

⚠️ **CRITICAL WARNING**: DO NOT run `pattern` CLI or test agents during development!
Production agents are running. CLI commands will disrupt active agents.

Core agent framework, memory management, and coordination system for Pattern's multi-agent ADHD support.

## Current Status
- SQLite migration complete, Loro CRDT memory, Jacquard ATProto client
- Active development: API server, MCP server, data sources

## Tool System Architecture

Following Letta/MemGPT patterns with multi-operation tools:

### Core Tools
1. **context** - Operations on context blocks
   - `append`, `replace`, `archive`, `load_from_archival`, `swap`

2. **recall** - Long-term storage operations
   - `insert`, `append`, `read`, `delete`
   - Full-text search with FTS5 BM25 scoring

3. **search** - Unified search across domains
   - Supports archival_memory, conversations, all
   - Domain-specific filters and limits

4. **send_message** - Agent communication
   - Routes through AgentMessageRouter
   - Supports CLI, Group, Discord, Queue endpoints

### Implementation Notes
- Each tool has single entry point with operation enum
- Tool usage rules bundled with tools via `usage_rule()` trait
- ToolRegistry automatically provides rules to context builder
- Archival labels included in context for intelligent memory management

## Message System Architecture

### Router Design
- Each agent has its own router (not singleton)
- Database queuing provides natural buffering
- Call chain prevents infinite loops
- Anti-looping: 30-second cooldown between rapid messages

### Endpoints
- **CliEndpoint**: Terminal output ✅
- **GroupEndpoint**: Coordination pattern routing ✅
- **DiscordEndpoint**: Discord integration ✅
- **QueueEndpoint**: Database persistence (stub)
- **BlueskyEndpoint**: ATProto posting ✅

## Architecture Overview

### Key Components

1. **Agent System** (`agent/`, `context/`)
   - Base `Agent` trait with memory and tool access
   - DatabaseAgent using `pattern-db`
   - AgentType enum with feature-gated ADHD variants

2. **Memory System** (`memory/`)
   - Loro CRDT based in-memory cache backed by `pattern-db`
   - **StructuredDocument sharing**: `MemoryCache::get_block()` returns a `StructuredDocument` where the internal `LoroDoc` is Arc-shared with the cache. Mutations via `set_text()`, `import_from_json()`, etc. propagate to the cached version. However, metadata fields (permission, label, accessor_agent_id) are *not* shared—they're cloned. After mutating, call `mark_dirty()` + `persist_block()` to save.

3. **Tool System** (`tool/`)
   - Type-safe `AiTool<Input, Output>` trait
   - Dynamic dispatch via `DynamicTool`
   - Thread-safe `ToolRegistry` using DashMap

4. **Coordination** (`coordination/`)
   - All patterns implemented and working
   - Type-erased `Arc<dyn Agent>` for group flexibility
   - Message routing and response aggregation

5. **Database** (`../pattern_db`, `../pattern_auth`)
   - SQLite embedded databases

6. **Data Sources** (`data_source/`)
   - Generic trait for pull/push consumption
   - Type-erased wrapper for concrete→generic bridging
   - Prompt templates using minijinja

## Common Patterns

### Creating a Tool
```rust
#[derive(Debug, Clone)]
struct MyTool;

#[async_trait]
impl AiTool for MyTool {
    type Input = MyInput;   // Must impl JsonSchema + Deserialize + Serialize
    type Output = MyOutput; // Must impl JsonSchema + Serialize

    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does something useful" }

    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        // Implementation
    }
}
```

### Error Handling
```rust
// Use specific error variants with context
return Err(CoreError::tool_not_found(name, available_tools));
return Err(CoreError::memory_not_found(&agent_id, &block_name, available_blocks));
```

## Performance Notes
- CompactString inlines strings ≤ 24 bytes
- DashMap shards internally for concurrent access
- ToolContext via Arc<AgentRuntime> for cheap cloning
- Database operations are non-blocking with optimistic updates

## Embedding Providers
- **Candle (local)**: Pure Rust with Jina models (512/768 dims)
- **OpenAI**: text-embedding-3-small/large
- **Cohere**: embed-english-v3.0
- **Ollama**: Stub only - TODO

**Known Issues**: BERT models fail in Candle (dtype errors), use Jina models instead.
