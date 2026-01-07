# Pattern Framework Tool Rules Implementation Guide

**Status: 🚧 NOT YET IMPLEMENTED - Design Document**

This document provides a comprehensive implementation guide for the Pattern framework's tool rules system, allowing fine-grained control over how agents use tools during conversation processing.

## Overview

The tool rules system will provide sophisticated control over tool execution flow, enabling agents to follow complex workflows, enforce tool dependencies, and optimize performance through selective heartbeat management.

## Current State Analysis

### What Exists Now

Based on the codebase analysis, the current tool system includes:

**Core Structures:**
- `ToolCall` in `message.rs` - Represents a tool invocation
- `ToolResponse` in `message.rs` - Represents tool execution result  
- `AiTool` trait in `tool/mod.rs` - Type-safe tool interface
- `DynamicTool` trait in `tool/mod.rs` - Type-erased tool interface
- `ToolRegistry` in `tool/mod.rs` - Tool management and execution

**Current Tool Execution Flow:**
1. Agent receives message and generates response via model
2. Model may include `ToolCalls` in response content
3. Agent processes tool calls sequentially in `process_message_stream()`
4. Each tool is executed via `context.process_tool_call()`
5. Tool responses are collected and added to response content
6. Process continues until no more tool calls

**Missing Components:**
- No rule-based tool orchestration
- No tool execution constraints or dependencies
- No selective heartbeat management
- No tool ordering enforcement
- No initialization or cleanup tool requirements

## Implementation Plan

### Phase 1: Core Rule Types and Structures

#### 1.1 Define Rule Types

**File:** `pattern/crates/pattern_core/src/agent/tool_rules.rs`

```rust
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
    /// Tool doesn't require heartbeat checks during execution
    NoHeartbeat,
    
    /// End conversation loop after this tool is called
    Terminal,
    
    /// End conversation if condition is met after tool execution
    TerminalIf,
    
    /// This tool must be called after specified tools
    MustFollow,
    
    /// This tool must be called before specified tools  
    MustPrecede,
    
    /// Only one tool from this group can be called per conversation
    ExclusiveGroup,
    
    /// Call this tool at conversation start
    InitialCall,
    
    /// This tool must be called before conversation ends
    RequiredForExit,
    
    /// Required for exit if condition is met
    RequiredForExitIf,
    
    /// Maximum number of times this tool can be called
    MaxCalls(u32),
    
    /// Minimum cooldown period between calls
    Cooldown(Duration),
    
    /// Call this tool periodically during long conversations
    Periodic(Duration),
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
    pub last_execution: std::collections::HashMap<String, std::time::Instant>,
    
    /// Call count for each tool
    pub call_counts: std::collections::HashMap<String, u32>,
}

#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub tool_name: String,
    pub call_id: String,
    pub timestamp: std::time::Instant,
    pub success: bool,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionPhase {
    Initialization,
    Processing,
    Cleanup,
    Complete,
}
```

#### 1.2 Rule Engine Implementation

**File:** `pattern/crates/pattern_core/src/agent/tool_rules.rs` (continued)

