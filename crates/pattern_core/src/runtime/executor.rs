//! ToolExecutor: Rule-aware, permission-aware tool execution
//!
//! Implements three-tier state scoping:
//! - Per-process state: resets each process() call including heartbeats
//! - Per-batch state: survives heartbeat continuations within same batch
//! - Persistent state: spans all batches (dedupe, cooldowns, standing grants)

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::SnowflakePosition;
use crate::agent::tool_rules::{
    ExecutionPhase, ToolExecution, ToolRule, ToolRuleType, ToolRuleViolation,
};
use crate::messages::{ToolCall, ToolResponse};
use crate::permission::{PermissionGrant, PermissionScope, broker};
use crate::tool::{ExecutionMeta, ToolRegistry};
use crate::{AgentId, ToolCallId};

/// Configuration for tool execution behavior
#[derive(Debug, Clone)]
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

impl Default for ToolExecutorConfig {
    fn default() -> Self {
        Self {
            execution_timeout: Duration::from_secs(120),
            permission_timeout: Duration::from_secs(300),
            dedupe_window: Duration::from_secs(300),
            require_permissions: true,
        }
    }
}

/// Per-process state (created fresh each process() call, including heartbeats)
#[derive(Debug, Clone, Default)]
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

impl ProcessToolState {
    /// Create new process state
    pub fn new() -> Self {
        Self::default()
    }

    /// Get current execution phase
    pub fn phase(&self) -> &ExecutionPhase {
        &self.phase
    }

    /// Get list of tools executed this process
    pub fn executed_tools(&self) -> Vec<&str> {
        self.execution_history
            .iter()
            .map(|e| e.tool_name.as_str())
            .collect()
    }

    /// Check if a tool was executed this process
    pub fn tool_was_executed(&self, tool_name: &str) -> bool {
        self.execution_history
            .iter()
            .any(|e| e.tool_name == tool_name && e.success)
    }
}

/// Per-batch constraints (survives heartbeat continuations)
/// Keyed by batch_id on ToolExecutor
#[derive(Debug, Clone)]
struct BatchConstraints {
    /// Call count per tool within this batch
    call_counts: HashMap<String, u32>,
    /// Which tool was used from each exclusive group
    exclusive_group_selections: HashMap<String, String>,
    /// When this batch started (for cleanup)
    created_at: Instant,
}

impl Default for BatchConstraints {
    fn default() -> Self {
        Self {
            call_counts: HashMap::new(),
            exclusive_group_selections: HashMap::new(),
            created_at: Instant::now(),
        }
    }
}

/// Result of executing a tool (low-level)
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    /// The tool response
    pub response: ToolResponse,
    /// Tool requested heartbeat continuation (via request_heartbeat param)
    pub requests_continuation: bool,
    /// Tool has ContinueLoop rule (implicit continuation, no heartbeat needed)
    pub has_continue_rule: bool,
}

/// What action the processing loop should take after tool execution
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAction {
    /// Continue processing normally
    Continue,
    /// Exit the processing loop (tool triggered ExitLoop rule)
    ExitLoop,
    /// Request external heartbeat continuation
    RequestHeartbeat { tool_name: String, call_id: String },
}

/// High-level outcome of tool execution with determined action
#[derive(Debug, Clone)]
pub struct ToolExecutionOutcome {
    /// The tool response
    pub response: ToolResponse,
    /// What the processing loop should do next
    pub action: ToolAction,
}

