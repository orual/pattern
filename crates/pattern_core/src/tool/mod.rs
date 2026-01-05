pub mod builtin;
mod mod_utils;
mod registry;
pub mod rules;
pub mod schema_filter;

pub use registry::{CustomToolFactory, available_custom_tools, create_custom_tool};
pub use schema_filter::filter_schema_enum;

// Re-export rule types at tool module level
pub use rules::{
    ExecutionPhase, ToolExecution, ToolExecutionState, ToolRule, ToolRuleEngine, ToolRuleType,
    ToolRuleViolation,
};

use async_trait::async_trait;
use compact_str::{CompactString, ToCompactString};
use schemars::{JsonSchema, generate::SchemaGenerator, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, fmt::Debug, sync::Arc};

use crate::Result;

/// Execution metadata provided to tools at runtime
#[derive(Debug, Clone, Default)]
pub struct ExecutionMeta {
    /// Optional permission grant for bypassing ACLs in specific scopes
    pub permission_grant: Option<crate::permission::PermissionGrant>,
    /// Whether the caller requests a heartbeat continuation after execution
    pub request_heartbeat: bool,
    /// Optional caller user context
    pub caller_user: Option<crate::UserId>,
    /// Optional tool call id for tracing
    pub call_id: Option<crate::ToolCallId>,
    /// Optional routing metadata (e.g., discord_channel_id) to help permission prompts reach the origin
    pub route_metadata: Option<serde_json::Value>,
}

/// A tool that can be executed by agents with type-safe input and output
#[async_trait]
pub trait AiTool: Send + Sync + Debug {
    /// The input type for this tool
    type Input: JsonSchema + for<'de> Deserialize<'de> + Serialize + Send + Sync;

    /// The output type for this tool
    type Output: JsonSchema + Serialize + Send + Sync;

    /// Get the name of this tool
    fn name(&self) -> &str;

    /// Get a human-readable description of what this tool does
    fn description(&self) -> &str;

    /// Execute the tool with the given parameters and execution metadata
    async fn execute(&self, params: Self::Input, meta: &ExecutionMeta) -> Result<Self::Output>;

    /// Get usage examples for this tool
    fn examples(&self) -> Vec<ToolExample<Self::Input, Self::Output>> {
        Vec::new()
    }

    /// Get the JSON schema for the tool's parameters (MCP-compatible, no refs)
    fn parameters_schema(&self) -> Value {
        let mut settings = SchemaSettings::default();
        settings.inline_subschemas = true;
        settings.meta_schema = None;

        let generator = SchemaGenerator::new(settings);
        let schema = generator.into_root_schema_for::<Self::Input>();

        let mut schema_val = serde_json::to_value(schema).unwrap_or_else(|_| {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        });

        // Best-effort inject request_heartbeat into the parameters schema
        crate::tool::mod_utils::inject_request_heartbeat(&mut schema_val);
        schema_val
    }

    /// Get the JSON schema for the tool's output (MCP-compatible, no refs)
    fn output_schema(&self) -> Value {
        let mut settings = SchemaSettings::default();
        settings.inline_subschemas = true;
        settings.meta_schema = None;

        let generator = SchemaGenerator::new(settings);
        let schema = generator.into_root_schema_for::<Self::Output>();

        serde_json::to_value(schema).unwrap_or_else(|_| {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
        })
    }

    /// Get the usage rule for this tool (e.g., "requires continuing your response when called")
    fn usage_rule(&self) -> Option<&'static str> {
        None
    }

    /// Get execution rules for this tool
    ///
    /// Tools can declare their execution behavior (continue/exit loop, dependencies, etc.)
    /// by returning ToolRule values. The tool_name field should match self.name().
    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![]
    }

    /// Operations this tool supports. Empty slice means not operation-based.
    /// Return static strings matching the operation enum variant names (snake_case).
    fn operations(&self) -> &'static [&'static str] {
        &[]
    }

    /// Generate schema filtered to only allowed operations.
    /// Default implementation returns full schema (no filtering).
    fn parameters_schema_filtered(&self, allowed_ops: &BTreeSet<String>) -> Value {
        let _ = allowed_ops; // unused in default impl
        self.parameters_schema()
    }

    /// Convert to a genai Tool
    fn to_genai_tool(&self) -> genai::chat::Tool {
        genai::chat::Tool::new(self.name())
            .with_description(self.description())
            .with_schema(self.parameters_schema())
    }
}

