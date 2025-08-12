# Pattern Configuration System Design

## Overview
Configuration system for Pattern that allows persisting agent settings, user preferences, and system configuration across sessions.

## Goals
- Persist agent identity and configuration across sessions
- User-friendly TOML format for manual editing
- Layered configuration (defaults → file → CLI args)
- Shared implementation between CLI and other consumers
- Secure handling of sensitive data (API keys stay in env vars)

## Structure

### Core Config Types (in pattern-core)

```rust
// Top-level configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternConfig {
    pub user: UserConfig,
    pub agent: AgentConfig,
    pub model: ModelConfig,
    pub database: DatabaseConfig,
}

// User configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    pub id: UserId,         // Persisted across sessions
    pub name: Option<String>,
}

// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: Option<AgentId>,  // Persisted once created
    pub name: String,
    pub persona: String,
    pub instructions: Option<String>,
    pub memory: HashMap<String, MemoryBlockConfig>,
}

// Memory block configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockConfig {
    pub content: String,
    pub permission: MemoryPermission,
}

// Model provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,  // "anthropic", "openai", etc.
    // API keys come from environment variables
    // Optional model-specific settings
    pub model: Option<String>,
    pub temperature: Option<f32>,
}

// Database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: Option<PathBuf>,  // None = in-memory
    pub namespace: Option<String>,
}
```

### Config Utilities (in pattern-core)

```rust
// Load config from TOML file
pub fn load_config(path: &Path) -> Result<PatternConfig>;

// Save config to TOML file
pub fn save_config(config: &PatternConfig, path: &Path) -> Result<()>;

// Merge configs with precedence
pub fn merge_configs(base: PatternConfig, overlay: PartialConfig) -> PatternConfig;

// All configs implement Default
impl Default for PatternConfig { ... }
```

### CLI Usage

1. **Config file locations** (checked in order):
   - CLI argument: `--config path/to/config.toml`
   - Project-specific: `./pattern.toml`
   - User default: `~/.config/pattern/config.toml`

2. **Config layering**:
   - Start with defaults
   - Apply config file settings
   - Override with CLI arguments

3. **First run experience**:
   - Generate user_id if not present
   - Create default config
   - Save to user config directory

## Example TOML File

```toml
[user]
id = "user_123e4567-e89b-12d3-a456-426614174000"
name = "Alice"

[agent]
id = "agent_456e7890-f12c-34d5-b678-537825291011"  # Generated on first run
name = "Assistant"
persona = """
You are a helpful AI assistant focused on software development.
You provide clear, concise answers and working code examples.
"""
instructions = "Always explain your reasoning step by step."

[agent.memory.human]
content = "The user prefers Rust and functional programming patterns."
permission = "read_write"

[agent.memory.preferences]
content = "Use snake_case for variables, avoid unwrap() in production code."
permission = "read_only"

[model]
provider = "anthropic"
model = "claude-3-7-sonnet-latest"
temperature = 0.7

[database]
path = "./data/pattern.db"
namespace = "dev"
```

## Implementation Steps

1. Create config module in pattern-core
2. Add serde derives to existing types that need serialization
3. Implement config loading/saving with TOML
4. Add config support to CLI with proper layering
5. Update CLI to persist agent_id between sessions
6. Add config validation and migration support

## Security Considerations

- API keys always come from environment variables, never stored in config
- Config files should have appropriate permissions (user-readable only)
- Sensitive data in memory blocks should be documented as such

## Future Extensions

- Config profiles (dev/prod/test)
- Config validation schema
- Migration between config versions
- Encryption for sensitive memory blocks
