//! Tool execution rules engine for Pattern agents
//!
//! This module provides sophisticated control over tool execution flow, enabling agents
//! to follow complex workflows, enforce tool dependencies, and optimize performance.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Rules governing tool execution behavior
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRule {
    /// Name of the tool this rule applies to (or "*" for wildcard)
    pub tool_name: String,

    /// The type of rule to enforce
    pub rule_type: ToolRuleType,

    /// Conditions or dependencies for this rule
    pub conditions: Vec<String>,

    /// Priority level (higher numbers = higher priority)
    pub priority: u8,

    /// Optional metadata for rule configuration
    pub metadata: Option<serde_json::Value>,
}

/// Types of tool execution rules
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolRuleType {
    /// Continue the conversation loop after this tool is called (no heartbeat required)
    ContinueLoop,

    /// Exit conversation loop after this tool is called
    ExitLoop,

    /// This tool must be called after specified tools (ordering dependency)
    RequiresPrecedingTools,

    /// This tool must be called before specified tools
    RequiresFollowingTools,

    /// Multiple exclusive groups - only one tool from each group can be called per conversation
    ExclusiveGroups(Vec<Vec<String>>),

    /// Call this tool at conversation start
    StartConstraint,

    /// This tool must be called before conversation ends
    RequiredBeforeExit,

    /// Required for exit if condition is met
    RequiredBeforeExitIf,

    /// Maximum number of times this tool can be called
    MaxCalls(u32),

    /// Minimum cooldown period between calls
    Cooldown(Duration),

    /// Call this tool periodically during long conversations
    Periodic(Duration),

    /// This tool requires explicit user consent before execution
    RequiresConsent {
        /// Optional scope hint (e.g., memory prefix or capability tag)
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

impl ToolRuleType {
    /// Convert rule type to natural language description for LLM context
    pub fn to_usage_description(&self, tool_name: &str, conditions: &[String]) -> String {
        match self {
            ToolRuleType::ContinueLoop => {
                format!(
                    "The conversation will be continued after calling `{}`",
                    tool_name
                )
            }
            ToolRuleType::ExitLoop => {
                format!("The conversation will end after calling `{}`", tool_name)
            }
            ToolRuleType::StartConstraint => {
                format!("Call `{}` first before any other tools", tool_name)
            }
            ToolRuleType::RequiresPrecedingTools => {
                if conditions.is_empty() {
                    format!("Call other tools before calling `{}`", tool_name)
                } else {
                    format!(
                        "Call `{}` only after calling: {}",
                        tool_name,
                        conditions.join(", ")
                    )
                }
            }
            ToolRuleType::RequiresFollowingTools => {
                if conditions.is_empty() {
                    format!("Call other tools after calling `{}`", tool_name)
                } else {
                    format!(
                        "Call these tools after calling `{}`: {}",
                        tool_name,
                        conditions.join(", ")
                    )
                }
            }
            ToolRuleType::RequiredBeforeExit => {
                format!("Call `{}` before ending the conversation", tool_name)
            }
            ToolRuleType::RequiredBeforeExitIf => {
                if conditions.is_empty() {
                    format!(
                        "Call `{}` before ending if certain conditions are met",
                        tool_name
                    )
                } else {
                    format!(
                        "Call `{}` before ending if: {}",
                        tool_name,
                        conditions.join(", ")
                    )
                }
            }
            ToolRuleType::MaxCalls(max) => {
                format!(
                    "Call `{}` at most {} times per conversation",
                    tool_name, max
                )
            }
            ToolRuleType::Cooldown(duration) => {
                format!(
                    "Wait at least {}ms between calls to `{}`",
                    duration.as_millis(),
                    tool_name
                )
            }
            ToolRuleType::ExclusiveGroups(groups) => {
                let group_descriptions: Vec<String> = groups
                    .iter()
                    .map(|group| format!("[{}]", group.join(", ")))
                    .collect();
                format!(
                    "Call only one tool from each group per conversation for `{}`: {}",
                    tool_name,
                    group_descriptions.join(", ")
                )
            }
            ToolRuleType::Periodic(interval) => {
                format!(
                    "Call `{}` every {}ms during long conversations",
                    tool_name,
                    interval.as_millis()
                )
            }
            ToolRuleType::RequiresConsent { scope } => {
                if let Some(s) = scope {
                    format!(
                        "User approval is required before calling `{}` (scope: {}).",
                        tool_name, s
                    )
                } else {
                    format!("User approval is required before calling `{}`.", tool_name)
                }
            }
        }
    }
}

/// Execution state for tracking rule compliance
#[derive(Debug, Clone, Default)]
pub struct ToolExecutionState {
    /// Tools that have been executed in order
    pub execution_history: Vec<ToolExecution>,

    /// Current conversation phase
    pub phase: ExecutionPhase,

    /// Tools required before exit
    pub pending_exit_requirements: Vec<String>,

    /// Last execution time for each tool (for cooldowns)
    pub last_execution: HashMap<String, Instant>,

    /// Call count for each tool
    pub call_counts: HashMap<String, u32>,

    /// Whether the conversation should continue after current tool
    pub should_continue: bool,
}

/// Record of a tool execution for rule tracking
#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub tool_name: String,
    pub call_id: String,
    pub timestamp: Instant,
    pub success: bool,
    pub metadata: Option<serde_json::Value>,
}

/// Conversation execution phases
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ExecutionPhase {
    #[default]
    Initialization,
    Processing,
    Cleanup,
    Complete,
}

/// Engine for enforcing tool execution rules
#[derive(Debug, Clone)]
pub struct ToolRuleEngine {
    rules: Vec<ToolRule>,
    state: ToolExecutionState,
}

impl ToolRuleEngine {
    /// Create a new rule engine with the given rules
    pub fn new(rules: Vec<ToolRule>) -> Self {
        // Sort rules by priority (highest first)
        let mut sorted_rules = rules;
        sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        Self {
            rules: sorted_rules,
            state: ToolExecutionState::default(),
        }
    }

    /// Get all rules as natural language descriptions for LLM context
    pub fn to_usage_descriptions(&self) -> Vec<String> {
        self.rules
            .iter()
            .map(|rule| rule.to_usage_description())
            .collect()
    }

    /// Get all rules (for database persistence)
    pub fn get_rules(&self) -> &[ToolRule] {
        &self.rules
    }

    /// Check if a tool can be executed given current state
    pub fn can_execute_tool(&self, tool_name: &str) -> Result<bool, ToolRuleViolation> {
        // First, check if start constraints are satisfied
        if !self.start_constraints_satisfied() && !self.is_start_constraint_tool(tool_name) {
            let unsatisfied_start_tools = self.get_unsatisfied_start_constraint_tools();
            return Err(ToolRuleViolation::StartConstraintsNotMet {
                tool: tool_name.to_string(),
                required_start_tools: unsatisfied_start_tools,
            });
        }

        let applicable_rules = self.get_applicable_rules(tool_name);

        for rule in &applicable_rules {
            match &rule.rule_type {
                ToolRuleType::RequiresPrecedingTools => {
                    if !self.prerequisites_satisfied(&rule.conditions) {
                        return Err(ToolRuleViolation::PrerequisitesNotMet {
                            tool: tool_name.to_string(),
                            required: rule.conditions.clone(),
                            executed: self.get_executed_tools(),
                        });
                    }
                }
                ToolRuleType::MaxCalls(max_calls) => {
                    let current_count = self.state.call_counts.get(tool_name).unwrap_or(&0);
                    if current_count >= max_calls {
                        return Err(ToolRuleViolation::MaxCallsExceeded {
                            tool: tool_name.to_string(),
                            max: *max_calls,
                            current: *current_count,
                        });
                    }
                }
                ToolRuleType::Cooldown(duration) => {
                    if let Some(last_time) = self.state.last_execution.get(tool_name) {
                        let elapsed = last_time.elapsed();
                        if elapsed < *duration {
                            return Err(ToolRuleViolation::CooldownActive {
                                tool: tool_name.to_string(),
                                remaining: *duration - elapsed,
                            });
                        }
                    }
                }
                ToolRuleType::ExclusiveGroups(groups) => {
                    for group in groups {
                        if group.contains(&tool_name.to_string()) {
                            // Check if any OTHER tool in the group has been called
                            let other_tools_called: Vec<String> = group
                                .iter()
                                .filter(|&tool| tool != tool_name && self.tool_was_called(tool))
                                .cloned()
                                .collect();

                            if !other_tools_called.is_empty() {
                                return Err(ToolRuleViolation::ExclusiveGroupViolation {
                                    tool: tool_name.to_string(),
                                    group: group.clone(),
                                    already_called: other_tools_called,
                                });
                            }
                        }
                    }
                }
                ToolRuleType::RequiresFollowingTools => {
                    if self.any_following_tools_called(&rule.conditions) {
                        return Err(ToolRuleViolation::OrderingViolation {
                            tool: tool_name.to_string(),
                            must_precede: rule.conditions.clone(),
                            already_executed: self.get_executed_tools(),
                        });
                    }
                }
                _ => {} // ContinueLoop, ExitLoop, StartConstraint don't prevent execution
            }
        }

        Ok(true)
    }

    /// Record tool execution and update state
    pub fn record_execution(&mut self, execution: ToolExecution) {
        let tool_name = &execution.tool_name;

        // Update execution history
        self.state.execution_history.push(execution.clone());

        // Update call count
        let count = self.state.call_counts.entry(tool_name.clone()).or_insert(0);
        *count += 1;

        // Update last execution time
        self.state
            .last_execution
            .insert(tool_name.clone(), execution.timestamp);

        // Check for loop control rules
        self.update_loop_control_after_tool(tool_name);

        // Check if we should advance to cleanup phase
        if self.should_exit_after_tool(tool_name) {
            self.state.phase = ExecutionPhase::Cleanup;
        }
    }

    /// Get tools that must be called at conversation start
    pub fn get_start_constraint_tools(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter_map(|rule| {
                if matches!(rule.rule_type, ToolRuleType::StartConstraint) {
                    Some(rule.tool_name.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get tools required before conversation can end
    pub fn get_required_before_exit_tools(&self) -> Vec<String> {
        let mut required = Vec::new();

        for rule in &self.rules {
            match &rule.rule_type {
                ToolRuleType::RequiredBeforeExit => {
                    if !self.tool_was_called(&rule.tool_name) {
                        required.push(rule.tool_name.clone());
                    }
                }
                ToolRuleType::RequiredBeforeExitIf => {
                    if self.conditions_met(&rule.conditions)
                        && !self.tool_was_called(&rule.tool_name)
                    {
                        required.push(rule.tool_name.clone());
                    }
                }
                _ => {}
            }
        }

        required
    }

    /// Check if conversation should exit the loop
    pub fn should_exit_loop(&self) -> bool {
        // Check for explicit exit loop rules
        if self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::ExitLoop)
                && self.tool_was_called(&rule.tool_name)
        }) {
            return true;
        }

        // Check if we're in cleanup phase and all requirements are met
        if self.state.phase == ExecutionPhase::Cleanup {
            return self.get_required_before_exit_tools().is_empty();
        }

        false
    }

    /// Check if conversation should continue the loop
    pub fn should_continue_loop(&self) -> bool {
        // Explicit continue loop rules override default behavior
        let has_continue_rule = self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::ContinueLoop)
                && self.tool_was_called(&rule.tool_name)
        });

        if has_continue_rule {
            return true;
        }

        // Default: continue unless explicitly told to exit
        !self.should_exit_loop()
    }

    /// Check if tool requires heartbeat
    pub fn requires_heartbeat(&self, tool_name: &str) -> bool {
        !self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::ContinueLoop)
                && (rule.tool_name == tool_name
                    || (rule.tool_name == "*" && rule.conditions.contains(&tool_name.to_string())))
        })
    }

    /// Get current execution state (for debugging/monitoring)
    pub fn get_execution_state(&self) -> &ToolExecutionState {
        &self.state
    }

    /// Reset the engine state (for new conversations)
    pub fn reset(&mut self) {
        self.state = ToolExecutionState::default();
    }

    // Private helper methods

    fn get_applicable_rules(&self, tool_name: &str) -> Vec<&ToolRule> {
        self.rules
            .iter()
            .filter(|rule| rule.tool_name == tool_name || rule.tool_name == "*")
            .collect()
    }

    fn prerequisites_satisfied(&self, required_tools: &[String]) -> bool {
        required_tools.iter().all(|tool| self.tool_was_called(tool))
    }

    fn tool_was_called(&self, tool_name: &str) -> bool {
        self.state
            .execution_history
            .iter()
            .any(|exec| exec.tool_name == tool_name && exec.success)
    }

    fn get_executed_tools(&self) -> Vec<String> {
        self.state
            .execution_history
            .iter()
            .filter(|exec| exec.success)
            .map(|exec| exec.tool_name.clone())
            .collect()
    }

    fn start_constraints_satisfied(&self) -> bool {
        let start_tools = self.get_start_constraint_tools();
        if start_tools.is_empty() {
            return true; // No start constraints
        }
        start_tools.iter().all(|tool| self.tool_was_called(tool))
    }

    fn is_start_constraint_tool(&self, tool_name: &str) -> bool {
        self.rules.iter().any(|rule| {
            rule.tool_name == tool_name && matches!(rule.rule_type, ToolRuleType::StartConstraint)
        })
    }

    fn get_unsatisfied_start_constraint_tools(&self) -> Vec<String> {
        self.get_start_constraint_tools()
            .into_iter()
            .filter(|tool| !self.tool_was_called(tool))
            .collect()
    }

    fn any_following_tools_called(&self, following_tools: &[String]) -> bool {
        following_tools
            .iter()
            .any(|tool| self.tool_was_called(tool))
    }

    pub fn should_exit_after_tool(&self, tool_name: &str) -> bool {
        self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::ExitLoop) && rule.tool_name == tool_name
        })
    }

    fn update_loop_control_after_tool(&mut self, tool_name: &str) {
        // Check for explicit continue loop rules
        let should_continue = self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::ContinueLoop) && rule.tool_name == tool_name
        });

        if should_continue {
            self.state.should_continue = true;
        }

        // Check for exit loop rules
        let should_exit = self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::ExitLoop) && rule.tool_name == tool_name
        });

        if should_exit {
            self.state.should_continue = false;
        }
    }

    fn conditions_met(&self, conditions: &[String]) -> bool {
        // For now, assume conditions are tool names that must have been called
        // This can be expanded to support more complex condition logic
        conditions
            .iter()
            .all(|condition| self.tool_was_called(condition))
    }
}

