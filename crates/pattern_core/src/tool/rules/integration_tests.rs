//! Comprehensive Integration Tests for Tool Rules System
//!
//! This module provides extensive testing coverage for all aspects of the tool rules system,
//! including real agent scenarios, edge cases, performance benchmarks, and configuration testing.

use super::{ToolExecution, ToolRule, ToolRuleEngine};
use crate::{Result, config::ToolRuleConfig, error::CoreError};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Mock tool that tracks its execution
#[derive(Debug, Clone)]
struct MockTool {
    name: String,
    execution_count: Arc<Mutex<u32>>,
    should_fail: bool,
    execution_time: Duration,
}

impl MockTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            execution_count: Arc::new(Mutex::new(0)),
            should_fail: false,
            execution_time: Duration::from_millis(10),
        }
    }

    fn with_failure(mut self) -> Self {
        self.should_fail = true;
        self
    }

    fn with_execution_time(mut self, duration: Duration) -> Self {
        self.execution_time = duration;
        self
    }

    async fn execute(&self) -> Result<String> {
        tokio::time::sleep(self.execution_time).await;

        let mut count = self.execution_count.lock().unwrap();
        *count += 1;

        if self.should_fail {
            Err(CoreError::ToolExecutionFailed {
                tool_name: self.name.clone(),
                cause: format!("Tool {} failed", self.name),
                parameters: serde_json::Value::Null,
            })
        } else {
            Ok(format!("Tool {} executed (count: {})", self.name, *count))
        }
    }

    fn execution_count(&self) -> u32 {
        *self.execution_count.lock().unwrap()
    }
}

/// Mock tool registry for testing
/// Mock agent state for agent-level integration testing
#[derive(Debug, Clone)]
struct MockAgentState {
    executed_tools: Arc<Mutex<Vec<String>>>,
    tool_results: Arc<Mutex<HashMap<String, String>>>,
    rule_engine: Arc<Mutex<ToolRuleEngine>>,
}

impl MockAgentState {
    fn new(rules: Vec<ToolRule>) -> Self {
        Self {
            executed_tools: Arc::new(Mutex::new(Vec::new())),
            tool_results: Arc::new(Mutex::new(HashMap::new())),
            rule_engine: Arc::new(Mutex::new(ToolRuleEngine::new(rules))),
        }
    }

    async fn execute_tool(&self, tool_name: &str) -> Result<String> {
        // Check if tool can be executed according to rules
        {
            let engine = self.rule_engine.lock().unwrap();
            if let Err(violation) = engine.can_execute_tool(tool_name) {
                return Err(CoreError::ToolExecutionFailed {
                    tool_name: tool_name.to_string(),
                    cause: format!("Rule violation: {:?}", violation),
                    parameters: serde_json::Value::Null,
                });
            }
        }

        // Simulate tool execution
        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = format!("Tool {} executed successfully", tool_name);

        // Record execution
        {
            let mut executed = self.executed_tools.lock().unwrap();
            executed.push(tool_name.to_string());

            let mut results = self.tool_results.lock().unwrap();
            results.insert(tool_name.to_string(), result.clone());

            let mut engine = self.rule_engine.lock().unwrap();
            let execution = ToolExecution {
                tool_name: tool_name.to_string(),
                call_id: format!("test_{}", uuid::Uuid::new_v4().simple()),
                timestamp: Instant::now(),
                success: true,
                metadata: None,
            };
            engine.record_execution(execution);
        }

        Ok(result)
    }

    fn get_executed_tools(&self) -> Vec<String> {
        self.executed_tools.lock().unwrap().clone()
    }

    fn can_exit(&self) -> bool {
        let engine = self.rule_engine.lock().unwrap();
        let required_tools = engine.get_required_before_exit_tools();
        required_tools
            .iter()
            .all(|tool| self.executed_tools.lock().unwrap().contains(tool))
    }

