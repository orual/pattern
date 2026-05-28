# Delegation and orchestration system

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable agents to delegate work to other agents or spawn subagents, with streaming feedback and long-running orchestration.

**Architecture:** Builds on existing coordination infrastructure. Adds delegation tracking, subagent lifecycle management with inherited memory access, goal-based orchestration, and internal triggers.

**Tech Stack:** SQLite (pattern_db), existing agent/group infrastructure (pattern_core), streaming via existing event system.

---

## Summary

Extend the agent coordination system to support:
1. **Delegations** - Agent A requests work from Agent B (or spawns subagent), gets structured result
2. **Subagents** - Ephemeral or persistent specialized agents with inherited read-only memory
3. **Goals** - Long-running orchestration with hierarchical task breakdown
4. **Internal triggers** - Recurring/event-driven agent-internal tasks

All operations are intra-constellation (single database), but can cross group boundaries.

## Problem

Current coordination infrastructure supports:
- `coordination_tasks` - Simple task assignment (no delegation tracking, no feedback)
- `handoff_notes` - Informal notes (no structured results, no streaming)
- Group patterns - Supervisor/RoundRobin/etc (message routing, not task delegation)

Missing capabilities:
- **Delegation with feedback** - Request work, get structured result back
- **Subagent spawning** - Create specialized agent for specific work
- **Inherited context** - Subagents shouldn't need explicit context packaging
- **Progress streaming** - Real-time updates during long work
- **Goal orchestration** - Break complex work into delegations over time
- **Triggered tasks** - Agent-internal cron/event-driven work

## Solution

### New database tables

#### `delegations`

```sql
CREATE TABLE delegations (
    id TEXT PRIMARY KEY,
    requesting_agent TEXT NOT NULL,

    -- Target can be: existing agent, group, or spawned subagent
    target_type TEXT NOT NULL,  -- 'agent', 'group', 'subagent'
    target_agent_id TEXT,       -- for agent/subagent targets
    target_group_id TEXT,       -- for group targets

    -- Request details
    description TEXT NOT NULL,
    context JSON NOT NULL,              -- relevant state/memory refs
    expected_output TEXT,               -- schema/format hint
    constraints JSON,                   -- boundaries, requirements

    -- Execution
    status TEXT NOT NULL DEFAULT 'pending',  -- pending, accepted, in_progress, completed, failed, cancelled
    priority TEXT NOT NULL DEFAULT 'medium',
    deadline TEXT,                      -- optional soft deadline (hint)
    timeout_seconds INTEGER,            -- optional hard timeout

    -- Result (populated on completion)
    result_summary TEXT,
    result_output JSON,
    result_artifacts JSON,              -- memory block refs, file refs
    follow_up TEXT,                     -- suggested next steps

    -- Metadata
    parent_goal_id TEXT,                -- if spawned from a goal
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,

    FOREIGN KEY (requesting_agent) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (target_agent_id) REFERENCES agents(id) ON DELETE SET NULL,
    FOREIGN KEY (target_group_id) REFERENCES agent_groups(id) ON DELETE SET NULL,
    FOREIGN KEY (parent_goal_id) REFERENCES agent_goals(id) ON DELETE SET NULL
);

CREATE INDEX idx_delegations_requesting ON delegations(requesting_agent, status);
CREATE INDEX idx_delegations_target ON delegations(target_agent_id, status);
CREATE INDEX idx_delegations_goal ON delegations(parent_goal_id);
```

#### `delegation_updates` (streaming progress)

```sql
CREATE TABLE delegation_updates (
    id TEXT PRIMARY KEY,
    delegation_id TEXT NOT NULL,

    update_type TEXT NOT NULL,  -- 'status_change', 'progress', 'partial_result', 'blocker', 'note'
    content JSON NOT NULL,

    created_at TEXT NOT NULL,

    FOREIGN KEY (delegation_id) REFERENCES delegations(id) ON DELETE CASCADE
);

CREATE INDEX idx_delegation_updates_delegation ON delegation_updates(delegation_id, created_at);
```

