# Config System Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Redesign the configuration system to establish clear TOML/DB ownership, remove vestigial user primitives, and support proper multi-agent config with merge semantics.

**Architecture:** TOML defines agent structure and config metadata. DB owns content. Merge strategy by default with CLI override for primacy. `[[agents]]` replaces singular `[agent]`, `[user]` block removed, database path is directory not file.

**Tech Stack:** Rust, TOML (serde), SQLx, pattern_core, pattern_cli, pattern_db

**Reference:** Design doc at `docs/plans/2026-01-04-config-system-redesign.md`

---

## Task 1: Add ConfigPriority enum and update DatabaseConfig

**Files:**
- Modify: `crates/pattern_core/src/config.rs`
- Test: `crates/pattern_core/src/config.rs` (inline tests)

**Step 1: Write failing tests for ConfigPriority and DatabaseConfig helpers**

Add to `config.rs` in the test module:

```rust
#[test]
fn test_database_config_directory_helpers() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = DatabaseConfig {
        path: temp_dir.path().to_path_buf(),
    };
    assert_eq!(config.constellation_db(), temp_dir.path().join("constellation.db"));
    assert_eq!(config.auth_db(), temp_dir.path().join("auth.db"));
}

#[test]
fn test_config_priority_default() {
    assert_eq!(ConfigPriority::default(), ConfigPriority::Merge);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p pattern-core test_database_config_directory_helpers test_config_priority_default`
Expected: FAIL - methods/types don't exist

**Step 3: Implement ConfigPriority and DatabaseConfig helpers**

Add near top of `config.rs`:

```rust
/// Controls how TOML config and DB config are merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigPriority {
    /// DB values win for content, TOML wins for config metadata.
    #[default]
    Merge,
    /// TOML overwrites everything except memory content.
    TomlWins,
    /// Ignore TOML entirely for existing agents.
    DbWins,
}
```

Update `DatabaseConfig` impl:

```rust
impl DatabaseConfig {
    /// Path to the constellation database file.
    pub fn constellation_db(&self) -> PathBuf {
        self.path.join("constellation.db")
    }

    /// Path to the auth database file.
    pub fn auth_db(&self) -> PathBuf {
        self.path.join("auth.db")
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p pattern-core test_database_config_directory_helpers test_config_priority_default`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/config.rs
git commit -m "[pattern-core] add ConfigPriority enum and DatabaseConfig directory helpers"
```

---

## Task 2: Add AgentConfigRef for inline vs file reference

**Files:**
- Modify: `crates/pattern_core/src/config.rs`
- Test: `crates/pattern_core/src/config.rs` (inline tests)

**Step 1: Write failing tests for AgentConfigRef**

```rust
#[test]
fn test_agent_config_ref_inline_deserialize() {
    let toml = r#"
        name = "TestAgent"
        system_prompt = "Hello"
    "#;
    let parsed: AgentConfigRef = toml::from_str(toml).unwrap();
    match parsed {
        AgentConfigRef::Inline(config) => {
            assert_eq!(config.name, "TestAgent");
        }
        _ => panic!("Expected Inline variant"),
    }
}