```rust
/// Engine for enforcing tool execution rules
#[derive(Debug, Clone)]
pub struct ToolRuleEngine {
    rules: Vec<ToolRule>,
    state: ToolExecutionState,
}

impl ToolRuleEngine {
    pub fn new(rules: Vec<ToolRule>) -> Self {
        // Sort rules by priority (highest first)
        let mut sorted_rules = rules;
        sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        
        Self {
            rules: sorted_rules,
            state: ToolExecutionState::default(),
        }
    }
    
    /// Check if a tool can be executed given current state
    pub fn can_execute_tool(&self, tool_name: &str) -> Result<(), ToolRuleViolation> {
        let applicable_rules = self.get_applicable_rules(tool_name);
        
        for rule in applicable_rules {
            match &rule.rule_type {
                ToolRuleType::MustFollow => {
                    if !self.prerequisites_satisfied(&rule.conditions) {
                        return Err(ToolRuleViolation::PrerequisitesNotMet {
                            tool: tool_name.to_string(),
                            required: rule.conditions.clone(),
                            executed: self.get_executed_tools(),
                        });
                    }
                }
                ToolRuleType::MaxCalls(max) => {
                    let current_count = self.state.call_counts.get(tool_name).unwrap_or(&0);
                    if current_count >= max {
                        return Err(ToolRuleViolation::MaxCallsExceeded {
                            tool: tool_name.to_string(),
                            max: *max,
                            current: *current_count,
                        });
                    }
                }
                ToolRuleType::Cooldown(duration) => {
                    if let Some(last_time) = self.state.last_execution.get(tool_name) {
                        let elapsed = std::time::Instant::now().duration_since(*last_time);
                        if elapsed < *duration {
                            return Err(ToolRuleViolation::CooldownActive {
                                tool: tool_name.to_string(),
                                remaining: *duration - elapsed,
                            });
                        }
                    }
                }
                ToolRuleType::ExclusiveGroup => {
                    if self.group_tool_already_called(&rule.conditions) {
                        return Err(ToolRuleViolation::ExclusiveGroupViolation {
                            tool: tool_name.to_string(),
                            group: rule.conditions.clone(),
                            already_called: self.get_executed_tools_in_group(&rule.conditions),
                        });
                    }
                }
                _ => {} // Other rules don't prevent execution
            }
        }
        
        Ok(())
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
        self.state.last_execution.insert(tool_name.clone(), execution.timestamp);
        
        // Check for terminal conditions
        if self.should_terminate_after_tool(tool_name) {
            self.state.phase = ExecutionPhase::Cleanup;
        }
    }
    
    /// Get tools that must be called at initialization
    pub fn get_initial_tools(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter_map(|rule| {
                if matches!(rule.rule_type, ToolRuleType::InitialCall) {
                    Some(rule.tool_name.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    
    /// Get tools required before conversation can end
    pub fn get_required_exit_tools(&self) -> Vec<String> {
        let mut required = Vec::new();
        
        for rule in &self.rules {
            match &rule.rule_type {
                ToolRuleType::RequiredForExit => {
                    if !self.tool_was_called(&rule.tool_name) {
                        required.push(rule.tool_name.clone());
                    }
                }
                ToolRuleType::RequiredForExitIf => {
                    if self.conditions_met(&rule.conditions) && 
                       !self.tool_was_called(&rule.tool_name) {
                        required.push(rule.tool_name.clone());
                    }
                }
                _ => {}
            }
        }
        
        required
    }
    
    /// Check if conversation should terminate
    pub fn should_terminate(&self) -> bool {
        // Check for explicit terminal rules
        if self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::Terminal) &&
            self.tool_was_called(&rule.tool_name)
        }) {
            return true;
        }
        
        // Check conditional terminal rules
        self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::TerminalIf) &&
            self.tool_was_called(&rule.tool_name) &&
            self.conditions_met(&rule.conditions)
        })
    }
    
    /// Check if tool requires heartbeat
    pub fn requires_heartbeat(&self, tool_name: &str) -> bool {
        !self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::NoHeartbeat) &&
            (rule.tool_name == tool_name || 
             rule.tool_name == "*" && rule.conditions.contains(&tool_name.to_string()))
        })
    }
    
    // Helper methods
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
        self.state.execution_history
            .iter()
            .any(|exec| exec.tool_name == tool_name && exec.success)
    }
    
    fn get_executed_tools(&self) -> Vec<String> {
        self.state.execution_history
            .iter()
            .filter(|exec| exec.success)
            .map(|exec| exec.tool_name.clone())
            .collect()
    }
    
    fn group_tool_already_called(&self, group_tools: &[String]) -> bool {
        group_tools.iter().any(|tool| self.tool_was_called(tool))
    }
    
    fn get_executed_tools_in_group(&self, group_tools: &[String]) -> Vec<String> {
        self.get_executed_tools()
            .into_iter()
            .filter(|tool| group_tools.contains(tool))
            .collect()
    }
    
    fn should_terminate_after_tool(&self, tool_name: &str) -> bool {
        self.rules.iter().any(|rule| {
            matches!(rule.rule_type, ToolRuleType::Terminal) &&
            rule.tool_name == tool_name
        })
    }
    
    fn conditions_met(&self, conditions: &[String]) -> bool {
        // This would need to be expanded based on how conditions are defined
        // For now, assume conditions are tool names that must have been called
        conditions.iter().all(|condition| self.tool_was_called(condition))
    }
}

/// Errors that can occur when validating tool rules
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolRuleViolation {
    #[error("Tool {tool} cannot be executed: prerequisites {required:?} not met. Executed tools: {executed:?}")]
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
    CooldownActive {
        tool: String,
        remaining: Duration,
    },
    
    #[error("Tool {tool} cannot be executed: exclusive group violation. Group {group:?} already has executed tools: {already_called:?}")]
    ExclusiveGroupViolation {
        tool: String,
        group: Vec<String>,
        already_called: Vec<String>,
    },
}
```

