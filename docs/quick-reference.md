# Pattern Quick Reference Guide

Quick overview of Pattern's current implementation and key patterns.

## Project Structure

```
pattern/
├── crates/
│   ├── pattern_api/      # Shared API types and contracts
│   ├── pattern_auth/     # Credential storage (ATProto, Discord, providers)
│   ├── pattern_cli/      # Command-line interface with TUI builders
│   ├── pattern_core/     # Core framework (agents, memory, tools, coordination)
│   ├── pattern_db/       # SQLite database layer (FTS5, sqlite-vec)
│   ├── pattern_nd/       # ADHD-specific tools and personalities
│   ├── pattern_mcp/      # MCP client (working) and server (stub)
│   ├── pattern_discord/  # Discord bot integration
│   └── pattern_server/   # Backend API (in development)
├── docs/                 # Documentation
└── pattern.toml          # Configuration
```

## CLI Commands

```bash
# Single agent chat
pattern chat

# Group chat
pattern chat --group main

# Discord mode (requires group)
pattern chat --group main --discord

# Agent management
pattern agent create              # Interactive TUI builder
pattern agent edit <name>         # Edit existing agent
pattern agent list
pattern agent status <name>
pattern agent export <name>

# Group management
pattern group create              # Interactive TUI builder
pattern group edit <name>         # Edit existing group
pattern group list
pattern group status <name>
pattern group add member <group> <agent> --role regular

# Memory inspection
pattern debug list-core <agent>
pattern debug list-archival <agent>
pattern debug show-context <agent>
pattern debug search-archival --agent <name> "query"

# Export/Import
pattern export agent <name> -o agent.car
pattern export group <name>
pattern export constellation
pattern import car agent.car
pattern import letta agent.af
```

## Core Architecture

### RuntimeContext

Central management for agents and shared infrastructure:

```rust
use pattern_core::runtime::RuntimeContext;

let ctx = RuntimeContext::builder()
    .dbs_owned(dbs)
    .model_provider(model)
    .build()
    .await?;

// Load agents
let agent = ctx.load_agent("my_agent").await?;

// Start background processors
ctx.start_heartbeat_processor(event_handler).await?;
ctx.start_queue_processor().await;
```

### AgentRuntime

Per-agent runtime with memory, tools, and routing:

```rust
use pattern_core::runtime::AgentRuntime;

let runtime = AgentRuntime::builder()
    .agent_id(agent_id)
    .agent_name(name)
    .memory(memory_cache)
    .messages(message_store)
    .tools_shared(tool_registry)
    .model(model_provider)
    .dbs(dbs)
    .build()?;
```

### ToolContext Trait

What tools can access from the runtime:

```rust
pub trait ToolContext: Send + Sync {
    fn agent_id(&self) -> &str;
    fn memory(&self) -> &dyn MemoryStore;
    fn router(&self) -> &AgentMessageRouter;
    fn model(&self) -> Option<&dyn ModelProvider>;
    fn permission_broker(&self) -> &'static PermissionBroker;
    fn sources(&self) -> Option<Arc<dyn SourceManager>>;
}
```

## Built-in Tools

### block (Memory Operations)
```json
{
  "operation": "append",
  "label": "human",
  "content": "User prefers dark mode"
}
```
Operations: `append`, `replace`, `archive`, `load`, `swap`

### recall (Archival Memory)
```json
{
  "operation": "insert",
  "label": "meeting_notes_2024",
  "content": "Discussed project timeline"
}
```
Operations: `insert`, `append`, `read`, `delete`

### search (Unified Search)
```json
{
  "domain": "archival_memory",
  "query": "project timeline",
  "limit": 10
}
```
Domains: `archival_memory`, `conversations`, `all`

### send_message (Agent Communication)
```json
{
  "target": {
    "type": "agent",
    "id": "agent-123"
  },
  "content": "Please review this"
}
```
Target types: `user`, `agent`, `group`, `discord`, `bluesky`

## Memory System

### Loro CRDT Memory with MemoryStore

```rust
use pattern_core::memory::{MemoryCache, MemoryStore, BlockType};

// Create memory cache backed by pattern_db
let memory = MemoryCache::new(dbs.clone());

// Create a block
memory.create_block(
    agent_id,
    "persona",
    "Agent personality",
    BlockType::Core,
    BlockSchema::text(),
    5000,  // char limit
).await?;

// Update content
memory.update_block_text(agent_id, "persona", "I am helpful").await?;

// Append content
memory.append_to_block(agent_id, "human", "\nNew info").await?;

// Search archival memory
let results = memory.search_blocks(agent_id, "project deadline", 10).await?;
```