#### `subagents` (ephemeral/persistent spawned agents)

```sql
CREATE TABLE subagents (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL UNIQUE,      -- FK to agents table
    spawned_by TEXT NOT NULL,           -- parent agent

    lifecycle TEXT NOT NULL,            -- 'ephemeral', 'persistent'
    specialization TEXT,                -- domain description

    -- Memory inheritance
    inherited_blocks JSON NOT NULL,     -- list of block IDs with read-only access
    inherit_mode TEXT NOT NULL,         -- 'explicit', 'parent_core', 'parent_all'

    -- Lifecycle tracking
    spawn_delegation_id TEXT,           -- delegation that spawned this subagent
    last_active TEXT,
    total_delegations INTEGER DEFAULT 0,

    created_at TEXT NOT NULL,

    FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (spawned_by) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (spawn_delegation_id) REFERENCES delegations(id) ON DELETE SET NULL
);

CREATE INDEX idx_subagents_spawned_by ON subagents(spawned_by);
CREATE INDEX idx_subagents_lifecycle ON subagents(lifecycle);
```

#### `agent_goals` (long-running orchestration)

```sql
CREATE TABLE agent_goals (
    id TEXT PRIMARY KEY,
    owner_agent TEXT NOT NULL,

    title TEXT NOT NULL,
    description TEXT,

    status TEXT NOT NULL DEFAULT 'active',  -- active, paused, completed, failed, cancelled

    -- Progress tracking
    steps_total INTEGER,                -- NULL if unknown
    steps_completed INTEGER DEFAULT 0,
    progress_notes TEXT,
    blockers JSON,                      -- list of blocker descriptions

    -- Hierarchy
    parent_goal_id TEXT,

    -- Timing
    deadline TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,

    FOREIGN KEY (owner_agent) REFERENCES agents(id) ON DELETE CASCADE,
    FOREIGN KEY (parent_goal_id) REFERENCES agent_goals(id) ON DELETE CASCADE
);

CREATE INDEX idx_goals_owner ON agent_goals(owner_agent, status);
CREATE INDEX idx_goals_parent ON agent_goals(parent_goal_id);
```

#### `internal_triggers` (recurring/event-driven)

```sql
CREATE TABLE internal_triggers (
    id TEXT PRIMARY KEY,
    owner_agent TEXT NOT NULL,

    name TEXT NOT NULL,
    description TEXT,

    -- Trigger specification
    trigger_type TEXT NOT NULL,     -- 'cron', 'interval', 'event', 'condition'
    trigger_config JSON NOT NULL,   -- type-specific config

    -- Action specification
    action_type TEXT NOT NULL,      -- 'delegation', 'goal_update', 'handoff', 'custom'
    action_config JSON NOT NULL,    -- type-specific config

    -- State
    enabled INTEGER NOT NULL DEFAULT 1,
    in_flight INTEGER NOT NULL DEFAULT 0,  -- prevents re-firing while processing
    last_fired TEXT,
    next_fire TEXT,                 -- precomputed for cron/interval
    fire_count INTEGER DEFAULT 0,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    FOREIGN KEY (owner_agent) REFERENCES agents(id) ON DELETE CASCADE
);

CREATE INDEX idx_triggers_owner ON internal_triggers(owner_agent, enabled);
CREATE INDEX idx_triggers_next ON internal_triggers(next_fire) WHERE enabled = 1;
```

### Core types (pattern_core)

#### Delegation types

