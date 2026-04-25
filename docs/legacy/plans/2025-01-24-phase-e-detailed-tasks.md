# Phase E: V2 Integration - Detailed Implementation Tasks

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Integrate DatabaseAgentV2 and AgentRuntime with the rest of the codebase, swapping V2 traits to become the primary Agent trait.

**Architecture:** Pre-E1 fixes ContextBuilder gaps, E1-E2 create ToolContext and update tools, E2.5 swaps traits (expect breaks), E3-E5 fix consumers guided by compiler, E6 creates RuntimeContext and simplifies CLI/Discord.

**Tech Stack:** Rust, async-trait, tokio, pattern_db (SQLite), Loro CRDT

**Reference:** See `docs/plans/2025-01-24-phase-e-integration.md` for architecture decisions and context.

---

## Pre-E1: Fix ContextBuilder Missing Fields

### Task Pre-E1.1: Add base_instructions to ContextBuilder

**Files:**
- Modify: `crates/pattern_core/src/context_v2/builder.rs`

**Step 1: Add base_instructions field to ContextBuilder struct**

In `builder.rs`, find the `ContextBuilder` struct (around line 23-32) and add:

```rust
pub struct ContextBuilder<'a> {
    memory: &'a dyn MemoryStore,
    messages: Option<&'a MessageStore>,
    tools: Option<&'a ToolRegistry>,
    config: &'a ContextConfig,
    agent_id: Option<String>,
    model_info: Option<&'a ModelInfo>,
    active_batch_id: Option<SnowflakePosition>,
    model_provider: Option<Arc<dyn ModelProvider>>,
    base_instructions: Option<String>,  // ADD THIS
}
```

**Step 2: Update the new() constructor**

Find `fn new()` and add initialization:

```rust
pub fn new(memory: &'a dyn MemoryStore, config: &'a ContextConfig) -> Self {
    Self {
        memory,
        messages: None,
        tools: None,
        config,
        agent_id: None,
        model_info: None,
        active_batch_id: None,
        model_provider: None,
        base_instructions: None,  // ADD THIS
    }
}
```

**Step 3: Add builder method**

After `with_model_provider()` method, add:

```rust
/// Set the base instructions (system prompt) for this context
///
/// These instructions are prepended to the system prompt before memory blocks.
pub fn with_base_instructions(mut self, instructions: impl Into<String>) -> Self {
    self.base_instructions = Some(instructions.into());
    self
}
```

**Step 4: Update build_system_prompt to include base_instructions**

Find `async fn build_system_prompt()` (around line 158-189) and modify:

```rust
async fn build_system_prompt(&self, agent_id: &str) -> Result<Vec<String>, CoreError> {
    let mut prompt_parts = Vec::new();

    // Base instructions come first (if set)
    if let Some(ref instructions) = self.base_instructions {
        if !instructions.is_empty() {
            prompt_parts.push(instructions.clone());
        }
    }

    // Get and render Core blocks
    let core_blocks = self
        .memory
        .list_blocks_by_type(agent_id, BlockType::Core)
        // ... rest unchanged
```

**Step 5: Run cargo check**

```bash
cargo check -p pattern_core
```

Expected: Compiles without errors.

**Step 6: Commit**

```bash
git add crates/pattern_core/src/context_v2/builder.rs
git commit -m "feat(context_v2): add base_instructions to ContextBuilder"
```

---

### Task Pre-E1.2: Add tool_rules to ContextBuilder

**Files:**
- Modify: `crates/pattern_core/src/context_v2/builder.rs`

**Step 1: Add import for ToolRule**

At the top of builder.rs, add:

```rust
use crate::agent::tool_rules::ToolRule;
```

**Step 2: Add tool_rules field to ContextBuilder struct**

```rust
pub struct ContextBuilder<'a> {
    // ... existing fields ...
    base_instructions: Option<String>,
    tool_rules: Vec<ToolRule>,  // ADD THIS
}
```

**Step 3: Update new() constructor**

```rust
tool_rules: Vec::new(),  // ADD THIS in new()
```

**Step 4: Add builder method**

```rust
/// Set the tool rules that should be included in the system prompt
///
/// Tool rules are appended to the system prompt so the model knows the constraints.
pub fn with_tool_rules(mut self, rules: Vec<ToolRule>) -> Self {
    self.tool_rules = rules;
    self
}
```

