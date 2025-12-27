# Phase E: V2 Integration Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Integrate DatabaseAgentV2 and AgentRuntime with the rest of the codebase - coordination patterns, built-in tools, heartbeat processor, CLI, and Discord consumers.

**Depends On:** Phase D (AgentV2 trait, DatabaseAgentV2, AgentRuntime with ToolExecutor)

**Outcome:** New Agent trait replaces old, RuntimeContext centralizes agent creation, CLI/Discord simplified.

---

## Pre-E1 Fix: ContextBuilder Missing Fields

**Problem:** V2 ContextBuilder/ContextConfig is missing fields from the old ContextConfig:
- `base_instructions` (system prompt) - exists in AgentRecord, not flowing to v2
- Tool rules need to appear in system prompt (model must know the constraints)

**Current Flow:**
- ContextBuilder reads Core/Working blocks → system prompt
- No explicit base_instructions field
- Tool rules go to RuntimeBuilder → ToolExecutor (enforcement only)

**Fix (do before E1):**
1. Add to ContextBuilder:
   - `.with_base_instructions(instructions: &str)` - prepended to system prompt
   - `.with_tool_rules(rules: &[ToolRule])` - included in system prompt for model awareness
2. Update `build_system_prompt()` to:
   - Start with base_instructions if set
   - Then render Core blocks
   - Then render Working blocks
   - Then append tool rules summary

**Data Flow:**
```
AgentRecord.base_instructions ──────────────────────────────┐
                                                            ↓
RuntimeBuilder.tool_rules ─────→ ToolExecutor (enforcement)
                           └───→ ContextBuilder (awareness) ─→ system prompt
                                                            ↑
Memory Blocks (Core/Working) ───────────────────────────────┘
```

---

## Architecture Decisions

### 1. MemoryStore Scoping
- **Singleton per constellation** - One MemoryStore instance shared by all agents
- **Agent scoping via parameter** - Methods take `agent_id: &str` to scope operations
- **Constellation search** - `agent_id = None` searches all data in constellation DB
- **Multi-agent search** - Run parallel single-agent searches, merge results

### 2. Tool Interface: ToolContext Trait
Tools need a defined API surface, not full AgentRuntime access.

```rust
/// What tools can access from the runtime (minimal, extensible)
pub trait ToolContext: Send + Sync {
    /// Current agent's ID (for default scoping)
    fn agent_id(&self) -> &str;

    /// Memory store for blocks, archival, AND search (including messages)
    fn memory(&self) -> &dyn MemoryStore;

    /// Message router for send_message
    fn router(&self) -> &AgentMessageRouter;

    /// Model provider for tools that need LLM calls
    fn model(&self) -> &dyn ModelProvider;
}
```

AgentRuntime implements `ToolContext`. Tools receive `Arc<dyn ToolContext>`.

### 3. Search with Explicit Scope and Permissions

Single search method with `SearchScope` parameter enables permission checks:

```rust
pub enum SearchScope {
    CurrentAgent,              // Default - search own data (always allowed)
    Agent(AgentId),            // Specific agent (permission check)
    Agents(Vec<AgentId>),      // Multiple agents (permission check each)
    Constellation,             // All data in constellation DB (permission check)
}

pub trait ToolContext: Send + Sync {
    // ... core methods ...

    /// Search with explicit scope - enables permission checks
    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,  // Existing type from memory_v2/types.rs
    ) -> MemoryResult<Vec<MemorySearchResult>>;
}
```

Permission checks based on scope:
- `CurrentAgent`: Always allowed
- `Agent(id)`: Check if current agent has permission to search that agent's data
- `Constellation`: Check if current agent has constellation-wide search permission

MemoryStore methods take `Option<&str>` for agent_id, ToolContext's search method handles scope → agent_id translation with permission checks.

### 4. StateWatcher in AgentV2
AgentV2 needs state watching capability for heartbeat processor and other consumers.

**Add to AgentV2 trait or DatabaseAgentV2:**
```rust
/// Subscribe to state changes
async fn state_watcher(&self) -> watch::Receiver<AgentState>;
```

Or expose via a method that returns both current state and watcher:
```rust
async fn state_with_watcher(&self) -> (AgentState, watch::Receiver<AgentState>);
```

This preserves the existing heartbeat processor pattern - wait for Ready state before processing continuation.