### Phase 2: Agent Integration

#### 2.1 Update Agent Trait

**File:** `pattern/crates/pattern_core/src/agent/mod.rs`

Add to the `Agent` trait:

```rust
/// Get the tool rules for this agent  
async fn tool_rules(&self) -> Vec<ToolRule> {
    Vec::new() // Default: no rules
}

/// Update the agent's tool rules
async fn set_tool_rules(&self, rules: Vec<ToolRule>) -> Result<()>;
```

#### 2.2 Update DatabaseAgent

**File:** `pattern/crates/pattern_core/src/agent/impls/db_agent.rs`

Add to `DatabaseAgent` struct:

```rust
/// Tool execution rules engine
rule_engine: Arc<RwLock<Option<ToolRuleEngine>>>,
```

Update constructor to accept tool rules:

```rust
pub fn new(
    // ... existing parameters ...
    tool_rules: Vec<ToolRule>, // Add this parameter
) -> Self {
    // ... existing initialization ...
    
    let rule_engine = if tool_rules.is_empty() {
        Arc::new(RwLock::new(None))
    } else {
        Arc::new(RwLock::new(Some(ToolRuleEngine::new(tool_rules))))
    };
    
    Self {
        // ... existing fields ...
        rule_engine,
    }
}
```

#### 2.3 Modify Tool Execution Logic

**File:** `pattern/crates/pattern_core/src/agent/impls/db_agent.rs`

Replace the tool execution loop in `process_message_stream()`:

```rust
// Around line 1083, replace the tool execution logic:

if !calls.is_empty() {
    // Check tool rules before execution
    if let Some(ref mut engine) = rule_engine.write().await.as_mut() {
        // Execute initial tools if this is the first iteration
        if engine.state.execution_history.is_empty() {
            let initial_tools = engine.get_initial_tools();
            for tool_name in initial_tools {
                if let Some(initial_call) = Self::create_synthetic_tool_call(&tool_name) {
                    // Execute initial tool
                    if let Some(response) = ctx.process_tool_call(&initial_call).await? {
                        engine.record_execution(ToolExecution {
                            tool_name: tool_name.clone(),
                            call_id: initial_call.call_id.clone(),
                            timestamp: std::time::Instant::now(),
                            success: !response.content.starts_with("Error:"),
                            metadata: None,
                        });
                    }
                }
            }
        }
    }

    send_event(ResponseEvent::ToolCalls {
        calls: calls.clone(),
    })
    .await;
    
    // Collect responses matching these tool calls
    for call in calls {
        // Validate tool execution against rules
        if let Some(ref engine) = rule_engine.read().await.as_ref() {
            if let Err(violation) = engine.can_execute_tool(&call.fn_name) {
                send_event(ResponseEvent::Error {
                    message: format!("Tool rule violation: {}", violation),
                    recoverable: false,
                }).await;
                continue;
            }
        }

        send_event(ResponseEvent::ToolCallStarted {
            call_id: call.call_id.clone(),
            fn_name: call.fn_name.clone(),
            args: call.fn_arguments.clone(),
        })
        .await;
        
        // Check if heartbeat is required for this tool
        let requires_heartbeat = rule_engine
            .read()
            .await
            .as_ref()
            .map(|engine| engine.requires_heartbeat(&call.fn_name))
            .unwrap_or(true); // Default to requiring heartbeat
            
        if requires_heartbeat && check_heartbeat_request(&call.fn_arguments) {
            // ... existing heartbeat logic ...
        }

        // Execute tool using the context method
        let tool_response = ctx
            .process_tool_call(&call)
            .await
            .unwrap_or_else(|e| {
                Some(ToolResponse {
                    call_id: call.call_id.clone(),
                    content: format!("Error executing tool: {}", e),
                })
            });
            
        if let Some(tool_response) = tool_response {
            let success = !tool_response.content.starts_with("Error:");
            
            // Record execution in rule engine
            if let Some(ref mut engine) = rule_engine.write().await.as_mut() {
                engine.record_execution(ToolExecution {
                    tool_name: call.fn_name.clone(),
                    call_id: call.call_id.clone(),
                    timestamp: std::time::Instant::now(),
                    success,
                    metadata: None,
                });
                
                // Check if we should terminate after this tool
                if engine.should_terminate() {
                    // Execute any required exit tools first
                    let exit_tools = engine.get_required_exit_tools();
                    for exit_tool in exit_tools {
                        if let Some(exit_call) = Self::create_synthetic_tool_call(&exit_tool) {
                            if let Some(exit_response) = ctx.process_tool_call(&exit_call).await? {
                                engine.record_execution(ToolExecution {
                                    tool_name: exit_tool.clone(),
                                    call_id: exit_call.call_id.clone(),
                                    timestamp: std::time::Instant::now(),
                                    success: !exit_response.content.starts_with("Error:"),
                                    metadata: None,
                                });
                            }
                        }
                    }
                    
                    // Signal termination
                    should_terminate = true;
                }
            }

            let tool_result = if success {
                Ok(tool_response.content.clone())
            } else {
                Err(tool_response.content.clone())
            };

            send_event(ResponseEvent::ToolCallCompleted {
                call_id: call.call_id.clone(),
                result: tool_result,
            })
            .await;

            our_responses.push(tool_response);
        }
    }
    
    // Add our tool responses if we have any
    if !our_responses.is_empty() {
        processed_response
            .content
            .push(MessageContent::ToolResponses(our_responses));
    }
}

// Later in the loop, check for termination
if should_terminate {
    break;
}
```