**Step 5: Update build_system_prompt to include tool rules**

At the end of `build_system_prompt()`, before the `Ok(prompt_parts)`:

```rust
    // ... after Working blocks ...

    // Append tool rules summary if any
    if !self.tool_rules.is_empty() {
        let rules_text = self.render_tool_rules();
        if !rules_text.is_empty() {
            prompt_parts.push(rules_text);
        }
    }

    Ok(prompt_parts)
}
```

**Step 6: Add render_tool_rules helper method**

Add this method to the impl block:

```rust
/// Render tool rules as a human-readable section for the system prompt
fn render_tool_rules(&self) -> String {
    if self.tool_rules.is_empty() {
        return String::new();
    }

    let mut lines = vec!["# Tool Usage Rules".to_string()];
    lines.push(String::new());

    for rule in &self.tool_rules {
        lines.push(format!("- {}", rule.description()));
    }

    lines.join("\n")
}
```

**Step 7: Run cargo check**

```bash
cargo check -p pattern_core
```

Expected: Compiles. If `ToolRule::description()` doesn't exist, check `tool_rules.rs` for the actual method name (might be `to_string()` or similar).

**Step 8: Commit**

```bash
git add crates/pattern_core/src/context_v2/builder.rs
git commit -m "feat(context_v2): add tool_rules to ContextBuilder system prompt"
```

---

### Task Pre-E1.3: Add test for ContextBuilder with base_instructions and tool_rules

**Files:**
- Modify: `crates/pattern_core/src/context_v2/builder.rs` (tests module)

**Step 1: Add test**

In the `#[cfg(test)] mod tests` section at the bottom of builder.rs, add:

```rust
#[tokio::test]
async fn test_builder_with_base_instructions() {
    let memory = MockMemoryStore;
    let config = ContextConfig::default();

    let builder = ContextBuilder::new(&memory, &config)
        .for_agent("test-agent")
        .with_base_instructions("You are a helpful assistant.");

    let request = builder.build().await.unwrap();

    let system = request.system.unwrap();
    // Base instructions should be first
    assert!(system[0].contains("helpful assistant"));
}

#[tokio::test]
async fn test_builder_with_tool_rules() {
    use crate::agent::tool_rules::{ToolRule, ToolRuleType};

    let memory = MockMemoryStore;
    let config = ContextConfig::default();

    // Create a simple tool rule for testing
    let rule = ToolRule::new(
        ToolRuleType::StartConstraint,
        "send_message".to_string(),
        "Must start with send_message".to_string(),
    );

    let builder = ContextBuilder::new(&memory, &config)
        .for_agent("test-agent")
        .with_tool_rules(vec![rule]);

    let request = builder.build().await.unwrap();

    let system = request.system.unwrap();
    // Tool rules should be in the system prompt
    let full_prompt = system.join("\n");
    assert!(full_prompt.contains("Tool Usage Rules"));
}
```

**Step 2: Run the test**

```bash
cargo test -p pattern_core test_builder_with_base_instructions -- --nocapture
cargo test -p pattern_core test_builder_with_tool_rules -- --nocapture
```

Expected: Both tests pass.

**Step 3: Commit**

```bash
git add crates/pattern_core/src/context_v2/builder.rs
git commit -m "test(context_v2): add tests for base_instructions and tool_rules"
```

---

## E1: Define ToolContext Trait

### Task E1.1: Create tool_context.rs with ToolContext trait and SearchScope

**Files:**
- Create: `crates/pattern_core/src/runtime/tool_context.rs`
- Modify: `crates/pattern_core/src/runtime/mod.rs`

**Step 1: Create the new file**

Create `crates/pattern_core/src/runtime/tool_context.rs`:

