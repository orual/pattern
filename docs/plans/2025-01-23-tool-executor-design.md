# ToolExecutor Design

## Overview

The ToolExecutor is a component of AgentRuntime that handles tool execution with:
- **Rule validation** (ToolRuleEngine) - prevent invalid sequences, enforce constraints
- **Permission arbitration** (PermissionBroker) - consent flow for sensitive tools
- **Deduplication** - avoid redundant executions within time window
- **Timeout handling** - prevent hung tools
- **Continuation tracking** - heartbeat/ContinueLoop logic

## State Scoping

Three categories of state with different lifecycles:

### Per-Process State (resets each `process()` call, including heartbeats)
Ordering and sequencing within a single processing invocation:
- `execution_history: Vec<ToolExecution>` - for ordering constraints
- `start_constraints_satisfied: bool`
- `exit_requirements_pending: Vec<String>`
- `phase: ExecutionPhase`

### Per-Batch State (survives heartbeat continuations within same batch)
Loop prevention that must span heartbeat continuations:
- `call_counts: HashMap<String, u32>` - for MaxCalls rule
- `exclusive_groups_used: HashMap<String, String>` - group_id -> tool_name

This state is keyed by batch_id and cached on ToolExecutor:
```rust
batch_constraints: DashMap<SnowflakePosition, BatchConstraints>
```

### Persistent State (lives on ToolExecutor, spans all batches)
These span across batches:
- `dedupe_cache: DashMap<String, Instant>` - recent executions by canonical key
- `last_execution: DashMap<String, Instant>` - for Cooldown rules
- `standing_grants: DashMap<String, PermissionGrant>` - standing approvals only

## Architecture

```
AgentRuntime
    ├── tools: Arc<ToolRegistry>
    ├── tool_executor: ToolExecutor
    │       ├── tools: Arc<ToolRegistry>  (shared ref)
    │       ├── rules: Vec<ToolRule>      (from agent config)
    │       ├── persistent_state: PersistentToolState
    │       └── config: ToolExecutorConfig
    └── ...
```

## Types

```rust
/// Configuration for tool execution behavior
pub struct ToolExecutorConfig {
    /// Timeout for individual tool execution
    pub execution_timeout: Duration,
    /// Timeout for permission request (user approval)
    pub permission_timeout: Duration,
    /// Window for deduplication (default 5 min)
    pub dedupe_window: Duration,
    /// Whether to enforce permission checks
    pub require_permissions: bool,
}

/// Per-process state (created fresh each process() call, including heartbeats)
pub struct ProcessToolState {
    /// Tools executed this process() call (for ordering constraints)
    execution_history: Vec<ToolExecution>,
    /// Whether start constraints have been satisfied this process() call
    start_constraints_done: bool,
    /// Exit requirements still pending
    exit_requirements_pending: Vec<String>,
    /// Current phase
    phase: ExecutionPhase,
}

/// Per-batch constraints (survives heartbeat continuations)
/// Keyed by batch_id on ToolExecutor
struct BatchConstraints {
    /// Call count per tool within this batch
    call_counts: HashMap<String, u32>,
    /// Which tool was used from each exclusive group
    exclusive_group_selections: HashMap<String, String>,
    /// When this batch started (for cleanup)
    created_at: Instant,
}

/// Result of executing a tool
pub struct ToolExecutionResult {
    pub response: ToolResponse,
    /// Tool requested heartbeat continuation (via request_heartbeat param)
    pub requests_continuation: bool,
    /// Tool has ContinueLoop rule (implicit continuation, no heartbeat needed)
    pub has_continue_rule: bool,
}

/// Errors during tool execution (distinct from tool returning error content)
pub enum ToolExecutionError {
    /// Tool not found in registry
    NotFound { tool_name: String, available: Vec<String> },
    /// Rule violation prevented execution
    RuleViolation(ToolRuleViolation),
    /// Permission denied or timed out
    PermissionDenied { tool_name: String, scope: PermissionScope },
    /// Execution timed out
    Timeout { tool_name: String, duration: Duration },
    /// Duplicate call within dedupe window
    Deduplicated { tool_name: String },
}
```

## ToolExecutor API