/// Errors that can occur when validating tool rules
#[derive(Debug, Clone, Error)]
pub enum ToolRuleViolation {
    #[error(
        "Tool {tool} cannot be executed: prerequisites {required:?} not met. Executed tools: {executed:?}"
    )]
    PrerequisitesNotMet {
        tool: String,
        required: Vec<String>,
        executed: Vec<String>,
    },

    #[error("Tool {tool} has exceeded maximum calls ({max}). Current: {current}")]
    MaxCallsExceeded {
        tool: String,
        max: u32,
        current: u32,
    },

    #[error("Tool {tool} is in cooldown. Remaining: {remaining:?}")]
    CooldownActive { tool: String, remaining: Duration },

    #[error(
        "Tool {tool} cannot be executed: exclusive group violation. Group {group:?} already has executed tools: {already_called:?}"
    )]
    ExclusiveGroupViolation {
        tool: String,
        group: Vec<String>,
        already_called: Vec<String>,
    },

    #[error(
        "Tool {tool} violates ordering constraint: must be called before {must_precede:?}, but these were already executed: {already_executed:?}"
    )]
    OrderingViolation {
        tool: String,
        must_precede: Vec<String>,
        already_executed: Vec<String>,
    },

    #[error(
        "Tool {tool} cannot be executed until start constraints are satisfied. Required: {required_start_tools:?}"
    )]
    StartConstraintsNotMet {
        tool: String,
        required_start_tools: Vec<String>,
    },
}

