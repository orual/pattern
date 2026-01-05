//! Integration test for tool operation gating.
//!
//! This test demonstrates the full tool operation gating flow:
//! 1. Define a multi-operation tool with `operations()` and `parameters_schema_filtered()`
//! 2. Register it in a ToolRegistry
//! 3. Apply AllowedOperations rules
//! 4. Verify the filtered schema only shows allowed operations
//! 5. Verify runtime checking with ToolRuleEngine

use pattern_core::Result;
use pattern_core::tool::{
    AiTool, DynamicToolAdapter, ExecutionMeta, ToolRegistry, ToolRule, ToolRuleEngine,
    filter_schema_enum,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    Append,
    Insert,
    Patch,
    Save,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileInput {
    pub path: String,
    pub operation: FileOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileTool;

#[async_trait::async_trait]
impl AiTool for FileTool {
    type Input = FileInput;
    type Output = String;

    fn name(&self) -> &str {
        "file"
    }

    fn description(&self) -> &str {
        "Read, write, and manipulate files"
    }

    fn operations(&self) -> &'static [&'static str] {
        &["read", "append", "insert", "patch", "save"]
    }

    fn parameters_schema_filtered(&self, allowed_ops: &BTreeSet<String>) -> serde_json::Value {
        let mut schema = self.parameters_schema();
        filter_schema_enum(&mut schema, "operation", allowed_ops);
        schema
    }

    async fn execute(&self, params: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        Ok(format!(
            "Executed {:?} on {}",
            params.operation, params.path
        ))
    }
}

#[tokio::test]
async fn test_file_tool_operation_gating() {
    // Set up registry with tool
    let registry = ToolRegistry::new();
    registry.register(FileTool);

    // Define rules that only allow read and append
    let allowed: BTreeSet<String> = ["read", "append"].iter().map(|s| s.to_string()).collect();
    let rules = vec![ToolRule::allowed_operations("file", allowed.clone())];

    // Get filtered tools - schema should only show allowed operations
    let genai_tools = registry.to_genai_tools_with_rules(&rules);
    assert_eq!(genai_tools.len(), 1);

    // Verify schema only contains allowed operations
    let tool_schema = genai_tools[0]
        .schema
        .as_ref()
        .expect("tool should have schema");
    let enum_values = tool_schema["properties"]["operation"]["enum"]
        .as_array()
        .expect("operation should have enum");
    assert_eq!(enum_values.len(), 2);
    assert!(enum_values.contains(&serde_json::json!("read")));
    assert!(enum_values.contains(&serde_json::json!("append")));
    assert!(!enum_values.contains(&serde_json::json!("patch")));
    assert!(!enum_values.contains(&serde_json::json!("save")));

    // Set up rule engine for runtime checking
    let engine = ToolRuleEngine::new(rules);

    // Check allowed operations pass
    assert!(engine.check_operation_allowed("file", "read").is_ok());
    assert!(engine.check_operation_allowed("file", "append").is_ok());

    // Check disallowed operations fail
    assert!(engine.check_operation_allowed("file", "patch").is_err());
    assert!(engine.check_operation_allowed("file", "save").is_err());
    assert!(engine.check_operation_allowed("file", "insert").is_err());
}

#[tokio::test]
async fn test_tool_without_operations_ignores_rules() {
    // A tool without operations() defined
    #[derive(Debug, Clone)]
    struct SimpleTool;

    #[async_trait::async_trait]
    impl AiTool for SimpleTool {
        type Input = serde_json::Value;
        type Output = String;

        fn name(&self) -> &str {
            "simple"
        }

        fn description(&self) -> &str {
            "A simple tool"
        }

        async fn execute(
            &self,
            _params: Self::Input,
            _meta: &ExecutionMeta,
        ) -> Result<Self::Output> {
            Ok("done".to_string())
        }
    }

    let registry = ToolRegistry::new();
    registry.register(SimpleTool);

    // Rules for a tool without operations should be ignored (with warning)
    let allowed: BTreeSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
    let rules = vec![ToolRule::allowed_operations("simple", allowed)];

    // Should still work - tool appears in output
    let genai_tools = registry.to_genai_tools_with_rules(&rules);
    assert_eq!(genai_tools.len(), 1);
}

#[tokio::test]
async fn test_dynamic_tool_adapter_preserves_operations() {
    // Verify that DynamicToolAdapter correctly delegates operations() and parameters_schema_filtered()
    let file_tool = FileTool;
    let dynamic_tool: Box<dyn pattern_core::DynamicTool> =
        Box::new(DynamicToolAdapter::new(file_tool));

    // Check operations are preserved
    assert_eq!(
        dynamic_tool.operations(),
        &["read", "append", "insert", "patch", "save"]
    );

    // Check filtered schema works through dynamic interface
    let allowed: BTreeSet<String> = ["read", "save"].iter().map(|s| s.to_string()).collect();
    let filtered_schema = dynamic_tool.parameters_schema_filtered(&allowed);

    let enum_values = filtered_schema["properties"]["operation"]["enum"]
        .as_array()
        .expect("operation should have enum");
    assert_eq!(enum_values.len(), 2);
    assert!(enum_values.contains(&serde_json::json!("read")));
    assert!(enum_values.contains(&serde_json::json!("save")));
    assert!(!enum_values.contains(&serde_json::json!("append")));
}

#[tokio::test]
async fn test_operation_gating_with_multiple_tools() {
    // Test that rules are applied correctly when multiple tools are registered

    #[derive(Debug, Clone)]
    struct DatabaseTool;

    #[async_trait::async_trait]
    impl AiTool for DatabaseTool {
        type Input = serde_json::Value;
        type Output = String;

        fn name(&self) -> &str {
            "database"
        }

        fn description(&self) -> &str {
            "Database operations"
        }

        fn operations(&self) -> &'static [&'static str] {
            &["select", "insert", "update", "delete"]
        }

        fn parameters_schema_filtered(&self, allowed_ops: &BTreeSet<String>) -> serde_json::Value {
            // Return a schema that shows which operations were allowed
            serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "enum": allowed_ops.iter().cloned().collect::<Vec<_>>()
                    }
                }
            })
        }

        async fn execute(
            &self,
            _params: Self::Input,
            _meta: &ExecutionMeta,
        ) -> Result<Self::Output> {
            Ok("done".to_string())
        }
    }

    let registry = ToolRegistry::new();
    registry.register(FileTool);
    registry.register(DatabaseTool);

    // Different rules for each tool
    let file_allowed: BTreeSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
    let db_allowed: BTreeSet<String> = ["select", "insert"].iter().map(|s| s.to_string()).collect();

    let rules = vec![
        ToolRule::allowed_operations("file", file_allowed),
        ToolRule::allowed_operations("database", db_allowed),
    ];

    let engine = ToolRuleEngine::new(rules.clone());

    // File tool: only read allowed
    assert!(engine.check_operation_allowed("file", "read").is_ok());
    assert!(engine.check_operation_allowed("file", "append").is_err());

    // Database tool: select and insert allowed
    assert!(engine.check_operation_allowed("database", "select").is_ok());
    assert!(engine.check_operation_allowed("database", "insert").is_ok());
    assert!(
        engine
            .check_operation_allowed("database", "delete")
            .is_err()
    );

    // Verify genai tools are generated with correct filtering
    let genai_tools = registry.to_genai_tools_with_rules(&rules);
    assert_eq!(genai_tools.len(), 2);
}