```rust
//! ToolContext: What tools can access from the runtime
//!
//! This trait provides a minimal, well-defined API surface for tools
//! instead of giving them full access to AgentRuntime.

use async_trait::async_trait;
use std::sync::Arc;

use crate::AgentId;
use crate::ModelProvider;
use crate::error::CoreError;
use crate::memory_v2::{MemoryResult, MemorySearchResult, MemoryStore, SearchOptions};
use crate::runtime::AgentMessageRouter;

/// Scope for search operations - determines what data is searched
#[derive(Debug, Clone)]
pub enum SearchScope {
    /// Search only the current agent's data (always allowed)
    CurrentAgent,
    /// Search a specific agent's data (requires permission)
    Agent(AgentId),
    /// Search multiple agents' data (requires permission for each)
    Agents(Vec<AgentId>),
    /// Search all data in the constellation (requires broad permission)
    Constellation,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self::CurrentAgent
    }
}

/// What tools can access from the runtime
///
/// This is the minimal interface tools need. AgentRuntime implements this trait.
/// Tools receive `Arc<dyn ToolContext>` instead of direct runtime access.
#[async_trait]
pub trait ToolContext: Send + Sync {
    /// Get the current agent's ID (for default scoping)
    fn agent_id(&self) -> &str;

    /// Get the memory store for blocks, archival, and search
    fn memory(&self) -> &dyn MemoryStore;

    /// Get the message router for send_message
    fn router(&self) -> &AgentMessageRouter;

    /// Get the model provider for tools that need LLM calls
    fn model(&self) -> Option<&dyn ModelProvider>;

    /// Search with explicit scope and permission checks
    ///
    /// Permission checks based on scope:
    /// - `CurrentAgent`: Always allowed
    /// - `Agent(id)`: Check if current agent has permission
    /// - `Constellation`: Check for constellation-wide permission
    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;
}
```

**Step 2: Add module export to runtime/mod.rs**

At the top of `runtime/mod.rs`, add the module declaration after the existing ones:

```rust
mod executor;
mod router;
mod tool_context;  // ADD THIS
mod types;
```

And add the pub use:

```rust
pub use executor::{
    ProcessToolState, ToolExecutionError, ToolExecutionResult, ToolExecutor, ToolExecutorConfig,
};
pub use router::{AgentMessageRouter, MessageEndpoint, MessageOrigin};
pub use tool_context::{SearchScope, ToolContext};  // ADD THIS
pub use types::RuntimeConfig;
```

**Step 3: Run cargo check**

```bash
cargo check -p pattern_core
```

Expected: Compiles. May have warnings about unused code (that's fine for now).

**Step 4: Commit**

```bash
git add crates/pattern_core/src/runtime/tool_context.rs
git add crates/pattern_core/src/runtime/mod.rs
git commit -m "feat(runtime): add ToolContext trait and SearchScope enum"
```

---

### Task E1.2: Implement ToolContext for AgentRuntime

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs`

**Step 1: Add the impl block**

At the end of `runtime/mod.rs`, before any test modules, add:

```rust
#[async_trait::async_trait]
impl ToolContext for AgentRuntime {
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn memory(&self) -> &dyn MemoryStore {
        self.memory.as_ref()
    }

    fn router(&self) -> &AgentMessageRouter {
        &self.router
    }

    fn model(&self) -> Option<&dyn ModelProvider> {
        self.model.as_ref().map(|m| m.as_ref())
    }

    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        use crate::memory_v2::MemoryResult;

        match scope {
            SearchScope::CurrentAgent => {
                self.memory.search(&self.agent_id, query, options).await
            }
            SearchScope::Agent(ref id) => {
                // TODO: Add permission check here
                // For now, allow all (will be restricted by role config later)
                self.memory.search(id.as_str(), query, options).await
            }
            SearchScope::Agents(ref ids) => {
                // Search each agent and merge results
                let mut all_results = Vec::new();
                for id in ids {
                    // TODO: Add permission check per agent
                    let results = self.memory.search(id.as_str(), query, options.clone()).await?;
                    all_results.extend(results);
                }
                Ok(all_results)
            }
            SearchScope::Constellation => {
                // TODO: Add permission check for broad access
                // Search with None for agent_id to get all
                self.memory.search_all(query, options).await
            }
        }
    }
}
```

**Step 2: Check if MemoryStore has search_all method**

If `search_all` doesn't exist on MemoryStore, you may need to add it or use a different approach. Check `memory_v2/store.rs`. If missing, use:

```rust
SearchScope::Constellation => {
    // Constellation-wide search - pass empty string or use dedicated method
    // This is a placeholder - actual implementation depends on MemoryStore
    self.memory.search("", query, options).await
}
```

**Step 3: Add import at top of mod.rs**

```rust
use crate::memory_v2::{MemoryResult, MemorySearchResult, SearchOptions};
use tool_context::{SearchScope, ToolContext};
```

**Step 4: Run cargo check**

```bash
cargo check -p pattern_core
```

Fix any missing imports or method signature mismatches.

**Step 5: Commit**

```bash
git add crates/pattern_core/src/runtime/mod.rs
git commit -m "feat(runtime): implement ToolContext for AgentRuntime"
```

---

### Task E1.3: Add method to get Arc<dyn ToolContext> from AgentRuntime

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs`

