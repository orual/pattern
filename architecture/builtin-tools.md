# Built-in Tools Architecture

This document describes the built-in tools system in Pattern, which provides standard agent capabilities following the Letta/MemGPT pattern for stateful memory management.

## Overview

Pattern agents come with a set of built-in tools that provide core functionality for memory management, archival storage, and communication. These tools are implemented using the same `AiTool` trait as external tools, ensuring consistency and allowing customization.

## Architecture

### ToolContext Trait

The `ToolContext` trait provides tools with controlled access to agent runtime services. Unlike the old `AgentHandle` approach, tools receive a trait object that exposes only the APIs they need:

```rust
#[async_trait]
pub trait ToolContext: Send + Sync {
    /// Get the current agent's ID (for default scoping)
    fn agent_id(&self) -> &str;

    /// Get the memory store for blocks, archival, and search
    fn memory(&self) -> &dyn MemoryStore;

    /// Get the message router for send_message
    fn router(&self) -> &AgentMessageRouter;

    /// Get the model provider for tools that need LLM calls
    fn model(&self) -> Option<&dyn ModelProvider>;

    /// Get the permission broker for consent requests
    fn permission_broker(&self) -> &'static PermissionBroker;

    /// Search with explicit scope and permission checks
    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;

    /// Get the source manager for data source operations
    fn sources(&self) -> Option<Arc<dyn SourceManager>>;

    /// Get the shared block manager for block sharing operations
    fn shared_blocks(&self) -> Option<Arc<SharedBlockManager>>;
}
```

Tools access memory through the `MemoryStore` trait:
- `create_block()` - Create new memory blocks
- `get_block()` / `list_blocks()` - Read block content and metadata
- `update_block_text()` / `append_to_block()` - Modify block content
- `search_blocks()` - Full-text search with FTS5 BM25 scoring
- `persist_block()` - Flush changes to database

## Built-in Tools

### 1. Block Tool (context)

Manages core memory blocks following the Letta/MemGPT pattern. Each operation modifies memory and requires the agent to continue their response.

**Operations:**
- `append` - Add content to existing memory (always uses \n separator)
- `replace` - Replace specific content within memory
- `archive` - Move a core memory block to archival storage
- `load` - Load an archival memory block into working/core
- `swap` - Atomic operation to archive one block and load another

```rust
// Example: Append to memory
{
    "operation": "append",
    "label": "human",
    "content": "Prefers morning meetings"
}

// Example: Swap memory blocks
{
    "operation": "swap",
    "archive_label": "old_project",
    "load_label": "new_project"
}
```

### 2. Recall Tool

Manages long-term archival storage with full-text search capabilities via FTS5.

**Operations:**
- `insert` - Add new memories to archival storage
- `append` - Add content to existing archival memory
- `read` - Read specific archival memory by label
- `delete` - Remove archived memories

```rust
// Example: Insert new archival memory
{
    "operation": "insert",
    "label": "meeting_notes_2024_01",
    "content": "Discussed project timeline..."
}
```

### 3. Search Tool

Unified search interface across different domains using hybrid FTS5 + vector search.

**Domains:**
- `archival_memory` - Search archival storage
- `conversations` - Search message history
- `all` - Search everything

```rust
// Example: Search archival memory
{
    "domain": "archival_memory",
    "query": "project deadline",
    "limit": 10
}
```

### 4. Send Message Tool

Sends messages to the user (required for agents to yield control):

```rust
// Input
{
    "message": "I've updated your preferences. How else can I help?"
}

// Output
{
    "success": true,
    "message": "Message sent successfully"
}
```

## Registration and Customization

### Default Registration

```rust
// In agent loading via RuntimeContext
let builtin = BuiltinTools::new(runtime.clone());
builtin.register_all(&tools);
```

### Custom Memory Backend

For a custom memory backend (e.g., Redis, external database), implement the `MemoryStore` trait:

```rust
use pattern_core::memory::{MemoryStore, MemoryResult, BlockMetadata, StructuredDocument};

#[derive(Debug)]
struct RedisMemoryStore {
    redis: Arc<RedisClient>,
}

#[async_trait]
impl MemoryStore for RedisMemoryStore {
    async fn create_block(&self, agent_id: &str, label: &str, ...) -> MemoryResult<String> {
        // Store in Redis
        self.redis.hset(agent_id, label, block_data).await?;
        Ok(block_id)
    }

    async fn get_block(&self, agent_id: &str, label: &str)
        -> MemoryResult<Option<StructuredDocument>>
    {
        // Retrieve from Redis
        self.redis.hget(agent_id, label).await
    }

    // ... implement other MemoryStore methods
}

// Use when building RuntimeContext
let memory = Arc::new(RedisMemoryStore::new(redis_client));
let ctx = RuntimeContext::builder()
    .dbs_owned(dbs)
    .model_provider(model)
    .memory(memory)  // Custom memory backend
    .build()
    .await?;
```