Add helper method:

```rust
impl<C, M, E> DatabaseAgent<C, M, E> {
    /// Create a synthetic tool call for rule-required tools
    fn create_synthetic_tool_call(tool_name: &str) -> Option<ToolCall> {
        Some(ToolCall {
            call_id: format!("rule_required_{}", uuid::Uuid::new_v4()),
            fn_name: tool_name.to_string(),
            fn_arguments: serde_json::json!({}),
        })
    }
}
```

### Phase 3: Configuration Integration

#### 3.1 Update Agent Configuration

**File:** `pattern/crates/pattern_core/src/config.rs`

Add to agent configuration:

```rust
/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    // ... existing fields ...
    
    /// Tool execution rules
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_rules: Vec<ToolRule>,
}
```

#### 3.2 TOML Configuration Support

Example configuration file:

```toml
[agent]
name = "DataProcessor"
tools = ["load_data", "validate_input", "process_data", "generate_report"]

[[agent.tool_rules]]
tool_name = "initialize_db"
rule_type = "InitialCall"
priority = 10

[[agent.tool_rules]]
tool_name = "validate_input"
rule_type = "MustFollow"
conditions = ["load_data"] 
priority = 7

[[agent.tool_rules]]
tool_name = "cache_lookup" 
rule_type = "NoHeartbeat"
priority = 1

[[agent.tool_rules]]
tool_name = "generate_report"
rule_type = "Terminal"
priority = 8

[[agent.tool_rules]]
tool_name = "close_connections"
rule_type = "RequiredForExit"
priority = 9
```

### Phase 4: Builder Pattern Updates

#### 4.1 Update DatabaseAgent Builder

**File:** `pattern/crates/pattern_core/src/agent/impls/db_agent.rs`

Add builder methods:

```rust
impl DatabaseAgentBuilder {
    /// Add a tool rule to the agent
    pub fn with_tool_rule(mut self, rule: ToolRule) -> Self {
        self.tool_rules.push(rule);
        self
    }
    
    /// Set all tool rules for the agent
    pub fn with_tool_rules(mut self, rules: Vec<ToolRule>) -> Self {
        self.tool_rules = rules;
        self
    }
    
    /// Add a rule that a tool must follow another tool
    pub fn with_tool_dependency(mut self, tool: &str, must_follow: &str) -> Self {
        self.tool_rules.push(ToolRule {
            tool_name: tool.to_string(),
            rule_type: ToolRuleType::MustFollow,
            conditions: vec![must_follow.to_string()],
            priority: 5,
            metadata: None,
        });
        self
    }
    
    /// Mark a tool as not requiring heartbeats
    pub fn with_no_heartbeat_tool(mut self, tool: &str) -> Self {
        self.tool_rules.push(ToolRule {
            tool_name: tool.to_string(),
            rule_type: ToolRuleType::NoHeartbeat,
            conditions: vec![],
            priority: 1,
            metadata: None,
        });
        self
    }
    
    /// Mark a tool as terminal (ends conversation)
    pub fn with_terminal_tool(mut self, tool: &str) -> Self {
        self.tool_rules.push(ToolRule {
            tool_name: tool.to_string(),
            rule_type: ToolRuleType::Terminal,
            conditions: vec![],
            priority: 8,
            metadata: None,
        });
        self
    }
}
```