#[test]
fn test_agent_config_ref_path_deserialize() {
    let toml = r#"
        config_path = "agents/pattern.toml"
    "#;
    let parsed: AgentConfigRef = toml::from_str(toml).unwrap();
    match parsed {
        AgentConfigRef::Path { config_path } => {
            assert_eq!(config_path, PathBuf::from("agents/pattern.toml"));
        }
        _ => panic!("Expected Path variant"),
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p pattern-core test_agent_config_ref`
Expected: FAIL - type doesn't exist

**Step 3: Implement AgentConfigRef**

```rust
/// Reference to an agent config - either inline or from a file path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentConfigRef {
    /// Load config from an external file.
    Path {
        /// Path to the agent config TOML file.
        config_path: PathBuf,
    },
    /// Inline agent configuration.
    Inline(AgentConfig),
}

impl AgentConfigRef {
    /// Resolve to an AgentConfig, loading from file if needed.
    pub async fn resolve(&self, base_dir: &Path) -> Result<AgentConfig, ConfigError> {
        match self {
            AgentConfigRef::Inline(config) => Ok(config.clone()),
            AgentConfigRef::Path { config_path } => {
                let path = resolve_path(base_dir, config_path);
                AgentConfig::load_from_file(&path).await
            }
        }
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p pattern-core test_agent_config_ref`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/config.rs
git commit -m "[pattern-core] add AgentConfigRef for inline vs file-path agent configs"
```

---

## Task 3: Change PatternConfig to use plural agents

**Files:**
- Modify: `crates/pattern_core/src/config.rs`
- Test: `crates/pattern_core/src/config.rs` (inline tests)

**Step 1: Write failing tests for plural agents**

```rust
#[test]
fn test_pattern_config_plural_agents() {
    let temp_dir = tempfile::tempdir().unwrap();
    let toml = format!(r#"
        [database]
        path = "{}"

        [[agents]]
        name = "Agent1"

        [[agents]]
        name = "Agent2"
    "#, temp_dir.path().display());
    let config: PatternConfig = toml::from_str(&toml).unwrap();
    assert_eq!(config.agents.len(), 2);
}

#[test]
fn test_pattern_config_agent_config_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let toml = format!(r#"
        [database]
        path = "{}"

        [[agents]]
        config_path = "agents/pattern.toml"
    "#, temp_dir.path().display());
    let config: PatternConfig = toml::from_str(&toml).unwrap();
    assert_eq!(config.agents.len(), 1);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p pattern-core test_pattern_config_plural test_pattern_config_agent_config_path`
Expected: FAIL - `agents` field doesn't exist

**Step 3: Update PatternConfig struct**

Replace the singular `agent` field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternConfig {
    /// Database configuration (path is directory containing both DBs).
    #[serde(default)]
    pub database: DatabaseConfig,

    /// Global model defaults.
    #[serde(default)]
    pub model: ModelConfig,

    /// Agent configurations (inline or file references).
    #[serde(default)]
    pub agents: Vec<AgentConfigRef>,

    /// Group configurations.
    #[serde(default)]
    pub groups: Vec<GroupConfig>,

    /// Bluesky configuration.
    #[serde(default)]
    pub bluesky: Option<BlueskyConfig>,

    /// Discord configuration.
    #[serde(default)]
    pub discord: Option<DiscordAppConfig>,
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p pattern-core test_pattern_config_plural test_pattern_config_agent_config_path`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/config.rs
git commit -m "[pattern-core] change PatternConfig.agent to agents: Vec<AgentConfigRef>"
```

---

## Task 4: Remove UserConfig and related code

**Files:**
- Modify: `crates/pattern_core/src/config.rs`

**Step 1: Locate and remove UserConfig**

Search for and remove:
- `UserConfig` struct
- `ensure_stable_user_id()` function
- `stable_user_id_path()` function
- Any `user` field in `PatternConfig`
- Related imports and uses

**Step 2: Run cargo check to find compilation errors**

Run: `cargo check -p pattern-core`

Fix any remaining references to removed types.

**Step 3: Run tests to verify nothing breaks**

Run: `cargo nextest run -p pattern-core`
Expected: PASS (or some tests need updating if they used UserConfig)

**Step 4: Commit**

```bash
git add crates/pattern_core/src/config.rs
git commit -m "[pattern-core] remove vestigial UserConfig and home-directory ID dance"
```

---

## Task 5: Expand MemoryBlockConfig with new fields

**Files:**
- Modify: `crates/pattern_core/src/config.rs`
- Test: `crates/pattern_core/src/config.rs` (inline tests)

**Step 1: Write failing tests for new memory config fields**

```rust
#[test]
fn test_memory_block_config_new_fields() {
    let toml = r#"
        content = "Test content"
        permission = "read_write"
        memory_type = "core"
        pinned = true
        char_limit = 4096
    "#;
    let config: MemoryBlockConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.pinned, Some(true));
    assert_eq!(config.char_limit, Some(4096));
}

#[test]
fn test_memory_block_config_defaults() {
    let toml = r#"
        permission = "read_write"
        memory_type = "working"
    "#;
    let config: MemoryBlockConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.pinned, None);
    assert_eq!(config.char_limit, None);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p pattern-core test_memory_block_config_new_fields test_memory_block_config_defaults`
Expected: FAIL - fields don't exist

**Step 3: Add new fields to MemoryBlockConfig**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockConfig {
    /// Inline content for the memory block.
    #[serde(default)]
    pub content: Option<String>,

    /// Path to load content from (alternative to inline).
    #[serde(default)]
    pub content_path: Option<PathBuf>,

    /// Permission level for the block.
    #[serde(default)]
    pub permission: MemoryPermission,

    /// Type of memory block.
    #[serde(default)]
    pub memory_type: MemoryType,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Whether block is always loaded into context.
    #[serde(default)]
    pub pinned: Option<bool>,

    /// Maximum content size in characters.
    #[serde(default)]
    pub char_limit: Option<usize>,

    /// Schema for structured content.
    #[serde(default)]
    pub schema: Option<BlockSchema>,

    /// ID for shared blocks.
    #[serde(default)]
    pub id: Option<MemoryId>,

    /// Whether to share with other agents.
    #[serde(default)]
    pub shared: bool,
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p pattern-core test_memory_block_config_new_fields test_memory_block_config_defaults`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/config.rs
git commit -m "[pattern-core] add pinned, char_limit, schema fields to MemoryBlockConfig"
```

---

## Task 6: Add update_block_config query to pattern_db

**Files:**
- Modify: `crates/pattern_db/src/queries/memory.rs`
- Test: `crates/pattern_db/src/queries/memory.rs` (or integration test)

**Step 1: Write failing test for update_block_config**

```rust
#[tokio::test]
async fn test_update_block_config() {
    let pool = test_pool().await;

    // Create a block first
    let block_id = create_test_block(&pool, "test-agent", "test-block").await;

    // Update just the config fields
    update_block_config(
        &pool,
        block_id,
        Some(MemoryPermission::ReadOnly),
        Some(MemoryBlockType::Core),
        Some("Updated description"),
        Some(true), // pinned
        Some(8192), // char_limit
    ).await.unwrap();

    // Verify the update
    let block = get_block(&pool, block_id).await.unwrap().unwrap();
    assert_eq!(block.permission, MemoryPermission::ReadOnly);
    assert_eq!(block.block_type, MemoryBlockType::Core);
    assert_eq!(block.description, "Updated description");
    assert!(block.pinned);
    assert_eq!(block.char_limit, 8192);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo nextest run -p pattern-db test_update_block_config`
Expected: FAIL - function doesn't exist

**Step 3: Implement update_block_config**

```rust
/// Update config metadata for a memory block without touching content.
pub async fn update_block_config(
    pool: &SqlitePool,
    block_id: i64,
    permission: Option<MemoryPermission>,
    block_type: Option<MemoryBlockType>,
    description: Option<&str>,
    pinned: Option<bool>,
    char_limit: Option<usize>,
) -> Result<(), sqlx::Error> {
    let mut query = String::from("UPDATE memory_blocks SET updated_at = datetime('now')");

    if permission.is_some() {
        query.push_str(", permission = ?");
    }
    if block_type.is_some() {
        query.push_str(", block_type = ?");
    }
    if description.is_some() {
        query.push_str(", description = ?");
    }
    if pinned.is_some() {
        query.push_str(", pinned = ?");
    }
    if char_limit.is_some() {
        query.push_str(", char_limit = ?");
    }

    query.push_str(" WHERE id = ?");

    let mut q = sqlx::query(&query);

    if let Some(p) = permission {
        q = q.bind(p.to_string());
    }
    if let Some(t) = block_type {
        q = q.bind(t.to_string());
    }
    if let Some(d) = description {
        q = q.bind(d);
    }
    if let Some(p) = pinned {
        q = q.bind(p);
    }
    if let Some(c) = char_limit {
        q = q.bind(c as i64);
    }
    q = q.bind(block_id);

    q.execute(pool).await?;
    Ok(())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo nextest run -p pattern-db test_update_block_config`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_db/src/queries/memory.rs
git commit -m "[pattern-db] add update_block_config query for config-only updates"
```

---

## Task 7: Update RuntimeContext resolve_config with merge logic

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`
- Test: Integration tests

**Step 1: Write failing tests for merge behavior**

Create or update `crates/pattern_core/tests/config_merge.rs`:

```rust
#[tokio::test]
async fn test_existing_agent_keeps_db_content() {
    let dbs = test_dbs().await;
    let ctx = RuntimeContext::builder().dbs(dbs).build().await.unwrap();

    // Create agent with initial content
    let config = AgentConfig {
        name: "TestAgent".into(),
        memory: hashmap! {
            "persona".into() => MemoryBlockConfig {
                content: Some("original content".into()),
                ..Default::default()
            }
        },
        ..Default::default()
    };
    ctx.create_agent(&config).await.unwrap();

    // Reload with different TOML content
    let new_config = AgentConfig {
        name: "TestAgent".into(),
        memory: hashmap! {
            "persona".into() => MemoryBlockConfig {
                content: Some("OVERWRITE ATTEMPT".into()),
                permission: MemoryPermission::ReadOnly,
                ..Default::default()
            }
        },
        ..Default::default()
    };

    let agent = ctx.load_agent_with_config("agent_id", &new_config, ConfigPriority::Merge).await.unwrap();

    // Content should be original, but permission should be updated
    let block = agent.runtime().memory().get_block("agent_id", "persona").await.unwrap().unwrap();
    assert_eq!(block.text(), "original content");
    // Permission should be updated from TOML
}

#[tokio::test]
async fn test_memory_permission_updated_on_reload() {
    // Similar test focusing on permission updates
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p pattern-core test_existing_agent_keeps_db_content`
Expected: FAIL - method doesn't exist or doesn't have merge logic

**Step 3: Implement merge logic in resolve_config**

Update `RuntimeContext::resolve_config` to accept `ConfigPriority`:

```rust
pub async fn resolve_config(
    &self,
    agent_id: &str,
    toml_config: Option<&AgentConfig>,
    priority: ConfigPriority,
) -> Result<ResolvedAgentConfig, CoreError> {
    // Load from DB if exists
    let db_agent = self.load_agent_from_db(agent_id).await?;

    match (db_agent, toml_config, priority) {
        // DB exists, TOML provided, merge mode
        (Some(db), Some(toml), ConfigPriority::Merge) => {
            self.merge_configs(db, toml).await
        }
        // DB exists, TOML provided, TOML wins
        (Some(db), Some(toml), ConfigPriority::TomlWins) => {
            self.toml_wins_merge(db, toml).await
        }
        // DB exists, TOML provided, DB wins
        (Some(db), Some(_), ConfigPriority::DbWins) => {
            Ok(ResolvedAgentConfig::from(db))
        }
        // DB exists, no TOML
        (Some(db), None, _) => {
            Ok(ResolvedAgentConfig::from(db))
        }
        // No DB, TOML provided (seed new agent)
        (None, Some(toml), _) => {
            self.create_agent(toml).await?;
            self.load_agent(agent_id).await
        }
        // Neither exists
        (None, None, _) => {
            Err(CoreError::AgentNotFound(agent_id.into()))
        }
    }
}

async fn merge_configs(
    &self,
    db_agent: Agent,
    toml_config: &AgentConfig,
) -> Result<ResolvedAgentConfig, CoreError> {
    // For each memory block in TOML:
    // - Update config metadata (permission, type, pinned, etc.)
    // - Do NOT update content
    for (label, block_config) in &toml_config.memory {
        if let Some(block_id) = self.get_block_id(&db_agent.id, label).await? {
            update_block_config(
                self.dbs().constellation.pool(),
                block_id,
                Some(block_config.permission),
                Some(block_config.memory_type.into()),
                block_config.description.as_deref(),
                block_config.pinned,
                block_config.char_limit,
            ).await?;
        } else {
            // Block doesn't exist in DB, create it with TOML content
            self.create_memory_block(&db_agent.id, label, block_config).await?;
        }
    }

    // Update agent-level config from TOML
    // (tools, rules, model, etc.)

    self.load_agent(&db_agent.id).await
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p pattern-core test_existing_agent_keeps_db_content test_memory_permission_updated_on_reload`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/runtime/context.rs
git commit -m "[pattern-core] implement config merge logic with ConfigPriority"
```

---

## Task 8: Add --config-priority CLI flag

**Files:**
- Modify: `crates/pattern_cli/src/commands/agent.rs` (or relevant command files)
- Modify: `crates/pattern_cli/src/main.rs` (if needed)

**Step 1: Add ConfigPriority to CLI args**

```rust
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum ConfigPriorityArg {
    #[default]
    Merge,
    Toml,
    Db,
}

impl From<ConfigPriorityArg> for ConfigPriority {
    fn from(arg: ConfigPriorityArg) -> Self {
        match arg {
            ConfigPriorityArg::Merge => ConfigPriority::Merge,
            ConfigPriorityArg::Toml => ConfigPriority::TomlWins,
            ConfigPriorityArg::Db => ConfigPriority::DbWins,
        }
    }
}
```

Add to relevant command structs:

```rust
#[derive(Parser)]
pub struct LoadAgentArgs {
    /// Agent name or ID
    pub agent: String,

    /// Config priority when TOML and DB conflict
    #[arg(long, default_value = "merge")]
    pub config_priority: ConfigPriorityArg,
}
```

**Step 2: Wire up to RuntimeContext calls**

Update command handlers to pass the priority:

```rust
let priority: ConfigPriority = args.config_priority.into();
let agent = ctx.load_agent_with_config(&args.agent, toml_config.as_ref(), priority).await?;
```

**Step 3: Run cargo check and test**

Run: `cargo check -p pattern-cli && cargo nextest run -p pattern-cli`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_cli/src/
git commit -m "[pattern-cli] add --config-priority flag for merge behavior control"
```

---

## Task 9: Update CLI builder for new memory fields

**Files:**
- Modify: `crates/pattern_cli/src/commands/builder/agent.rs`
- Modify: `crates/pattern_cli/src/commands/builder/editors.rs`

**Step 1: Add pinned and char_limit to memory block editor**

In `agent.rs`, update `add_memory_block()` and `edit_memory_block()`:

```rust
fn add_memory_block(&mut self) -> Result<()> {
    // ... existing code ...

    let pinned = editors::confirm("Pin to always load in context?", false)?;

    let char_limit_str = editors::input_optional("Character limit (empty for default)")?;
    let char_limit = char_limit_str.and_then(|s| s.parse().ok());

    self.config.memory.insert(
        label.clone(),
        MemoryBlockConfig {
            content,
            content_path,
            permission,
            memory_type,
            description: None,
            pinned: Some(pinned),
            char_limit,
            schema: None,
            id: None,
            shared,
        },
    );
    // ...
}
```

**Step 2: Update memory block display**

In `render_summary()`:

```rust
for (label, block) in &self.config.memory {
    let perm = format!("[{:?}]", block.permission).to_lowercase();
    let pinned = if block.pinned.unwrap_or(false) { " 📌" } else { "" };
    let preview = block
        .content
        .as_ref()
        .map(|c| truncate(c, 30))
        .unwrap_or_else(|| "(empty)".to_string());
    r.list_item(&format!(
        "{}{} {} {}",
        label.cyan(),
        pinned,
        perm.dimmed(),
        preview.dimmed()
    ));
}
```

**Step 3: Run cargo check**

Run: `cargo check -p pattern-cli`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_cli/src/commands/builder/
git commit -m "[pattern-cli] add pinned and char_limit to memory block builder"
```

---

## Task 10: Add deprecation detection and migration command

**Files:**
- Modify: `crates/pattern_core/src/config.rs`
- Create: `crates/pattern_cli/src/commands/config.rs` (if not exists)

**Step 1: Add deprecation detection in config loading**

```rust
impl PatternConfig {
    pub fn load_with_deprecation_check(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let raw: toml::Value = toml::from_str(&content)?;

        // Check for deprecated patterns
        if raw.get("agent").is_some() && raw.get("agents").is_none() {
            return Err(ConfigError::Deprecated {
                field: "agent".into(),
                message: "Singular [agent] is deprecated. Use [[agents]].\n\
                         Run: pattern config migrate".into(),
            });
        }

        if raw.get("user").is_some() {
            tracing::warn!("[user] block is deprecated and ignored. Remove it from config.");
        }

        toml::from_str(&content).map_err(Into::into)
    }
}
```

**Step 2: Add migrate subcommand**

```rust
#[derive(Parser)]
pub struct MigrateArgs {
    /// Path to config file
    pub path: PathBuf,

    /// Modify file in place
    #[arg(long)]
    pub in_place: bool,
}

pub async fn migrate_config(args: MigrateArgs) -> Result<()> {
    let content = tokio::fs::read_to_string(&args.path).await?;
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;

    // Convert [agent] to [[agents]]
    if let Some(agent) = doc.remove("agent") {
        let mut agents = toml_edit::Array::new();
        agents.push(agent);
        doc["agents"] = toml_edit::Item::ArrayOfTables(agents.into());
    }

    // Remove [user]
    doc.remove("user");

    let output = doc.to_string();

    if args.in_place {
        tokio::fs::write(&args.path, &output).await?;
        println!("Migrated {}", args.path.display());
    } else {
        println!("{}", output);
    }

    Ok(())
}
```

**Step 3: Run cargo check**

Run: `cargo check -p pattern-cli`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/config.rs crates/pattern_cli/src/commands/
git commit -m "[pattern-cli] add config migrate command and deprecation detection"
```

---

## Task 11: Update all callers of old config structure

**Files:**
- Various files using `PatternConfig.agent` or `UserConfig`

**Step 1: Run cargo check to find all errors**

Run: `cargo check --workspace`

Collect all compilation errors related to:
- `config.agent` (now `config.agents`)
- `UserConfig` (removed)
- `config.user` (removed)

**Step 2: Fix each caller**

For each error, update the code:

```rust
// Old
let agent_config = config.agent;

// New
let agent_configs: Vec<AgentConfig> = futures::future::try_join_all(
    config.agents.iter().map(|r| r.resolve(&config_dir))
).await?;
let agent_config = agent_configs.first()
    .ok_or_else(|| anyhow!("No agents defined"))?;
```

**Step 3: Run full test suite**

Run: `cargo nextest run --workspace`
Expected: PASS

**Step 4: Commit**

```bash
git add -A
git commit -m "[meta] update all callers to new plural agents config"
```

---

## Task 12: Write comprehensive integration tests

**Files:**
- Create: `crates/pattern_core/tests/config_integration.rs`

**Step 1: Write full integration test suite**

```rust
//! Integration tests for config loading and merge behavior.

use pattern_core::config::*;
use pattern_core::runtime::RuntimeContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_new_agent_seeds_from_toml() {
    // ...
}

#[tokio::test]
async fn test_existing_agent_keeps_db_content() {
    // ...
}

#[tokio::test]
async fn test_existing_agent_updates_config_from_toml() {
    // ...
}

#[tokio::test]
async fn test_toml_priority_overwrites_db_config() {
    // ...
}

#[tokio::test]
async fn test_db_priority_ignores_toml() {
    // ...
}

#[tokio::test]
async fn test_memory_content_not_overwritten_on_reload() {
    // ...
}

#[tokio::test]
async fn test_memory_permission_updated_on_reload() {
    // ...
}

#[tokio::test]
async fn test_memory_pinned_updated_on_reload() {
    // ...
}

#[tokio::test]
async fn test_new_memory_block_added_on_reload() {
    // ...
}

#[tokio::test]
async fn test_group_member_by_reference() {
    // ...
}

#[tokio::test]
async fn test_group_member_inline_config() {
    // ...
}

#[tokio::test]
async fn test_old_singular_agent_field_errors_helpfully() {
    let temp_dir = tempfile::tempdir().unwrap();
    let toml = format!(r#"
        [database]
        path = "{}"

        [agent]
        name = "Test"
    "#, temp_dir.path().display());
    let result = PatternConfig::load_with_deprecation_check_str(&toml);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("deprecated"));
}

#[tokio::test]
async fn test_missing_user_block_ok() {
    let temp_dir = tempfile::tempdir().unwrap();
    let toml = format!(r#"
        [database]
        path = "{}"

        [[agents]]
        name = "Test"
    "#, temp_dir.path().display());
    let config: PatternConfig = toml::from_str(&toml).unwrap();
    assert_eq!(config.agents.len(), 1);
}
```

**Step 2: Run tests**

Run: `cargo nextest run -p pattern-core config_integration`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/tests/
git commit -m "[pattern-core] add comprehensive config integration tests"
```

---

## Task 13: Update example config files

**Files:**
- Modify: `pattern.example.toml`
- Modify: Any other example configs in repo

**Step 1: Update pattern.example.toml**

```toml
# Pattern Configuration Example
# Database directory (contains constellation.db and auth.db)
[database]
path = "~/.local/share/pattern"

# Global model defaults
[model]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
temperature = 0.7

# Agent definitions
[[agents]]
name = "Pattern"
system_prompt = "You are Pattern, an ADHD support agent..."
# Alternative: config_path = "agents/pattern.toml"

[agents.memory.persona]
content = "I am Pattern, a supportive AI companion..."
permission = "read_only"
memory_type = "core"
pinned = true

[agents.memory.working_notes]
permission = "read_write"
memory_type = "working"

[[agents]]
name = "Archivist"
system_prompt = "You manage long-term memory and retrieval..."

# Group definitions
[[groups]]
name = "main"
description = "Primary support constellation"

[groups.pattern]
type = "round_robin"
skip_unavailable = true

[[groups.members]]
name = "Pattern"

[[groups.members]]
name = "Archivist"
```

**Step 2: Commit**

```bash
git add pattern.example.toml
git commit -m "[meta] update example config to new plural agents format"
```

---

## Task 14: Run pre-commit and final validation

**Step 1: Run full pre-commit**

Run: `just pre-commit-all`
Expected: PASS

**Step 2: Run doctests**

Run: `cargo test --doc --workspace`
Expected: PASS

**Step 3: Final commit if any formatting changes**

```bash
git add -A
git commit -m "[meta] formatting and final cleanup"
```

---

## Summary

Total tasks: 14
Estimated time: 4-6 hours

Key files modified:
- `crates/pattern_core/src/config.rs` - Main config types
- `crates/pattern_core/src/runtime/context.rs` - Merge logic
- `crates/pattern_db/src/queries/memory.rs` - Config update query
- `crates/pattern_cli/src/commands/` - CLI integration
- `crates/pattern_cli/src/commands/builder/` - TUI builder updates