### Memory Permissions and ACL

- Levels: `read_only`, `partner`, `human`, `append`, `read_write` (default), `admin`.
- ACL rules:
  - Read: allowed for all.
  - Append: allowed for `append`/`read_write`/`admin`; `partner`/`human` require approval.
  - Overwrite/Replace: allowed for `read_write`/`admin`; `partner`/`human` require approval.
  - Delete: `admin` only.
- Consent prompts route to the originating channel (Discord) when available.

## Data Sources

### DataStream (Event-driven sources)

```rust
use pattern_core::data_source::DataStream;

// Bluesky firehose, Discord events, etc.
impl DataStream for MySource {
    async fn start(&self, ctx: Arc<dyn ToolContext>, owner: AgentId)
        -> Result<broadcast::Receiver<Notification>>;
}

// Register with RuntimeContext
ctx.register_stream(Arc::new(bluesky_source));
ctx.start_stream("bluesky", owner_agent_id).await?;
```

### DataBlock (Document-oriented sources)

```rust
use pattern_core::data_source::DataBlock;

// Files, configs with Loro versioning
impl DataBlock for FileSource {
    async fn load(&self, path: &Path, ctx: Arc<dyn ToolContext>, owner: AgentId)
        -> Result<BlockRef>;
    async fn save(&self, block_ref: &BlockRef, ctx: Arc<dyn ToolContext>)
        -> Result<()>;
}

// Register with RuntimeContext
ctx.register_block_source(Arc::new(file_source)).await;
```

## Group Coordination Patterns

```rust
use pattern_core::coordination::CoordinationPattern;

// Six patterns available:
// - Supervisor: Leader with delegation
// - RoundRobin: Turn-based distribution
// - Pipeline: Sequential processing
// - Dynamic: Selector-based routing
// - Voting: Quorum-based decisions
// - Sleeptime: Background monitoring
```

## Agent Naming, Roles, and Defaults

- Agent names are arbitrary: features no longer depend on specific names.
- Group roles drive behavior:
  - Supervisor: orchestrator; data sources route through by default.
  - Specialist with domain "system_integrity": gets SystemIntegrityTool.
  - Specialist with domain "memory_management": gets ConstellationSearchTool.
- Discord defaults:
  - Default agent selection prefers the Supervisor when unspecified.
  - Bot self-mentions resolve to the Supervisor agent's name.
- CLI sender labels by origin:
  - Agent: agent name
  - Bluesky: `@handle`
  - Discord: `Discord`
  - DataSource: `source_id`
  - CLI: `CLI`
  - API: `API`
  - Other: `origin_type`
  - None/unknown: `Runtime`

## Error Handling

```rust
use pattern_core::error::CoreError;

// Specific error variants with context
return Err(CoreError::tool_not_found(
    name,
    available_tools
));

return Err(CoreError::memory_not_found(
    &agent_id,
    &block_name,
    available_blocks
));
```

## Environment Variables

```bash
# Discord
DISCORD_TOKEN=your-bot-token
DISCORD_BATCH_DELAY_MS=1500

# Bluesky
BLUESKY_IDENTIFIER=your.handle.bsky.social
BLUESKY_PASSWORD=app-password
JETSTREAM_URL=wss://jetstream.atproto.tools

# Model providers
ANTHROPIC_API_KEY=sk-ant-...
OPENAI_API_KEY=sk-...
GOOGLE_API_KEY=...
```

## Common Patterns

### Creating Custom Tool
```rust
#[derive(Debug, Clone)]
struct MyTool {
    runtime: Arc<AgentRuntime>,
}

#[async_trait]
impl AiTool for MyTool {
    type Input = MyInput;
    type Output = MyOutput;

    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does something" }

    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        let ctx = self.runtime.as_ref() as &dyn ToolContext;
        // Use ctx.memory(), ctx.router(), etc.
    }
}

// Register
tool_registry.register_dynamic(tool.clone_box());
```

### Anti-looping Protection
```rust
// Router has built-in cooldown (30 seconds between rapid agent-to-agent messages)
// Messages between agents are throttled automatically
```

## Performance Notes

- CompactString inlines ≤24 byte strings
- DashMap shards for concurrent access
- MemoryCache uses write-through to SQLite
- Loro CRDT enables versioning and conflict-free merging
- Database ops are non-blocking with WAL mode