    fn get_required_exit_tools(&self) -> Vec<String> {
        let engine = self.rule_engine.lock().unwrap();
        engine.get_required_before_exit_tools()
    }

    fn should_continue_after_tool(&self, tool_name: &str) -> bool {
        let engine = self.rule_engine.lock().unwrap();
        !engine.requires_heartbeat(tool_name)
    }

    fn should_exit_after_tool(&self, tool_name: &str) -> bool {
        let engine = self.rule_engine.lock().unwrap();
        engine.should_exit_after_tool(tool_name)
    }
}

struct MockToolRegistry {
    tools: HashMap<String, MockTool>,
}

impl MockToolRegistry {
    fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    fn add_tool(&mut self, tool: MockTool) {
        self.tools.insert(tool.name.clone(), tool);
    }

    async fn execute_tool(&self, name: &str) -> Result<String> {
        if let Some(tool) = self.tools.get(name) {
            tool.execute().await
        } else {
            Err(CoreError::ToolNotFound {
                tool_name: name.to_string(),
                available_tools: self.tools.keys().cloned().collect(),
                src: "mock_registry".to_string(),
                span: (0, name.len()),
            })
        }
    }

    fn get_tool(&self, name: &str) -> Option<&MockTool> {
        self.tools.get(name)
    }
}

/// Test the complete ETL workflow with tool rules
#[tokio::test]
async fn test_etl_workflow_integration() {
    let mut registry = MockToolRegistry::new();

    // Create ETL tools
    registry.add_tool(MockTool::new("connect_database"));
    registry.add_tool(MockTool::new("extract_data"));
    registry.add_tool(MockTool::new("validate_data"));
    registry.add_tool(MockTool::new("transform_data"));
    registry.add_tool(MockTool::new("load_to_warehouse"));
    registry.add_tool(MockTool::new("disconnect_database"));

    // Create tool rules for ETL workflow
    let rules = vec![
        ToolRule::start_constraint("connect_database".to_string()),
        ToolRule::requires_preceding_tools(
            "extract_data".to_string(),
            vec!["connect_database".to_string()],
        ),
        ToolRule::requires_preceding_tools(
            "validate_data".to_string(),
            vec!["extract_data".to_string()],
        ),
        ToolRule::requires_preceding_tools(
            "transform_data".to_string(),
            vec!["validate_data".to_string()],
        ),
        ToolRule::requires_preceding_tools(
            "load_to_warehouse".to_string(),
            vec!["transform_data".to_string()],
        ),
        ToolRule::required_before_exit("disconnect_database".to_string()),
    ];

    let mut engine = ToolRuleEngine::new(rules.clone());

    // Debug: Print the actual rules that were created
    println!("Created rules:");
    for rule in &rules {
        println!(
            "  Rule: {} -> {:?} with conditions: {:?}",
            rule.tool_name, rule.rule_type, rule.conditions
        );
    }

    // Test proper execution order
    let tools_to_execute = vec![
        "connect_database",
        "extract_data",
        "validate_data",
        "transform_data",
        "load_to_warehouse",
        "disconnect_database",
    ];

    for tool_name in tools_to_execute {
        // Validate the tool can be executed
        let can_execute = engine.can_execute_tool(tool_name);
        if let Err(ref error) = can_execute {
            println!("Tool {} failed validation: {:?}", tool_name, error);
            println!(
                "Start constraint tools: {:?}",
                engine.get_start_constraint_tools()
            );
            println!(
                "Execution history: {:?}",
                engine.get_execution_state().execution_history
            );
        }
        assert!(
            can_execute.is_ok(),
            "Tool {} should be executable",
            tool_name
        );

        // Execute the tool
        let result = registry.execute_tool(tool_name).await;
        assert!(
            result.is_ok(),
            "Tool {} should execute successfully",
            tool_name
        );

        // Record the execution
        let execution = ToolExecution {
            tool_name: tool_name.to_string(),
            call_id: format!("test_{}", uuid::Uuid::new_v4().simple()),
            timestamp: Instant::now(),
            success: true,
            metadata: None,
        };
        engine.record_execution(execution);
    }

    // Verify all tools were executed exactly once
    for tool_name in &[
        "connect_database",
        "extract_data",
        "validate_data",
        "transform_data",
        "load_to_warehouse",
        "disconnect_database",
    ] {
        assert_eq!(registry.get_tool(tool_name).unwrap().execution_count(), 1);
    }

    // Verify engine state
    assert_eq!(engine.get_execution_state().execution_history.len(), 6);
}

