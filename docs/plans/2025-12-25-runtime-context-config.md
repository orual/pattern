# RuntimeContext Config System Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Complete RuntimeContext with full agent creation, config resolution, and group loading capabilities.

**Key Design Decisions:**
- RuntimeContext owns default ModelProvider and EmbeddingProvider
- Per-constellation SQLite database (DatabaseConfig with path)
- Config cascade: RuntimeContext defaults → DB stored config → provided overrides
- load_group creates agents that don't exist yet

---

## Part 1: Database Config for SQLite

### Task 1.1: Uncomment and update DatabaseConfig

**Files:**
- Modify: `crates/pattern_core/src/config.rs`

**Step 1: Add DatabaseConfig struct**

Uncomment and update the DatabaseConfig (around line 46-47):

```rust
/// Database configuration for SQLite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Path to SQLite database file
    pub path: PathBuf,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("pattern")
                .join("constellation.db"),
        }
    }
}
```

**Step 2: Add to PatternConfig**

Uncomment the database field in PatternConfig:

```rust
pub struct PatternConfig {
    // ... existing fields

    /// Database configuration
    #[serde(default)]
    pub database: DatabaseConfig,

    // ... rest
}
```

**Step 3: Update Default impl and merge_configs**

Uncomment database in Default impl and merge_configs function.

**Step 4: Run cargo check**

Run: `cargo check -p pattern_core`

---

## Part 2: AgentOverrides and ResolvedAgentConfig

### Task 2.1: Add AgentOverrides struct

**Files:**
- Modify: `crates/pattern_core/src/config.rs`

**Add after PartialAgentConfig:**

```rust
/// Per-agent overrides - highest priority in config cascade
///
/// Used when loading an agent with runtime modifications that
/// shouldn't be persisted to the database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOverrides {
    /// Override model provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,

    /// Override model name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,

    /// Override system prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Override temperature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Override tool rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_rules: Option<Vec<ToolRuleConfig>>,

    /// Override enabled tools
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,

    /// Override context settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigOptions>,
}

impl AgentOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_model(mut self, provider: &str, name: &str) -> Self {
        self.model_provider = Some(provider.to_string());
        self.model_name = Some(name.to_string());
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }
}
```

### Task 2.2: Add ResolvedAgentConfig struct

**Files:**
- Modify: `crates/pattern_core/src/config.rs`

**Add after AgentOverrides:**

```rust
/// Fully resolved agent configuration
///
/// All fields are concrete (no Options for required values).
/// Created by resolving the config cascade.
#[derive(Debug, Clone)]
pub struct ResolvedAgentConfig {
    pub id: AgentId,
    pub name: String,
    pub model_provider: String,
    pub model_name: String,
    pub system_prompt: String,
    pub persona: Option<String>,
    pub tool_rules: Vec<ToolRule>,
    pub enabled_tools: Vec<String>,
    pub memory_blocks: HashMap<String, MemoryBlockConfig>,
    pub context: ContextConfigOptions,
    pub temperature: Option<f32>,
}

impl ResolvedAgentConfig {
    /// Resolve from AgentConfig with defaults filled in
    pub fn from_agent_config(config: &AgentConfig, defaults: &AgentConfig) -> Self {
        let model = config.model.as_ref().or(defaults.model.as_ref());

        Self {
            id: config.id.clone().unwrap_or_else(AgentId::generate),
            name: config.name.clone(),
            model_provider: model.map(|m| m.provider.clone())
                .unwrap_or_else(|| "anthropic".to_string()),
            model_name: model.and_then(|m| m.model.clone())
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
            system_prompt: config.system_prompt.clone().unwrap_or_default(),
            persona: config.persona.clone(),
            tool_rules: config.get_tool_rules().unwrap_or_default(),
            enabled_tools: config.tools.clone(),
            memory_blocks: config.memory.clone(),
            context: config.context.clone().unwrap_or_default(),
            temperature: model.and_then(|m| m.temperature),
        }
    }

    /// Apply overrides to this resolved config
    pub fn apply_overrides(mut self, overrides: &AgentOverrides) -> Self {
        if let Some(ref provider) = overrides.model_provider {
            self.model_provider = provider.clone();
        }
        if let Some(ref name) = overrides.model_name {
            self.model_name = name.clone();
        }
        if let Some(ref prompt) = overrides.system_prompt {
            self.system_prompt = prompt.clone();
        }
        if let Some(temp) = overrides.temperature {
            self.temperature = Some(temp);
        }
        if let Some(ref rules) = overrides.tool_rules {
            self.tool_rules = rules.iter()
                .filter_map(|r| r.to_tool_rule().ok())
                .collect();
        }
        if let Some(ref tools) = overrides.enabled_tools {
            self.enabled_tools = tools.clone();
        }
        if let Some(ref ctx) = overrides.context {
            self.context = ctx.clone();
        }
        self
    }
}

impl Default for ContextConfigOptions {
    fn default() -> Self {
        Self {
            max_messages: None,
            max_message_age_hours: None,
            compression_threshold: None,
            compression_strategy: None,
            memory_char_limit: None,
            enable_thinking: None,
        }
    }
}
```