### 5. Trait Swap Strategy (After E2)

After E2, V2 core is complete. Rather than maintaining parallel traits, we swap:

1. **Move old files out of source tree** (don't delete yet - reference if needed)
   - `agent/impls/db_agent.rs` → `agent/impls/db_agent_v1.rs.bak`
   - `context/state.rs` (AgentHandle) → backup
   - Old `Agent` trait definition → backup

2. **Rename V2 files/modules/traits** (remove v2 suffix)
   - `agent_v2/` → merge into `agent/`
   - `AgentV2` trait → `Agent`
   - `DatabaseAgentV2` → `DatabaseAgent`
   - `AgentV2Ext` → `AgentExt`
   - `ToolContext` stays as-is (new name)

3. **Expect compilation failures** - this is the guide
   - Errors like "trait Agent has no method X" = expected, fix them
   - Errors that aren't trait incompatibility = investigate carefully

4. **Fix consumers in order** (E3-E6)
   - Each fix resolves more compilation errors
   - When `cargo check` passes, swap is complete

This is cleaner than maintaining two parallel implementations through E3-E6.

---

## Component Analysis

### Components to Update

| Component | Location | Changes | Priority |
|-----------|----------|---------|----------|
| ToolContext trait | runtime/tool_context.rs | New trait definition + SearchScope | **E1** |
| Built-in tools | tool/builtin/*.rs | Use ToolContext instead of AgentHandle | **E2** |
| **TRAIT SWAP** | agent/, agent_v2/ | Rename, remove v2 suffix, compilation breaks | **E2.5** |
| Heartbeat processor | context/heartbeat.rs | process(), state_watcher() | **E3** |
| Coordination patterns | coordination/patterns/*.rs | process() method, name() return | **E4** |
| Coordination selectors | coordination/selectors/*.rs | Same changes | **E4** |
| Groups core | coordination/groups.rs | Type parameters | **E4** |
| QueuedMessage | message_queue.rs | Align with pattern_db | **E5** |
| ScheduledWakeup | message_queue.rs | Add to pattern_db | **E5** |
| CLI agent ops | pattern_cli/agent_ops.rs | New instantiation pattern | **E6** |
| Discord bot | pattern_discord/bot.rs | Accept new Agent trait | **E6** |

---

## Task Breakdown

### Task E1: Define ToolContext Trait

**Files:**
- Create: `crates/pattern_core/src/runtime/tool_context.rs`
- Modify: `crates/pattern_core/src/runtime/mod.rs`

**Steps:**
1. Define `ToolContext` trait with core methods (agent_id, memory, router)
2. Define `SearchScope` enum (uses existing `SearchOptions` from memory_v2/types.rs)
3. Add `search()` method to ToolContext with scope-based permission checks
4. Implement `ToolContext` for `AgentRuntime`
5. Add method to get `Arc<dyn ToolContext>` from runtime

**Acceptance:**
- Trait compiles
- AgentRuntime passes trait object to tools

---

### Task E2: Update Built-in Tools

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/context.rs`
- Modify: `crates/pattern_core/src/tool/builtin/recall.rs`
- Modify: `crates/pattern_core/src/tool/builtin/search.rs`
- Modify: `crates/pattern_core/src/tool/builtin/send_message.rs`
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs`

**Changes per tool:**

| Tool | AgentHandle Usage | ToolContext Equivalent |
|------|-------------------|----------------------|
| context | `handle.memory.get_block()` | `ctx.memory().get_block(ctx.agent_id(), name)` |
| context | `handle.memory.alter_block()` | `ctx.memory().alter_block(ctx.agent_id(), ...)` |
| recall | `handle.insert_archival_memory()` | `ctx.memory().insert_archival(ctx.agent_id(), ...)` |
| recall | `handle.get_archival_memory_by_label()` | `ctx.memory().get_archival(ctx.agent_id(), ...)` |
| search | `handle.search_archival_memories_with_options()` | `ctx.search(query, SearchOptions { scope: CurrentAgent, ... })` |
| search | `handle.search_conversations_with_options()` | `ctx.search(query, SearchOptions { content_types: [Conversations], ... })` |
| search | `handle.search_constellation_messages_with_options()` | `ctx.search(query, SearchOptions { scope: Constellation, ... })` |
| send_message | `handle.message_router()` | `ctx.router()` |

**Steps:**
1. Update tool structs to hold `Arc<dyn ToolContext>` instead of `AgentHandle`
2. Update tool constructors (new factory in mod.rs)
3. Update each tool's execute methods
4. Update tests with mock ToolContext

**Acceptance:**
- All 4 tools compile with ToolContext
- Existing functionality preserved
- Tests pass

---

### Task E2.5: Trait Swap

**Goal:** Remove V2 suffix, make new traits the primary implementation. Compilation will break - that's expected.

**Step 1: Backup old files** (move out of source tree, keep for reference)
```bash
# In crates/pattern_core/src/
mv agent/impls/db_agent.rs agent/impls_old/db_agent.rs
mv context/state.rs context/state_v1.rs.bak  # AgentHandle lives here
# Old Agent trait definition - identify file and backup
```

**Step 2: Rename V2 modules/files**
```bash
# Merge agent_v2 into agent
mv agent_v2/db_agent.rs agent/db_agent.rs
mv agent_v2/traits.rs agent/traits.rs  # or merge into mod.rs
mv agent_v2/collect.rs agent/collect.rs
# Update agent/mod.rs to export from new locations
```

**Step 3: Rename types** (in the moved files)
- `AgentV2` → `Agent`
- `DatabaseAgentV2` → `DatabaseAgent`
- `AgentV2Ext` → `AgentExt`
- Update all `use` statements accordingly

**Step 4: Update lib.rs exports**
- Remove `pub mod agent_v2`
- Ensure `agent` module exports the new types

**Step 5: Run cargo check, note errors**
- Expected: "trait Agent has no method process_message_stream"
- Expected: "struct DatabaseAgent has no field X"
- Unexpected errors = investigate before proceeding

**Acceptance:**
- Old files backed up (not deleted)
- V2 suffix removed from all types
- `cargo check` fails with expected trait incompatibility errors
- No unexpected errors (missing dependencies, etc.)

---

### Task E3: Update Heartbeat Processor

**Files:**
- Modify: `crates/pattern_core/src/context/heartbeat.rs`

**Changes:**
1. Update type: `Vec<Arc<dyn Agent>>` (now uses new slim trait)
2. Replace `process_message_stream()` with `process()`
3. Use `state_watcher()` or `state_with_watcher()` method (preserved from V1 pattern)
4. Handle `name()` returning `&str` (add `.to_string()` where needed)

**Acceptance:**
- Heartbeat works with new `DatabaseAgent` (slim trait)
- StateWatcher pattern preserved
- Continuation messages process correctly

---

### Task E4: Update Coordination Patterns

**Files (12 total):**
- `coordination/groups.rs` - Types and GroupManager trait
- `coordination/types.rs` - CoordinationPattern enum
- `coordination/patterns/dynamic.rs`
- `coordination/patterns/round_robin.rs`
- `coordination/patterns/supervisor.rs`
- `coordination/patterns/voting.rs`
- `coordination/patterns/pipeline.rs`
- `coordination/patterns/sleeptime.rs`
- `coordination/selectors/random.rs`
- `coordination/selectors/capability.rs`
- `coordination/selectors/load_balancing.rs`
- `coordination/selectors/supervisor.rs`

**After E2.5 trait swap:** Only one `Agent` trait exists. Update all coordination code to use the new trait signature.

**Changes per file:**
- Replace `process_message_stream()` with `process()`
- Handle `name()` → `&str` (add `.to_string()` where needed)
- Update type bounds to use new `Agent` trait

**Acceptance:**
- All 6 patterns work with new `Agent` trait
- All 4 selectors work with new `Agent` trait
- Groups can contain agents

---

### Task E5: Port QueuedMessage/ScheduledWakeup to pattern_db

**Files:**
- Create: `crates/pattern_db/src/models/wakeup.rs`
- Create: `crates/pattern_db/src/queries/wakeup.rs`
- Create: `crates/pattern_db/migrations/NNNN_scheduled_wakeups.sql`
- Modify: `crates/pattern_db/src/models/message.rs` (align QueuedMessage)
- Modify: `crates/pattern_core/src/message_queue.rs` (use pattern_db)

**Schema additions:**
```sql
-- ScheduledWakeup table
CREATE TABLE scheduled_wakeups (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    scheduled_for TEXT NOT NULL,
    reason TEXT NOT NULL,
    recurring_seconds INTEGER,
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    last_triggered TEXT,
    metadata TEXT,
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);
CREATE INDEX idx_wakeups_due ON scheduled_wakeups(scheduled_for) WHERE active = 1;

-- QueuedMessage alignment (add missing fields)
ALTER TABLE queued_messages ADD COLUMN call_chain TEXT;  -- JSON array
ALTER TABLE queued_messages ADD COLUMN from_user TEXT;
ALTER TABLE queued_messages ADD COLUMN read INTEGER NOT NULL DEFAULT 0;
ALTER TABLE queued_messages ADD COLUMN read_at TEXT;
```

**Steps:**
1. Add SQLite migration
2. Create wakeup model and queries in pattern_db
3. Align QueuedMessage fields between pattern_core and pattern_db
4. Update message_queue.rs to use pattern_db queries instead of Entity/SurrealDB

**Acceptance:**
- ScheduledWakeup persists to SQLite
- QueuedMessage call_chain preserved
- No Entity macro usage

---

### Task E6: Update CLI and Discord Consumers

**Files:**
- Modify: `crates/pattern_cli/src/agent_ops.rs`
- Modify: `crates/pattern_cli/src/chat.rs`
- Modify: `crates/pattern_cli/src/discord.rs`
- Modify: `crates/pattern_discord/src/bot.rs`

**Key Issues Found (from exploration):**

**CLI (`agent_ops.rs`):**
- `create_agent()`: 350+ lines, violates single responsibility
- `load_or_create_agent_from_member()`: 550+ lines with deeply nested conditionals
- Memory block update logic duplicated 3 times
- Model resolution has 4-level implicit priority (CLI > config > stored > default)
- Bluesky endpoint setup called in 3 different places
- Constellation activity block has SurrealDB bug workaround

**Discord (`bot.rs`):**
- Dual-mode `DiscordBot` with `cli_mode` hack - carries unused fields
- 11 separate `Arc<Mutex>` for what should be one state machine
- Config parsing explosion with fallback chains (100+ lines duplicated)
- Two `process::exit()` calls, async leakage
- Heartbeat channel created twice (one unused)

**What Should Move to pattern_core (new `RuntimeContext`):**
```rust
/// Shared context for creating/loading agents with consistent defaults
pub struct RuntimeContext {
    db: Arc<ConstellationDb>,
    model_provider: Arc<dyn ModelProvider>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    memory_store: Arc<dyn MemoryStore>,  // Constellation-scoped, shared
    default_config: RuntimeConfig,
}

impl RuntimeContext {
    pub fn builder() -> RuntimeContextBuilder;

    /// Load from DB record, apply context defaults
    pub async fn load_agent(&self, agent_id: &AgentId) -> Result<Arc<dyn Agent>>;

    /// Load with per-agent overrides (model, tools, etc.)
    pub async fn load_agent_with(
        &self,
        agent_id: &AgentId,
        overrides: impl Into<AgentOverrides>,
    ) -> Result<Arc<dyn Agent>>;

    /// Create new agent from config (persists to DB)
    pub async fn create_agent(&self, config: &AgentConfig) -> Result<Arc<dyn Agent>>;

    /// Load group - agents share this context's stores
    pub async fn load_group(&self, agent_ids: &[AgentId]) -> Result<Vec<Arc<dyn Agent>>>;
}

/// Per-agent overrides, cascades: context defaults → DB stored config → overrides
pub struct AgentOverrides {
    pub model_id: Option<String>,
    pub response_options: Option<ResponseOptions>,
    pub tool_rules: Option<Vec<ToolRule>>,
    // ... other overridable fields
}
```

**Config Resolution:**
```
1. Start with RuntimeContext.default_config
2. If DB has stored config → overlay that
3. If overrides provided → overlay those
```

- Nothing in DB → use defaults
- Stuff in DB → use that
- Overrides provided → use those (highest priority)

**Config File → Overrides:**
When user provides a config file but agent exists in DB:
1. Load stored config from DB
2. Load config from file
3. Diff them → differences become overrides
4. Apply overrides on load

**Storage (pattern_db Agent model already has this):**
```rust
pub struct Agent {
    pub system_prompt: String,                    // Base instructions
    pub config: Json<serde_json::Value>,          // All settings as JSON
    pub tool_rules: Option<Json<serde_json::Value>>, // Tool rules
    pub enabled_tools: Json<Vec<String>>,         // Enabled tool names
    // ...
}
```

- On `create_agent()`: serialize settings → store in `config` field
- On `load_agent()`: deserialize `config` → overlay on defaults
- File config diffed against DB config → becomes overrides

**Steps:**

1. **Create RuntimeContext in pattern_core** (`runtime/context.rs`):
   - Holds shared resources (db, providers, memory_store)
   - Holds default_config (RuntimeConfig)
   - `load_agent()` / `load_agent_with()` / `create_agent()` / `load_group()`
   - Config resolution: defaults → DB → overrides
   - Bridge PatternConfig → RuntimeConfig

2. **Simplify CLI agent_ops.rs**:
   - Replace 350-line `create_agent()` with `RuntimeContext::create_agent()`
   - Replace load functions with `RuntimeContext::load_agent()`
   - Config file → overrides via diff
   - Remove duplicated memory block logic
   - Remove duplicated Bluesky endpoint setup

3. **Simplify Discord bot.rs**:
   - Remove `cli_mode` hack (separate struct or just use RuntimeContext)
   - Replace 11 `Arc<Mutex>` with proper state machine
   - Use RuntimeContext for agent loading

4. **Update group setup**:
   - Use `RuntimeContext::load_group()` for shared stores
   - Remove duplicated setup between chat.rs and discord.rs

**Acceptance:**
- CLI uses RuntimeContext (not 350-line inline code)
- Discord uses RuntimeContext (no cli_mode hack)
- Agent load/create is < 10 lines in consumers
- Groups share stores properly
- Config persists to DB, loads back correctly

---

## Execution Order

```
E1 (ToolContext trait)
    ↓
E2 (Built-in tools)
    ↓
E2.5 (TRAIT SWAP - compilation breaks here)
    ↓
    ├── E3 (Heartbeat) ─────────────┐
    ├── E4 (Coordination patterns) ─┤ Fix in parallel, guided by compiler
    └── E5 (Queue/Wakeup) ──────────┘
            ↓
    E6 (CLI/Discord consumers)
            ↓
    cargo check passes (swap complete)
            ↓
    Integration testing
            ↓
    Phase F: Cleanup (delete .bak files)
```

After E2.5, work is guided by compilation errors. E3, E4, E5 can be done in parallel.

---

## Success Criteria

- [ ] `cargo check -p pattern_core` passes
- [ ] `cargo test -p pattern_core` passes
- [ ] ToolContext trait defined and implemented
- [ ] All 4 built-in tools use ToolContext
- [ ] Heartbeat processor works with new Agent trait
- [ ] All 6 coordination patterns work with new Agent trait
- [ ] ScheduledWakeup in pattern_db
- [ ] RuntimeContext created and working
- [ ] CLI uses RuntimeContext for agent load/create
- [ ] Discord bot uses RuntimeContext (no cli_mode hack)
- [ ] Config persists to DB and loads correctly

---

## Risk Mitigation

1. **Breaking changes:** Old files backed up as .bak, can restore if needed
2. **Search scope complexity:** Start with CurrentAgent + Constellation, add multi-agent later
3. **Compilation errors post-swap:** Expected - errors guide what to fix next
4. **CLI/Discord complexity:** RuntimeContext centralizes logic, reduces risk of divergence

---

## Resolved Questions

1. **Permission model for cross-agent search:**
   - Static role-based, defined in agent's group config
   - E.g., supervisor role has broad search permission, workers have narrow
   - Future extension: tightly-scoped tool variants that share codebase but hide illegal operations from agent's prompt entirely

2. **ToolContext extensions:**
   - Add model provider access (tools may need LLM calls)
   - Activity logging happens in `process()`, revisit if needed elsewhere

3. **RuntimeContext lifecycle:**
   - **One per constellation** (scoped by database)
   - Separate constellation = separate DB = separate RuntimeContext
   - Discord: one per init, can share across channels if permissions allow
   - CLI hybrid mode: pass existing RuntimeContext to Discord
   - RuntimeContext IS the constellation handle

## Resolved Questions (continued)

4. **Config schema versioning:**
   - Add version field to config JSON (`config_version: u32`)
   - Migrate on load: detect version, apply migrations to current
   - Since we're changing config schema in this work, add migration function now
   - Future schema changes: bump version, add migration step