/// Errors during tool execution (distinct from tool returning error content)
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolExecutionError {
    #[error("Tool not found: {tool_name}. Available: {available:?}")]
    NotFound {
        tool_name: String,
        available: Vec<String>,
    },

    #[error("Rule violation: {0}")]
    RuleViolation(#[from] ToolRuleViolation),

    #[error("Permission denied for tool {tool_name} (scope: {scope:?})")]
    PermissionDenied {
        tool_name: String,
        scope: PermissionScope,
    },

    #[error("Tool {tool_name} execution timed out after {duration:?}")]
    Timeout {
        tool_name: String,
        duration: Duration,
    },

    #[error("Duplicate call to {tool_name} within dedupe window")]
    Deduplicated { tool_name: String },
}

/// ToolExecutor handles tool execution with rule validation, permission arbitration,
/// deduplication, timeout handling, and continuation tracking.
pub struct ToolExecutor {
    /// Shared tool registry
    tools: Arc<ToolRegistry>,
    /// Rules from agent config
    rules: Vec<ToolRule>,
    /// Configuration
    config: ToolExecutorConfig,
    /// Agent ID for permission requests
    agent_id: AgentId,

    // === Per-batch state (keyed by batch_id) ===
    batch_constraints: DashMap<SnowflakePosition, BatchConstraints>,

    // === Persistent state ===
    /// Recent executions by canonical key for deduplication
    dedupe_cache: DashMap<String, Instant>,
    /// Last execution time per tool (for Cooldown rules)
    last_execution: DashMap<String, Instant>,
    /// Standing permission grants (ApproveForDuration, ApproveForScope only)
    standing_grants: DashMap<String, PermissionGrant>,
}

impl std::fmt::Debug for ToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolExecutor")
            .field("agent_id", &self.agent_id)
            .field("rules_count", &self.rules.len())
            .field("tools", &"<ToolRegistry>")
            .field("config", &self.config)
            .finish()
    }
}

impl ToolExecutor {
    /// Create executor with rules and config
    pub fn new(
        agent_id: AgentId,
        tools: Arc<ToolRegistry>,
        rules: Vec<ToolRule>,
        config: ToolExecutorConfig,
    ) -> Self {
        Self {
            agent_id,
            tools,
            rules,
            config,
            batch_constraints: DashMap::new(),
            dedupe_cache: DashMap::new(),
            last_execution: DashMap::new(),
            standing_grants: DashMap::new(),
        }
    }