**Step 1: Add as_tool_context method to AgentRuntime impl**

In the `impl AgentRuntime` block, add:

```rust
/// Get this runtime as a ToolContext trait object
///
/// This is used to pass the runtime to tools without exposing
/// the full AgentRuntime API.
pub fn as_tool_context(self: &Arc<Self>) -> Arc<dyn ToolContext> {
    // Since AgentRuntime implements ToolContext, we can clone the Arc
    // and cast it to the trait object
    Arc::clone(self) as Arc<dyn ToolContext>
}
```

Note: This requires AgentRuntime to be held in an Arc. Check if this is already the case in DatabaseAgentV2.

**Step 2: Alternative if AgentRuntime is not in Arc**

If AgentRuntime is owned directly (not in Arc), we need a different approach. Check `db_agent.rs` to see how runtime is stored. If it's not Arc, add this instead:

```rust
// In AgentRuntime impl
/// Create a ToolContext wrapper that borrows this runtime
///
/// Note: The returned wrapper has the same lifetime as &self
pub fn tool_context(&self) -> &dyn ToolContext {
    self
}
```

**Step 3: Run cargo check**

```bash
cargo check -p pattern_core
```

**Step 4: Commit**

```bash
git add crates/pattern_core/src/runtime/mod.rs
git commit -m "feat(runtime): add as_tool_context method for tool access"
```

---

## E2: Update Built-in Tools

### Task E2.1: Create ToolContext-based tool factory

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs`

**Step 1: Add import for ToolContext**

At the top of `builtin/mod.rs`:

```rust
use crate::runtime::ToolContext;
use std::sync::Arc;
```

**Step 2: Add new struct for V2 tools**

After `BuiltinTools` struct, add:

```rust
/// Built-in tools that use ToolContext (V2 API)
#[derive(Clone)]
pub struct BuiltinToolsV2 {
    context: Arc<dyn ToolContext>,
}

impl BuiltinToolsV2 {
    /// Create built-in tools with a ToolContext
    pub fn new(context: Arc<dyn ToolContext>) -> Self {
        Self { context }
    }

    /// Register all V2 tools to a registry
    pub fn register_all(&self, registry: &ToolRegistry) {
        // TODO: Register V2 versions of tools
        // This will be filled in as we update each tool

        // For now, just a placeholder
        tracing::debug!("BuiltinToolsV2::register_all called (tools not yet implemented)");
    }
}
```

**Step 3: Run cargo check**

```bash
cargo check -p pattern_core
```

**Step 4: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/mod.rs
git commit -m "feat(tools): add BuiltinToolsV2 struct for ToolContext-based tools"
```

---

### Task E2.2: Update ContextTool to use ToolContext

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/context.rs`

**Step 1: Add ToolContext import and create V2 struct**

At the top of `context.rs`, add:

```rust
use crate::runtime::ToolContext;
use std::sync::Arc;
```

After `ContextTool` struct, add:

```rust
/// V2 Context tool using ToolContext instead of AgentHandle
pub struct ContextToolV2 {
    ctx: Arc<dyn ToolContext>,
}

impl ContextToolV2 {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }
}
```

**Step 2: Implement AiTool for ContextToolV2**

Copy the `AiTool` impl from `ContextTool` and adapt it:

```rust
#[async_trait]
impl AiTool for ContextToolV2 {
    type Input = ContextInput;
    type Output = ContextOutput;

    fn name(&self) -> &str {
        "context"
    }

    fn description(&self) -> &str {
        // Same as ContextTool
        "Manage the context/working memory..."
    }

    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        let agent_id = self.ctx.agent_id();
        let memory = self.ctx.memory();