### Phase 5: Error Handling and Debugging

#### 5.1 Enhanced Error Types

**File:** `pattern/crates/pattern_core/src/agent/tool_rules.rs`

Add to `CoreError`:

```rust
/// Tool rule violation error
#[error("Tool rule violation: {violation}")]
ToolRuleViolation { violation: ToolRuleViolation },
```

#### 5.2 Debug and Validation Tools

**File:** `pattern/crates/pattern_core/src/agent/tool_rules.rs`

```rust
impl ToolRuleEngine {
    /// Validate rule set for conflicts and issues
    pub fn validate_rules(&self) -> Vec<RuleValidationWarning> {
        let mut warnings = Vec::new();
        
        // Check for conflicting rules
        for (i, rule_a) in self.rules.iter().enumerate() {
            for rule_b in self.rules.iter().skip(i + 1) {
                if rule_a.tool_name == rule_b.tool_name {
                    match (&rule_a.rule_type, &rule_b.rule_type) {
                        (ToolRuleType::Terminal, ToolRuleType::RequiredForExit) => {
                            warnings.push(RuleValidationWarning::ConflictingRules {
                                tool: rule_a.tool_name.clone(),
                                rule1: "Terminal".to_string(),
                                rule2: "RequiredForExit".to_string(),
                                description: "Tool marked as terminal cannot be required for exit".to_string(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        
        // Check for unreachable tools due to dependencies
        self.check_dependency_cycles(&mut warnings);
        
        warnings
    }
    
    /// Generate execution plan showing tool order
    pub fn generate_execution_plan(&self) -> ToolExecutionPlan {
        ToolExecutionPlan {
            initial_tools: self.get_initial_tools(),
            required_dependencies: self.get_dependency_graph(),
            terminal_tools: self.get_terminal_tools(),
            exit_requirements: self.get_required_exit_tools(),
        }
    }
    
    /// Enable debug mode for rule violations
    pub fn enable_debug_mode(&mut self, enabled: bool) {
        // Implementation would track rule evaluations for debugging
    }
}

#[derive(Debug, Clone)]
pub enum RuleValidationWarning {
    ConflictingRules {
        tool: String,
        rule1: String,
        rule2: String,
        description: String,
    },
    CircularDependency {
        tools: Vec<String>,
    },
    UnreachableTool {
        tool: String,
        missing_dependencies: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ToolExecutionPlan {
    pub initial_tools: Vec<String>,
    pub required_dependencies: std::collections::HashMap<String, Vec<String>>,
    pub terminal_tools: Vec<String>,
    pub exit_requirements: Vec<String>,
}
```

### Phase 6: Testing Framework

#### 6.1 Rule Testing Utilities