/// Type-erased version of AiTool for dynamic dispatch
#[async_trait]
pub trait DynamicTool: Send + Sync + Debug {
    /// Clone the tool into a boxed trait object
    fn clone_box(&self) -> Box<dyn DynamicTool>;

    /// Get the name of this tool
    fn name(&self) -> &str;

    /// Get a human-readable description of what this tool does
    fn description(&self) -> &str;

    /// Get the JSON schema for the tool's parameters
    fn parameters_schema(&self) -> Value;

    /// Get the JSON schema for the tool's output
    fn output_schema(&self) -> Value;

    /// Execute the tool with the given parameters and metadata
    async fn execute(&self, params: Value, meta: &ExecutionMeta) -> Result<Value>;

    /// Validate the parameters against the schema
    fn validate_params(&self, _params: &Value) -> Result<()> {
        // Default implementation that just passes validation
        // In a real implementation, this would validate against the schema
        Ok(())
    }

    /// Get usage examples for this tool
    fn examples(&self) -> Vec<DynamicToolExample>;

    /// Get the usage rule for this tool
    fn usage_rule(&self) -> Option<&'static str>;

    /// Get execution rules for this tool
    fn tool_rules(&self) -> Vec<ToolRule>;

    /// Convert to a genai Tool
    fn to_genai_tool(&self) -> genai::chat::Tool {
        genai::chat::Tool::new(self.name())
            .with_description(self.description())
            .with_schema(self.parameters_schema())
    }

    /// Operations this tool supports. Empty slice means not operation-based.
    fn operations(&self) -> &'static [&'static str] {
        &[]
    }

    /// Generate schema filtered to only allowed operations.
    fn parameters_schema_filtered(&self, allowed_ops: &BTreeSet<String>) -> serde_json::Value {
        let _ = allowed_ops;
        self.parameters_schema()
    }

    /// Convert to genai Tool with operation filtering applied
    fn to_genai_tool_filtered(&self, allowed_ops: Option<&BTreeSet<String>>) -> genai::chat::Tool {
        let schema = match allowed_ops {
            Some(ops) => self.parameters_schema_filtered(ops),
            None => self.parameters_schema(),
        };
        genai::chat::Tool::new(self.name())
            .with_description(self.description())
            .with_schema(schema)
    }
}

/// Implement Clone for Box<dyn DynamicTool>
impl Clone for Box<dyn DynamicTool> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Adapter to convert a typed AiTool into a DynamicTool
#[derive(Clone)]
pub struct DynamicToolAdapter<T: AiTool> {
    inner: T,
}

impl<T: AiTool> DynamicToolAdapter<T> {
    pub fn new(tool: T) -> Self {
        Self { inner: tool }
    }
}

impl<T: AiTool> Debug for DynamicToolAdapter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicToolAdapter")
            .field("tool", &self.inner)
            .finish()
    }
}

#[async_trait]
impl<T> DynamicTool for DynamicToolAdapter<T>
where
    T: AiTool + Clone + 'static,
{
    fn clone_box(&self) -> Box<dyn DynamicTool> {
        Box::new(self.clone())
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }

    fn output_schema(&self) -> Value {
        self.inner.output_schema()
    }

    async fn execute(&self, mut params: Value, meta: &ExecutionMeta) -> Result<Value> {
        // Deserialize the JSON value into the tool's input type
        // Strip request_heartbeat if present in object form
        if let Value::Object(ref mut map) = params {
            map.remove("request_heartbeat");
        }
        let input: T::Input = serde_json::from_value(params)
            .map_err(|e| crate::CoreError::tool_validation_error(self.name(), e.to_string()))?;

        // Execute the tool
        let output = self.inner.execute(input, meta).await?;

        // Serialize the output back to JSON
        serde_json::to_value(output)
            .map_err(|e| crate::CoreError::tool_exec_error_simple(self.name(), e))
    }

    fn examples(&self) -> Vec<DynamicToolExample> {
        self.inner
            .examples()
            .into_iter()
            .map(|ex| DynamicToolExample {
                description: ex.description,
                parameters: serde_json::to_value(ex.parameters).unwrap_or(Value::Null),
                expected_output: ex
                    .expected_output
                    .map(|o| serde_json::to_value(o).unwrap_or(Value::Null)),
            })
            .collect()
    }

    fn usage_rule(&self) -> Option<&'static str> {
        self.inner.usage_rule()
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        self.inner.tool_rules()
    }

    fn operations(&self) -> &'static [&'static str] {
        self.inner.operations()
    }

    fn parameters_schema_filtered(&self, allowed_ops: &BTreeSet<String>) -> serde_json::Value {
        self.inner.parameters_schema_filtered(allowed_ops)
    }
}