        match params.operation {
            CoreMemoryOperationType::Append => {
                let name = params.name.ok_or_else(|| /* error */)?;
                let content = params.content.ok_or_else(|| /* error */)?;

                // Use memory store directly
                if let Some(mut block) = memory.get_block(agent_id, &name).await
                    .map_err(|e| CoreError::MemoryError(e.to_string()))?
                {
                    // Append to block
                    // ... adapt from ContextTool
                }
                // ...
            }
            // ... other operations
        }
    }
}
```

**Note:** This is complex - the full implementation requires adapting all operations. The key changes are:
- Replace `self.handle.memory` with `self.ctx.memory()`
- Replace `self.handle.agent_id()` with `self.ctx.agent_id()`
- Use MemoryStore methods directly instead of AgentHandle convenience methods

**Step 3: Run cargo check**

```bash
cargo check -p pattern_core
```

Fix errors as they appear. This will likely require multiple iterations.

**Step 4: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/context.rs
git commit -m "feat(tools): add ContextToolV2 using ToolContext"
```

---

### Task E2.3: Update RecallTool to use ToolContext

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/recall.rs`

Follow the same pattern as E2.2:
1. Add `RecallToolV2` struct with `Arc<dyn ToolContext>`
2. Implement `AiTool` adapting memory operations
3. Replace `handle.insert_archival_memory()` with `ctx.memory().insert_archival()`
4. Replace `handle.get_archival_memory_by_label()` with `ctx.memory().get_archival()`

**Commit message:** `feat(tools): add RecallToolV2 using ToolContext`

---

### Task E2.4: Update SearchTool to use ToolContext

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/search.rs`

Key changes:
1. Add `SearchToolV2` struct with `Arc<dyn ToolContext>`
2. Use `ctx.search()` with appropriate `SearchScope`:
   - Archival search: `SearchScope::CurrentAgent`
   - Conversation search: `SearchScope::CurrentAgent` with message filter
   - Constellation search: `SearchScope::Constellation`

**Commit message:** `feat(tools): add SearchToolV2 using ToolContext`

---

### Task E2.5: Update SendMessageTool to use ToolContext

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/send_message.rs`

Key changes:
1. Add `SendMessageToolV2` struct with `Arc<dyn ToolContext>`
2. Replace `handle.message_router()` with `ctx.router()`

**Commit message:** `feat(tools): add SendMessageToolV2 using ToolContext`

---

### Task E2.6: Complete BuiltinToolsV2 registration

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs`

Update `BuiltinToolsV2::register_all()`:

```rust
pub fn register_all(&self, registry: &ToolRegistry) {
    registry.register_dynamic(Box::new(DynamicToolAdapter::new(
        ContextToolV2::new(Arc::clone(&self.context))
    )));
    registry.register_dynamic(Box::new(DynamicToolAdapter::new(
        RecallToolV2::new(Arc::clone(&self.context))
    )));
    registry.register_dynamic(Box::new(DynamicToolAdapter::new(
        SearchToolV2::new(Arc::clone(&self.context))
    )));
    registry.register_dynamic(Box::new(DynamicToolAdapter::new(
        SendMessageToolV2::new(Arc::clone(&self.context))
    )));
}
```

**Commit message:** `feat(tools): complete BuiltinToolsV2 registration`

---

### Task E2.7: Run all tool tests

```bash
cargo test -p pattern_core tool -- --nocapture
```

Fix any failing tests. Create mock ToolContext if needed for tests.

**Commit message:** `test(tools): ensure all tool tests pass with V2 implementations`

---

## E2.5: Trait Swap

### Task E2.5.1: Backup old agent files

**Step 1: Move old files out of source tree**

```bash
cd crates/pattern_core/src

# Backup old DatabaseAgent
mv agent/impls/db_agent.rs agent/impls/db_agent_v1.rs.bak

# Backup AgentHandle (context state)
mv context/state.rs context/state_v1.rs.bak
```

**Step 2: Verify files are moved**

```bash
ls agent/impls/*.bak
ls context/*.bak
```

Expected: See the .bak files listed.

**Step 3: Commit the backup**

```bash
git add -A
git commit -m "chore: backup V1 agent files before trait swap"
```

---

### Task E2.5.2: Move V2 files to primary locations

**Step 1: Move agent_v2 files into agent**