```rust
// crates/pattern_core/src/coordination/delegation.rs

use crate::prelude::*;

/// Unique identifier for a delegation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DelegationId(pub String);

impl DelegationId {
    pub fn new() -> Self {
        Self(crate::id::generate_id("del"))
    }
}

/// Target for a delegation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DelegationTarget {
    /// Delegate to an existing agent.
    Agent { agent_id: AgentId },

    /// Delegate to a group (uses group's coordination pattern).
    Group { group_id: GroupId },

    /// Spawn a new subagent for this work.
    Subagent {
        config: SubagentConfig,
        /// Filled after spawning.
        #[serde(skip_serializing_if = "Option::is_none")]
        spawned_id: Option<AgentId>,
    },
}

/// Configuration for spawning a subagent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentConfig {
    /// Human-readable name for the subagent.
    pub name: String,

    /// Specialization description (becomes part of system prompt).
    pub specialization: String,

    /// Lifecycle: ephemeral (dies after delegation) or persistent (kept for reuse).
    pub lifecycle: SubagentLifecycle,

    /// How to inherit memory from parent.
    pub memory_inheritance: MemoryInheritance,

    /// Optional: base agent config to extend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_config: Option<AgentConfigRef>,

    /// Optional: explicit model override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,

    /// Optional: maximum tokens the subagent can use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,

    /// Optional: maximum execution time in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_limit_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentLifecycle {
    /// Agent is destroyed after delegation completes.
    Ephemeral,
    /// Agent persists and can be reused.
    Persistent,
}

/// How a subagent inherits memory from its parent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MemoryInheritance {
    /// Inherit specific blocks (read-only).
    Explicit { block_ids: Vec<String> },

    /// Inherit parent's core memory blocks (read-only).
    ParentCore,

    /// Inherit all of parent's memory blocks (read-only).
    ParentAll,

    /// No inheritance - subagent starts fresh.
    None,
}

/// What to do with a delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationRequest {
    /// Description of what needs to be done.
    pub description: String,

    /// Additional context (JSON blob - can include memory refs, conversation context, etc.)
    pub context: serde_json::Value,

    /// Expected output format/schema hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<String>,

    /// Constraints or boundaries for the work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,

    /// Optional soft deadline (hint to target agent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,

    /// Optional hard timeout in seconds (enforced).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,

    /// Priority level.
    #[serde(default)]
    pub priority: DelegationPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPriority {
    Low,
    #[default]
    Medium,
    High,
    Urgent,
}

/// Status of a delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    /// Waiting for target to accept.
    Pending,
    /// Target has accepted, work not started.
    Accepted,
    /// Work in progress.
    InProgress,
    /// Work completed successfully.
    Completed,
    /// Work failed.
    Failed,
    /// Delegation was cancelled.
    Cancelled,
}

/// Result of a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    /// Summary of what was accomplished.
    pub summary: String,

    /// Structured output (JSON blob).
    pub output: serde_json::Value,

    /// References to artifacts created (memory blocks, files, etc.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,

    /// Suggested follow-up actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
}

/// Reference to an artifact produced by delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactRef {
    MemoryBlock { block_id: String },
    File { path: String },
    Custom { kind: String, reference: String },
}

/// Full delegation record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub id: DelegationId,
    pub requesting_agent: AgentId,
    pub target: DelegationTarget,
    pub request: DelegationRequest,
    pub status: DelegationStatus,
    pub result: Option<DelegationResult>,
    pub parent_goal: Option<GoalId>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
```

#### Progress updates

```rust
// crates/pattern_core/src/coordination/delegation.rs (continued)

/// Update during delegation execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationUpdate {
    pub id: String,
    pub delegation_id: DelegationId,
    pub update_type: DelegationUpdateType,
    pub content: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DelegationUpdateType {
    /// Status changed (content: { "from": "...", "to": "..." }).
    StatusChange,

    /// Progress update (content: { "steps_done": N, "steps_total": M, "note": "..." }).
    Progress,

    /// Partial result available (content: partial result JSON).
    PartialResult,

    /// Blocker encountered (content: { "description": "...", "severity": "..." }).
    Blocker,

    /// Informational note (content: { "note": "..." }).
    Note,
}
```

#### Goal types