/// An example of how to use a tool with typed parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExample<I, O> {
    pub description: String,
    pub parameters: I,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<O>,
}

/// An example of how to use a tool with dynamic parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolExample {
    pub description: String,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<Value>,
}

/// A registry for managing available tools
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    tools: Arc<dashmap::DashMap<CompactString, Box<dyn DynamicTool>>>,
}

impl ToolRegistry {
    /// Create a new empty tool registry
    pub fn new() -> Self {
        Self {
            tools: Arc::new(dashmap::DashMap::new()),
        }
    }

    /// Register a typed tool
    pub fn register<T: AiTool + Clone + 'static>(&self, tool: T) {
        let dynamic_tool = DynamicToolAdapter::new(tool);
        self.tools.insert(
            dynamic_tool.name().to_compact_string(),
            Box::new(dynamic_tool),
        );
    }

    /// Register a dynamic tool directly
    pub fn register_dynamic(&self, tool: Box<dyn DynamicTool>) {
        self.tools.insert(tool.name().to_compact_string(), tool);
    }

    /// Remove a tool by name, returning it if it existed
    pub fn remove(&self, name: &str) -> Option<Box<dyn DynamicTool>> {
        self.tools.remove(name).map(|(_, tool)| tool)
    }

    /// Get a tool by name
    pub fn get(
        &self,
        name: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, CompactString, Box<dyn DynamicTool>>> {
        self.tools.get(name)
    }

    /// Get all tool names
    pub fn list_tools(&self) -> Arc<[CompactString]> {
        self.tools.iter().map(|e| e.key().clone()).collect()
    }

    /// Create a deep clone of the registry with all tools copied to a new registry
    pub fn deep_clone(&self) -> Self {
        let new_registry = Self::new();
        for entry in self.tools.iter() {
            new_registry
                .tools
                .insert(entry.key().clone(), entry.value().clone_box());
        }
        new_registry
    }

    /// Execute a tool by name
    pub async fn execute(
        &self,
        tool_name: &str,
        params: Value,
        meta: &ExecutionMeta,
    ) -> Result<Value> {
        let tool = self.get(tool_name).ok_or_else(|| {
            crate::CoreError::tool_not_found(
                tool_name,
                self.list_tools()
                    .iter()
                    .map(CompactString::to_string)
                    .collect(),
            )
        })?;

        tool.execute(params, meta).await
    }

    /// Get all tools as genai tools
    pub fn to_genai_tools(&self) -> Vec<genai::chat::Tool> {
        self.tools
            .iter()
            .map(|entry| entry.value().to_genai_tool())
            .collect()
    }

    /// Get all tools as genai tools, applying operation gating from rules.
    pub fn to_genai_tools_with_rules(&self, rules: &[ToolRule]) -> Vec<genai::chat::Tool> {
        self.tools
            .iter()
            .map(|entry| {
                let tool = entry.value();
                let tool_name = tool.name();

                // Find AllowedOperations rule for this tool
                let allowed_ops = self.find_allowed_operations(tool_name, rules);

                // Validate configured operations if present
                if let Some(ref ops) = allowed_ops {
                    self.validate_operations(tool_name, tool.operations(), ops);
                }

                // Use the filtered conversion method on DynamicTool
                tool.to_genai_tool_filtered(allowed_ops.as_ref())
            })
            .collect()
    }

    /// Find AllowedOperations rule for a tool.
    fn find_allowed_operations(
        &self,
        tool_name: &str,
        rules: &[ToolRule],
    ) -> Option<BTreeSet<String>> {
        rules
            .iter()
            .find(|r| r.tool_name == tool_name)
            .and_then(|r| match &r.rule_type {
                ToolRuleType::AllowedOperations(ops) => Some(ops.clone()),
                _ => None,
            })
    }

    /// Validate that configured operations exist on the tool.
    fn validate_operations(
        &self,
        tool_name: &str,
        declared: &'static [&'static str],
        configured: &BTreeSet<String>,
    ) {
        if declared.is_empty() {
            tracing::warn!(
                tool = tool_name,
                "AllowedOperations rule applied to tool that doesn't declare operations"
            );
            return;
        }

        let declared_set: std::collections::HashSet<&str> = declared.iter().copied().collect();
        for op in configured {
            if !declared_set.contains(op.as_str()) {
                tracing::warn!(
                    tool = tool_name,
                    operation = op,
                    available = ?declared,
                    "Configured operation not found in tool's declared operations"
                );
            }
        }
    }

    /// Get all tools as dynamic tool trait objects
    pub fn get_all_as_dynamic(&self) -> Vec<Box<dyn DynamicTool>> {
        self.tools
            .iter()
            .map(|entry| entry.value().clone_box())
            .collect()
    }

    /// Get tool execution rules for all registered tools
    pub fn get_tool_rules(&self) -> Vec<ToolRule> {
        self.tools
            .iter()
            .flat_map(|entry| entry.value().tool_rules())
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of executing a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub metadata: ToolResultMetadata,
}