```bash
cd crates/pattern_core/src

# Move V2 database agent
mv agent_v2/db_agent.rs agent/impls/db_agent.rs

# Move V2 traits (may merge into agent/mod.rs)
mv agent_v2/traits.rs agent/traits_v2.rs

# Move collect helper
mv agent_v2/collect.rs agent/collect.rs
```

**Step 2: Update agent/impls/mod.rs**

Edit to export from the new location:

```rust
mod db_agent;
pub use db_agent::{DatabaseAgent, DatabaseAgentBuilder};
// Remove old exports if any
```

Note: File names and structure may vary. Adjust based on actual file contents.

**Step 3: Commit the move**

```bash
git add -A
git commit -m "refactor: move V2 agent files to primary locations"
```

---

### Task E2.5.3: Rename V2 types (remove V2 suffix)

**Files:**
- Modify: `crates/pattern_core/src/agent/impls/db_agent.rs`
- Modify: `crates/pattern_core/src/agent/traits_v2.rs` (or wherever AgentV2 trait is)
- Modify: `crates/pattern_core/src/agent/collect.rs`

**Step 1: In db_agent.rs, rename:**
- `DatabaseAgentV2` → `DatabaseAgent`
- `DatabaseAgentV2Builder` → `DatabaseAgentBuilder`

Use find/replace:
```bash
sed -i 's/DatabaseAgentV2/DatabaseAgent/g' agent/impls/db_agent.rs
```

**Step 2: In traits file, rename:**
- `AgentV2` → `Agent`
- `AgentV2Ext` → `AgentExt`

**Step 3: In collect.rs:**
- Update any references to `AgentV2`

**Step 4: Commit**

```bash
git add -A
git commit -m "refactor: remove V2 suffix from agent types"
```

---

### Task E2.5.4: Update agent/mod.rs exports

**Files:**
- Modify: `crates/pattern_core/src/agent/mod.rs`

**Step 1: Remove old trait export, add new**

Update the exports to use the new trait:

```rust
mod entity;
mod impls;
mod traits_v2;  // or wherever the new Agent trait is
mod collect;
#[cfg(test)]
mod tests;
pub mod tool_rules;

// Export new Agent trait (was AgentV2)
pub use traits_v2::{Agent, AgentExt};

// Export new DatabaseAgent (was DatabaseAgentV2)
pub use impls::{DatabaseAgent, DatabaseAgentBuilder};

// Export collect helper
pub use collect::collect_response;

// Keep existing exports that are still valid
pub use entity::{
    AgentMemoryRelation, AgentRecord, SnowflakePosition,
    get_next_message_position, get_next_message_position_string,
    get_next_message_position_sync,
};
pub use tool_rules::{/* ... */};
```

**Step 2: Comment out or remove old Agent trait**

If the old Agent trait was in this file, comment it out or remove it.

**Step 3: Commit**

```bash
git add crates/pattern_core/src/agent/mod.rs
git commit -m "refactor: update agent module exports for new trait"
```

---

### Task E2.5.5: Update lib.rs exports

**Files:**
- Modify: `crates/pattern_core/src/lib.rs`

**Step 1: Remove pub mod agent_v2**

Find and remove or comment out:
```rust
// pub mod agent_v2;  // REMOVE THIS
```

**Step 2: Ensure agent module exports the new types**

The agent module should now export `Agent`, `AgentExt`, `DatabaseAgent`.

**Step 3: Run cargo check and note errors**

```bash
cargo check -p pattern_core 2>&1 | head -100
```

Expected: Many errors about trait mismatches. This is correct - we'll fix them in E3-E6.

**Step 4: Commit**

```bash
git add crates/pattern_core/src/lib.rs
git commit -m "refactor: remove agent_v2 module, use agent module for new trait"
```

---

### Task E2.5.6: Document expected compilation errors

**Step 1: Run cargo check and save output**

```bash
cargo check -p pattern_core 2>&1 | tee /tmp/e25-errors.txt
```

**Step 2: Categorize errors**

Look for patterns:
- "trait Agent has no method X" → Expected, fix in E3/E4
- "struct DatabaseAgent has no field X" → Expected, fix in E3/E4
- Missing imports → Fix now
- Unexpected errors → Investigate

**Step 3: Commit the current state**