**File:** `pattern/crates/pattern_core/src/agent/tool_rules.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    /// Helper for creating test rules
    pub fn create_test_rule(tool: &str, rule_type: ToolRuleType) -> ToolRule {
        ToolRule {
            tool_name: tool.to_string(),
            rule_type,
            conditions: vec![],
            priority: 5,
            metadata: None,
        }
    }
    
    /// Test agent with forced tool execution for rule validation
    pub struct TestRuleAgent {
        engine: ToolRuleEngine,
    }
    
    impl TestRuleAgent {
        pub fn new(rules: Vec<ToolRule>) -> Self {
            Self {
                engine: ToolRuleEngine::new(rules),
            }
        }
        
        /// Simulate tool execution and check for violations
        pub fn simulate_tool_execution(&mut self, tools: Vec<&str>) -> Result<(), ToolRuleViolation> {
            for tool in tools {
                self.engine.can_execute_tool(tool)?;
                self.engine.record_execution(ToolExecution {
                    tool_name: tool.to_string(),
                    call_id: format!("test_{}", uuid::Uuid::new_v4()),
                    timestamp: std::time::Instant::now(),
                    success: true,
                    metadata: None,
                });
            }
            Ok(())
        }
    }

    #[test]
    fn test_must_follow_rule() {
        let rules = vec![
            create_test_rule("validate", ToolRuleType::MustFollow)
                .with_conditions(vec!["load".to_string()]),
        ];
        
        let mut agent = TestRuleAgent::new(rules);
        
        // Should fail - validate before load
        assert!(agent.simulate_tool_execution(vec!["validate"]).is_err());
        
        // Should succeed - load then validate
        assert!(agent.simulate_tool_execution(vec!["load", "validate"]).is_ok());
    }
    
    #[test]
    fn test_terminal_rule() {
        let rules = vec![
            create_test_rule("deploy", ToolRuleType::Terminal),
        ];
        
        let mut agent = TestRuleAgent::new(rules);
        agent.simulate_tool_execution(vec!["deploy"]).unwrap();
        
        assert!(agent.engine.should_terminate());
    }
    
    #[test]
    fn test_exclusive_group() {
        let rules = vec![
            ToolRule {
                tool_name: "format_json".to_string(),
                rule_type: ToolRuleType::ExclusiveGroup,
                conditions: vec!["format_xml".to_string(), "format_yaml".to_string()],
                priority: 5,
                metadata: None,
            },
        ];
        
        let mut agent = TestRuleAgent::new(rules);
        agent.simulate_tool_execution(vec!["format_xml"]).unwrap();
        
        // Should fail - exclusive group violation
        assert!(agent.simulate_tool_execution(vec!["format_json"]).is_err());
    }
}
```

## Usage Examples

### Example 1: ETL Pipeline Agent

```rust
let etl_agent = DatabaseAgent::builder()
    .with_name("ETLProcessor")
    .with_tool_rules(vec![
        // Initialization
        ToolRule {
            tool_name: "connect_database".to_string(),
            rule_type: ToolRuleType::InitialCall,
            conditions: vec![],
            priority: 10,
            metadata: None,
        },
        
        // Pipeline dependencies
        ToolRule {
            tool_name: "extract_data".to_string(),
            rule_type: ToolRuleType::MustFollow,
            conditions: vec!["connect_database".to_string()],
            priority: 8,
            metadata: None,
        },
        
        ToolRule {
            tool_name: "validate_data".to_string(),
            rule_type: ToolRuleType::MustFollow,
            conditions: vec!["extract_data".to_string()],
            priority: 7,
            metadata: None,
        },
        
        // Terminal operation
        ToolRule {
            tool_name: "load_warehouse".to_string(),
            rule_type: ToolRuleType::Terminal,
            conditions: vec![],
            priority: 9,
            metadata: None,
        },
        
        // Cleanup always required
        ToolRule {
            tool_name: "close_database".to_string(),
            rule_type: ToolRuleType::RequiredForExit,
            conditions: vec![],
            priority: 10,
            metadata: None,
        },
    ])
    .build()
    .await?;
```

### Example 2: API Integration Agent

```rust
let api_agent = DatabaseAgent::builder()
    .with_name("APIProcessor")
    .with_tool_dependency("api_request", "authenticate")
    .with_no_heartbeat_tool("format_response")
    .with_terminal_tool("send_notification")
    .with_tool_rule(ToolRule {
        tool_name: "api_request".to_string(),
        rule_type: ToolRuleType::MaxCalls(3),
        conditions: vec![],
        priority: 5,
        metadata: None,
    })
    .build()
    .await?;
```

### Example 3: Configuration File

```toml
[agent]
name = "SmartAssistant"
tools = ["search", "calculate", "send_email", "schedule"]

# Fast tools don't need heartbeats
[[agent.tool_rules]]
tool_name = "*"
rule_type = "NoHeartbeat"
conditions = ["calculate", "format_text", "validate"]
priority = 1

# Email sending is terminal
[[agent.tool_rules]]
tool_name = "send_email"
rule_type = "Terminal"
priority = 8

# Must search before scheduling
[[agent.tool_rules]]
tool_name = "schedule"
rule_type = "MustFollow"
conditions = ["search"]
priority = 6

# Required cleanup
[[agent.tool_rules]]
tool_name = "save_session"
rule_type = "RequiredForExit"
priority = 9
```

