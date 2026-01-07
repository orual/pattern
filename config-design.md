# Configuration System

Pattern uses TOML configuration files to define agents, groups, memory blocks, data sources, and model settings. The system supports a cascade of configuration sources with runtime overrides.

## Configuration Files

### Single-Agent Configuration (`pattern.toml`)

```toml
[user]
id = "user-id"        # Optional: auto-generated and persisted if not specified
name = "Partner"

[agent]
name = "Assistant"
persona_path = "persona.md"           # Path to persona file (relative to config)
system_prompt_path = "instructions.md" # Path to system prompt file
tools = ["block", "recall", "search", "send_message", "web"]

[agent.model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[agent.memory.active_context]
content = "Currently working on..."
permission = "read_write"
memory_type = "core"

[agent.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "Assistant"
friends = ["did:plc:friend1", "did:plc:friend2"]

[[agent.tool_rules]]
tool_name = "send_message"
rule_type = { type = "exit_loop" }

[model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[database]
path = "./constellation.db"
```

### Constellation Configuration (`constellation.toml`)

For multi-agent setups with groups:

```toml
[user]
id = "owner-id"
name = "Partner"

[model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[agent]
name = "Pattern"
persona_path = "agents/pattern/persona.md"

[[groups]]
name = "Main Support"
description = "Primary ADHD support group"

[groups.pattern]
type = "dynamic"
selector = "capability"

[[groups.members]]
name = "Pattern"
config_path = "agents/pattern/agent.toml"
role = "supervisor"
capabilities = ["coordination", "planning"]

[[groups.members]]
name = "Entropy"
config_path = "agents/entropy/agent.toml"
role = { specialist = { domain = "task_breakdown" } }
capabilities = ["task_analysis", "decomposition"]

[[groups.members]]
name = "Archive"
config_path = "agents/archive/agent.toml"
role = "regular"
capabilities = ["memory", "recall"]

[groups.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "Main Support"
friends = ["did:plc:friend1"]
require_agent_participation = true
```

## Core Types

### PatternConfig

Top-level configuration structure:

```rust
pub struct PatternConfig {
    pub user: UserConfig,
    pub agent: AgentConfig,
    pub model: ModelConfig,
    pub database: DatabaseConfig,
    pub groups: Vec<GroupConfig>,
    pub bluesky: Option<BlueskyConfig>,
    pub discord: Option<DiscordAppConfig>,
}
```

### AgentConfig

```rust
pub struct AgentConfig {
    pub id: Option<AgentId>,
    pub name: String,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<PathBuf>,
    pub persona: Option<String>,
    pub persona_path: Option<PathBuf>,
    pub instructions: Option<String>,
    pub memory: HashMap<String, MemoryBlockConfig>,
    pub bluesky_handle: Option<String>,
    pub data_sources: HashMap<String, DataSourceConfig>,
    pub tool_rules: Vec<ToolRuleConfig>,
    pub tools: Vec<String>,
    pub model: Option<ModelConfig>,
    pub context: Option<ContextConfigOptions>,
}
```

### MemoryBlockConfig

```rust
pub struct MemoryBlockConfig {
    pub content: Option<String>,
    pub content_path: Option<PathBuf>,
    pub permission: MemoryPermission,
    pub memory_type: MemoryType,
    pub description: Option<String>,
    pub id: Option<MemoryId>,
    pub shared: bool,
}
```

### GroupConfig

```rust
pub struct GroupConfig {
    pub id: Option<GroupId>,
    pub name: String,
    pub description: String,
    pub pattern: GroupPatternConfig,
    pub members: Vec<GroupMemberConfig>,
    pub shared_memory: HashMap<String, MemoryBlockConfig>,
    pub data_sources: HashMap<String, DataSourceConfig>,
}
```

## Coordination Pattern Configuration

Six patterns are supported:

### Supervisor

```toml
[groups.pattern]
type = "supervisor"
leader = "Pattern"  # Member name who leads
```

### Round Robin

```toml
[groups.pattern]
type = "round_robin"
skip_unavailable = true
```

### Pipeline

```toml
[groups.pattern]
type = "pipeline"
stages = ["Analysis", "Planning", "Execution"]
```

### Dynamic

```toml
[groups.pattern]
type = "dynamic"
selector = "capability"  # or "random", "load_balancing", "supervisor"

[groups.pattern.selector_config]
preferred_domain = "task_management"
```

### Sleeptime

```toml
[groups.pattern]
type = "sleeptime"
check_interval = 1200  # 20 minutes in seconds

[[groups.pattern.triggers]]
name = "hyperfocus_check"
priority = "high"
[groups.pattern.triggers.condition]
type = "time_elapsed"
duration = 5400  # 90 minutes

[[groups.pattern.triggers]]
name = "activity_sync"
priority = "medium"
[groups.pattern.triggers.condition]
type = "constellation_activity"
message_threshold = 20
time_threshold = 3600
```

