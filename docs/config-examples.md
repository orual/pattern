# Configuration Examples

This document provides examples of Pattern configuration files for common use cases.

1. Copy `pattern.example.toml` to `pattern.toml` (or `~/.config/pattern/config.toml`)
2. Edit the configuration to match your setup
3. Set required environment variables for your chosen model provider:
   - `OPENAI_API_KEY` for OpenAI
   - `ANTHROPIC_API_KEY` for Anthropic
   - `GEMINI_API_KEY` for Google Gemini
   - `OPENROUTER_API_KEY` for OpenRouter
   - `GROQ_API_KEY` for Groq
   - `COHERE_API_KEY` for Cohere
   - etc.

## OpenRouter Setup

OpenRouter provides access to multiple AI providers through a single API. It's especially useful for:
- Accessing models from multiple providers without managing separate API keys
- Using models that may not be directly available to you
- Cost optimization by routing to the best model for your use case

### Configuration

1. Get your API key from [OpenRouter](https://openrouter.ai/keys)
2. Set the environment variable:
   ```bash
   export OPENROUTER_API_KEY=sk-or-v1-your-key-here
   ```
3. Configure in `pattern.toml`:
   ```toml
   [model]
   provider = "OpenRouter"
   model = "anthropic/claude-3-opus"  # Use provider/model format
   ```

### Model Naming Convention

OpenRouter uses `provider/model-name` format for model IDs:
- `anthropic/claude-3-opus` - Claude 3 Opus via OpenRouter
- `openai/gpt-4o` - GPT-4o via OpenRouter
- `google/gemini-pro` - Gemini Pro via OpenRouter
- `meta-llama/llama-3.1-70b-instruct` - Llama 3.1 70B via OpenRouter
- `mistralai/mistral-large` - Mistral Large via OpenRouter

See [OpenRouter Models](https://openrouter.ai/models) for the full list of available models.

## Single Agent

Basic single-agent configuration (`pattern.toml`):

```toml
[user]
name = "Partner"

[agent]
name = "Assistant"
persona_path = "persona.md"
tools = ["block", "recall", "search", "send_message"]

[agent.model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[agent.memory.active_context]
content = ""
permission = "read_write"
memory_type = "core"

[model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[database]
path = "./constellation.db"
```

## Agent with Bluesky Integration

Single agent with Bluesky data source (`pattern.toml`):

```toml
[user]
name = "Partner"

[agent]
name = "SocialBot"
persona_path = "persona.md"
bluesky_handle = "mybot.bsky.social"
tools = ["block", "recall", "search", "send_message"]

[agent.model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[agent.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "SocialBot"
friends = ["did:plc:friend1", "did:plc:friend2"]
keywords = ["adhd", "executive function"]
languages = ["en"]
require_agent_participation = true

[[agent.tool_rules]]
tool_name = "send_message"
rule_type = { type = "exit_loop" }

[model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[database]
path = "./constellation.db"
```

## Multi-Agent Constellation

Full constellation with groups (`constellation.toml`):

```toml
[user]
name = "Partner"

[model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[agent]
name = "Pattern"
persona_path = "agents/pattern/persona.md"

[database]
path = "./constellation.db"

# Main conversational group with dynamic selection
[[groups]]
name = "Main Support"
description = "Primary ADHD support team"

[groups.pattern]
type = "dynamic"
selector = "capability"

[[groups.members]]
name = "Pattern"
config_path = "agents/pattern/agent.toml"
role = "supervisor"
capabilities = ["coordination", "planning", "emotional_support"]

[[groups.members]]
name = "Entropy"
config_path = "agents/entropy/agent.toml"
role = { specialist = { domain = "task_breakdown" } }
capabilities = ["task_analysis", "decomposition", "prioritization"]

[[groups.members]]
name = "Archive"
config_path = "agents/archive/agent.toml"
role = "regular"
capabilities = ["memory", "recall", "pattern_recognition"]

[groups.data_sources.bluesky]
type = "bluesky"
name = "bluesky"
target = "Main Support"
friends = ["did:plc:friend1"]
require_agent_participation = true

# Crisis response with round-robin
[[groups]]
name = "Crisis Response"
description = "High-priority support rotation"

[groups.pattern]
type = "round_robin"
skip_unavailable = true

[[groups.members]]
name = "Pattern"
agent_id = "pattern-agent-id"
role = "regular"

[[groups.members]]
name = "Anchor"
agent_id = "anchor-agent-id"
role = "regular"

# Planning pipeline
[[groups]]
name = "Planning"
description = "Sequential planning process"

[groups.pattern]
type = "pipeline"
stages = ["Entropy", "Pattern"]

[[groups.members]]
name = "Entropy"
agent_id = "entropy-agent-id"
role = { specialist = { domain = "analysis" } }

[[groups.members]]
name = "Pattern"
agent_id = "pattern-agent-id"
role = "supervisor"
```

## Agent Configuration File

Individual agent config (`agents/entropy/agent.toml`):

```toml
name = "Entropy"
persona_path = "persona.md"
system_prompt_path = "instructions.md"
tools = ["block", "recall", "search", "send_message"]

[model]
provider = "Anthropic"
model = "claude-sonnet-4-5-20250929"

[memory.persona]
content_path = "persona.md"
permission = "read_only"
memory_type = "core"

[memory.active_tasks]
content = ""
permission = "read_write"
memory_type = "working"

[[tool_rules]]
tool_name = "send_message"
rule_type = { type = "exit_loop" }

[[tool_rules]]
tool_name = "search"
rule_type = { type = "continue_loop" }
```

## Sleeptime Group

Background monitoring group:

```toml
[[groups]]
name = "Background Monitor"
description = "Periodic check-ins and monitoring"

[groups.pattern]
type = "sleeptime"
check_interval = 1200  # 20 minutes
intervention_agent = "Pattern"

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

[[groups.members]]
name = "Pattern"
agent_id = "pattern-agent-id"
role = "regular"

[[groups.members]]
name = "Archive"
agent_id = "archive-agent-id"
role = "observer"
```

## File Data Source

Agent with file monitoring:

```toml
[agent]
name = "FileWatcher"
tools = ["block", "recall", "search", "send_message", "file"]

[agent.data_sources.notes]
type = "file"
name = "notes"
paths = ["./notes", "./documents"]
recursive = true
include_patterns = ["*.md", "*.txt"]
exclude_patterns = ["*.tmp", ".git/**"]

[[agent.data_sources.notes.permission_rules]]
pattern = "*.config.toml"
permission = "read_only"

[[agent.data_sources.notes.permission_rules]]
pattern = "notes/**/*.md"
permission = "read_write"
```

## Tool Rules Examples

Comprehensive tool rules:

```toml
# Exit loop after send
[[agent.tool_rules]]
tool_name = "send_message"
rule_type = { type = "exit_loop" }

# Continue processing after search
[[agent.tool_rules]]
tool_name = "search"
rule_type = { type = "continue_loop" }

# Must call context first
[[agent.tool_rules]]
tool_name = "context"
rule_type = { type = "start_constraint" }

# Limit API calls
[[agent.tool_rules]]
tool_name = "web_fetch"
rule_type = { type = "max_calls", value = 5 }

# Cooldown for expensive operations
[[agent.tool_rules]]
tool_name = "summarize"
rule_type = { type = "cooldown", value = 60 }

# Restrict block operations
[[agent.tool_rules]]
tool_name = "block"
rule_type = { type = "allowed_operations", value = ["append", "replace", "load_from_archival"] }

# Require consent for destructive actions
[[agent.tool_rules]]
tool_name = "delete_archival"
rule_type = { type = "requires_consent", scope = "destructive" }
```

## Directory Structure

Recommended constellation directory layout:

```
constellation/
├── constellation.toml       # Main config
├── agents/
│   ├── pattern/
│   │   ├── agent.toml
│   │   ├── persona.md
│   │   └── instructions.md
│   ├── entropy/
│   │   ├── agent.toml
│   │   ├── persona.md
│   │   └── instructions.md
│   └── archive/
│       ├── agent.toml
│       ├── persona.md
│       └── instructions.md
├── shared/
│   └── common_context.md    # Shared memory content
├── notes/                   # For file data source
└── constellation.db         # SQLite database
```

## Environment Variables

Required environment variables (not in config files):

```bash
# Model providers (at least one required)
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
export GEMINI_API_KEY="..."

# Discord (if using Discord integration)
export DISCORD_TOKEN="..."

# Bluesky credentials stored in auth.db via CLI
# pattern atproto login your.handle.bsky.social
```
See `pattern.example.toml` for examples of all three methods.