---

## Part 3: Config Conversion Methods

### Task 3.1: Add AgentConfig to DB Agent conversion

**Files:**
- Modify: `crates/pattern_core/src/config.rs`

**Add to impl AgentConfig:**

```rust
impl AgentConfig {
    // ... existing methods

    /// Convert to database Agent model for persistence
    pub fn to_db_agent(&self, id: &str) -> pattern_db::models::Agent {
        use pattern_db::models::{Agent, AgentStatus};
        use sqlx::types::Json;
        use chrono::Utc;

        let model = self.model.as_ref();

        Agent {
            id: id.to_string(),
            name: self.name.clone(),
            description: None,
            model_provider: model.map(|m| m.provider.clone())
                .unwrap_or_else(|| "anthropic".to_string()),
            model_name: model.and_then(|m| m.model.clone())
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
            system_prompt: self.system_prompt.clone().unwrap_or_default(),
            config: Json(serde_json::to_value(self).unwrap_or_default()),
            enabled_tools: Json(self.tools.clone()),
            tool_rules: if self.tool_rules.is_empty() {
                None
            } else {
                Some(Json(serde_json::to_value(&self.tool_rules).unwrap_or_default()))
            },
            status: AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}
```

### Task 3.2: Add DB Agent to PartialAgentConfig conversion

**Files:**
- Modify: `crates/pattern_core/src/config.rs`

```rust
impl From<&pattern_db::models::Agent> for PartialAgentConfig {
    fn from(agent: &pattern_db::models::Agent) -> Self {
        // Try to deserialize from the config JSON field
        if let Ok(config) = serde_json::from_value::<PartialAgentConfig>(agent.config.0.clone()) {
            return config;
        }

        // Fallback: construct from individual fields
        PartialAgentConfig {
            id: Some(AgentId(agent.id.clone())),
            name: Some(agent.name.clone()),
            system_prompt: Some(agent.system_prompt.clone()),
            model: Some(ModelConfig {
                provider: agent.model_provider.clone(),
                model: Some(agent.model_name.clone()),
                temperature: None,
                settings: HashMap::new(),
            }),
            tools: Some(agent.enabled_tools.0.clone()),
            tool_rules: agent.tool_rules.as_ref().and_then(|r| {
                serde_json::from_value(r.0.clone()).ok()
            }),
            ..Default::default()
        }
    }
}
```

---

## Part 4: RuntimeContext Builder and Provider Storage

### Task 4.1: Add provider fields and builder to RuntimeContext

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

**Update RuntimeContext struct:**

```rust
pub struct RuntimeContext {
    // ... existing fields (db, agents, memory, tools, etc.)

    /// Default model provider for agents
    model_provider: Arc<dyn ModelProvider>,

    /// Default embedding provider (optional)
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,

    /// Default agent configuration
    default_config: AgentConfig,
}
```

**Add RuntimeContextBuilder:**