```rust
impl ToolExecutor {
    /// Create executor with rules and config
    pub fn new(
        tools: Arc<ToolRegistry>,
        rules: Vec<ToolRule>,
        config: ToolExecutorConfig,
    ) -> Self;

    /// Create fresh process state for a process() call
    pub fn new_process_state(&self) -> ProcessToolState;

    /// Execute a single tool with full checks
    ///
    /// Flow:
    /// 1. Check dedupe cache → Deduplicated error if recent duplicate
    /// 2. Check process rules (ordering) → RuleViolation if blocked
    /// 3. Check batch rules (MaxCalls, ExclusiveGroups) → RuleViolation if blocked
    /// 4. Check cooldown → RuleViolation if in cooldown
    /// 5. Check RequiresConsent → request permission if needed
    /// 6. Execute with timeout
    /// 7. Record execution (process state + batch constraints + persistent state)
    /// 8. Return result with continuation info
    pub async fn execute(
        &self,
        call: &ToolCall,
        batch_id: SnowflakePosition,
        process_state: &mut ProcessToolState,
        meta: &ExecutionMeta,
    ) -> Result<ToolExecutionResult, ToolExecutionError>;

    /// Execute multiple tools in sequence
    ///
    /// Returns (responses, needs_continuation)
    /// Stops early if a tool errors (not tool error content, but execution error)
    pub async fn execute_batch(
        &self,
        calls: &[ToolCall],
        batch_id: SnowflakePosition,
        process_state: &mut ProcessToolState,
        meta: &ExecutionMeta,
    ) -> (Vec<ToolResponse>, bool);

    /// Execute start constraint tools (tools with StartConstraint rule)
    pub async fn execute_start_constraints(
        &self,
        batch_id: SnowflakePosition,
        process_state: &mut ProcessToolState,
    ) -> Vec<ToolResponse>;

    /// Execute required exit tools (tools with RequiredBeforeExit rule)
    pub async fn execute_exit_requirements(
        &self,
        batch_id: SnowflakePosition,
        process_state: &mut ProcessToolState,
    ) -> Vec<ToolResponse>;

    /// Check if loop should exit based on process state
    pub fn should_exit_loop(&self, process_state: &ProcessToolState) -> bool;

    /// Check if tool requires heartbeat (no ContinueLoop rule)
    pub fn requires_heartbeat(&self, tool_name: &str) -> bool;

    /// Mark a batch as complete (allows cleanup of BatchConstraints)
    pub fn complete_batch(&self, batch_id: SnowflakePosition);

    /// Prune expired entries from persistent state (dedupe, cooldowns, grants, old batches)
    pub fn prune_expired(&self);
}
```

## Execution Flow Detail

```
execute(call, batch_id, process_state, meta)
│
├─► Dedupe Check (persistent)
│   └─ Build canonical key: "{tool_name}|{sorted_args_json}"
│   └─ If in dedupe_cache within window → Err(Deduplicated)
│
├─► Process Rule Validation (per-process)
│   ├─ StartConstraint: if not satisfied and not a start tool → Err
│   ├─ RequiresPrecedingTools: check process_state.execution_history → Err if missing
│   └─ RequiresFollowingTools: check none called yet → Err if violated
│
├─► Batch Rule Validation (per-batch, lookup by batch_id)
│   ├─ MaxCalls: check batch_constraints.call_counts[tool] < max → Err if exceeded
│   └─ ExclusiveGroups: check group not used by different tool → Err
│
├─► Cooldown Check (persistent)
│   └─ If last_execution[tool] + cooldown > now → Err(RuleViolation::Cooldown)
│
├─► Permission Check (RequiresConsent rule)
│   ├─ Check standing_grants for valid grant → execute if found
│   ├─ Otherwise: permission_broker.request() with timeout
│   │   ├─ ApproveOnce → execute, do NOT cache
│   │   ├─ ApproveForDuration/Scope → execute AND cache in standing_grants
│   │   └─ Deny/Timeout → Err(PermissionDenied)
│   └─ Attach grant to ExecutionMeta
│
├─► Execute Tool
│   ├─ Get tool from registry
│   ├─ tokio::time::timeout(config.execution_timeout, tool.execute(...))
│   └─ If timeout → Err(Timeout)
│
├─► Record Execution
│   ├─ process_state.execution_history.push(...)
│   ├─ batch_constraints.call_counts[tool] += 1  (by batch_id)
│   ├─ persistent.dedupe_cache.insert(canonical_key, now)
│   ├─ persistent.last_execution.insert(tool, now)
│   └─ Update process_state.phase if ExitLoop tool
│
└─► Build Result
    ├─ response: ToolResponse from execution
    ├─ requests_continuation: check_heartbeat_request(args)
    └─ has_continue_rule: !requires_heartbeat(tool)
```