impl ToolRule {
    /// Create a new tool rule
    pub fn new(tool_name: String, rule_type: ToolRuleType) -> Self {
        Self {
            tool_name,
            rule_type,
            conditions: Vec::new(),
            priority: 5,
            metadata: None,
        }
    }

    /// Set conditions for this rule
    pub fn with_conditions(mut self, conditions: Vec<String>) -> Self {
        self.conditions = conditions;
        self
    }

    /// Set priority for this rule
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set metadata for this rule
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Convert this rule to a natural language description for LLM context
    pub fn to_usage_description(&self) -> String {
        self.rule_type
            .to_usage_description(&self.tool_name, &self.conditions)
    }

    /// Create a continue loop rule (no heartbeat required)
    pub fn continue_loop(tool_name: String) -> Self {
        Self::new(tool_name, ToolRuleType::ContinueLoop).with_priority(1)
    }

    /// Create a start constraint rule
    pub fn start_constraint(tool_name: String) -> Self {
        Self::new(tool_name, ToolRuleType::StartConstraint).with_priority(10)
    }

    /// Create an exit loop rule
    pub fn exit_loop(tool_name: String) -> Self {
        Self::new(tool_name, ToolRuleType::ExitLoop).with_priority(8)
    }

    /// Create exclusive groups rule
    pub fn exclusive_groups(tool_name: String, groups: Vec<Vec<String>>) -> Self {
        Self::new(tool_name, ToolRuleType::ExclusiveGroups(groups)).with_priority(6)
    }