```rust
/// Builder for RuntimeContext
pub struct RuntimeContextBuilder {
    db: Option<Arc<ConstellationDb>>,
    model_provider: Option<Arc<dyn ModelProvider>>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    memory: Option<Arc<MemoryCache>>,
    tools: Option<Arc<ToolRegistry>>,
    default_config: Option<AgentConfig>,
    context_config: RuntimeContextConfig,
}

impl RuntimeContextBuilder {
    pub fn new() -> Self {
        Self {
            db: None,
            model_provider: None,
            embedding_provider: None,
            memory: None,
            tools: None,
            default_config: None,
            context_config: RuntimeContextConfig::default(),
        }
    }

    pub fn db(mut self, db: Arc<ConstellationDb>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    pub fn embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    pub fn memory(mut self, memory: Arc<MemoryCache>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn default_config(mut self, config: AgentConfig) -> Self {
        self.default_config = Some(config);
        self
    }

    pub fn context_config(mut self, config: RuntimeContextConfig) -> Self {
        self.context_config = config;
        self
    }

    pub async fn build(self) -> Result<RuntimeContext> {
        let db = self.db.ok_or_else(|| CoreError::ConfigurationError {
            field: "db".to_string(),
            config_path: "RuntimeContextBuilder".to_string(),
            expected: "database connection".to_string(),
            cause: crate::error::ConfigError::MissingField("db".to_string()),
        })?;

        let model_provider = self.model_provider.ok_or_else(|| CoreError::ConfigurationError {
            field: "model_provider".to_string(),
            config_path: "RuntimeContextBuilder".to_string(),
            expected: "model provider".to_string(),
            cause: crate::error::ConfigError::MissingField("model_provider".to_string()),
        })?;

        let memory = self.memory.unwrap_or_else(|| Arc::new(MemoryCache::new(db.clone())));
        let tools = self.tools.unwrap_or_else(|| Arc::new(ToolRegistry::new()));
        let default_config = self.default_config.unwrap_or_default();

        RuntimeContext::new_with_providers(
            db,
            model_provider,
            self.embedding_provider,
            memory,
            tools,
            default_config,
            self.context_config,
        ).await
    }
}

impl RuntimeContext {
    pub fn builder() -> RuntimeContextBuilder {
        RuntimeContextBuilder::new()
    }

    // Add getters
    pub fn model_provider(&self) -> &Arc<dyn ModelProvider> {
        &self.model_provider
    }

    pub fn embedding_provider(&self) -> Option<&Arc<dyn EmbeddingProvider>> {
        self.embedding_provider.as_ref()
    }

    pub fn default_config(&self) -> &AgentConfig {
        &self.default_config
    }
}
```

---

## Part 5: Agent Creation and Config Resolution

### Task 5.1: Add config resolution to RuntimeContext

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

```rust
impl RuntimeContext {
    /// Resolve configuration cascade: defaults → DB → overrides
    fn resolve_config(
        &self,
        db_agent: &pattern_db::models::Agent,
        overrides: Option<&AgentOverrides>,
    ) -> ResolvedAgentConfig {
        // 1. Start with defaults
        let mut config = self.default_config.clone();

        // 2. Overlay DB stored config
        let db_partial: PartialAgentConfig = db_agent.into();
        config = merge_agent_configs(config, db_partial);

        // 3. Resolve to concrete config
        let mut resolved = ResolvedAgentConfig::from_agent_config(&config, &self.default_config);

        // 4. Apply overrides if provided
        if let Some(ovr) = overrides {
            resolved = resolved.apply_overrides(ovr);
        }

        resolved
    }
}
```

### Task 5.2: Add create_agent method

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

```rust
impl RuntimeContext {
    /// Create a new agent from config (persists to DB)
    pub async fn create_agent(&self, config: &AgentConfig) -> Result<Arc<dyn Agent>> {
        let id = config.id.clone()
            .map(|id| id.0)
            .unwrap_or_else(|| AgentId::generate().0);

        // Check if agent already exists
        if pattern_db::queries::get_agent(self.db.pool(), &id).await?.is_some() {
            return Err(CoreError::InvalidFormat {
                data_type: "agent".to_string(),
                details: format!("Agent with id '{}' already exists", id),
            });
        }

        // 1. Convert to DB model and persist
        let db_agent = config.to_db_agent(&id);
        pattern_db::queries::create_agent(self.db.pool(), &db_agent).await?;

        // 2. Create memory blocks from config
        for (label, block_config) in &config.memory {
            let content = block_config.load_content().await?;
            self.memory.create_block(
                &id,
                label,
                &content,
                block_config.memory_type.into(),
                block_config.permission,
            ).await?;
        }

        // 3. Create persona block if specified
        if let Some(ref persona) = config.persona {
            self.memory.create_block(
                &id,
                "persona",
                persona,
                BlockType::Core,
                MemoryPermission::ReadWrite,
            ).await?;
        }

        // 4. Load and register the agent
        self.load_agent(&id).await
    }
}
```

### Task 5.3: Add load_agent_with method

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