    /// Create fresh process state for a process() call
    pub fn new_process_state(&self) -> ProcessToolState {
        ProcessToolState::new()
    }

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
    ) -> Result<ToolExecutionResult, ToolExecutionError> {
        let tool_name = &call.fn_name;

        // 1. Dedupe check (persistent)
        let canonical_key = self.build_canonical_key(call);
        if let Some(last_time) = self.dedupe_cache.get(&canonical_key) {
            if last_time.elapsed() < self.config.dedupe_window {
                return Err(ToolExecutionError::Deduplicated {
                    tool_name: tool_name.clone(),
                });
            }
        }

        // 2. Process rule validation (per-process)
        self.check_process_rules(tool_name, process_state)?;

        // 3. Batch rule validation (per-batch)
        self.check_batch_rules(tool_name, batch_id)?;

        // 4. Cooldown check (persistent)
        self.check_cooldown(tool_name)?;

        // 5. Permission check (RequiresConsent rule)
        let permission_grant = self.check_permission(tool_name, meta).await?;

        // 6. Execute tool
        let tool = self.tools.get(tool_name).ok_or_else(|| {
            let available = self
                .tools
                .list_tools()
                .iter()
                .map(|s| s.to_string())
                .collect();
            ToolExecutionError::NotFound {
                tool_name: tool_name.clone(),
                available,
            }
        })?;

        let exec_meta = ExecutionMeta {
            permission_grant: permission_grant.clone(),
            request_heartbeat: meta.request_heartbeat,
            caller_user: meta.caller_user.clone(),
            call_id: Some(ToolCallId(call.call_id.clone())),
            route_metadata: meta.route_metadata.clone(),
        };

        let result = tokio::time::timeout(
            self.config.execution_timeout,
            tool.execute(call.fn_arguments.clone(), &exec_meta),
        )
        .await;

        let (response, success) = match result {
            Ok(Ok(output)) => (
                ToolResponse {
                    call_id: call.call_id.clone(),
                    content: serde_json::to_string(&output).unwrap_or_else(|_| output.to_string()),
                    is_error: None,
                },
                true,
            ),
            Ok(Err(e)) => (
                ToolResponse {
                    call_id: call.call_id.clone(),
                    content: format!("Tool error: {}", e),
                    is_error: Some(true),
                },
                false,
            ),
            Err(_) => {
                return Err(ToolExecutionError::Timeout {
                    tool_name: tool_name.clone(),
                    duration: self.config.execution_timeout,
                });
            }
        };

        // 7. Record execution (pass full call for proper dedupe key)
        // Note: batch constraints updated atomically in check_batch_rules
        self.record_execution(call, process_state, success);

        // 8. Build result with continuation info
        let has_continue_rule = !self.requires_heartbeat(tool_name);
        let requests_continuation = meta.request_heartbeat || has_continue_rule;

        Ok(ToolExecutionResult {
            response,
            requests_continuation,
            has_continue_rule,
        })
    }

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
    ) -> (Vec<ToolResponse>, bool) {
        let mut responses = Vec::with_capacity(calls.len());
        let mut needs_continuation = false;

        for call in calls {
            match self.execute(call, batch_id, process_state, meta).await {
                Ok(result) => {
                    if result.requests_continuation {
                        needs_continuation = true;
                    }
                    responses.push(result.response);
                }
                Err(e) => {
                    // Execution error (not tool-returned error) - stop processing
                    responses.push(ToolResponse {
                        call_id: call.call_id.clone(),
                        content: format!("Execution error: {}", e),
                        is_error: Some(true),
                    });
                    break;
                }
            }
        }

        (responses, needs_continuation)
    }

    /// Get unsatisfied start constraint tools
    ///
    /// Returns list of tool names that must be called before other tools.
    /// The agent should call these with appropriate arguments.
    pub fn get_unsatisfied_start_constraints(
        &self,
        process_state: &ProcessToolState,
    ) -> Vec<String> {
        if process_state.start_constraints_done {
            return Vec::new();
        }

        self.rules
            .iter()
            .filter(|r| matches!(r.rule_type, ToolRuleType::StartConstraint))
            .filter(|r| !process_state.tool_was_executed(&r.tool_name))
            .map(|r| r.tool_name.clone())
            .collect()
    }

    /// Get pending exit requirement tools
    ///
    /// Returns list of tool names that must be called before exit.
    /// The agent should call these with appropriate arguments.
    pub fn get_pending_exit_requirements(&self, process_state: &ProcessToolState) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| matches!(r.rule_type, ToolRuleType::RequiredBeforeExit))
            .filter(|r| !process_state.tool_was_executed(&r.tool_name))
            .map(|r| r.tool_name.clone())
            .collect()
    }

    /// Mark start constraints as satisfied
    ///
    /// Called after all start constraint tools have been executed by the agent.
    pub fn mark_start_constraints_done(&self, process_state: &mut ProcessToolState) {
        process_state.start_constraints_done = true;
    }

    /// Mark processing as complete
    ///
    /// Called after all exit requirements have been satisfied.
    pub fn mark_complete(&self, process_state: &mut ProcessToolState) {
        process_state.phase = ExecutionPhase::Complete;
    }

    /// Check if loop should exit based on process state
    pub fn should_exit_loop(&self, process_state: &ProcessToolState) -> bool {
        // Check for ExitLoop tool called this process
        for exec in &process_state.execution_history {
            if self.is_exit_loop_tool(&exec.tool_name) && exec.success {
                return true;
            }
        }

        // Check if in cleanup phase with no pending requirements
        if matches!(
            process_state.phase,
            ExecutionPhase::Cleanup | ExecutionPhase::Complete
        ) {
            return process_state.exit_requirements_pending.is_empty();
        }

        false
    }

    /// Check if tool requires heartbeat
    pub fn requires_heartbeat(&self, tool_name: &str) -> bool {
        // Does the tool have a continue rule itself?
        // Do we have any explicit ExitLoop rules to override?
        // If it does and we don't, then it doesn't need a heartbeat
        if self.tools.get(tool_name).is_some_and(|t| {
            t.value()
                .tool_rules()
                .iter()
                .any(|r| matches!(r.rule_type, ToolRuleType::ContinueLoop))
        }) && !self
            .rules
            .iter()
            .any(|r| matches!(r.rule_type, ToolRuleType::ExitLoop) && r.tool_name == tool_name)
        {
            false
        } else {
            // otherwise, it requires one unless there's an explicit continue rule.
            !self.rules.iter().any(|r| {
                matches!(r.rule_type, ToolRuleType::ContinueLoop) && r.tool_name == tool_name
            })
        }
    }

    /// Mark a batch as complete (allows cleanup of BatchConstraints)
    pub fn complete_batch(&self, batch_id: SnowflakePosition) {
        self.batch_constraints.remove(&batch_id);
    }

    /// Prune expired entries from persistent state (dedupe, cooldowns, grants, old batches)
    pub fn prune_expired(&self) {
        // Prune dedupe cache
        self.dedupe_cache
            .retain(|_, instant| instant.elapsed() < self.config.dedupe_window);

        // Prune old batch constraints (batches older than 1 hour are stale)
        let max_batch_age = Duration::from_secs(3600);
        self.batch_constraints
            .retain(|_, constraints| constraints.created_at.elapsed() < max_batch_age);

        // Prune expired standing grants
        let utc_now = chrono::Utc::now();
        self.standing_grants.retain(|_, grant| {
            grant.expires_at.map(|exp| exp > utc_now).unwrap_or(true) // Keep grants without expiry
        });

        // Note: last_execution is pruned based on cooldown rules, which vary per tool
        // For now, keep entries for rules we have
        let cooldown_tools: std::collections::HashSet<_> = self
            .rules
            .iter()
            .filter_map(|r| {
                if let ToolRuleType::Cooldown(_) = r.rule_type {
                    Some(r.tool_name.clone())
                } else {
                    None
                }
            })
            .collect();

        self.last_execution
            .retain(|tool, _| cooldown_tools.contains(tool));
    }

    // ========================================================================
    // Private helpers
    // ========================================================================

    fn build_canonical_key(&self, call: &ToolCall) -> String {
        // Sort args for consistent key
        let args_str = serde_json::to_string(&call.fn_arguments).unwrap_or_default();
        format!("{}|{}", call.fn_name, args_str)
    }

    fn check_process_rules(
        &self,
        tool_name: &str,
        process_state: &ProcessToolState,
    ) -> Result<(), ToolExecutionError> {
        // Check start constraints
        let has_start_constraints = self
            .rules
            .iter()
            .any(|r| matches!(r.rule_type, ToolRuleType::StartConstraint));

        if has_start_constraints && !process_state.start_constraints_done {
            let is_start_tool = self.rules.iter().any(|r| {
                matches!(r.rule_type, ToolRuleType::StartConstraint) && r.tool_name == tool_name
            });

            if !is_start_tool {
                let required: Vec<String> = self
                    .rules
                    .iter()
                    .filter(|r| matches!(r.rule_type, ToolRuleType::StartConstraint))
                    .map(|r| r.tool_name.clone())
                    .collect();

                return Err(ToolExecutionError::RuleViolation(
                    ToolRuleViolation::StartConstraintsNotMet {
                        tool: tool_name.to_string(),
                        required_start_tools: required,
                    },
                ));
            }
        }

        // Check RequiresPrecedingTools
        for rule in &self.rules {
            if rule.tool_name == tool_name {
                if let ToolRuleType::RequiresPrecedingTools = rule.rule_type {
                    let missing: Vec<String> = rule
                        .conditions
                        .iter()
                        .filter(|c| !process_state.tool_was_executed(c))
                        .cloned()
                        .collect();

                    if !missing.is_empty() {
                        return Err(ToolExecutionError::RuleViolation(
                            ToolRuleViolation::PrerequisitesNotMet {
                                tool: tool_name.to_string(),
                                required: missing,
                                executed: process_state
                                    .executed_tools()
                                    .into_iter()
                                    .map(|s| s.to_string())
                                    .collect(),
                            },
                        ));
                    }
                }
            }
        }

        // Check RequiresFollowingTools hasn't been violated
        for rule in &self.rules {
            if rule.tool_name == tool_name {
                if let ToolRuleType::RequiresFollowingTools = rule.rule_type {
                    let already_called: Vec<String> = rule
                        .conditions
                        .iter()
                        .filter(|c| process_state.tool_was_executed(c))
                        .cloned()
                        .collect();

                    if !already_called.is_empty() {
                        return Err(ToolExecutionError::RuleViolation(
                            ToolRuleViolation::OrderingViolation {
                                tool: tool_name.to_string(),
                                must_precede: rule.conditions.clone(),
                                already_executed: already_called,
                            },
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    fn check_batch_rules(
        &self,
        tool_name: &str,
        batch_id: SnowflakePosition,
    ) -> Result<(), ToolExecutionError> {
        // Get or create batch constraints - hold mutable ref for atomic check+increment
        let mut constraints =
            self.batch_constraints
                .entry(batch_id)
                .or_insert_with(|| BatchConstraints {
                    created_at: Instant::now(),
                    ..Default::default()
                });

        // Check AND increment MaxCalls atomically to prevent TOCTOU race
        for rule in &self.rules {
            if rule.tool_name == tool_name || rule.tool_name == "*" {
                if let ToolRuleType::MaxCalls(max) = rule.rule_type {
                    let count = constraints
                        .call_counts
                        .entry(tool_name.to_string())
                        .or_insert(0);
                    if *count >= max {
                        return Err(ToolExecutionError::RuleViolation(
                            ToolRuleViolation::MaxCallsExceeded {
                                tool: tool_name.to_string(),
                                max,
                                current: *count,
                            },
                        ));
                    }
                    // Reserve slot atomically - failures after this point still count as a use
                    *count += 1;
                }
            }
        }

        // Check AND claim ExclusiveGroups atomically
        for rule in &self.rules {
            if let ToolRuleType::ExclusiveGroups(groups) = &rule.rule_type {
                for group in groups {
                    if group.contains(&tool_name.to_string()) {
                        let group_key = group.join(",");
                        if let Some(existing) =
                            constraints.exclusive_group_selections.get(&group_key)
                        {
                            if existing != tool_name {
                                return Err(ToolExecutionError::RuleViolation(
                                    ToolRuleViolation::ExclusiveGroupViolation {
                                        tool: tool_name.to_string(),
                                        group: group.clone(),
                                        already_called: vec![existing.clone()],
                                    },
                                ));
                            }
                        } else {
                            // Claim this group atomically
                            constraints
                                .exclusive_group_selections
                                .insert(group_key, tool_name.to_string());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn check_cooldown(&self, tool_name: &str) -> Result<(), ToolExecutionError> {
        for rule in &self.rules {
            if rule.tool_name == tool_name {
                if let ToolRuleType::Cooldown(duration) = rule.rule_type {
                    if let Some(last_time) = self.last_execution.get(tool_name) {
                        let elapsed = last_time.elapsed();
                        if elapsed < duration {
                            return Err(ToolExecutionError::RuleViolation(
                                ToolRuleViolation::CooldownActive {
                                    tool: tool_name.to_string(),
                                    remaining: duration - elapsed,
                                },
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn check_permission(
        &self,
        tool_name: &str,
        _meta: &ExecutionMeta,
    ) -> Result<Option<PermissionGrant>, ToolExecutionError> {
        // Find RequiresConsent rule for this tool
        let consent_rule = self.rules.iter().find(|r| {
            r.tool_name == tool_name && matches!(r.rule_type, ToolRuleType::RequiresConsent { .. })
        });

        let consent_rule = match consent_rule {
            Some(r) => r,
            None => return Ok(None), // No consent required
        };

        let scope_hint = if let ToolRuleType::RequiresConsent { scope } = &consent_rule.rule_type {
            scope.clone()
        } else {
            None
        };

        if !self.config.require_permissions {
            // Permissions disabled - allow execution
            return Ok(None);
        }

        // Check for standing grant
        let grant_key = format!("tool:{}", tool_name);
        if let Some(grant) = self.standing_grants.get(&grant_key) {
            // Verify grant hasn't expired
            let utc_now = chrono::Utc::now();
            if grant.expires_at.map(|exp| exp > utc_now).unwrap_or(true) {
                return Ok(Some(grant.clone()));
            } else {
                // Expired - remove it
                drop(grant);
                self.standing_grants.remove(&grant_key);
            }
        }

        // Request permission
        let scope = PermissionScope::ToolExecution {
            tool: tool_name.to_string(),
            args_digest: scope_hint,
        };

        let grant = broker()
            .request(
                self.agent_id.clone(),
                tool_name.to_string(),
                scope.clone(),
                None,
                None,
                self.config.permission_timeout,
            )
            .await;

        match grant {
            Some(g) => {
                // Only cache standing grants (ApproveForDuration has expires_at set)
                // ApproveOnce grants should NOT be cached - they're one-time use
                // ApproveForScope grants also have expires_at (set to far future or None)
                // For safety, only cache if expires_at is explicitly set
                if g.expires_at.is_some() {
                    self.standing_grants.insert(grant_key, g.clone());
                }
                Ok(Some(g))
            }
            None => Err(ToolExecutionError::PermissionDenied {
                tool_name: tool_name.to_string(),
                scope,
            }),
        }
    }

    fn record_execution(
        &self,
        call: &ToolCall,
        process_state: &mut ProcessToolState,
        success: bool,
    ) {
        let tool_name = &call.fn_name;
        let now = Instant::now();

        // Record in process state
        process_state.execution_history.push(ToolExecution {
            tool_name: tool_name.to_string(),
            call_id: call.call_id.clone(),
            timestamp: now,
            success,
            metadata: None,
        });

        // Note: batch constraints (call_counts, exclusive_groups) are updated
        // atomically in check_batch_rules, not here. This prevents TOCTOU races.

        // Record in persistent state - use canonical key (tool+args) for dedupe
        let canonical_key = self.build_canonical_key(call);
        self.dedupe_cache.insert(canonical_key, now);
        self.last_execution.insert(tool_name.to_string(), now);

        // Update phase if exit loop tool
        if self.is_exit_loop_tool(tool_name) && success {
            process_state.phase = ExecutionPhase::Cleanup;
        }
    }

    /// Does this tool have any sort of forced exit rule configured?
    fn is_exit_loop_tool(&self, tool_name: &str) -> bool {
        self.tools.get(tool_name).is_some_and(|t| {
            t.value()
                .tool_rules()
                .iter()
                .any(|r| matches!(r.rule_type, ToolRuleType::ExitLoop))
        }) || self
            .rules
            .iter()
            .any(|r| matches!(r.rule_type, ToolRuleType::ExitLoop) && r.tool_name == tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::get_next_message_position_sync;

    fn test_executor(rules: Vec<ToolRule>) -> ToolExecutor {
        let tools = Arc::new(ToolRegistry::new());
        let config = ToolExecutorConfig::default();
        ToolExecutor::new(AgentId::nil(), tools, rules, config)
    }

    fn test_batch_id() -> SnowflakePosition {
        get_next_message_position_sync()
    }

    #[test]
    fn test_new_process_state() {
        let executor = test_executor(vec![]);
        let state = executor.new_process_state();
        assert!(state.execution_history.is_empty());
        assert!(!state.start_constraints_done);
        assert!(matches!(state.phase, ExecutionPhase::Initialization));
    }

    #[test]
    fn test_requires_heartbeat() {
        let rules = vec![ToolRule::continue_loop("fast_tool".to_string())];
        let executor = test_executor(rules);

        assert!(!executor.requires_heartbeat("fast_tool"));
        assert!(executor.requires_heartbeat("slow_tool"));
    }

    #[test]
    fn test_should_exit_loop() {
        let rules = vec![ToolRule::exit_loop("done".to_string())];
        let executor = test_executor(rules);
        let mut state = executor.new_process_state();

        assert!(!executor.should_exit_loop(&state));

        // Simulate "done" tool execution
        state.execution_history.push(ToolExecution {
            tool_name: "done".to_string(),
            call_id: "test".to_string(),
            timestamp: Instant::now(),
            success: true,
            metadata: None,
        });

        assert!(executor.should_exit_loop(&state));
    }

    #[test]
    fn test_batch_constraints_cleanup() {
        let executor = test_executor(vec![]);
        let batch_id = test_batch_id();

        // Insert some batch constraints
        executor.batch_constraints.insert(
            batch_id,
            BatchConstraints {
                created_at: Instant::now(),
                ..Default::default()
            },
        );

        assert!(executor.batch_constraints.contains_key(&batch_id));

        executor.complete_batch(batch_id);

        assert!(!executor.batch_constraints.contains_key(&batch_id));
    }

    #[test]
    fn test_prune_expired() {
        let executor = test_executor(vec![]);

        // Insert old dedupe entry
        executor.dedupe_cache.insert(
            "old_key".to_string(),
            Instant::now() - Duration::from_secs(600), // 10 minutes ago
        );

        // Insert fresh dedupe entry
        executor
            .dedupe_cache
            .insert("fresh_key".to_string(), Instant::now());

        executor.prune_expired();

        // Old entry should be gone, fresh should remain
        assert!(!executor.dedupe_cache.contains_key("old_key"));
        assert!(executor.dedupe_cache.contains_key("fresh_key"));
    }
}