## Implementation Checklist

### Phase 1: Core Structures ✅ (Designed)
- [ ] Create `ToolRule` and `ToolRuleType` enums
- [ ] Implement `ToolRuleEngine` with validation logic
- [ ] Add `ToolExecutionState` tracking
- [ ] Define `ToolRuleViolation` error types

### Phase 2: Agent Integration
- [ ] Add `tool_rules` field to `DatabaseAgent`
- [ ] Update constructor to accept rules
- [ ] Modify `process_message_stream()` to use rule engine
- [ ] Implement rule validation before tool execution
- [ ] Add synthetic tool call creation for required tools

### Phase 3: Configuration Support
- [ ] Add `tool_rules` to agent configuration
- [ ] Support TOML configuration format
- [ ] Add validation for configuration loading
- [ ] Create migration path for existing agents

### Phase 4: Builder Pattern
- [ ] Add rule-related builder methods
- [ ] Create convenience methods for common patterns
- [ ] Support fluent API for rule definition
- [ ] Add validation during build process

### Phase 5: Error Handling
- [ ] Integrate with existing error system
- [ ] Add detailed error messages
- [ ] Create debugging utilities
- [ ] Implement rule conflict detection

### Phase 6: Testing
- [ ] Create unit tests for rule engine
- [ ] Add integration tests with agents
- [ ] Create test utilities for rule validation
- [ ] Add performance benchmarks

## Migration Strategy

### Backwards Compatibility
1. **Default Behavior**: Agents without rules behave exactly as before
2. **Opt-in**: Tool rules are optional and explicitly configured
3. **Graceful Degradation**: Rule violations log warnings but don't crash agents
4. **Configuration Migration**: Existing configurations work without changes

### Rollout Plan
1. **Phase 1**: Implement core rule engine (no breaking changes)
2. **Phase 2**: Add agent integration with feature flag
3. **Phase 3**: Enable by default with comprehensive testing
4. **Phase 4**: Add advanced features and optimizations

## Performance Considerations

### Rule Engine Optimization
- **Rule Sorting**: Sort rules by priority during initialization
- **Caching**: Cache rule evaluations for frequently called tools
- **Lazy Evaluation**: Only evaluate rules when needed
- **Memory Efficient**: Use compact data structures for rule storage

### Tool Execution Impact
- **Minimal Overhead**: Rule checking adds ~1-2ms per tool call
- **Selective Heartbeat**: Can improve performance by 10-20% for fast tools
- **Early Termination**: Reduce unnecessary model calls with terminal rules
- **Batch Operations**: Group rule evaluations when possible

## Security Implications

### Rule Validation
- **Input Sanitization**: Validate tool names and conditions
- **Circular Dependencies**: Prevent infinite loops in dependencies
- **Resource Limits**: Limit rule complexity and execution depth
- **Access Control**: Ensure users can only modify their own agent rules

### Execution Safety
- **Timeout Protection**: Prevent infinite tool execution loops
- **Error Isolation**: Rule violations don't crash the entire agent
- **Audit Logging**: Log all rule evaluations for security analysis
- **Sandboxing**: Isolate rule engine from core agent functionality

## Future Enhancements

### Advanced Rule Types
- **Conditional Logic**: Support complex boolean conditions
- **Time-based Rules**: Rules that activate at specific times
- **Context-aware Rules**: Rules that depend on conversation context
- **Dynamic Rules**: Rules that can be modified during execution

### Integration Opportunities
- **Workflow Engine**: Integration with external workflow systems
- **Monitoring**: Real-time rule compliance monitoring
- **Analytics**: Rule usage and performance analytics
- **Auto-optimization**: Machine learning for rule optimization

## Conclusion

The tool rules system provides a powerful foundation for controlling agent behavior while maintaining the flexibility that makes Pattern agents effective. The implementation plan ensures backwards compatibility while enabling sophisticated workflow control.

Key benefits:
- **Workflow Control**: Enforce complex tool execution patterns
- **Performance Optimization**: Selective heartbeat management
- **Safety**: Prevent invalid tool execution sequences
- **Flexibility**: Configurable rules for different use cases
- **Debuggability**: Clear error messages and validation tools

This system will enable users to create reliable, predictable agent behaviors while optimizing performance and ensuring proper resource management.