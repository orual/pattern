# Config system redesign

## Overview

Redesign the Pattern configuration system to establish clear ownership between TOML files and database, eliminate vestigial user primitives, and support proper multi-agent configuration.

## Goals

1. Agents consistently load whether from config or DB
2. Config goes into database canonically per agent and groups
3. Config overridable from TOML with explicit priority control
4. Memory block content is DB-authoritative; config metadata is TOML-updatable

## Changes

### TOML structure

Remove `[user]` block entirely. Change singular `[agent]` to plural `[[agents]]`. Database path specifies directory, not file.

```toml
[database]
path = "~/.local/share/pattern"  # directory containing both DBs

[model]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
temperature = 0.7

[[agents]]
name = "Pattern"
system_prompt = "You are Pattern, an ADHD support agent..."
# or: config_path = "agents/pattern.toml"  # load from external file

[agents.model]
provider = "gemini"
model = "gemini-2.5-flash"

[agents.memory.persona]
content = "I am Pattern..."
permission = "read_only"
memory_type = "core"
pinned = true

[agents.memory.working_notes]
permission = "read_write"
memory_type = "working"

[[agents]]
name = "Archivist"

[[groups]]
name = "main"
description = "Primary support constellation"
pattern = { type = "round_robin" }

[[groups.members]]
name = "Pattern"

[[groups.members]]
name = "TempHelper"
agent_config = { name = "TempHelper", system_prompt = "..." }

[groups.shared_memory.context]
permission = "read_write"
memory_type = "working"
```

### Config resolution

Resolution order: CLI flags -> TOML file -> Database -> Hardcoded defaults

CLI flag `--config-priority` controls merge behavior:
- `merge` (default): DB values win for content, TOML wins for config metadata
- `toml`: TOML overwrites everything except memory content
- `db`: Ignore TOML entirely for existing agents

### Load behavior

On `load_agent("Pattern")`:

1. Check if agent exists in DB
2. If exists AND TOML defines same agent name:
   - Merge mode: DB content preserved, TOML config metadata applied
   - TOML priority: TOML overwrites config, DB content preserved
   - DB priority: Ignore TOML entirely
3. If exists but no TOML definition: load from DB as-is
4. If not in DB but TOML defines it: create in DB from TOML (seed)
5. If neither: error

### Memory block merge rules

| Field | Source of truth | Merge behavior |
|-------|-----------------|----------------|
| `content` | DB | TOML only seeds new blocks |
| `permission` | TOML | Updated on reload if specified |
| `memory_type` | TOML | Updated on reload if specified |
| `description` | TOML | Updated on reload if specified |
| `pinned` | TOML | Updated on reload if specified |
| `char_limit` | TOML | Updated on reload if specified |
| `schema` | TOML | Updated on reload if specified |
| `shared` | TOML | Updated on reload if specified |

### Groups

Group definitions from TOML always update DB. Member list from TOML replaces DB member list. Shared memory follows same rules as agent memory.

Members can reference existing agents by name or define inline agent configs for group-specific agents.

## Code changes

### pattern_core/src/config.rs

- Remove `UserConfig` struct
- Remove `ensure_stable_user_id` function and related logic (~80 lines)
- Rename `PatternConfig.agent` to `PatternConfig.agents: Vec<AgentConfigRef>`
- Add `AgentConfigRef` enum for inline vs file reference:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentConfigRef {
    Inline(AgentConfig),
    Path { config_path: PathBuf },
}
```
- Change `DatabaseConfig.path` to directory, add helpers:

```rust
impl DatabaseConfig {
    pub fn constellation_db(&self) -> PathBuf {
        self.path.join("constellation.db")
    }
    pub fn auth_db(&self) -> PathBuf {
        self.path.join("auth.db")
    }
}
```

- Add `ConfigPriority` enum:

```rust
pub enum ConfigPriority {
    Merge,
    TomlWins,
    DbWins,
}
```

- Expand `MemoryBlockConfig`:

```rust
pub struct MemoryBlockConfig {
    // Content (TOML seeds, DB authoritative)
    pub content: Option<String>,
    pub content_path: Option<PathBuf>,

    // Config (TOML can update on reload)
    pub permission: MemoryPermission,
    pub memory_type: MemoryType,
    pub description: Option<String>,
    pub pinned: Option<bool>,
    pub char_limit: Option<usize>,
    pub schema: Option<BlockSchema>,

    // Sharing
    pub id: Option<MemoryId>,
    pub shared: bool,
}
```

### pattern_core/src/runtime/context.rs

- Update `resolve_config` to take `ConfigPriority` parameter
- Add memory block config-vs-content merge logic
- Update `create_agent` to handle "agent exists" case with merge

### pattern_db/src/queries/memory.rs

Add query for updating config without touching content:

```rust
pub async fn update_block_config(
    pool: &SqlitePool,
    block_id: i64,
    permission: Option<MemoryPermission>,
    block_type: Option<MemoryBlockType>,
    description: Option<&str>,
    pinned: Option<bool>,
    char_limit: Option<usize>,
) -> Result<()>
```

### pattern_cli/src/commands/

- Add `--config-priority` flag to relevant commands
- Update config loading to use new `agents` field
- Remove any `user` references
- Add `pattern config migrate` command

## Testing

Test module at `pattern_core/src/config/tests.rs`:

```rust
// Basic loading
#[test] fn test_load_agents_plural()
#[test] fn test_database_path_is_directory()

// Merge behavior
#[tokio::test] async fn test_new_agent_seeds_from_toml()
#[tokio::test] async fn test_existing_agent_keeps_db_content()
#[tokio::test] async fn test_existing_agent_updates_config_from_toml()
#[tokio::test] async fn test_toml_priority_overwrites_db_config()
#[tokio::test] async fn test_db_priority_ignores_toml()

// Memory block specific
#[tokio::test] async fn test_memory_content_not_overwritten_on_reload()
#[tokio::test] async fn test_memory_permission_updated_on_reload()
#[tokio::test] async fn test_memory_pinned_updated_on_reload()
#[tokio::test] async fn test_new_memory_block_added_on_reload()

// Groups
#[tokio::test] async fn test_group_member_by_reference()
#[tokio::test] async fn test_group_member_inline_config()
#[tokio::test] async fn test_group_shared_memory_merge()

// Migration
#[test] fn test_old_singular_agent_field_errors_helpfully()
#[test] fn test_missing_user_block_ok()
```

## Migration

### Detection and errors

```rust
if parsed.get("agent").is_some() && parsed.get("agents").is_none() {
    return Err(CoreError::ConfigurationError {
        field: "agent".into(),
        expected: "[[agents]] (plural)".into(),
        cause: ConfigError::Deprecated(
            "Singular [agent] is deprecated. Use [[agents]].\n\
             Run: pattern config migrate"
        ),
    });
}

if parsed.get("user").is_some() {
    tracing::warn!("[user] block is deprecated and ignored");
}
```

### Migration command

```bash
pattern config migrate [--in-place] [path]
```

Transforms singular `[agent]` to `[[agents]]` and removes `[user]` block.

### Timeline

1. This release: Error on `[agent]`, warn on `[user]`, provide migration command
2. No silent fallback - explicit errors preferred over magic
