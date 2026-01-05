# CLAUDE.md - Pattern CLI

> **CRITICAL WARNING**: DO NOT run ANY CLI commands during development!
> Production agents are running. Any CLI invocation will disrupt active agents.
> Testing must be done offline after stopping production agents.

Command-line interface for the Pattern ADHD support system. Binary output: `pattern`.

## CLI Command Reference

### Chat Commands

```bash
# Single agent chat (default agent: Pattern)
pattern chat
pattern chat --agent MyAgent

# Group chat
pattern chat --group main

# Discord mode (requires group)
pattern chat --group main --discord
```

### Agent Commands

```bash
# List all agents
pattern agent list

# Show agent details
pattern agent status <name>

# Create new agent (interactive TUI builder)
pattern agent create
pattern agent create --from config.toml

# Edit existing agent (interactive TUI builder)
pattern agent edit <name>

# Export agent to TOML
pattern agent export <name>
pattern agent export <name> -o output.toml

# Add configuration
pattern agent add source <agent> <source-name> -t bluesky
pattern agent add memory <agent> <label> --content "text" -t core
pattern agent add tool <agent> <tool-name>
pattern agent add rule <agent> <tool> <rule-type>

# Remove configuration
pattern agent remove source <agent> <source-name>
pattern agent remove memory <agent> <label>
pattern agent remove tool <agent> <tool-name>
pattern agent remove rule <agent> <tool>
```

### Group Commands

```bash
# List all groups
pattern group list

# Show group details and members
pattern group status <name>

# Create new group (interactive TUI builder)
pattern group create
pattern group create --from config.toml

# Edit existing group (interactive TUI builder)
pattern group edit <name>

# Export group to TOML
pattern group export <name>
pattern group export <name> -o output.toml

# Add configuration
pattern group add member <group> <agent> --role regular
pattern group add memory <group> <label> --content "text"
pattern group add source <group> <source-name> -t discord

# Remove configuration
pattern group remove member <group> <agent>
pattern group remove memory <group> <label>
pattern group remove source <group> <source-name>
```

### Export/Import Commands

```bash
# Export to CAR format
pattern export agent <name>
pattern export agent <name> -o agent.car
pattern export group <name>
pattern export constellation

# Import from CAR
pattern import car agent.car
pattern import car agent.car --rename-to NewName

# Convert Letta/MemGPT format
pattern import letta agent.af
```

### Debug Commands

```bash
# Memory inspection
pattern debug list-core <agent>
pattern debug list-archival <agent>
pattern debug list-all-memory <agent>
pattern debug edit-memory <agent> <label>
pattern debug modify-memory <agent> <label> --new-label <name>

# Search operations
pattern debug search-archival --agent <name> "query"
pattern debug search-conversations <agent> --query "text"

# Context inspection
pattern debug show-context <agent>
pattern debug context-cleanup <agent> --dry-run
```

### ATProto/Bluesky Commands

```bash
# Authentication
pattern atproto login <handle> -p <app-password>
pattern atproto oauth <handle>
pattern atproto status
pattern atproto unlink <handle>
pattern atproto test
```

### Configuration Commands

```bash
pattern config show
pattern config save pattern.toml

pattern db stats
```

## Interactive TUI Builders

The CLI includes interactive builders for creating and editing agents and groups:

### Agent Builder (`pattern agent create` / `pattern agent edit`)

Sections:
- **Basic Info**: Name, system prompt (inline or file path), persona, instructions
- **Model**: Provider (anthropic/openai/gemini/ollama), model name, temperature
- **Memory Blocks**: Add/edit/remove memory blocks with permissions and types
- **Tools & Rules**: Enable tools from registry, add workflow rules
- **Context Options**: Max messages, compression strategy, thinking mode
- **Data Sources**: Configure Bluesky, Discord, file, or custom sources
- **Integrations**: Bluesky handle linking

### Group Builder (`pattern group create` / `pattern group edit`)

Sections:
- **Basic Info**: Name, description
- **Coordination Pattern**: round_robin, supervisor, pipeline, dynamic, sleeptime
- **Members**: Add agents with roles (regular, supervisor, observer, specialist)
- **Shared Memory**: Memory blocks accessible to all group members
- **Data Sources**: Event sources for the group

Both builders:
- Display a live configuration summary
- Support loading from TOML files (`--from`)
- Auto-save state to cache for recovery
- Offer save destinations: database, file, both, or preview

## Architecture

### Command Structure
```rust
#[derive(Subcommand)]
enum Commands {
    Chat { agent, group, discord },
    Agent { cmd: AgentCommands },
    Group { cmd: GroupCommands },
    Debug { cmd: DebugCommands },
    Export { cmd: ExportCommands },
    Import { cmd: ImportCommands },
    Atproto { cmd: AtprotoCommands },
    Config { cmd: ConfigCommands },
    Db { cmd: DbCommands },
}
```

### Chat Mode Features
- Interactive terminal UI with `ratatui`
- Typing indicators during agent processing
- Memory block visibility in context
- Tool call display with results
- Discord integration via `--discord` flag

### Output System
- Colored terminal output with `owo_colors`
- Progress bars via `indicatif`
- Tables via `comfy-table`
- Markdown rendering via `termimad`

### Sender Labels (CLI display)
Based on message origin:
- Agent: agent name
- Bluesky: `@handle`
- Discord: `Discord`
- DataSource: `source_id`
- CLI: `CLI`
- API: `API`
- Unknown: `Runtime`

## Implementation Notes

- `clap` for command parsing with derive macros
- `tokio` async runtime
- `dialoguer` for interactive prompts in builders
- `rustyline-async` for readline in chat mode
- Direct database access via `pattern_db` through `RuntimeContext`
