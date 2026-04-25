# Interactive TUI Builders

Pattern includes interactive terminal UI builders for creating and editing agents and groups. These builders provide a guided experience with live configuration previews, section-based editing, and multiple save options.

## Quick Start

```bash
# Create a new agent interactively
pattern agent create

# Edit an existing agent
pattern agent edit MyAgent

# Create a new group interactively
pattern group create

# Edit an existing group
pattern group edit MyGroup

# Load from TOML template
pattern agent create --from template.toml
pattern group create --from template.toml
```

## How Builders Work

Both builders follow the same interaction pattern:

1. **Summary Display**: Shows a formatted overview of the current configuration
2. **Section Menu**: Choose which section to edit, or finish/cancel
3. **Section Editor**: Interactive prompts to modify that section's values
4. **Repeat**: After each section edit, returns to the summary view
5. **Save Options**: When done, choose where to save (database, file, both, or preview)

### Navigation

- Use arrow keys or number keys to select menu options
- Press Enter to confirm selections
- Type values directly when prompted
- Use `@filepath` syntax to link to external files (e.g., `@./prompts/system.md`)
- Empty input typically means "keep current value"

## Agent Builder

### Sections

#### Basic Info
- **Name**: Agent identifier (required)
- **System Prompt**: Core instructions, either inline or `@path/to/file.md`
- **Persona**: Agent personality/character description
- **Instructions**: Additional operational guidelines

#### Model
- **Provider**: anthropic, openai, gemini, or ollama
- **Model Name**: Specific model (e.g., claude-sonnet-4-20250514, gpt-4o)
- **Temperature**: Response creativity (0.0-1.0)

#### Memory Blocks
Manage agent's memory:
- **Add**: Create new memory blocks
- **Edit**: Modify existing blocks
- **Remove**: Delete blocks
- **Edit as TOML**: View TOML representation (preview)

Block properties:
- **Label**: Identifier for the block (e.g., "human", "persona", "notes")
- **Content**: Text content or `@path` to file
- **Permission**: read_only, read_write, append, partner, human, admin
- **Type**: core (always in context), working (swappable), archival (searchable)
- **Shared**: Whether other agents can access this block

#### Tools & Rules
- **Tools**: Multi-select from available tools (block, recall, search, send_message, web, file, calculator, etc.)
- **Rules**: Add workflow rules for tool execution:
  - ContinueLoop: Tool continues conversation loop
  - ExitLoop: Tool ends conversation when called
  - StartConstraint: Tool must be called first
  - MaxCalls: Limit how many times tool can be called
  - Cooldown: Minimum time between calls
  - RequiresPreceding: Tool requires other tools to be called first

#### Context Options
- **Max Messages**: Limit messages in context window
- **Enable Thinking**: Turn on reasoning/thinking mode

#### Data Sources
Configure event-driven data inputs:
- **Bluesky**: Jetstream firehose subscriptions
- **Discord**: Channel monitoring
- **File**: File system watching
- **Custom**: Custom source configuration

#### Integrations
- **Bluesky Handle**: Link agent to a Bluesky identity

### Example Flow

```
╭──────────────────────────────────────╮
│         Agent: MyAssistant           │
├──────────────────────────────────────┤
│ ─ Basic Info ─                       │
│ Name         MyAssistant             │
│ System       (from file: prompt.md)  │
│ Persona      (none)                  │
│ Instructions (none)                  │
│                                      │
│ ─ Model ─                            │
│ Provider     anthropic               │
│ Model        claude-sonnet-4-20250514│
│ Temperature  (default)               │
│                                      │
│ ─ Memory Blocks (2) ─                │
│   • human [read_write] User prefere… │
│   • persona [read_only] I am a help… │
│                                      │
│ ─ Tools (4) ─                        │
│ block, recall, search, send_message  │
╰──────────────────────────────────────╯

? What would you like to change?
❯ Basic Info
  Model
  Memory Blocks
  Tools & Rules
  Context Options
  Data Sources
  Integrations
  Done - Save
  Cancel
```

## Group Builder

### Sections

#### Basic Info
- **Name**: Group identifier (required)
- **Description**: What this group does (required)

#### Coordination Pattern
- **round_robin**: Agents take turns in order
  - Skip unavailable: Whether to skip inactive agents