/// Metadata about a tool execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolResultMetadata {
    #[serde(
        with = "crate::utils::duration_millis",
        skip_serializing_if = "Option::is_none"
    )]
    pub execution_time: Option<std::time::Duration>,
    pub retries: usize,
    pub warnings: Vec<String>,
    pub custom: Value,
}

impl<T> ToolResult<T> {
    /// Create a successful tool result
    pub fn success(output: T) -> Self {
        Self {
            success: true,
            output: Some(output),
            error: None,
            metadata: ToolResultMetadata::default(),
        }
    }

    /// Create a failed tool result
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: None,
            error: Some(error.into()),
            metadata: ToolResultMetadata::default(),
        }
    }

    /// Add execution time to the result
    pub fn with_execution_time(mut self, duration: std::time::Duration) -> Self {
        self.metadata.execution_time = Some(duration);
        self
    }

    /// Add a warning to the result
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.metadata.warnings.push(warning.into());
        self
    }
}

/// Helper macro to implement a simple tool
#[macro_export]
macro_rules! impl_tool {
    (
        name: $name:expr,
        description: $desc:expr,
        input: $input:ty,
        output: $output:ty,
        execute: $execute:expr
    ) => {
        #[derive(Debug)]
        struct Tool;

        #[async_trait::async_trait]
        impl $crate::tool::AiTool for Tool {
            type Input = $input;
            type Output = $output;

            fn name(&self) -> &str {
                $name
            }

            fn description(&self) -> &str {
                $desc
            }

            async fn execute(
                &self,
                params: Self::Input,
                _meta: &$crate::tool::ExecutionMeta,
            ) -> $crate::Result<Self::Output> {
                $execute(params).await
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    struct TestInput {
        message: String,
        #[serde(default)]
        count: Option<u32>,
    }

    #[derive(Debug, Serialize, JsonSchema)]
    struct TestOutput {
        response: String,
        processed_count: u32,
    }

    #[derive(Debug, Clone)]
    struct TestTool;

    #[async_trait]
    impl AiTool for TestTool {
        type Input = TestInput;
        type Output = TestOutput;

        fn name(&self) -> &str {
            "test_tool"
        }

        fn description(&self) -> &str {
            "A tool for testing"
        }

        async fn execute(
            &self,
            params: Self::Input,
            _meta: &ExecutionMeta,
        ) -> Result<Self::Output> {
            Ok(TestOutput {
                response: format!("Received: {}", params.message),
                processed_count: params.count.unwrap_or(1),
            })
        }

        fn examples(&self) -> Vec<ToolExample<Self::Input, Self::Output>> {
            vec![ToolExample {
                description: "Basic example".to_string(),
                parameters: TestInput {
                    message: "Hello".to_string(),
                    count: Some(5),
                },
                expected_output: Some(TestOutput {
                    response: "Received: Hello".to_string(),
                    processed_count: 5,
                }),
            }]
        }
    }

    #[tokio::test]
    async fn test_typed_tool() {
        let tool = TestTool;

        let result = tool
            .execute(
                TestInput {
                    message: "Hello, world!".to_string(),
                    count: Some(3),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert_eq!(result.response, "Received: Hello, world!");
        assert_eq!(result.processed_count, 3);
    }

    #[tokio::test]
    async fn test_tool_registry() {
        let registry = ToolRegistry::new();
        registry.register(TestTool);

        assert_eq!(
            registry.list_tools(),
            Arc::from(vec!["test_tool".to_compact_string()])
        );

        let result = registry
            .execute(
                "test_tool",
                serde_json::json!({
                    "message": "Hello, world!",
                    "count": 42
                }),
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert_eq!(result["response"], "Received: Hello, world!");
        assert_eq!(result["processed_count"], 42);
    }

    #[test]
    fn test_schema_generation() {
        let tool = TestTool;
        let schema = tool.parameters_schema();

        // Check that the schema has no $ref
        let schema_str = serde_json::to_string(&schema).unwrap();
        assert!(!schema_str.contains("\"$ref\""));

        // Check basic structure
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["message"].is_object());
        assert!(schema["properties"]["count"].is_object());
    }

    #[test]
    fn test_tool_result() {
        let result = ToolResult::success("test output")
            .with_execution_time(std::time::Duration::from_millis(100))
            .with_warning("This is a test warning");

        assert!(result.success);
        assert_eq!(result.output, Some("test output"));
        assert_eq!(result.metadata.warnings.len(), 1);
    }

    #[test]
    fn test_ai_tool_operations_default() {
        #[derive(Debug, Clone)]
        struct TestToolOps;

        #[async_trait]
        impl AiTool for TestToolOps {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "test"
            }
            fn description(&self) -> &str {
                "test tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> Result<Self::Output> {
                Ok("done".to_string())
            }
        }

        let tool = TestToolOps;
        // Default should return empty slice
        assert!(tool.operations().is_empty());
    }

    #[test]
    fn test_ai_tool_operations_custom() {
        use std::collections::BTreeSet;

        #[derive(Debug, Clone)]
        struct MultiOpTool;

        #[async_trait]
        impl AiTool for MultiOpTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "multi"
            }
            fn description(&self) -> &str {
                "multi-op tool"
            }

            fn operations(&self) -> &'static [&'static str] {
                &["read", "write", "delete"]
            }

            fn parameters_schema_filtered(
                &self,
                allowed_ops: &BTreeSet<String>,
            ) -> serde_json::Value {
                serde_json::json!({
                    "allowed": allowed_ops.iter().cloned().collect::<Vec<_>>()
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

        let tool = MultiOpTool;
        assert_eq!(tool.operations(), &["read", "write", "delete"]);

        let allowed: BTreeSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
        let filtered = tool.parameters_schema_filtered(&allowed);
        assert!(
            filtered["allowed"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("read"))
        );
    }

    #[test]
    fn test_dynamic_tool_operations() {
        use std::collections::BTreeSet;

        #[derive(Debug, Clone)]
        struct OpTool;

        #[async_trait]
        impl AiTool for OpTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "optool"
            }
            fn description(&self) -> &str {
                "op tool"
            }

            fn operations(&self) -> &'static [&'static str] {
                &["op1", "op2"]
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> Result<Self::Output> {
                Ok("done".to_string())
            }
        }

        let tool = OpTool;
        let dynamic: Box<dyn DynamicTool> = Box::new(DynamicToolAdapter::new(tool));

        assert_eq!(dynamic.operations(), &["op1", "op2"]);

        let allowed: BTreeSet<String> = ["op1"].iter().map(|s| s.to_string()).collect();
        let genai_tool = dynamic.to_genai_tool_filtered(Some(&allowed));
        assert_eq!(genai_tool.name, "optool");
    }

    #[tokio::test]
    async fn test_registry_with_rules_filtering() {
        use crate::tool::rules::engine::{ToolRule, ToolRuleType};
        use std::collections::BTreeSet;

        #[derive(Debug, Clone)]
        struct FilterableTool;

        #[async_trait]
        impl AiTool for FilterableTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "filterable"
            }
            fn description(&self) -> &str {
                "filterable tool"
            }

            fn operations(&self) -> &'static [&'static str] {
                &["alpha", "beta", "gamma"]
            }

            fn parameters_schema_filtered(
                &self,
                allowed_ops: &BTreeSet<String>,
            ) -> serde_json::Value {
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": {
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
        registry.register(FilterableTool);

        let allowed: BTreeSet<String> = ["alpha", "beta"].iter().map(|s| s.to_string()).collect();
        let rules = vec![ToolRule {
            tool_name: "filterable".to_string(),
            rule_type: ToolRuleType::AllowedOperations(allowed),
            conditions: vec![],
            priority: 0,
            metadata: None,
        }];

        let genai_tools = registry.to_genai_tools_with_rules(&rules);
        assert_eq!(genai_tools.len(), 1);

        let tool = &genai_tools[0];
        assert_eq!(tool.name, "filterable");

        // Verify the schema was actually filtered
        let schema = tool.schema.as_ref().expect("schema should be present");
        let op_enum = schema["properties"]["op"]["enum"].as_array().unwrap();
        assert_eq!(op_enum.len(), 2);
        assert!(op_enum.contains(&serde_json::json!("alpha")));
        assert!(op_enum.contains(&serde_json::json!("beta")));
        assert!(!op_enum.contains(&serde_json::json!("gamma"))); // gamma should be filtered out
    }
}