```rust
// crates/pattern_core/src/coordination/goals.rs

/// Unique identifier for a goal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoalId(pub String);

impl GoalId {
    pub fn new() -> Self {
        Self(crate::id::generate_id("goal"))
    }
}

/// Status of a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// A long-running goal that may spawn multiple delegations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentGoal {
    pub id: GoalId,
    pub owner_agent: AgentId,

    pub title: String,
    pub description: Option<String>,

    pub status: GoalStatus,
    pub progress: GoalProgress,

    pub parent_goal: Option<GoalId>,
    pub deadline: Option<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoalProgress {
    /// Total steps if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps_total: Option<u32>,

    /// Steps completed.
    #[serde(default)]
    pub steps_completed: u32,

    /// Free-form progress notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// Current blockers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
}
```

#### Trigger types

```rust
// crates/pattern_core/src/coordination/triggers.rs

/// Unique identifier for a trigger.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TriggerId(pub String);

impl TriggerId {
    pub fn new() -> Self {
        Self(crate::id::generate_id("trig"))
    }
}

/// When to fire a trigger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerType {
    /// Cron expression (e.g., "0 9 * * *" for 9am daily).
    Cron { expression: String },

    /// Fixed interval.
    Interval { seconds: u64 },

    /// Fire on specific activity event types.
    Event { event_types: Vec<String> },

    /// Fire when a condition is met (evaluated periodically).
    Condition {
        /// SQL-like condition or custom evaluator name.
        query: String,
        /// How often to check (seconds).
        check_interval: u64,
    },
}

/// What to do when trigger fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerAction {
    /// Spawn a delegation.
    Delegation { request: DelegationRequest, target: DelegationTarget },

    /// Update a goal's progress/status.
    GoalUpdate { goal_id: GoalId, action: GoalUpdateAction },

    /// Send a handoff note.
    Handoff { to: Option<AgentId>, content: String },

    /// Custom action (handler name + params).
    Custom { handler: String, params: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalUpdateAction {
    IncrementProgress { amount: u32 },
    SetProgress { completed: u32, total: Option<u32> },
    AddBlocker { description: String },
    RemoveBlocker { index: usize },
    SetStatus { status: GoalStatus },
    AddNote { note: String },
}

/// Internal trigger definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalTrigger {
    pub id: TriggerId,
    pub owner_agent: AgentId,

    pub name: String,
    pub description: Option<String>,

    pub trigger_type: TriggerType,
    pub action: TriggerAction,

    pub enabled: bool,
    /// Prevents re-firing while a previous execution is still running.
    pub in_flight: bool,
    pub last_fired: Option<DateTime<Utc>>,
    pub next_fire: Option<DateTime<Utc>>,
    pub fire_count: u64,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### Subagent spawning and memory inheritance

The key insight: subagents shouldn't need the parent to manually package context. Instead, they get automatic read-only access to relevant memory.

```rust
// crates/pattern_core/src/coordination/subagent.rs

/// Subagent metadata (stored alongside agent record).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentMetadata {
    pub agent_id: AgentId,
    pub spawned_by: AgentId,
    pub lifecycle: SubagentLifecycle,
    pub specialization: Option<String>,
    pub memory_inheritance: MemoryInheritance,
    pub spawn_delegation: Option<DelegationId>,
    pub last_active: DateTime<Utc>,
    pub total_delegations: u64,
    pub created_at: DateTime<Utc>,
}

impl SubagentMetadata {
    /// Resolve inherited block IDs based on inheritance mode.
    pub async fn resolve_inherited_blocks(
        &self,
        pool: &SqlitePool,
    ) -> DbResult<Vec<String>> {
        match &self.memory_inheritance {
            MemoryInheritance::Explicit { block_ids } => Ok(block_ids.clone()),

            MemoryInheritance::ParentCore => {
                // Get parent's core memory blocks
                queries::memory::get_agent_blocks_by_type(
                    pool,
                    &self.spawned_by.0,
                    BlockType::Core,
                ).await
            }

            MemoryInheritance::ParentAll => {
                // Get all parent's memory blocks
                queries::memory::get_agent_block_ids(pool, &self.spawned_by.0).await
            }

            MemoryInheritance::None => Ok(vec![]),
        }
    }
}