- **supervisor**: One agent leads and delegates
  - Leader: Which agent is the supervisor
- **pipeline**: Sequential processing through stages
  - Stages: Ordered list of agents
- **dynamic**: Context-based agent selection
  - Selector: random, capability, load_balancing
- **sleeptime**: Background monitoring
  - Check interval: Seconds between checks
  - Intervention agent: Who acts on triggers
  - Triggers: Conditions for intervention

#### Members
Manage group membership:
- **Add**: Add an agent to the group
- **Edit**: Change member role/capabilities
- **Remove**: Remove member from group

Member properties:
- **Name**: Agent name (must exist in database)
- **Role**: regular, supervisor, observer, or specialist (with domain)
- **Capabilities**: Tags for capability-based routing

#### Shared Memory
Memory blocks accessible to all group members:
- Same editing interface as agent memory blocks
- Content automatically shared with all members
- Permission controls per-block access level

#### Data Sources
Configure event sources for the group:
- Same options as agent data sources
- Events are routed through the coordination pattern

### Example Flow

```
╭──────────────────────────────────────╮
│         Group: Support Team          │
├──────────────────────────────────────┤
│ ─ Basic Info ─                       │
│ Name         Support Team            │
│ Description  Primary support group   │
│                                      │
│ ─ Pattern ─                          │
│ Type         Dynamic (selector: cap… │
│                                      │
│ ─ Members (3) ─                      │
│   • Pattern [supervisor]             │
│   • Entropy [specialist]             │
│   • Archive [regular]                │
│                                      │
│ ─ Shared Memory (1) ─                │
│   • context [read_write]             │
│                                      │
│ ─ Data Sources (1) ─                 │
│   • bluesky [bluesky]                │
╰──────────────────────────────────────╯

? What would you like to change?
❯ Basic Info
  Coordination Pattern
  Members
  Memory Blocks
  Integrations
  Done - Save
  Cancel
```

## Save Options

When you select "Done - Save", you'll choose where to save:

1. **Save to database**: Writes directly to pattern_db
2. **Export to file**: Saves as TOML file (prompts for filename)
3. **Both**: Database and file
4. **Preview**: Shows TOML representation without saving
5. **Cancel**: Returns to editing

## Tips

### File References
Use `@` prefix to link to external files:
```
System prompt: @./prompts/assistant.md
Persona: @./personas/helpful.md
```
This keeps large prompts maintainable and version-controlled.

### State Recovery
Builders auto-save state to `~/.cache/pattern/builder-state.toml` after each section edit. If the builder crashes, the next run can potentially recover.

### TOML Templates
Create templates by exporting existing configurations:
```bash
pattern agent export MyAgent -o template.toml
# Edit template.toml
pattern agent create --from template.toml
```

### Validation
Builders validate configuration before saving:
- Agent: Name and model are required
- Group: Name, description, and valid pattern configuration required
- Supervisor pattern: Leader must be specified
- Pipeline pattern: At least one stage required

## TOML Configuration Reference

### Agent TOML Structure

```toml
name = "MyAgent"
system_prompt = "You are a helpful assistant."
# Or use file reference:
# system_prompt_path = "./prompts/system.md"

persona = "I am friendly and concise."
instructions = "Always be helpful."

[model]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
temperature = 0.7

tools = ["block", "recall", "search", "send_message"]

[[tool_rules]]
tool_name = "send_message"
rule_type = "ExitLoop"
priority = 9

[memory.human]
content = "User prefers short responses."
permission = "read_write"
memory_type = "core"
shared = false

[memory.persona]
content_path = "./personas/assistant.md"
permission = "read_only"
memory_type = "core"

[context]
max_messages = 50
enable_thinking = true

[data_sources.bluesky_mentions]
type = "bluesky"
# ... bluesky-specific config
```

### Group TOML Structure

```toml
name = "Support Team"
description = "Primary support group"

[pattern]
type = "dynamic"
selector = "capability"

[[members]]
name = "Pattern"
role = "supervisor"
capabilities = ["coordination", "planning"]

[[members]]
name = "Entropy"
role = { specialist = { domain = "task_breakdown" } }
capabilities = ["analysis", "decomposition"]

[shared_memory.context]
content = "Shared context for the team."
permission = "read_write"

[data_sources.discord]
type = "discord"
# ... discord-specific config
```