```rust
impl RuntimeContext {
    /// Load an agent with per-agent overrides
    pub async fn load_agent_with(
        &self,
        agent_id: &str,
        overrides: AgentOverrides,
    ) -> Result<Arc<dyn Agent>> {
        let db_agent = pattern_db::queries::get_agent(self.db.pool(), agent_id).await?
            .ok_or_else(|| CoreError::AgentNotFound {
                agent_id: agent_id.to_string(),
            })?;

        let resolved = self.resolve_config(&db_agent, Some(&overrides));
        self.build_agent_from_resolved(agent_id, &resolved).await
    }

    /// Internal: build agent from resolved config
    async fn build_agent_from_resolved(
        &self,
        agent_id: &str,
        resolved: &ResolvedAgentConfig,
    ) -> Result<Arc<dyn Agent>> {
        // Build runtime
        let runtime = RuntimeBuilder::new()
            .agent_id(agent_id)
            .agent_name(&resolved.name)
            .memory(self.memory.clone())
            .messages(MessageStore::new(self.db.clone(), agent_id.to_string()))
            .tools(self.tools.clone())
            .model(self.model_provider.clone())
            .db(self.db.clone())
            .build()?;

        // Build agent
        let agent = DatabaseAgentBuilder::new()
            .id(agent_id)
            .name(&resolved.name)
            .runtime(Arc::new(runtime))
            .model(self.model_provider.clone())
            .heartbeat_sender(self.heartbeat_sender())
            .build()?;

        let agent: Arc<dyn Agent> = Arc::new(agent);
        self.register_agent(agent.clone()).await;

        Ok(agent)
    }
}
```

---

## Part 6: Group Loading with Agent Creation

### Task 6.1: Add load_group method

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

```rust
impl RuntimeContext {
    /// Load a group of agents, creating any that don't exist
    ///
    /// All agents share this context's stores (memory, tools).
    pub async fn load_group(&self, agent_ids: &[String]) -> Result<Vec<Arc<dyn Agent>>> {
        let mut agents = Vec::with_capacity(agent_ids.len());

        for id in agent_ids {
            let agent = self.load_agent(id).await?;
            agents.push(agent);
        }

        Ok(agents)
    }

    /// Load a group from GroupConfig, creating agents as needed
    pub async fn load_group_from_config(&self, config: &GroupConfig) -> Result<Vec<Arc<dyn Agent>>> {
        let mut agents = Vec::with_capacity(config.members.len());

        for member in &config.members {
            let agent = self.load_or_create_group_member(member).await?;
            agents.push(agent);
        }

        Ok(agents)
    }

    /// Load or create a single group member
    async fn load_or_create_group_member(
        &self,
        member: &GroupMemberConfig,
    ) -> Result<Arc<dyn Agent>> {
        // If agent_id is provided, try to load it
        if let Some(ref agent_id) = member.agent_id {
            if let Ok(agent) = self.load_agent(&agent_id.0).await {
                return Ok(agent);
            }
            // Agent doesn't exist, fall through to creation
        }

        // Get agent config from member
        let agent_config = if let Some(ref config) = member.agent_config {
            config.clone()
        } else if let Some(ref config_path) = member.config_path {
            AgentConfig::load_from_file(config_path).await?
        } else {
            // Create minimal config from member info
            AgentConfig {
                id: member.agent_id.clone(),
                name: member.name.clone(),
                ..Default::default()
            }
        };

        // Create the agent
        self.create_agent(&agent_config).await
    }
}
```

---

## Part 7: Update Existing load_agent

### Task 7.1: Update load_agent to use config resolution

**Files:**
- Modify: `crates/pattern_core/src/runtime/context.rs`

Update the existing `load_agent` method to use the new config resolution:

```rust
impl RuntimeContext {
    /// Load an agent from the database by ID
    pub async fn load_agent(&self, agent_id: &str) -> Result<Arc<dyn Agent>> {
        // Check if already loaded
        if let Some(agent) = self.get_agent(agent_id).await {
            return Ok(agent);
        }

        // Load from DB
        let db_agent = pattern_db::queries::get_agent(self.db.pool(), agent_id).await?
            .ok_or_else(|| CoreError::AgentNotFound {
                agent_id: agent_id.to_string(),
            })?;

        // Resolve config (defaults → DB, no overrides)
        let resolved = self.resolve_config(&db_agent, None);

        // Build and register
        self.build_agent_from_resolved(agent_id, &resolved).await
    }
}
```

---

## Verification Checklist

Before considering complete:

- [ ] `cargo check --workspace` passes
- [ ] DatabaseConfig uncommented and working
- [ ] AgentOverrides and ResolvedAgentConfig defined
- [ ] AgentConfig.to_db_agent() works
- [ ] RuntimeContext has model_provider and embedding_provider
- [ ] RuntimeContextBuilder works
- [ ] create_agent persists to DB and creates memory blocks
- [ ] load_agent_with applies overrides correctly
- [ ] load_group creates missing agents
- [ ] Config cascade: defaults → DB → overrides

---

## Testing Strategy

1. **Config resolution test**: Create agent with defaults, load with overrides, verify cascade
2. **Persistence test**: create_agent, then load_agent, verify same config
3. **Group test**: load_group_from_config with mix of existing and new agents
4. **Memory blocks test**: create_agent with memory config, verify blocks created