/// Spawn a subagent for delegation.
pub async fn spawn_subagent(
    pool: &SqlitePool,
    parent_agent: &AgentId,
    config: &SubagentConfig,
    delegation_id: Option<&DelegationId>,
) -> Result<(AgentId, SubagentMetadata), CoreError> {
    // 1. Create base agent config
    let agent_config = build_subagent_config(parent_agent, config).await?;

    // 2. Insert agent record
    let agent_id = AgentId::new();
    let agent = Agent {
        id: agent_id.0.clone(),
        name: config.name.clone(),
        description: Some(config.specialization.clone()),
        // ... fill from config
    };
    queries::agent::create_agent(pool, &agent).await?;

    // 3. Create subagent metadata
    let metadata = SubagentMetadata {
        agent_id: agent_id.clone(),
        spawned_by: parent_agent.clone(),
        lifecycle: config.lifecycle,
        specialization: Some(config.specialization.clone()),
        memory_inheritance: config.memory_inheritance.clone(),
        spawn_delegation: delegation_id.cloned(),
        last_active: Utc::now(),
        total_delegations: 0,
        created_at: Utc::now(),
    };
    queries::coordination::create_subagent(pool, &metadata).await?;

    // 4. Set up inherited memory access (read-only)
    let inherited_blocks = metadata.resolve_inherited_blocks(pool).await?;
    for block_id in &inherited_blocks {
        queries::memory::grant_block_access(
            pool,
            block_id,
            &agent_id.0,
            Permission::ReadOnly,
        ).await?;
    }

    // 5. Create subagent's own working memory if persistent
    if config.lifecycle == SubagentLifecycle::Persistent {
        create_subagent_memory(pool, &agent_id, config).await?;
    }

    // 6. Grant parent read-write access to any memory the subagent creates
    // This is done via a trigger or hook on memory creation, not here.
    // See: memory::on_block_created() which checks for subagent relationship
    // and automatically grants parent access.

    Ok((agent_id, metadata))
}

/// Clean up ephemeral subagent after delegation.
/// Transfers memory ownership to parent before deletion.
pub async fn cleanup_ephemeral_subagent(
    pool: &SqlitePool,
    agent_id: &AgentId,
) -> Result<(), CoreError> {
    // Verify it's ephemeral
    let metadata = queries::coordination::get_subagent(pool, &agent_id.0).await?
        .ok_or(CoreError::NotFound)?;

    if metadata.lifecycle != SubagentLifecycle::Ephemeral {
        return Err(CoreError::InvalidOperation("cannot cleanup persistent subagent".into()));
    }

    // Transfer ownership of subagent's memory blocks to parent
    let subagent_blocks = queries::memory::get_agent_owned_blocks(pool, &agent_id.0).await?;
    for block_id in subagent_blocks {
        queries::memory::transfer_block_ownership(
            pool,
            &block_id,
            &metadata.spawned_by.0,
        ).await?;
    }

    // Delete agent (subagent record cascades, memory blocks now owned by parent)
    queries::agent::delete_agent(pool, &agent_id.0).await?;

    Ok(())
}
```

### Delegation execution flow

```rust
// crates/pattern_core/src/coordination/executor.rs