/// Test API client scenario with rate limiting and exclusive operations
#[tokio::test]
async fn test_api_client_scenario() {
    let mut registry = MockToolRegistry::new();

    // Create API tools
    registry.add_tool(MockTool::new("authenticate"));
    registry.add_tool(MockTool::new("get_user_profile"));
    registry.add_tool(MockTool::new("post_status"));
    registry.add_tool(MockTool::new("delete_status"));
    registry.add_tool(MockTool::new("send_direct_message"));
    registry.add_tool(MockTool::new("logout"));

    let rules = vec![
        ToolRule::start_constraint("authenticate".to_string()),
        ToolRule::max_calls("post_status".to_string(), 5),
        ToolRule::max_calls("send_direct_message".to_string(), 10),
        ToolRule::cooldown("post_status".to_string(), Duration::from_millis(500)),
        ToolRule::exclusive_groups(
            "post_status".to_string(),
            vec![vec!["post_status".to_string(), "delete_status".to_string()]],
        ),
        ToolRule::exclusive_groups(
            "delete_status".to_string(),
            vec![vec!["post_status".to_string(), "delete_status".to_string()]],
        ),
        ToolRule::required_before_exit("logout".to_string()),
    ];

    let mut engine = ToolRuleEngine::new(rules);

    // Execute authentication first
    assert!(engine.can_execute_tool("authenticate").is_ok());
    let _result = registry.execute_tool("authenticate").await.unwrap();
    engine.record_execution(ToolExecution {
        tool_name: "authenticate".to_string(),
        call_id: "auth_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Test max calls limit
    for i in 1..=5 {
        let can_execute = engine.can_execute_tool("post_status");
        if let Err(ref error) = can_execute {
            println!("post_status call {} failed: {:?}", i, error);
            println!(
                "Current call counts: {:?}",
                engine.get_execution_state().call_counts
            );
            println!("Max calls rule should allow 5, current attempt: {}", i);
        }
        assert!(can_execute.is_ok(), "Should allow post_status call {}", i);
        let _result = registry.execute_tool("post_status").await.unwrap();
        engine.record_execution(ToolExecution {
            tool_name: "post_status".to_string(),
            call_id: format!("post_{}", i),
            timestamp: Instant::now(),
            success: true,
            metadata: None,
        });
        println!(
            "Completed post_status call {}, current count: {:?}",
            i,
            engine.get_execution_state().call_counts.get("post_status")
        );

        // Wait for cooldown between calls (500ms + buffer)
        if i < 5 {
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
    }

    // Sixth call should fail due to max calls
    assert!(engine.can_execute_tool("post_status").is_err());

    // Test exclusive groups - delete_status should be blocked while post_status is active
    let delete_result = engine.can_execute_tool("delete_status");
    if delete_result.is_ok() {
        println!("delete_status was allowed when it should be blocked!");
        println!(
            "Execution history: {:?}",
            engine
                .get_execution_state()
                .execution_history
                .iter()
                .map(|e| &e.tool_name)
                .collect::<Vec<_>>()
        );
        println!("Looking for exclusive group rule violations...");
    }
    assert!(
        delete_result.is_err(),
        "delete_status should be blocked due to exclusive group with post_status"
    );

    // Logout should be required before exit
    let exit_tools = engine.get_required_before_exit_tools();
    assert!(exit_tools.contains(&"logout".to_string()));

    let _result = registry.execute_tool("logout").await.unwrap();
    engine.record_execution(ToolExecution {
        tool_name: "logout".to_string(),
        call_id: "logout_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });
}

/// Test complex orchestrator scenario with multiple rule types
#[tokio::test]
async fn test_complex_orchestrator_scenario() {
    let mut registry = MockToolRegistry::new();

    // Create a complex set of tools
    let tool_names = vec![
        "initialize_system",
        "load_config",
        "connect_services",
        "health_check",
        "process_queue",
        "send_notifications",
        "update_metrics",
        "backup_data",
        "validate_state",
        "generate_report",
        "cleanup_temp",
        "archive_logs",
        "shutdown",
    ];

    for name in &tool_names {
        registry.add_tool(MockTool::new(name));
    }

    let rules = vec![
        // Initialization sequence
        ToolRule::start_constraint("initialize_system".to_string()),
        ToolRule::requires_preceding_tools(
            "load_config".to_string(),
            vec!["initialize_system".to_string()],
        ),
        ToolRule::requires_preceding_tools(
            "connect_services".to_string(),
            vec!["load_config".to_string()],
        ),
        ToolRule::requires_preceding_tools(
            "health_check".to_string(),
            vec!["connect_services".to_string()],
        ),
        // Processing tools with limits
        ToolRule::max_calls("process_queue".to_string(), 3),
        ToolRule::cooldown("send_notifications".to_string(), Duration::from_millis(100)),
        // Exclusive operations
        ToolRule::exclusive_groups(
            "backup_data".to_string(),
            vec![
                vec!["backup_data".to_string(), "generate_report".to_string()],
                vec!["cleanup_temp".to_string(), "archive_logs".to_string()],
            ],
        ),
        ToolRule::exclusive_groups(
            "generate_report".to_string(),
            vec![
                vec!["backup_data".to_string(), "generate_report".to_string()],
                vec!["cleanup_temp".to_string(), "archive_logs".to_string()],
            ],
        ),
        ToolRule::exclusive_groups(
            "cleanup_temp".to_string(),
            vec![
                vec!["backup_data".to_string(), "generate_report".to_string()],
                vec!["cleanup_temp".to_string(), "archive_logs".to_string()],
            ],
        ),
        ToolRule::exclusive_groups(
            "archive_logs".to_string(),
            vec![
                vec!["backup_data".to_string(), "generate_report".to_string()],
                vec!["cleanup_temp".to_string(), "archive_logs".to_string()],
            ],
        ),
        // Performance optimizations
        ToolRule::continue_loop("update_metrics".to_string()),
        ToolRule::continue_loop("validate_state".to_string()),
        // Cleanup sequence
        ToolRule::required_before_exit("cleanup_temp".to_string()),
        ToolRule::required_before_exit("shutdown".to_string()),
    ];

    let mut engine = ToolRuleEngine::new(rules);

    // Execute initialization sequence
    let init_sequence = vec![
        "initialize_system",
        "load_config",
        "connect_services",
        "health_check",
    ];
    for tool_name in init_sequence {
        assert!(engine.can_execute_tool(tool_name).is_ok());
        let _result = registry.execute_tool(tool_name).await.unwrap();
        engine.record_execution(ToolExecution {
            tool_name: tool_name.to_string(),
            call_id: format!("init_{}", tool_name),
            timestamp: Instant::now(),
            success: true,
            metadata: None,
        });
    }

    // Test processing with limits
    for i in 1..=3 {
        assert!(engine.can_execute_tool("process_queue").is_ok());
        let _result = registry.execute_tool("process_queue").await.unwrap();
        engine.record_execution(ToolExecution {
            tool_name: "process_queue".to_string(),
            call_id: format!("process_{}", i),
            timestamp: Instant::now(),
            success: true,
            metadata: None,
        });
    }

    // Fourth call should fail
    assert!(engine.can_execute_tool("process_queue").is_err());

    // Test exclusive groups
    assert!(engine.can_execute_tool("backup_data").is_ok());
    let _result = registry.execute_tool("backup_data").await.unwrap();
    engine.record_execution(ToolExecution {
        tool_name: "backup_data".to_string(),
        call_id: "backup_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // generate_report should be blocked
    assert!(engine.can_execute_tool("generate_report").is_err());

    // Execute performance-optimized tools
    let _result = registry.execute_tool("update_metrics").await.unwrap();
    engine.record_execution(ToolExecution {
        tool_name: "update_metrics".to_string(),
        call_id: "metrics_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Execute required cleanup
    let _result = registry.execute_tool("cleanup_temp").await.unwrap();
    engine.record_execution(ToolExecution {
        tool_name: "cleanup_temp".to_string(),
        call_id: "cleanup_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    let _result = registry.execute_tool("shutdown").await.unwrap();
    engine.record_execution(ToolExecution {
        tool_name: "shutdown".to_string(),
        call_id: "shutdown_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });
}

/// Test rule violations and error handling
#[tokio::test]
async fn test_rule_violations() {
    let rules = vec![
        ToolRule::start_constraint("init".to_string()),
        ToolRule::requires_preceding_tools("step2".to_string(), vec!["step1".to_string()]),
        ToolRule::max_calls("limited".to_string(), 2),
        ToolRule::exclusive_groups(
            "exclusive_a".to_string(),
            vec![vec!["exclusive_a".to_string(), "exclusive_b".to_string()]],
        ),
        ToolRule::exclusive_groups(
            "exclusive_b".to_string(),
            vec![vec!["exclusive_a".to_string(), "exclusive_b".to_string()]],
        ),
    ];

    let mut engine = ToolRuleEngine::new(rules);

    // Test missing start constraint
    let result = engine.can_execute_tool("step1");
    assert!(result.is_err());

    // Execute start constraint
    engine.record_execution(ToolExecution {
        tool_name: "init".to_string(),
        call_id: "init_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Test missing prerequisite
    let result = engine.can_execute_tool("step2");
    assert!(result.is_err());

    // Execute prerequisite
    engine.record_execution(ToolExecution {
        tool_name: "step1".to_string(),
        call_id: "step1_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Now step2 should work
    assert!(engine.can_execute_tool("step2").is_ok());

    // Test max calls
    engine.record_execution(ToolExecution {
        tool_name: "limited".to_string(),
        call_id: "limited_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });
    engine.record_execution(ToolExecution {
        tool_name: "limited".to_string(),
        call_id: "limited_2".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Third call should fail
    assert!(engine.can_execute_tool("limited").is_err());

    // Test exclusive groups
    engine.record_execution(ToolExecution {
        tool_name: "exclusive_a".to_string(),
        call_id: "exclusive_a_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // exclusive_b should be blocked
    assert!(engine.can_execute_tool("exclusive_b").is_err());
}

/// Test cooldown functionality
#[tokio::test]
async fn test_cooldown_functionality() {
    let rules = vec![ToolRule::cooldown(
        "slow_tool".to_string(),
        Duration::from_millis(100),
    )];

    let mut engine = ToolRuleEngine::new(rules);

    // First execution should work
    assert!(engine.can_execute_tool("slow_tool").is_ok());

    engine.record_execution(ToolExecution {
        tool_name: "slow_tool".to_string(),
        call_id: "slow_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Immediate second execution should fail
    assert!(engine.can_execute_tool("slow_tool").is_err());

    // Wait for cooldown
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Now should work again
    assert!(engine.can_execute_tool("slow_tool").is_ok());
}

/// Test performance rules (continue_loop, exit_loop)
#[tokio::test]
async fn test_performance_rules() {
    let rules = vec![
        ToolRule::continue_loop("fast_search".to_string()),
        ToolRule::exit_loop("send_result".to_string()),
    ];

    let engine = ToolRuleEngine::new(rules);

    // Check that performance rules are properly categorized
    // We can't access the rules directly, but we can test the behavior
    assert!(!engine.requires_heartbeat("fast_search"));
    assert!(engine.requires_heartbeat("other_tool"));

    // Test that the rules were created correctly by checking their effects
    let start_tools = engine.get_start_constraint_tools();
    assert!(start_tools.is_empty()); // No start constraints in this test

    // The engine should handle these tools appropriately
    assert!(engine.can_execute_tool("fast_search").is_ok());
    assert!(engine.can_execute_tool("send_result").is_ok());
}

/// Test configuration serialization and deserialization
#[tokio::test]
async fn test_configuration_roundtrip() {
    let original_rules = vec![
        ToolRule::start_constraint("init".to_string()),
        ToolRule::continue_loop("fast".to_string()),
        ToolRule::exit_loop("final".to_string()),
        ToolRule::requires_preceding_tools("step2".to_string(), vec!["step1".to_string()]),
    ];

    // Convert to config format
    let config_rules: Vec<ToolRuleConfig> = original_rules
        .iter()
        .map(|rule| ToolRuleConfig::from_tool_rule(rule))
        .collect();

    // Convert back to runtime format
    let restored_rules: Result<Vec<ToolRule>> = config_rules
        .into_iter()
        .map(|config| config.to_tool_rule())
        .collect();

    assert!(restored_rules.is_ok());
    let restored_rules = restored_rules.unwrap();

    // Verify they match
    assert_eq!(original_rules.len(), restored_rules.len());

    for (original, restored) in original_rules.iter().zip(restored_rules.iter()) {
        assert_eq!(original.tool_name, restored.tool_name);
        // Note: We can't easily compare rule_type due to complex enum structure
        assert_eq!(original.priority, restored.priority);
    }
}

/// Test agent-level workflow with exit requirements and loop control
#[tokio::test]
async fn test_agent_lifecycle_with_exit_requirements() {
    let rules = vec![
        ToolRule::start_constraint("initialize".to_string()),
        ToolRule::requires_preceding_tools(
            "process_data".to_string(),
            vec!["initialize".to_string()],
        ),
        ToolRule::continue_loop("process_data".to_string()),
        ToolRule::exit_loop("finalize_processing".to_string()),
        ToolRule::required_before_exit("cleanup".to_string()),
        ToolRule::required_before_exit("save_state".to_string()),
        ToolRule::max_calls("process_data".to_string(), 3),
    ];

    let agent = MockAgentState::new(rules);

    // Initially cannot exit - no tools executed
    assert!(!agent.can_exit());
    let required_tools = agent.get_required_exit_tools();
    assert_eq!(required_tools.len(), 2);
    assert!(required_tools.contains(&"cleanup".to_string()));
    assert!(required_tools.contains(&"save_state".to_string()));

    // Execute initialization
    assert!(agent.execute_tool("initialize").await.is_ok());
    assert!(!agent.can_exit()); // Still need exit requirements

    // Process data multiple times (continue loop)
    for _i in 0..3 {
        assert!(agent.execute_tool("process_data").await.is_ok());
        assert!(agent.should_continue_after_tool("process_data"));
        assert!(!agent.should_exit_after_tool("process_data"));
        assert!(!agent.can_exit()); // Still need exit requirements
    }

    // Fourth attempt should fail due to max calls
    assert!(agent.execute_tool("process_data").await.is_err());

    // Finalize processing (exit loop trigger)
    assert!(agent.execute_tool("finalize_processing").await.is_ok());
    assert!(agent.should_exit_after_tool("finalize_processing"));
    assert!(!agent.can_exit()); // Still need exit requirements

    // Execute one exit requirement
    assert!(agent.execute_tool("cleanup").await.is_ok());
    assert!(!agent.can_exit()); // Still need save_state

    // Execute final exit requirement
    assert!(agent.execute_tool("save_state").await.is_ok());
    assert!(agent.can_exit()); // Now we can exit

    // Verify execution order
    let executed = agent.get_executed_tools();
    assert_eq!(
        executed,
        vec![
            "initialize",
            "process_data",
            "process_data",
            "process_data",
            "finalize_processing",
            "cleanup",
            "save_state"
        ]
    );
}

/// Test tool failure scenarios and error handling
#[tokio::test]
async fn test_tool_failure_scenarios() {
    let mut registry = MockToolRegistry::new();

    // Create normal and failing tools
    registry.add_tool(MockTool::new("setup"));
    registry.add_tool(MockTool::new("reliable_task"));
    registry.add_tool(MockTool::new("flaky_task").with_failure());
    registry.add_tool(MockTool::new("cleanup"));

    let rules = vec![
        ToolRule::start_constraint("setup".to_string()),
        ToolRule::requires_preceding_tools("reliable_task".to_string(), vec!["setup".to_string()]),
        ToolRule::requires_preceding_tools(
            "flaky_task".to_string(),
            vec!["reliable_task".to_string()],
        ),
        ToolRule::required_before_exit("cleanup".to_string()),
        ToolRule::max_calls("flaky_task".to_string(), 3),
    ];

    let mut engine = ToolRuleEngine::new(rules);

    // Execute setup successfully
    assert!(engine.can_execute_tool("setup").is_ok());
    let result = registry.execute_tool("setup").await;
    assert!(result.is_ok());
    engine.record_execution(ToolExecution {
        tool_name: "setup".to_string(),
        call_id: "setup_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Execute reliable task successfully
    assert!(engine.can_execute_tool("reliable_task").is_ok());
    let result = registry.execute_tool("reliable_task").await;
    assert!(result.is_ok());
    engine.record_execution(ToolExecution {
        tool_name: "reliable_task".to_string(),
        call_id: "reliable_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Attempt flaky task multiple times - should fail but rules still allow retries
    for attempt in 1..=3 {
        assert!(
            engine.can_execute_tool("flaky_task").is_ok(),
            "Rule engine should allow attempt {}",
            attempt
        );

        let result = registry.execute_tool("flaky_task").await;
        assert!(
            result.is_err(),
            "Flaky task should fail on attempt {}",
            attempt
        );

        // Record failed execution
        engine.record_execution(ToolExecution {
            tool_name: "flaky_task".to_string(),
            call_id: format!("flaky_attempt_{}", attempt),
            timestamp: Instant::now(),
            success: false,
            metadata: None,
        });
    }

    // Fourth attempt should be blocked by max calls rule
    assert!(
        engine.can_execute_tool("flaky_task").is_err(),
        "Should be blocked by max calls after 3 attempts"
    );

    // Cleanup should still be required and executable
    let exit_tools = engine.get_required_before_exit_tools();
    assert!(exit_tools.contains(&"cleanup".to_string()));

    assert!(engine.can_execute_tool("cleanup").is_ok());
    let result = registry.execute_tool("cleanup").await;
    assert!(result.is_ok());
    engine.record_execution(ToolExecution {
        tool_name: "cleanup".to_string(),
        call_id: "cleanup_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    // Verify execution counts
    assert_eq!(registry.get_tool("setup").unwrap().execution_count(), 1);
    assert_eq!(
        registry
            .get_tool("reliable_task")
            .unwrap()
            .execution_count(),
        1
    );
    assert_eq!(
        registry.get_tool("flaky_task").unwrap().execution_count(),
        3
    );
    assert_eq!(registry.get_tool("cleanup").unwrap().execution_count(), 1);
}

/// Test performance and timing with slow tools
#[tokio::test]
async fn test_tool_timing_scenarios() {
    let mut registry = MockToolRegistry::new();

    // Create tools with different execution times
    registry.add_tool(MockTool::new("fast_task")); // Default 10ms
    registry.add_tool(MockTool::new("slow_task").with_execution_time(Duration::from_millis(100)));
    registry
        .add_tool(MockTool::new("very_slow_task").with_execution_time(Duration::from_millis(200)));

    let rules = vec![
        ToolRule::requires_preceding_tools("slow_task".to_string(), vec!["fast_task".to_string()]),
        ToolRule::requires_preceding_tools(
            "very_slow_task".to_string(),
            vec!["slow_task".to_string()],
        ),
    ];

    let mut engine = ToolRuleEngine::new(rules);

    // Measure execution times
    let start = Instant::now();

    // Fast task should complete quickly
    assert!(engine.can_execute_tool("fast_task").is_ok());
    let result = registry.execute_tool("fast_task").await;
    assert!(result.is_ok());
    engine.record_execution(ToolExecution {
        tool_name: "fast_task".to_string(),
        call_id: "fast_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    let after_fast = Instant::now();
    assert!(
        after_fast.duration_since(start) < Duration::from_millis(50),
        "Fast task took too long"
    );

    // Slow task should take longer
    assert!(engine.can_execute_tool("slow_task").is_ok());
    let result = registry.execute_tool("slow_task").await;
    assert!(result.is_ok());
    engine.record_execution(ToolExecution {
        tool_name: "slow_task".to_string(),
        call_id: "slow_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    let after_slow = Instant::now();
    assert!(
        after_slow.duration_since(after_fast) >= Duration::from_millis(90),
        "Slow task didn't take expected time"
    );

    // Very slow task should take even longer
    assert!(engine.can_execute_tool("very_slow_task").is_ok());
    let result = registry.execute_tool("very_slow_task").await;
    assert!(result.is_ok());
    engine.record_execution(ToolExecution {
        tool_name: "very_slow_task".to_string(),
        call_id: "very_slow_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    let total_time = Instant::now().duration_since(start);
    assert!(
        total_time >= Duration::from_millis(300),
        "Total execution time should reflect cumulative delays"
    );

    // Verify all tools executed once
    assert_eq!(registry.get_tool("fast_task").unwrap().execution_count(), 1);
    assert_eq!(registry.get_tool("slow_task").unwrap().execution_count(), 1);
    assert_eq!(
        registry
            .get_tool("very_slow_task")
            .unwrap()
            .execution_count(),
        1
    );
}

/// Benchmark tool rule validation performance
#[tokio::test]
async fn test_validation_performance() {
    let rules = vec![
        ToolRule::start_constraint("init".to_string()),
        ToolRule::requires_preceding_tools("step1".to_string(), vec!["init".to_string()]),
        ToolRule::requires_preceding_tools("step2".to_string(), vec!["step1".to_string()]),
        ToolRule::max_calls("limited".to_string(), 100),
        ToolRule::cooldown("slow".to_string(), Duration::from_millis(1)),
    ];

    let mut engine = ToolRuleEngine::new(rules);

    // Execute prerequisite
    engine.record_execution(ToolExecution {
        tool_name: "init".to_string(),
        call_id: "init_1".to_string(),
        timestamp: Instant::now(),
        success: true,
        metadata: None,
    });

    let start = Instant::now();
    let iterations = 10000;

    for _ in 0..iterations {
        let _ = engine.can_execute_tool("step1");
    }

    let duration = start.elapsed();
    let ops_per_sec = iterations as f64 / duration.as_secs_f64();

    println!("Validation performance: {:.0} ops/sec", ops_per_sec);

    // Should be able to do at least 1000 validations per second
    assert!(
        ops_per_sec > 1000.0,
        "Validation too slow: {} ops/sec",
        ops_per_sec
    );
}
