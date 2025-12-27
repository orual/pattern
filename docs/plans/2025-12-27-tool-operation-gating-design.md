# Tool Operation Gating Design

## Overview

This design extends the existing tool rules system to support **operation-level gating** for multi-operation tools. Rather than all-or-nothing tool access, agents can be restricted to specific operations within a tool (e.g., `file` tool with only `read` and `append`, not `patch`).

The key insight: **the schema shown to the LLM should only include operations the agent is allowed to use**.

## Goals

1. **Generic**: Works with any multi-operation tool without special-casing
2. **Type-safe**: Invalid operation names caught at config load or registration
3. **Non-breaking**: Existing tools work unchanged (default = all operations)
4. **Schema-aware**: Filtered operations don't appear in the LLM's tool schema

## Design

### 1. New ToolRuleType Variant

Add to `crates/pattern_core/src/tool/rules/engine.rs`:

```rust
pub enum ToolRuleType {
    // ... existing variants ...

    /// Only allow these operations for multi-operation tools.
    /// Operations not in this set are hidden from the schema and rejected at execution.
    AllowedOperations(HashSet<String>),
}

impl ToolRuleType {
    pub fn to_usage_description(&self, tool_name: &str, conditions: &[String]) -> String {
        match self {
            // ... existing matches ...

            ToolRuleType::AllowedOperations(ops) => {
                format!(
                    "`{}` is limited to these operations: {}",
                    tool_name,
                    ops.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            }
        }
    }
}
```

### 2. AiTool Trait Extensions

Add to `crates/pattern_core/src/tool/mod.rs`:

```rust
pub trait AiTool: Send + Sync + Debug {
    // ... existing methods ...

    /// Operations this tool supports. Empty slice means not operation-based.
    /// Should return static strings matching the operation enum variant names.
    fn operations(&self) -> &'static [&'static str] {
        &[]
    }

    /// Generate schema filtered to only allowed operations.
    /// Default implementation returns full schema (no filtering).
    fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> Value {
        let _ = allowed_ops; // unused in default impl
        self.parameters_schema()
    }
}
```

### 3. DynamicTool Trait Extensions

