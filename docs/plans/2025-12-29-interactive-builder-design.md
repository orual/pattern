# Interactive Agent & Group Builder CLI

Design for smart interactive configuration of agents and groups via the command line.

## Overview

A unified builder system for creating and editing agents and groups interactively. Supports multiple workflows:
- Start from defaults, build interactively, save to DB and/or file
- Load existing agent/group from DB, modify, update in place
- Import from TOML file, customize, save anywhere

## Command Structure

### AgentCommands

```
List                        # List all agents
Status <name>               # Show agent details
Create [--from <path>]      # Interactive builder, new agent
Edit <name>                 # Interactive builder, update existing
Export <name> [path]        # Export config to TOML file
Add <subcommand>            # Quick additions without full builder
  - Source <name> <source>
  - Memory <name> <label> [--content | --path]
  - Tool <name> <tool>
  - Rule <name> <tool> <rule-type> [options]
Remove <subcommand>         # Quick removals
  - Source <name> <source>
  - Memory <name> <label>
  - Tool <name> <tool>
  - Rule <name> <tool> [rule-type]
```

### GroupCommands

```
List                        # List all groups
Status <name>               # Show group details
Create [--from <path>]      # Interactive builder, new group
Edit <name>                 # Interactive builder, update existing
Export <name> [path]        # Export config to TOML file
Add <subcommand>
  - Member <group> <agent> [--role] [--capabilities]
  - Memory <group> <label> [--content | --path]  # shared memory
  - Source <group> <source>                       # data source
Remove <subcommand>
  - Member <group> <agent>
  - Memory <group> <label>
  - Source <group> <source>
```

## Interactive Builder Flow

### Entry Points

- `agent create` → Start with `AgentConfig::default()`
- `agent create --from ./agent.toml` → Load from file
- `agent edit myagent` → Load from database by name

### Main Loop

```
1. Load/initialize config
2. Display sectioned summary
3. Menu: "What would you like to change?"
   - Basic Info (name, system_prompt, persona, instructions)
   - Model (provider, model name, temperature)
   - Memory Blocks
   - Tools & Rules
   - Context Options
   - Integrations (bluesky_handle)
   - [Done - Save]
   - [Cancel]
4. User selects section → enters section editor
5. After edit, write state to ~/.cache/pattern/builder-state.toml
6. Returns to step 2
7. On "Done": Save menu (Database / File / Both / Preview)
```

### Group Builder Sections

- Basic Info (name, description)
- Pattern (supervisor/round-robin/pipeline/dynamic/sleeptime)
- Members
- Shared Memory
- Data Sources
- [Done - Save]
- [Cancel]

## Summary Display

Sectioned display with truncation for long values:

```
╭─ Agent: MyAssistant ─────────────────────────────────────╮
│                                                          │
│ Basic Info                                               │
│   Name:         MyAssistant                              │
│   System:       You are a helpful AI assistant that...   │
│   Persona:      (from file: ./persona.md)                │
│   Instructions: (none)                                   │
│                                                          │
│ Model                                                    │
│   Provider:     anthropic                                │
│   Model:        claude-sonnet-4-20250514                 │
│   Temperature:  (default)                                │
│                                                          │
│ Memory Blocks (2)                                        │
│   • core_memory    [read_write] "User preferences..."    │
│   • task_context   [read_only]  "Current project..."     │
│                                                          │
│ Tools (5)                                                │
│   block, block_edit, recall, search, send_message        │
│                                                          │
│ Tool Rules (1)                                           │
│   • send_message: exit_loop                              │
│                                                          │
│ Context                                                  │
│   Max messages: 100, Compression: sliding_window         │
│                                                          │
│ Integrations                                             │
│   Bluesky: @mybot.bsky.social                            │
│                                                          │
╰──────────────────────────────────────────────────────────╯
```

- Long text truncated with "..."
- File paths shown as "(from file: path)"
- Empty/default fields shown as "(none)" or "(default)"

## Section Editors

### Simple Text Fields

```
Current value: You are a helpful AI assistant...
New value (@ to link file, empty to keep current):
```

### Enum Fields

Arrow-key select via dialoguer:

```
Model provider:
> anthropic
  openai
  gemini
  ollama
```

### Tools List

Multi-select with custom option:

```
Select tools (space to toggle, enter to confirm):
  [x] block
  [x] block_edit
  [x] recall
  [x] search
  [x] send_message
  [ ] source
  [ ] web
  [ ] calculator
  [ ] file
  ---
  [ ] + Add custom tool...
```

### Collections (Memory Blocks, Tool Rules, Members)

List with CRUD + TOML escape hatch:

```
Memory Blocks:
  1. core_memory [read_write]
  2. task_context [read_only]

  [Add new]
  [Edit as TOML]
  [Done]
```

Select item to edit/remove, or add new with guided prompts.

## Save Flow

### Save Menu

```
Where to save?
> Save to database
  Export to file
  Both
  Preview (show TOML, don't save)
  Cancel
```

### Behavior

- **Database save (Create):** Generates new ID, inserts agent/group
- **Database save (Edit):** Updates existing by ID
- **File export:** Prompts for path, writes full TOML, preserves file paths
- **Preview:** Pretty-prints TOML, returns to save menu

### Error Handling

- Validation before save (name required, model required)
- On DB error: show error, offer to export to file instead
- On file error: show error, return to save menu

## Implementation Structure

### File Organization

```
src/commands/
  agent.rs          # AgentCommands enum, dispatch to handlers
  group.rs          # GroupCommands enum, dispatch to handlers
  builder/
    mod.rs          # Shared builder infrastructure
    agent.rs        # Agent-specific builder logic
    group.rs        # Group-specific builder logic
    display.rs      # Summary rendering (boxed display)
    editors.rs      # Section editors (text, enum, collections)
    save.rs         # Save flow (DB, file, preview)
```

### Key Types

```rust
/// Builder state machine
struct ConfigBuilder<T> {
    config: T,
    source: ConfigSource,
    modified: bool,
}

enum ConfigSource {
    New,
    FromFile(PathBuf),
    FromDb(String),  // agent/group name
}

/// Trait for buildable configs
trait InteractiveBuilder: Sized {
    fn sections() -> Vec<Section>;
    fn display_summary(&self) -> String;
    fn edit_section(&mut self, section: &Section) -> Result<()>;
    fn validate(&self) -> Result<()>;
}
```

## Edge Cases

### Name Conflicts

- On Create: Check if name exists, prompt to choose different or switch to Edit
- On Edit: Validate name changes against existing agents/groups

### Interrupted Sessions

- No auto-save/draft system
- State written to `~/.cache/pattern/builder-state.toml` after each edit
- User can recover with `--from` flag

### Large Configs

- TOML editor escape hatch for complex tool rules
- File path approach for long system prompts

### Group Member References

- Members reference agents by name
- Validate agent exists when adding
- Consider orphan check on agent delete

### Validation

- Validate on save, not per-field
- Show all errors at once, return to builder

## Dependencies

- `dialoguer` - Already in deps, used for interactive prompts
- `owo_colors` - Already in deps, used for colored output
- Pattern's existing TOML parsing via `pattern_core::config`