### Custom Tools

Users can also register additional tools alongside built-ins:

```rust
#[derive(Debug, Clone)]
struct WeatherTool {
    api_key: String,
}

#[async_trait]
impl AiTool for WeatherTool {
    type Input = WeatherInput;
    type Output = WeatherOutput;

    fn name(&self) -> &str { "get_weather" }
    fn description(&self) -> &str { "Get weather for a location" }

    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        // Call weather API
    }
}

// Register alongside built-ins
registry.register_dynamic(weather_tool.clone_box());
```

## Design Decisions

### Why Use the Same Tool System?

1. **Consistency**: All tools go through the same registry and execution path
2. **Discoverability**: Agents can list all available tools, including built-ins
3. **Testability**: Built-in tools can be tested like any other tool
4. **Flexibility**: Easy to override or extend built-in behavior

### Why ToolContext Trait?

1. **Abstraction**: Tools depend on interface, not implementation
2. **Testability**: Easy to mock in unit tests
3. **Safety**: Only exposes what tools need, not full runtime
4. **Future-proof**: Interface can evolve without breaking tools

### Why Not Special-Case Built-ins?

We considered having built-in tools as methods on the Agent trait or handled specially, but chose the unified approach because:

1. Users might want to disable or replace built-in tools
2. The tool registry provides a single source of truth
3. Special-casing would complicate the execution path
4. The performance overhead is minimal

## Implementation Details

### Type-Safe Tools

Built-in tools use the generic `AiTool` trait for type safety:

```rust
#[async_trait]
impl AiTool for BlockTool {
    type Input = BlockOperation;    // Strongly typed, deserializable
    type Output = BlockResult;      // Strongly typed, serializable

    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        let ctx = self.runtime.as_ref() as &dyn ToolContext;
        // Compile-time type checking
    }
}
```

### Dynamic Dispatch

The `DynamicTool` trait wraps typed tools for storage in the registry:

```rust
registry.register_dynamic(tool.clone_box());
```

### MCP Compatibility

Tool schemas are generated with `inline_subschemas = true` to ensure no `$ref` fields, meeting MCP requirements.

## Memory Permissions and Enforcement

Memory blocks carry a `permission` (enum `MemoryPermission`). New blocks default to `read_write` unless configured. Tools enforce an ACL as follows:

- Read: always allowed.
- Append: allowed for `append`/`read_write`/`admin`; `partner`/`human` require approval via PermissionBroker; `read_only` denied.
- Overwrite/Replace: allowed for `read_write`/`admin`; `partner`/`human` require approval; `append`/`read_only` denied.
- Delete: `admin` only.

Tool-specific notes:

- `block.append` and `block.replace` enforce ACL and request `MemoryEdit { key }` when needed.
- `block.archive` checks Overwrite ACL if the archival label already exists; deleting the source context requires Admin.
- `block.load` behavior:
  - Same label: convert archival → working in-memory.
  - Different label: create new working block and retain archival.
  - Does not delete archival.
- `block.swap` enforces Overwrite ACL on the destination (with possible approval) and deletes the source archival only with Admin.
- `recall.append` enforces ACL; `recall.delete` requires Admin.

Consent prompts are routed with origin metadata (e.g., Discord channel) for fast approval.

## Future Extensions

### Planned Built-in Tools

1. **semantic_search**: Enhanced semantic search with embedding support
2. **schedule_reminder**: Set time-based reminders
3. **track_task**: Create and track ADHD-friendly tasks

## Usage Examples

### Basic Memory Update via Block Tool

```rust
let result = registry.execute("block", json!({
    "operation": "append",
    "label": "preferences",
    "content": "User prefers dark mode"
})).await?;
```

### Testing Tools with Mock Context

```rust
#[tokio::test]
async fn test_block_tool() {
    // Create test runtime with mock memory
    let runtime = create_test_runtime().await;
    let tool = BlockTool::new(runtime);

    let result = tool.execute(BlockOperation::Append {
        label: "test".to_string(),
        content: "test value".to_string(),
    }).await.unwrap();

    assert!(result.success);
}
```

## Best Practices

1. **Keep tools focused**: Each tool should do one thing well
2. **Use type safety**: Define proper Input/Output types with JsonSchema
3. **Handle errors gracefully**: Return meaningful error messages
4. **Document tool behavior**: Provide clear descriptions and examples
5. **Consider concurrency**: MemoryStore is thread-safe via Arc
6. **Test thoroughly**: Built-in tools are critical infrastructure