The type-erased `DynamicTool` trait (what's actually stored in the registry) needs corresponding methods. Add to trait definition:

```rust
#[async_trait]
pub trait DynamicTool: Send + Sync + Debug {
    // ... existing methods (clone_box, name, description, parameters_schema,
    //     output_schema, execute, validate_params, examples, usage_rule,
    //     tool_rules, to_genai_tool) ...

    /// Operations this tool supports. Empty slice means not operation-based.
    fn operations(&self) -> &'static [&'static str] {
        &[]
    }

    /// Generate schema filtered to only allowed operations.
    fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> Value {
        let _ = allowed_ops;
        self.parameters_schema()
    }

    /// Convert to genai Tool with operation filtering applied
    fn to_genai_tool_filtered(&self, allowed_ops: Option<&HashSet<String>>) -> genai::chat::Tool {
        let schema = match allowed_ops {
            Some(ops) => self.parameters_schema_filtered(ops),
            None => self.parameters_schema(),
        };
        genai::chat::Tool::new(self.name())
            .with_description(self.description())
            .with_schema(schema)
    }
}
```

Update `DynamicToolAdapter<T>` implementation to delegate to inner tool:

```rust
#[async_trait]
impl<T> DynamicTool for DynamicToolAdapter<T>
where
    T: AiTool + Clone + 'static,
{
    // ... existing method impls ...

    fn operations(&self) -> &'static [&'static str] {
        self.inner.operations()
    }

    fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> Value {
        self.inner.parameters_schema_filtered(allowed_ops)
    }
}
```

### 4. ToolRegistry Changes

Add method to generate tools with rule-based filtering:

```rust
impl ToolRegistry {
    /// Get all tools as genai tools, applying operation gating from rules
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

    /// Find AllowedOperations rule for a tool
    fn find_allowed_operations(&self, tool_name: &str, rules: &[ToolRule]) -> Option<HashSet<String>> {
        rules.iter()
            .find(|r| r.tool_name == tool_name || r.tool_name == "*")
            .and_then(|r| match &r.rule_type {
                ToolRuleType::AllowedOperations(ops) => Some(ops.clone()),
                _ => None,
            })
    }

    /// Validate that configured operations exist on the tool
    fn validate_operations(
        &self,
        tool_name: &str,
        declared: &'static [&'static str],
        configured: &HashSet<String>,
    ) {
        if declared.is_empty() {
            // Tool doesn't declare operations - can't validate
            tracing::warn!(
                tool = tool_name,
                "AllowedOperations rule applied to tool that doesn't declare operations"
            );
            return;
        }

        let declared_set: HashSet<&str> = declared.iter().copied().collect();
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
}
```

### 5. ContextBuilder Integration

Update `crates/pattern_core/src/context/builder.rs`:

```rust
impl ContextBuilder<'_> {
    pub async fn build(self) -> Result<Request, ContextError> {
        // ... existing code ...

        // Get tools in genai format with operation gating applied
        let tools = self.tools.map(|registry| {
            registry.to_genai_tools_with_rules(&self.tool_rules)
        });

        // ... rest of build ...
    }
}
```

### 6. Schema Filtering Helper

Utility for filtering enum fields in JSON schemas:

```rust
// In crates/pattern_core/src/tool/mod.rs or a new utils module

/// Filter an enum field in a JSON schema to only include allowed values
pub fn filter_schema_enum(
    schema: &mut Value,
    field_name: &str,
    allowed_values: &HashSet<String>,
) {
    // Navigate to the field's schema
    if let Some(properties) = schema.get_mut("properties") {
        if let Some(field) = properties.get_mut(field_name) {
            // Handle direct enum
            if let Some(enum_values) = field.get_mut("enum") {
                if let Some(arr) = enum_values.as_array_mut() {
                    arr.retain(|v| {
                        v.as_str().map(|s| allowed_values.contains(s)).unwrap_or(false)
                    });
                }
            }

            // Handle oneOf pattern (for tagged enums with descriptions)
            if let Some(one_of) = field.get_mut("oneOf") {
                if let Some(arr) = one_of.as_array_mut() {
                    arr.retain(|variant| {
                        variant.get("const")
                            .and_then(|v| v.as_str())
                            .map(|s| allowed_values.contains(s))
                            .unwrap_or(false)
                    });
                }
            }
        }
    }
}
```

## Example: File Tool Implementation

```rust
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
}

impl AiTool for FileTool {
    type Input = FileInput;
    type Output = FileOutput;

    fn name(&self) -> &str { "file" }

    fn description(&self) -> &str {
        "Read, write, and manipulate files"
    }

    fn operations(&self) -> &'static [&'static str] {
        &["read", "append", "insert", "patch", "save"]
    }

    fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> Value {
        let mut schema = self.parameters_schema();
        filter_schema_enum(&mut schema, "operation", allowed_ops);
        schema
    }

    async fn execute(&self, params: FileInput, meta: &ExecutionMeta) -> Result<FileOutput> {
        // Execution still validates at runtime as defense in depth
        match params.operation {
            FileOperation::Read => self.do_read(&params.path).await,
            FileOperation::Append => self.do_append(&params.path, params.content.as_deref()).await,
            // ... etc
        }
    }
}
```

## Example: Agent Configuration

```rust
// In agent config or programmatic setup
let rules = vec![
    ToolRule {
        tool_name: "file".to_string(),
        rule_type: ToolRuleType::AllowedOperations(
            ["read", "append"].into_iter().map(String::from).collect()
        ),
        conditions: vec![],
        priority: 0,
        metadata: None,
    },
];

let builder = ContextBuilder::new(&memory, &config)
    .for_agent("limited-agent")
    .with_tool_rules(rules);
```

Or in TOML config:

```toml
[[tool_rules]]
tool_name = "file"
rule_type = { AllowedOperations = ["read", "append"] }
priority = 0
```

## Runtime Execution Gating (Defense in Depth)

While schema filtering prevents the LLM from seeing disallowed operations, we should also validate at execution time:

```rust
impl ToolRuleEngine {
    /// Check if operation is allowed before execution
    pub fn check_operation_allowed(
        &self,
        tool_name: &str,
        operation: &str,
    ) -> Result<(), ToolRuleViolation> {
        if let Some(rule) = self.find_rule(tool_name, |r| {
            matches!(r.rule_type, ToolRuleType::AllowedOperations(_))
        }) {
            if let ToolRuleType::AllowedOperations(ref allowed) = rule.rule_type {
                if !allowed.contains(operation) {
                    return Err(ToolRuleViolation::OperationNotAllowed {
                        tool: tool_name.to_string(),
                        operation: operation.to_string(),
                        allowed: allowed.iter().cloned().collect(),
                    });
                }
            }
        }
        Ok(())
    }
}

// New violation type
pub enum ToolRuleViolation {
    // ... existing variants ...

    OperationNotAllowed {
        tool: String,
        operation: String,
        allowed: Vec<String>,
    },
}
```

## Integration with Data Sources

This mechanism directly supports the data source v2 design where:

- `file` tool operations are gated by agent capability config
- `block_edit` tool operations (append, replace, patch, set_field) are gated per agent
- DataBlock and DataStream sources declare `required_tools()` with specific operations

Example from data source:

```rust
impl DataBlock for FileSource {
    fn required_tools(&self) -> Vec<ToolRule> {
        vec![
            ToolRule {
                tool_name: "file".to_string(),
                rule_type: ToolRuleType::AllowedOperations(
                    ["read", "append", "insert", "save"].into_iter().map(String::from).collect()
                    // Note: "patch" not included - requires advanced capability
                ),
                conditions: vec![],
                priority: 0,
                metadata: None,
            },
        ]
    }
}
```

## Open Questions

1. **Wildcard rules**: Should `tool_name = "*"` apply AllowedOperations to all tools? Probably not meaningful since operations are tool-specific.

2. **Rule merging**: If multiple rules apply (e.g., from agent config and data source), should we union or intersect the allowed operations? Intersect is safer (most restrictive).

3. **Dynamic operations**: Some tools might have operations that depend on runtime state. For now, operations are static (`&'static [&'static str]`). Could relax if needed.