    /// Create a required before exit rule
    pub fn required_before_exit(tool_name: String) -> Self {
        Self::new(tool_name, ToolRuleType::RequiredBeforeExit).with_priority(9)
    }

    /// Create a tool dependency rule (tool must follow others)
    pub fn requires_preceding_tools(tool_name: String, preceding_tools: Vec<String>) -> Self {
        Self::new(tool_name, ToolRuleType::RequiresPrecedingTools)
            .with_conditions(preceding_tools)
            .with_priority(7)
    }

    /// Create a max calls rule
    pub fn max_calls(tool_name: String, max: u32) -> Self {
        Self::new(tool_name, ToolRuleType::MaxCalls(max)).with_priority(5)
    }

    /// Create a cooldown rule
    pub fn cooldown(tool_name: String, duration: Duration) -> Self {
        Self::new(tool_name, ToolRuleType::Cooldown(duration)).with_priority(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_execution(tool_name: &str, success: bool) -> ToolExecution {
        ToolExecution {
            tool_name: tool_name.to_string(),
            call_id: format!("call_{}", tool_name),
            timestamp: Instant::now(),
            success,
            metadata: None,
        }
    }

    #[test]
    fn test_natural_language_rule_descriptions() {
        let rules = vec![
            ToolRule::start_constraint("context".to_string()),
            ToolRule::continue_loop("search".to_string()),
            ToolRule::exit_loop("send_message".to_string()),
            ToolRule::required_before_exit("cleanup".to_string()),
            ToolRule::max_calls("api_call".to_string(), 3),
            ToolRule::cooldown("heavy_task".to_string(), Duration::from_secs(2)),
            ToolRule::requires_preceding_tools(
                "validate".to_string(),
                vec!["extract".to_string(), "transform".to_string()],
            ),
        ];

        let engine = ToolRuleEngine::new(rules);
        let descriptions = engine.to_usage_descriptions();

        println!("Natural language descriptions:");
        for (i, desc) in descriptions.iter().enumerate() {
            println!("{}: {}", i + 1, desc);
        }

        // Check specific rule descriptions (without repetitive enforcement language)
        // Note: Rules are sorted by priority, so order may differ from creation order
        assert!(descriptions[0].contains("Call `context` first before any other tools"));
        assert!(descriptions[1].contains("Call `cleanup` before ending the conversation"));
        assert!(descriptions[2].contains("The conversation will end after calling `send_message`"));
        assert!(descriptions[3].contains("Call `validate` only after calling: extract, transform"));
        assert!(descriptions[4].contains("Call `api_call` at most 3 times"));
        assert!(descriptions[5].contains("Wait at least 2000ms between calls to `heavy_task`"));
        assert!(
            descriptions[6].contains("The conversation will be continued after calling `search`")
        );
    }

    #[test]
    fn test_requires_preceding_tools() {
        let rules = vec![ToolRule::requires_preceding_tools(
            "validate".to_string(),
            vec!["load".to_string()],
        )];

        let mut engine = ToolRuleEngine::new(rules);

        // Should fail - validate before load
        assert!(engine.can_execute_tool("validate").is_err());

        // Execute load first
        engine.record_execution(create_test_execution("load", true));

        // Should succeed now
        assert!(engine.can_execute_tool("validate").is_ok());
    }

    #[test]
    fn test_exit_loop_rule() {
        let rules = vec![ToolRule::exit_loop("deploy".to_string())];

        let mut engine = ToolRuleEngine::new(rules);

        // Should not exit initially
        assert!(!engine.should_exit_loop());

        // Execute deploy
        engine.record_execution(create_test_execution("deploy", true));

        // Should exit now
        assert!(engine.should_exit_loop());
    }

    #[test]
    fn test_start_constraint() {
        let rules = vec![ToolRule::start_constraint("init".to_string())];

        let engine = ToolRuleEngine::new(rules);
        let start_tools = engine.get_start_constraint_tools();

        assert_eq!(start_tools, vec!["init"]);
    }

    #[test]
    fn test_exclusive_group() {
        let rules = vec![ToolRule {
            tool_name: "format_json".to_string(),
            rule_type: ToolRuleType::ExclusiveGroups(vec![vec![
                "format_json".to_string(),
                "format_xml".to_string(),
                "format_yaml".to_string(),
            ]]),
            conditions: vec![],
            priority: 5,
            metadata: None,
        }];

        let mut engine = ToolRuleEngine::new(rules);

        // Execute one tool from the group
        engine.record_execution(create_test_execution("format_xml", true));

        // Should fail - exclusive group violation
        assert!(engine.can_execute_tool("format_json").is_err());
    }

    #[test]
    fn test_max_calls() {
        let rules = vec![ToolRule::max_calls("api_request".to_string(), 2)];

        let mut engine = ToolRuleEngine::new(rules);

        // First two calls should succeed
        assert!(engine.can_execute_tool("api_request").is_ok());
        engine.record_execution(create_test_execution("api_request", true));

        assert!(engine.can_execute_tool("api_request").is_ok());
        engine.record_execution(create_test_execution("api_request", true));

        // Third call should fail
        assert!(engine.can_execute_tool("api_request").is_err());
    }

    #[test]
    fn test_continue_loop_rule() {
        let rules = vec![ToolRule::continue_loop("fast_tool".to_string())];

        let engine = ToolRuleEngine::new(rules);

        // Tool should not require heartbeat
        assert!(!engine.requires_heartbeat("fast_tool"));
        assert!(engine.requires_heartbeat("slow_tool"));
    }

    #[test]
    fn test_required_before_exit() {
        let rules = vec![ToolRule::required_before_exit("cleanup".to_string())];

        let mut engine = ToolRuleEngine::new(rules);

        // Should require cleanup before exit
        let required = engine.get_required_before_exit_tools();
        assert_eq!(required, vec!["cleanup"]);

        // After cleanup is called, should be empty
        engine.record_execution(create_test_execution("cleanup", true));
        let required = engine.get_required_before_exit_tools();
        assert!(required.is_empty());
    }

    #[test]
    fn test_rule_priority_ordering() {
        let rules = vec![
            ToolRule::new("tool1".to_string(), ToolRuleType::ContinueLoop).with_priority(1),
            ToolRule::new("tool2".to_string(), ToolRuleType::ExitLoop).with_priority(10),
            ToolRule::new("tool3".to_string(), ToolRuleType::ContinueLoop).with_priority(5),
        ];

        let engine = ToolRuleEngine::new(rules);

        // Rules should be sorted by priority (highest first)
        assert_eq!(engine.rules[0].priority, 10);
        assert_eq!(engine.rules[1].priority, 5);
        assert_eq!(engine.rules[2].priority, 1);
    }

    #[test]
    fn test_reset_engine_state() {
        let rules = vec![ToolRule::max_calls("test_tool".to_string(), 1)];

        let mut engine = ToolRuleEngine::new(rules);

        // Execute tool
        engine.record_execution(create_test_execution("test_tool", true));

        // Should fail due to max calls
        assert!(engine.can_execute_tool("test_tool").is_err());

        // Reset state
        engine.reset();

        // Should succeed again
        assert!(engine.can_execute_tool("test_tool").is_ok());
    }
}