/// Execute a delegation request.
pub async fn execute_delegation(
    runtime: &AgentRuntime,
    delegation: &Delegation,
) -> Result<DelegationResult, CoreError> {
    let pool = runtime.pool();

    // 1. Update status to InProgress
    update_delegation_status(pool, &delegation.id, DelegationStatus::InProgress).await?;
    emit_update(pool, &delegation.id, DelegationUpdateType::StatusChange, json!({
        "from": "accepted",
        "to": "in_progress"
    })).await?;

    // 2. Resolve target agent
    let (target_agent, cleanup_after) = match &delegation.target {
        DelegationTarget::Agent { agent_id } => {
            (agent_id.clone(), false)
        }

        DelegationTarget::Group { group_id } => {
            // Use group's coordination pattern to select agent
            let agent_id = select_agent_from_group(pool, group_id, &delegation.request).await?;
            (agent_id, false)
        }

        DelegationTarget::Subagent { config, spawned_id } => {
            let agent_id = match spawned_id {
                Some(id) => id.clone(),
                None => {
                    // Spawn subagent
                    let (id, _) = spawn_subagent(
                        pool,
                        &delegation.requesting_agent,
                        config,
                        Some(&delegation.id),
                    ).await?;

                    // Update delegation with spawned ID
                    update_delegation_spawned_id(pool, &delegation.id, &id).await?;
                    id
                }
            };
            (agent_id, config.lifecycle == SubagentLifecycle::Ephemeral)
        }
    };

    // 3. Build message for target agent
    let message = build_delegation_message(&delegation.request).await?;

    // 4. Get target agent and process
    let agent = runtime.get_agent(&target_agent).await?;
    let mut stream = agent.process(message).await?;

    // 5. Stream progress updates
    let mut last_text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            ResponseEvent::TextChunk { text, .. } => {
                last_text.push_str(&text);
                // Periodic progress update
                if last_text.len() % 500 == 0 {
                    emit_update(pool, &delegation.id, DelegationUpdateType::Progress, json!({
                        "note": "Processing...",
                        "output_length": last_text.len()
                    })).await?;
                }
            }
            ResponseEvent::ToolCallCompleted { result, .. } => {
                // Emit partial result for significant tool completions
                if let Ok(output) = &result {
                    emit_update(pool, &delegation.id, DelegationUpdateType::PartialResult, json!({
                        "tool_output": output
                    })).await?;
                }
            }
            ResponseEvent::Complete { .. } => break,
            ResponseEvent::Error { error, .. } => {
                update_delegation_status(pool, &delegation.id, DelegationStatus::Failed).await?;
                if cleanup_after {
                    cleanup_ephemeral_subagent(pool, &target_agent).await?;
                }
                return Err(CoreError::DelegationFailed(error));
            }
            _ => {}
        }
    }

    // 6. Extract result from agent's response
    let result = extract_delegation_result(&last_text, &target_agent, pool).await?;

    // 7. Update delegation with result
    complete_delegation(pool, &delegation.id, &result).await?;

    // 8. Cleanup ephemeral subagent
    if cleanup_after {
        cleanup_ephemeral_subagent(pool, &target_agent).await?;
    }

    Ok(result)
}

/// Subscribe to delegation progress updates.
pub fn subscribe_delegation_updates(
    pool: &SqlitePool,
    delegation_id: &DelegationId,
) -> impl Stream<Item = DelegationUpdate> {
    // Implementation uses polling or change notification
    // Returns stream of DelegationUpdate as they're created
    todo!()
}
```

### Trigger execution

```rust
// crates/pattern_core/src/coordination/trigger_executor.rs

/// Check and fire due triggers for an agent.
pub async fn check_triggers(
    runtime: &AgentRuntime,
    agent_id: &AgentId,
) -> Result<Vec<TriggerId>, CoreError> {
    let pool = runtime.pool();
    let now = Utc::now();

    // Get triggers due to fire (excludes in_flight triggers)
    let due_triggers = queries::coordination::get_due_triggers(pool, &agent_id.0, now).await?;

    let mut fired = Vec::new();

    for trigger in due_triggers {
        // Mark as in_flight before firing (prevents re-triggering)
        queries::coordination::set_trigger_in_flight(pool, &trigger.id.0, true).await?;

        let runtime_clone = runtime.clone();
        let trigger_id = trigger.id.clone();
        let pool_clone = pool.clone();

        // Execute async - will clear in_flight when done
        tokio::spawn(async move {
            let result = fire_trigger(&runtime_clone, &trigger).await;

            // Update state regardless of success/failure
            let now = Utc::now();
            let next = compute_next_fire(&trigger.trigger_type, now);

            if let Err(e) = queries::coordination::update_trigger_after_fire(
                &pool_clone,
                &trigger_id.0,
                now,
                next,
                false, // clear in_flight
            ).await {
                tracing::error!(trigger_id = %trigger_id.0, error = %e, "failed to update trigger state");
            }

            if let Err(e) = result {
                tracing::error!(trigger_id = %trigger_id.0, error = %e, "trigger execution failed");
            }
        });

        fired.push(trigger.id);
    }

    Ok(fired)
}