## Integration with AgentRuntime

```rust
impl AgentRuntime {
    // ToolExecutor lives on Runtime
    tool_executor: ToolExecutor,
}

impl AgentRuntime {
    /// Get the tool executor
    pub fn tool_executor(&self) -> &ToolExecutor {
        &self.tool_executor
    }
}
```

## Integration with DatabaseAgentV2::process()

```rust
async fn process(self: Arc<Self>, message: Message) -> Result<...> {
    // Create fresh batch state for this process() call
    let mut batch_state = self.runtime().tool_executor().new_batch();

    // Execute start constraints if any
    let start_responses = self.runtime()
        .tool_executor()
        .execute_start_constraints(&mut batch_state)
        .await;

    // Main processing loop
    loop {
        let request = self.runtime().prepare_request(...).await?;
        let response = self.model.complete(request).await?;

        if let Some(tool_calls) = response.tool_calls() {
            let meta = ExecutionMeta { ... };
            let (responses, needs_continuation) = self.runtime()
                .tool_executor()
                .execute_batch(&tool_calls, &mut batch_state, &meta)
                .await;

            // Store tool responses...

            if needs_continuation {
                continue;  // Agent continues processing
            }
        }

        // Check if we should exit
        if self.runtime().tool_executor().should_exit_loop(&batch_state) {
            // Execute exit requirements
            let exit_responses = self.runtime()
                .tool_executor()
                .execute_exit_requirements(&mut batch_state)
                .await;
            break;
        }

        // Normal exit (no more tool calls, no continuation)
        break;
    }

    // batch_state dropped here - per-batch state gone
    // persistent state remains on tool_executor
}
```

## Rule Scoping Summary

| Rule Type | Scope | Prevents |
|-----------|-------|----------|
| MaxCalls(n) | Per-batch | Infinite loops |
| ExclusiveGroups | Per-batch | Conflicting operations |
| StartConstraint | Per-batch | Wrong ordering |
| RequiresPrecedingTools | Per-batch | Wrong ordering |
| RequiresFollowingTools | Per-batch | Wrong ordering |
| RequiredBeforeExit | Per-batch | Missing cleanup |
| ContinueLoop | N/A (static) | Unnecessary heartbeats |
| ExitLoop | Per-batch | Runaway processing |
| Cooldown(duration) | Persistent | Rate limiting |
| RequiresConsent | Persistent (grants) | Unauthorized actions |

## Permission Model for RequiresConsent

**RequiresConsent tools are rare, sensitive operations.** They ALWAYS require explicit permission unless a standing grant exists.

**Grant types:**
- `ApproveOnce` - Single use, not cached, next call requires new approval
- `ApproveForDuration(Duration)` - Standing grant with expiry, cached
- `ApproveForScope` - Standing grant for this scope, cached until revoked
- `Deny` - Rejected, not cached

**Flow:**
```
Tool has RequiresConsent rule?
│
├─► Check for standing grant (ApproveForDuration or ApproveForScope)
│   ├─ Found and not expired → Execute with grant
│   └─ Not found or expired → Request permission
│
└─► permission_broker.request(...)
    ├─ ApproveOnce → Execute, do NOT cache
    ├─ ApproveForDuration/Scope → Execute AND cache grant
    ├─ Deny → Err(PermissionDenied)
    └─ Timeout → Err(PermissionDenied)
```

**Key principle:** No implicit grant caching. Only explicit standing grants (`ApproveForDuration`, `ApproveForScope`) are cached. One-time approvals (`ApproveOnce`) are consumed immediately.

---

## Open Questions

1. **Dedupe granularity**: Current uses canonical args. Should some tools be dedupe-exempt?
   - Maybe a rule type: `NeverDedupe` or `AlwaysDedupe`

2. **Periodic tools**: The ToolRuleType::Periodic exists but isn't covered here.
   - Needs external timer to trigger, not part of execute() flow

3. **Error recovery**: If execute_batch hits an error, should it continue with remaining tools?
   - Current design: stops on execution error, continues on tool-returned error

---

## Next Steps

1. Implement `ToolExecutor` struct and `new_batch()`
2. Implement `execute()` with full flow
3. Implement `execute_batch()`, `execute_start_constraints()`, `execute_exit_requirements()`
4. Add to `AgentRuntime` via builder
5. Update `RuntimeConfig` to include `ToolExecutorConfig`
6. Tests for each rule type