## Data Source Configuration

### Bluesky

```toml
[agent.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "agent-or-group-name"
jetstream_endpoint = "wss://jetstream1.us-east.fire.hose.cam/subscribe"
friends = ["did:plc:abc123"]           # Always see posts from these
mentions = ["did:plc:mybot"]           # Agent's DID for self-detection
keywords = ["adhd", "productivity"]    # Filter by keywords
languages = ["en"]                     # Filter by language
exclude_dids = ["did:plc:spam"]        # Block these users
exclude_keywords = ["spam"]            # Filter out these
allow_any_mentions = false             # Only friends can mention
require_agent_participation = true     # Only show threads agent is in
```

### File Source

```toml
[agent.data_sources.notes]
type = "file"
name = "notes"
paths = ["./notes", "./documents"]
recursive = true
include_patterns = ["*.md", "*.txt"]
exclude_patterns = ["*.tmp"]

[[agent.data_sources.notes.permission_rules]]
pattern = "*.config.toml"
permission = "read_only"

[[agent.data_sources.notes.permission_rules]]
pattern = "notes/**/*.md"
permission = "read_write"
```

### Discord

```toml
[agent.data_sources.discord]
type = "discord"
name = "discord"
guild_id = "123456789"
channel_ids = ["987654321"]
```

## Tool Rules

Control tool execution behavior:

```toml
# Continue loop after calling (default for most tools)
[[agent.tool_rules]]
tool_name = "search"
rule_type = { type = "continue_loop" }

# Exit loop after calling (ends agent's turn)
[[agent.tool_rules]]
tool_name = "send_message"
rule_type = { type = "exit_loop" }

# Must call first
[[agent.tool_rules]]
tool_name = "context"
rule_type = { type = "start_constraint" }

# Maximum calls per turn
[[agent.tool_rules]]
tool_name = "web_search"
rule_type = { type = "max_calls", value = 3 }

# Cooldown between calls (seconds)
[[agent.tool_rules]]
tool_name = "expensive_api"
rule_type = { type = "cooldown", value = 30 }

# Requires preceding tool
[[agent.tool_rules]]
tool_name = "submit"
rule_type = { type = "requires_preceding_tools" }
conditions = ["validate"]

# Limit operations for multi-op tools
[[agent.tool_rules]]
tool_name = "block"
rule_type = { type = "allowed_operations", value = ["append", "replace"] }

# Requires user consent
[[agent.tool_rules]]
tool_name = "delete_file"
rule_type = { type = "requires_consent", scope = "destructive_actions" }
```

## Context Configuration

Override context building settings:

```toml
[agent.context]
max_messages = 50
memory_char_limit = 4000
enable_thinking = true
include_descriptions = true
include_schemas = false
activity_entries_limit = 10
```

## Configuration Cascade

Configuration is resolved in priority order:

1. **Runtime Overrides** (`AgentOverrides`) - Highest priority, not persisted
2. **Agent Config** (from TOML or database)
3. **Group Config** (for agents in groups)
4. **PatternConfig defaults**

```rust
// Apply runtime overrides
let resolved = ResolvedAgentConfig::from_agent_config(&agent_config, &defaults)
    .apply_overrides(&AgentOverrides::new()
        .with_model("openai", "gpt-4")
        .with_temperature(0.7));
```

## Path Resolution

Relative paths in configuration are resolved relative to the config file's directory:

- `persona_path: "persona.md"` → `./persona.md`
- `config_path: "agents/pattern/agent.toml"` → `./agents/pattern/agent.toml`
- `content_path: "../shared/block.md"` → `../shared/block.md`

## Loading Configuration

```rust
use pattern_core::config::{PatternConfig, load_config};

// Load from standard locations (pattern.toml, ~/.config/pattern/config.toml, etc.)
let config = PatternConfig::load().await?;

// Load from specific file
let config = PatternConfig::load_from(Path::new("./my-config.toml")).await?;

// Load just agent config
let agent_config = AgentConfig::load_from_file(Path::new("./agent.toml")).await?;

// Save config
config.save_to(Path::new("./pattern.toml")).await?;
```

## User ID Stability

The `user.id` is automatically generated and persisted to `~/.config/pattern/user_id` if not explicitly specified. This ensures the same user ID is used across sessions even without a config file.

## Environment Variables

Sensitive credentials are stored in `auth.db`, not config files:

- Bluesky OAuth tokens and app passwords
- Discord bot token (via `DISCORD_TOKEN` env var)
- Model provider API keys (via standard env vars like `ANTHROPIC_API_KEY`)