async fn fire_trigger(
    runtime: &AgentRuntime,
    trigger: &InternalTrigger,
) -> Result<(), CoreError> {
    match &trigger.action {
        TriggerAction::Delegation { request, target } => {
            let delegation = create_delegation(
                runtime.pool(),
                &trigger.owner_agent,
                target.clone(),
                request.clone(),
                None, // no parent goal
            ).await?;

            // Execute async (don't block trigger processing)
            let runtime = runtime.clone();
            tokio::spawn(async move {
                if let Err(e) = execute_delegation(&runtime, &delegation).await {
                    tracing::error!(delegation_id = %delegation.id.0, error = %e, "triggered delegation failed");
                }
            });
        }

        TriggerAction::GoalUpdate { goal_id, action } => {
            apply_goal_update(runtime.pool(), goal_id, action).await?;
        }

        TriggerAction::Handoff { to, content } => {
            queries::coordination::create_handoff(
                runtime.pool(),
                &HandoffNote {
                    id: generate_id("hnd"),
                    from_agent: trigger.owner_agent.0.clone(),
                    to_agent: to.as_ref().map(|a| a.0.clone()),
                    content: content.clone(),
                    created_at: Utc::now(),
                    read_at: None,
                },
            ).await?;
        }

        TriggerAction::Custom { handler, params } => {
            // Look up custom handler and execute
            let handler_fn = runtime.get_trigger_handler(handler)?;
            handler_fn(&trigger.owner_agent, params).await?;
        }
    }

    Ok(())
}

fn compute_next_fire(trigger_type: &TriggerType, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match trigger_type {
        TriggerType::Cron { expression } => {
            // Parse cron and compute next
            cron::Schedule::from_str(expression)
                .ok()
                .and_then(|s| s.after(&now).next())
        }

        TriggerType::Interval { seconds } => {
            Some(now + chrono::Duration::seconds(*seconds as i64))
        }

        // Event and Condition triggers don't have scheduled next_fire
        TriggerType::Event { .. } | TriggerType::Condition { .. } => None,
    }
}
```

### Integration with existing coordination

This system layers on top of existing coordination rather than replacing it:

| Existing | New | Relationship |
|----------|-----|--------------|
| `coordination_tasks` | `delegations` | Tasks are simpler, delegations have full lifecycle |
| `handoff_notes` | `delegation_updates` | Handoffs are informal, updates are structured |
| `activity_events` | `delegation_updates` | Activity is audit log, updates are real-time |
| Group patterns | Delegation to groups | Patterns route messages, delegations are task-oriented |

The existing `coordination_tasks` can remain for simple "assign and track" use cases. Delegations are for richer agent-to-agent work with structured feedback.

### Tools for agents

Agents need tools to use this system. Following existing patterns (e.g. `block_edit`), use mode-based tools:

```rust
// Tool: delegation
// Modes:
//   - create: spawn delegation to agent/group/subagent
//   - list: list delegations (filter by status, target, requester)
//   - get: get delegation details + updates
//   - update: report progress, partial results, blockers (for target agent)
//   - cancel: cancel a pending/in-progress delegation (for requester)
//   - complete: mark delegation complete with result (for target agent)

// Tool: goal
// Modes:
//   - create: create new goal (optionally as subgoal)
//   - list: list goals (filter by status, owner)
//   - get: get goal details + child goals + delegations
//   - update: update progress, add/remove blockers, set status
//   - delete: remove goal (cascades to children)