```bash
git add -A
git commit -m "chore: trait swap complete, compilation errors expected

Errors will be fixed in E3-E6:
- Heartbeat processor (E3)
- Coordination patterns (E4)
- Message queue (E5)
- CLI/Discord (E6)
"
```

---

## E3: Update Heartbeat Processor

### Task E3.1: Fix heartbeat.rs for new Agent trait

**Files:**
- Modify: `crates/pattern_core/src/context/heartbeat.rs`

**Step 1: Update trait import**

Change:
```rust
use crate::agent::{Agent, AgentState, ResponseEvent};
```

(This should now import the new slim Agent trait)

**Step 2: Update process_heartbeats function signature**

The agents parameter should work with new trait:
```rust
pub async fn process_heartbeats<F, Fut>(
    mut heartbeat_rx: HeartbeatReceiver,
    agents: Vec<std::sync::Arc<dyn Agent>>,  // Now uses new Agent trait
    event_handler: F,
)
```

**Step 3: Fix method calls on agent**

Find calls to old methods and update:
- `agent.process_message_stream()` → `agent.process()`
- `agent.state()` → Check new signature, may be `agent.state().await`
- `agent.name()` → Now returns `&str`, add `.to_string()` where needed

**Step 4: Fix state watching**

The new Agent trait has `state()` that returns `AgentState`. Check if there's a watcher mechanism. If not, adapt:

```rust
// Old: let (state, maybe_receiver) = agent.state().await;
// New:
let state = agent.state().await;
// If we need to wait for Ready, we may need a different approach
// or add state_watcher() to the trait
```

**Step 5: Run cargo check**

```bash
cargo check -p pattern_core
```

**Step 6: Commit**

```bash
git add crates/pattern_core/src/context/heartbeat.rs
git commit -m "fix(heartbeat): update for new Agent trait"
```

---

## E4: Update Coordination Patterns

### Task E4.1: Update coordination/groups.rs

Replace `process_message_stream()` with `process()`, handle `name() -> &str`.

### Task E4.2: Update each pattern file

For each file in `coordination/patterns/`:
- dynamic.rs
- round_robin.rs
- supervisor.rs
- voting.rs
- pipeline.rs
- sleeptime.rs

Same changes: `process_message_stream()` → `process()`, `name()` → add `.to_string()`.

### Task E4.3: Update each selector file

For each file in `coordination/selectors/`:
- random.rs
- capability.rs
- load_balancing.rs
- supervisor.rs

Same changes as patterns.

---

## E5: Port QueuedMessage/ScheduledWakeup to pattern_db

### Task E5.1: Create migration file

Create `crates/pattern_db/migrations/XXXX_scheduled_wakeups.sql` with schema from plan.

### Task E5.2: Create wakeup model

Create `crates/pattern_db/src/models/wakeup.rs`.

### Task E5.3: Create wakeup queries

Create `crates/pattern_db/src/queries/wakeup.rs`.

### Task E5.4: Update message_queue.rs

Replace Entity/SurrealDB usage with pattern_db queries.

---

## E6: Create RuntimeContext, Update CLI/Discord

### Task E6.1: Create RuntimeContext struct

**Files:**
- Create: `crates/pattern_core/src/runtime/context.rs`

### Task E6.2: Implement RuntimeContextBuilder

### Task E6.3: Implement load_agent and create_agent methods

### Task E6.4: Add config versioning and migration

### Task E6.5: Update CLI agent_ops.rs to use RuntimeContext

### Task E6.6: Update Discord bot.rs to use RuntimeContext

---

## Final Validation

### Task Final.1: Run full test suite

```bash
cargo test -p pattern_core
cargo test -p pattern_db
cargo test -p pattern_cli
```

### Task Final.2: Run cargo check on all crates

```bash
cargo check --workspace
```

### Task Final.3: Cleanup

Delete .bak files after everything works:
```bash
find . -name "*.bak" -delete
```

---

## Execution Notes

1. **Pre-E1 and E1-E2**: Can run without breaking compilation
2. **E2.5**: This WILL break compilation - that's expected
3. **E3-E5**: Fix based on compiler errors, can be parallel
4. **E6**: Depends on E3-E5 being mostly done
5. **After each task**: Run `cargo check` to verify progress