// Tool: trigger
// Modes:
//   - create: set up new trigger (cron/interval/event/condition)
//   - list: list triggers (filter by enabled, owner)
//   - get: get trigger details + fire history
//   - update: modify trigger config
//   - enable: enable trigger
//   - disable: disable trigger
//   - delete: remove trigger
//   - fire: manually fire trigger (for testing)
```

---

## Implementation steps

### Phase 1: Database schema

1. Add migration for new tables (`delegations`, `delegation_updates`, `subagents`, `agent_goals`, `internal_triggers`)
2. Add indexes
3. Run `sqlx prepare`

### Phase 2: Core types (pattern_core)

1. Create `coordination/delegation.rs` with delegation types
2. Create `coordination/goals.rs` with goal types
3. Create `coordination/triggers.rs` with trigger types
4. Create `coordination/subagent.rs` with subagent types
5. Update `coordination/mod.rs` to re-export

### Phase 3: Database queries (pattern_db)

1. Add `queries/delegation.rs` - CRUD for delegations and updates
2. Add `queries/goals.rs` - CRUD for goals
3. Add `queries/triggers.rs` - CRUD for triggers
4. Extend `queries/agent.rs` - subagent-specific queries
5. Run `sqlx prepare`

### Phase 4: Subagent spawning

1. Implement `spawn_subagent()` with memory inheritance
2. Implement `cleanup_ephemeral_subagent()`
3. Add tests for various inheritance modes

### Phase 5: Delegation execution

1. Implement `execute_delegation()` with streaming updates
2. Implement `subscribe_delegation_updates()` for consumers
3. Add delegation to group routing
4. Add tests

### Phase 6: Trigger execution

1. Implement `check_triggers()` and `fire_trigger()`
2. Implement `compute_next_fire()` with cron support (add `cron` crate)
3. Add trigger checking to agent lifecycle (called periodically)
4. Add tests

### Phase 7: Agent tools

1. Implement `delegation` tool with modes (create, list, get, update, cancel, complete)
2. Implement `goal` tool with modes (create, list, get, update, delete)
3. Implement `trigger` tool with modes (create, list, get, update, enable, disable, delete, fire)

### Phase 8: Integration

1. Wire trigger checking into agent runtime
2. Add delegation status to agent context (so agents know about pending work)
3. Update sleeptime pattern to use triggers

---

## Testing

- Unit tests for each type's serialization
- Unit tests for memory inheritance resolution
- Integration tests for delegation execution flow
- Integration tests for subagent spawn/cleanup
- Integration tests for trigger firing
- Property tests for cron next-fire computation

## Design decisions

1. **Delegation timeout** - Optional timeout field on delegations. On timeout: mark as failed, emit update, cleanup ephemeral subagent. Deadlines are soft hints, timeouts are hard limits.

2. **Subagent resource limits** - Optional token budget and time limit fields on `SubagentConfig`. Enforced during execution. Ephemeral subagents especially benefit from limits.

3. **Trigger backpressure** - Triggers cannot re-fire while a previous execution is still processing. Acts like a queued message. Add `in_flight` boolean to trigger state.

4. **Cross-group visibility** - Within a constellation, all agents can see all delegations regardless of group membership. Groups are for coordination patterns, not access control.

5. **Ephemeral memory transfer** - On ephemeral subagent cleanup, ownership of all subagent-created memory blocks transfers to the parent agent. Parent can consolidate/dedup later if desired. Archival entries transfer cleanly, working memory blocks may need agent attention.

6. **Parent access to subagent memory** - Parent always has read-write access to any memory blocks created by their subagents (both ephemeral and persistent).

---

Plan complete and saved to `docs/plans/2026-01-04-delegation-orchestration-system.md`.

**Two execution options:**

1. **Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

2. **Parallel Session (separate)** - Open new session in worktree with executing-plans, batch execution with checkpoints

Which approach? Or want to discuss the open questions / refine the design first?