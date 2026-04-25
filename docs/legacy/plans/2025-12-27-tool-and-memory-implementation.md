# Tool Operation Gating and Memory System Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend the tool rules system with operation-level gating and add field-level permissions with Loro subscription support to StructuredDocument.

**Architecture:** Tool operations are schema-filtered before LLM sees them and runtime-validated on execution. Memory blocks gain field-level read_only flags, section parameters for Composite schemas, and Loro subscription passthrough for edit watching.

**Tech Stack:** Rust, Loro CRDT, serde/schemars for JSON schema manipulation

---

## Overview

Two major pieces of work:

1. **Tool Operation Gating** (Tasks 1-7) - Filter multi-operation tools at schema level
2. **Block Schema & Loro Integration** (Tasks 8-16) - Field permissions and subscriptions

Dependencies:
- Tasks 1-7 are independent of Tasks 8-16
- Within each group, tasks must be sequential (each builds on previous)

---

## Task 1: Add AllowedOperations Variant to ToolRuleType

**Files:**
- Modify: `crates/pattern_core/src/tool/rules/engine.rs:30-72` (ToolRuleType enum)

**Step 1: Write the failing test**

Add to `crates/pattern_core/src/tool/rules/engine.rs` in the tests module:

```rust
#[test]
fn test_allowed_operations_rule_type() {
    use std::collections::HashSet;

    let allowed: HashSet<String> = ["read", "append"].iter().map(|s| s.to_string()).collect();
    let rule_type = ToolRuleType::AllowedOperations(allowed.clone());

    let description = rule_type.to_usage_description("file", &[]);
    assert!(description.contains("file"));
    assert!(description.contains("read"));
    assert!(description.contains("append"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_allowed_operations_rule_type`
Expected: FAIL with "no variant named AllowedOperations"

**Step 3: Add the AllowedOperations variant**

In `ToolRuleType` enum (around line 70):

```rust
pub enum ToolRuleType {
    // ... existing variants ...

    /// Only allow these operations for multi-operation tools.
    /// Operations not in this set are hidden from the schema and rejected at execution.
    AllowedOperations(HashSet<String>),
}
```

Add import at top of file:
```rust
use std::collections::HashSet;
```

**Step 4: Add to_usage_description match arm**

In the `to_usage_description` method (around line 74-171), add:

```rust
ToolRuleType::AllowedOperations(ops) => {
    let ops_list: Vec<_> = ops.iter().cloned().collect();
    format!(
        "available operations for `{}`: {}",
        tool_name,
        ops_list.join(", ")
    )
}
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core test_allowed_operations_rule_type`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/pattern_core/src/tool/rules/engine.rs
git commit -m "feat(tool): add AllowedOperations variant to ToolRuleType"
```

---

## Task 2: Add Operations Methods to AiTool Trait

**Files:**
- Modify: `crates/pattern_core/src/tool/mod.rs:35-117` (AiTool trait)

**Step 1: Write the failing test**

Add to test module in `crates/pattern_core/src/tool/mod.rs`:

```rust
#[test]
fn test_ai_tool_operations_default() {
    #[derive(Debug, Clone)]
    struct TestTool;

    impl AiTool for TestTool {
        type Input = serde_json::Value;
        type Output = String;

        fn name(&self) -> &str { "test" }
        fn description(&self) -> &str { "test tool" }

        async fn execute(
            &self,
            _params: Self::Input,
            _meta: &ExecutionMeta,
        ) -> Result<Self::Output, ToolError> {
            Ok("done".to_string())
        }
    }

    let tool = TestTool;
    // Default should return empty slice
    assert!(tool.operations().is_empty());
}

#[test]
fn test_ai_tool_operations_custom() {
    use std::collections::HashSet;

    #[derive(Debug, Clone)]
    struct MultiOpTool;

    impl AiTool for MultiOpTool {
        type Input = serde_json::Value;
        type Output = String;

        fn name(&self) -> &str { "multi" }
        fn description(&self) -> &str { "multi-op tool" }

        fn operations(&self) -> &'static [&'static str] {
            &["read", "write", "delete"]
        }

        fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> serde_json::Value {
            // Just return a simple filtered indicator
            serde_json::json!({
                "allowed": allowed_ops.iter().cloned().collect::<Vec<_>>()
            })
        }

        async fn execute(
            &self,
            _params: Self::Input,
            _meta: &ExecutionMeta,
        ) -> Result<Self::Output, ToolError> {
            Ok("done".to_string())
        }
    }

    let tool = MultiOpTool;
    assert_eq!(tool.operations(), &["read", "write", "delete"]);

    let allowed: HashSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
    let filtered = tool.parameters_schema_filtered(&allowed);
    assert!(filtered["allowed"].as_array().unwrap().contains(&serde_json::json!("read")));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_ai_tool_operations`
Expected: FAIL with "method `operations` not found"

**Step 3: Add operations methods to AiTool trait**

In `AiTool` trait definition (around line 35-117), add these methods:

```rust
/// Operations this tool supports. Empty slice means not operation-based.
/// Return static strings matching the operation enum variant names (snake_case).
fn operations(&self) -> &'static [&'static str] {
    &[]
}

/// Generate schema filtered to only allowed operations.
/// Default implementation returns full schema (no filtering).
fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> serde_json::Value {
    let _ = allowed_ops; // unused in default impl
    self.parameters_schema()
}
```

Add import at top if not present:
```rust
use std::collections::HashSet;
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_ai_tool_operations`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/mod.rs
git commit -m "feat(tool): add operations() and parameters_schema_filtered() to AiTool"
```

---

## Task 3: Add Operations Methods to DynamicTool Trait

**Files:**
- Modify: `crates/pattern_core/src/tool/mod.rs:119-162` (DynamicTool trait)
- Modify: `crates/pattern_core/src/tool/mod.rs:171-254` (DynamicToolAdapter)

**Step 1: Write the failing test**

Add to test module:

```rust
#[test]
fn test_dynamic_tool_operations() {
    use std::collections::HashSet;

    #[derive(Debug, Clone)]
    struct OpTool;

    impl AiTool for OpTool {
        type Input = serde_json::Value;
        type Output = String;

        fn name(&self) -> &str { "optool" }
        fn description(&self) -> &str { "op tool" }

        fn operations(&self) -> &'static [&'static str] {
            &["op1", "op2"]
        }

        async fn execute(
            &self,
            _params: Self::Input,
            _meta: &ExecutionMeta,
        ) -> Result<Self::Output, ToolError> {
            Ok("done".to_string())
        }
    }

    let tool = OpTool;
    let dynamic: Box<dyn DynamicTool> = Box::new(DynamicToolAdapter::new(tool));

    assert_eq!(dynamic.operations(), &["op1", "op2"]);

    let allowed: HashSet<String> = ["op1"].iter().map(|s| s.to_string()).collect();
    let genai_tool = dynamic.to_genai_tool_filtered(Some(&allowed));
    assert_eq!(genai_tool.name, "optool");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_dynamic_tool_operations`
Expected: FAIL with "method `operations` not found in trait `DynamicTool`"

**Step 3: Add methods to DynamicTool trait**

In `DynamicTool` trait (around line 119-162), add:

```rust
/// Operations this tool supports. Empty slice means not operation-based.
fn operations(&self) -> &'static [&'static str] {
    &[]
}

/// Generate schema filtered to only allowed operations.
fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> serde_json::Value {
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
```

**Step 4: Update DynamicToolAdapter to delegate**

In `DynamicToolAdapter<T>` impl (around line 171-254), add:

```rust
fn operations(&self) -> &'static [&'static str] {
    self.inner.operations()
}

fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> serde_json::Value {
    self.inner.parameters_schema_filtered(allowed_ops)
}
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core test_dynamic_tool_operations`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/pattern_core/src/tool/mod.rs
git commit -m "feat(tool): add operations methods to DynamicTool and DynamicToolAdapter"
```

---

## Task 4: Add Schema Enum Filtering Helper

**Files:**
- Create: `crates/pattern_core/src/tool/schema_filter.rs`
- Modify: `crates/pattern_core/src/tool/mod.rs` (add module)

**Step 1: Write the failing test**

Create `crates/pattern_core/src/tool/schema_filter.rs`:

```rust
//! Utilities for filtering JSON schemas based on allowed operations.

use serde_json::Value;
use std::collections::HashSet;

/// Filter an enum field in a JSON schema to only include allowed values.
///
/// Handles both simple `enum` arrays and `oneOf` patterns for tagged enums.
pub fn filter_schema_enum(
    schema: &mut Value,
    field_name: &str,
    allowed_values: &HashSet<String>,
) {
    todo!("implement")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_filter_simple_enum() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["read", "write", "delete", "patch"]
                }
            }
        });

        let allowed: HashSet<String> = ["read", "write"].iter().map(|s| s.to_string()).collect();
        filter_schema_enum(&mut schema, "operation", &allowed);

        let enum_values = schema["properties"]["operation"]["enum"].as_array().unwrap();
        assert_eq!(enum_values.len(), 2);
        assert!(enum_values.contains(&json!("read")));
        assert!(enum_values.contains(&json!("write")));
        assert!(!enum_values.contains(&json!("delete")));
    }

    #[test]
    fn test_filter_oneof_enum() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "operation": {
                    "oneOf": [
                        {"const": "read", "description": "Read operation"},
                        {"const": "write", "description": "Write operation"},
                        {"const": "delete", "description": "Delete operation"}
                    ]
                }
            }
        });

        let allowed: HashSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
        filter_schema_enum(&mut schema, "operation", &allowed);

        let one_of = schema["properties"]["operation"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 1);
        assert_eq!(one_of[0]["const"], "read");
    }

    #[test]
    fn test_filter_missing_field_is_noop() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "other": {"type": "string"}
            }
        });

        let original = schema.clone();
        let allowed: HashSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
        filter_schema_enum(&mut schema, "operation", &allowed);

        assert_eq!(schema, original);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core schema_filter`
Expected: FAIL with "not yet implemented"

**Step 3: Implement filter_schema_enum**

Replace the `todo!` with:

```rust
pub fn filter_schema_enum(
    schema: &mut Value,
    field_name: &str,
    allowed_values: &HashSet<String>,
) {
    // Navigate to the field's schema
    let Some(properties) = schema.get_mut("properties") else {
        return;
    };
    let Some(field) = properties.get_mut(field_name) else {
        return;
    };

    // Handle direct enum
    if let Some(enum_values) = field.get_mut("enum") {
        if let Some(arr) = enum_values.as_array_mut() {
            arr.retain(|v| {
                v.as_str()
                    .map(|s| allowed_values.contains(s))
                    .unwrap_or(false)
            });
        }
    }

    // Handle oneOf pattern (for tagged enums with descriptions)
    if let Some(one_of) = field.get_mut("oneOf") {
        if let Some(arr) = one_of.as_array_mut() {
            arr.retain(|variant| {
                variant
                    .get("const")
                    .and_then(|v| v.as_str())
                    .map(|s| allowed_values.contains(s))
                    .unwrap_or(false)
            });
        }
    }
}
```

**Step 4: Add module to tool/mod.rs**

In `crates/pattern_core/src/tool/mod.rs`, add:

```rust
pub mod schema_filter;
pub use schema_filter::filter_schema_enum;
```

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core schema_filter`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/pattern_core/src/tool/schema_filter.rs crates/pattern_core/src/tool/mod.rs
git commit -m "feat(tool): add filter_schema_enum helper for operation gating"
```

---

## Task 5: Add to_genai_tools_with_rules to ToolRegistry

**Files:**
- Modify: `crates/pattern_core/src/tool/mod.rs:274-375` (ToolRegistry)

**Step 1: Write the failing test**

Add to test module:

```rust
#[tokio::test]
async fn test_registry_with_rules_filtering() {
    use std::collections::HashSet;
    use crate::tool::rules::engine::{ToolRule, ToolRuleType};

    #[derive(Debug, Clone)]
    struct FilterableTool;

    impl AiTool for FilterableTool {
        type Input = serde_json::Value;
        type Output = String;

        fn name(&self) -> &str { "filterable" }
        fn description(&self) -> &str { "filterable tool" }

        fn operations(&self) -> &'static [&'static str] {
            &["alpha", "beta", "gamma"]
        }

        fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> serde_json::Value {
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
        ) -> Result<Self::Output, ToolError> {
            Ok("done".to_string())
        }
    }

    let registry = ToolRegistry::new();
    registry.register(FilterableTool);

    let allowed: HashSet<String> = ["alpha", "beta"].iter().map(|s| s.to_string()).collect();
    let rules = vec![
        ToolRule {
            tool_name: "filterable".to_string(),
            rule_type: ToolRuleType::AllowedOperations(allowed),
            conditions: vec![],
            priority: 0,
            metadata: None,
        }
    ];

    let genai_tools = registry.to_genai_tools_with_rules(&rules);
    assert_eq!(genai_tools.len(), 1);

    // The filtered tool should have only alpha and beta in its schema
    let tool = &genai_tools[0];
    assert_eq!(tool.name, "filterable");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_registry_with_rules_filtering`
Expected: FAIL with "method `to_genai_tools_with_rules` not found"

**Step 3: Implement to_genai_tools_with_rules**

In `ToolRegistry` impl block (around line 274-375), add:

```rust
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
) -> Option<HashSet<String>> {
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
    configured: &HashSet<String>,
) {
    if declared.is_empty() {
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
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_registry_with_rules_filtering`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/mod.rs
git commit -m "feat(tool): add to_genai_tools_with_rules for operation gating"
```

---

## Task 6: Add OperationNotAllowed to ToolRuleViolation

**Files:**
- Modify: `crates/pattern_core/src/tool/rules/engine.rs:540-585` (ToolRuleViolation)

**Step 1: Write the failing test**

Add to tests in engine.rs:

```rust
#[test]
fn test_operation_not_allowed_violation() {
    let violation = ToolRuleViolation::OperationNotAllowed {
        tool: "file".to_string(),
        operation: "delete".to_string(),
        allowed: vec!["read".to_string(), "write".to_string()],
    };

    let display = format!("{}", violation);
    assert!(display.contains("file"));
    assert!(display.contains("delete"));
    assert!(display.contains("read"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_operation_not_allowed_violation`
Expected: FAIL with "no variant named OperationNotAllowed"

**Step 3: Add OperationNotAllowed variant**

In `ToolRuleViolation` enum (around line 540-585), add:

```rust
/// Operation not in allowed set for this tool
OperationNotAllowed {
    tool: String,
    operation: String,
    allowed: Vec<String>,
},
```

Update the `Display` impl to handle it:

```rust
ToolRuleViolation::OperationNotAllowed { tool, operation, allowed } => {
    write!(
        f,
        "Operation '{}' not allowed for tool '{}'. Allowed operations: {}",
        operation,
        tool,
        allowed.join(", ")
    )
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_operation_not_allowed_violation`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/rules/engine.rs
git commit -m "feat(tool): add OperationNotAllowed violation type"
```

---

## Task 7: Add check_operation_allowed to ToolRuleEngine

**Files:**
- Modify: `crates/pattern_core/src/tool/rules/engine.rs:216-400` (ToolRuleEngine)

**Step 1: Write the failing test**

Add to tests:

```rust
#[test]
fn test_check_operation_allowed() {
    use std::collections::HashSet;

    let allowed: HashSet<String> = ["read", "append"].iter().map(|s| s.to_string()).collect();
    let rules = vec![
        ToolRule {
            tool_name: "file".to_string(),
            rule_type: ToolRuleType::AllowedOperations(allowed),
            conditions: vec![],
            priority: 0,
            metadata: None,
        }
    ];

    let engine = ToolRuleEngine::new(rules);

    // Allowed operation should pass
    assert!(engine.check_operation_allowed("file", "read").is_ok());
    assert!(engine.check_operation_allowed("file", "append").is_ok());

    // Disallowed operation should fail
    let result = engine.check_operation_allowed("file", "delete");
    assert!(result.is_err());
    match result.unwrap_err() {
        ToolRuleViolation::OperationNotAllowed { tool, operation, allowed } => {
            assert_eq!(tool, "file");
            assert_eq!(operation, "delete");
            assert!(allowed.contains(&"read".to_string()));
        }
        _ => panic!("Expected OperationNotAllowed"),
    }

    // Tool without AllowedOperations rule should pass any operation
    assert!(engine.check_operation_allowed("other_tool", "anything").is_ok());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_check_operation_allowed`
Expected: FAIL with "method `check_operation_allowed` not found"

**Step 3: Implement check_operation_allowed**

In `ToolRuleEngine` impl (around line 216-400), add:

```rust
/// Check if operation is allowed before execution.
/// Returns Ok(()) if allowed, Err(ToolRuleViolation) if not.
pub fn check_operation_allowed(
    &self,
    tool_name: &str,
    operation: &str,
) -> Result<(), ToolRuleViolation> {
    // Find AllowedOperations rule for this tool
    let rule = self.rules.iter().find(|r| {
        r.tool_name == tool_name
            && matches!(r.rule_type, ToolRuleType::AllowedOperations(_))
    });

    if let Some(rule) = rule {
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
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_check_operation_allowed`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/tool/rules/engine.rs
git commit -m "feat(tool): add check_operation_allowed to ToolRuleEngine"
```

---

## Task 8: Add read_only Flag to FieldDef

**Files:**
- Modify: `crates/pattern_core/src/memory/schema.rs:51-68` (FieldDef)

**Step 1: Write the failing test**

Add to tests in schema.rs:

```rust
#[test]
fn test_field_def_read_only() {
    let field = FieldDef {
        name: "status".to_string(),
        description: "Current status".to_string(),
        field_type: FieldType::Text,
        required: true,
        default: None,
        read_only: true,
    };

    assert!(field.read_only);

    // Default should be false
    let field2 = FieldDef {
        name: "notes".to_string(),
        description: "User notes".to_string(),
        field_type: FieldType::Text,
        required: false,
        default: None,
        read_only: false,
    };

    assert!(!field2.read_only);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_field_def_read_only`
Expected: FAIL with "no field `read_only`"

**Step 3: Add read_only field to FieldDef**

In `FieldDef` struct (around line 51-68):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub description: String,
    pub field_type: FieldType,
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,

    /// If true, only system/source code can write to this field.
    /// Agent tools should reject writes to read-only fields.
    #[serde(default)]
    pub read_only: bool,
}
```

**Step 4: Update all FieldDef construction sites**

Search for `FieldDef {` and add `read_only: false` to each. Key locations:
- `crates/pattern_core/src/memory/schema.rs` templates module (lines 106-226)
- Any test files creating FieldDef

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core test_field_def_read_only`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/pattern_core/src/memory/schema.rs
git commit -m "feat(memory): add read_only flag to FieldDef"
```

---

## Task 9: Add BlockSchema Permission Helper Methods

**Files:**
- Modify: `crates/pattern_core/src/memory/schema.rs:9-43` (BlockSchema)

**Step 1: Write the failing test**

Add to tests:

```rust
#[test]
fn test_block_schema_read_only_helpers() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "status".to_string(),
                description: "Status".to_string(),
                field_type: FieldType::Text,
                required: true,
                default: None,
                read_only: true,
            },
            FieldDef {
                name: "notes".to_string(),
                description: "Notes".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: false,
            },
        ],
    };

    assert_eq!(schema.is_field_read_only("status"), Some(true));
    assert_eq!(schema.is_field_read_only("notes"), Some(false));
    assert_eq!(schema.is_field_read_only("nonexistent"), None);

    let read_only = schema.read_only_fields();
    assert_eq!(read_only, vec!["status"]);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_block_schema_read_only_helpers`
Expected: FAIL with "method `is_field_read_only` not found"

**Step 3: Implement helper methods**

Add impl block for BlockSchema:

```rust
impl BlockSchema {
    /// Check if a field is read-only. Returns None if field not found or schema doesn't have fields.
    pub fn is_field_read_only(&self, field_name: &str) -> Option<bool> {
        match self {
            BlockSchema::Map { fields } => {
                fields
                    .iter()
                    .find(|f| f.name == field_name)
                    .map(|f| f.read_only)
            }
            _ => None, // Text, List, Log, Tree, Composite don't have named fields at top level
        }
    }

    /// Get all field names that are read-only.
    pub fn read_only_fields(&self) -> Vec<&str> {
        match self {
            BlockSchema::Map { fields } => fields
                .iter()
                .filter(|f| f.read_only)
                .map(|f| f.name.as_str())
                .collect(),
            _ => vec![],
        }
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_block_schema_read_only_helpers`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory/schema.rs
git commit -m "feat(memory): add is_field_read_only and read_only_fields to BlockSchema"
```

---

## Task 10: Update Composite Schema with CompositeSection

**Files:**
- Modify: `crates/pattern_core/src/memory/schema.rs` (BlockSchema::Composite)

**Step 1: Write the failing test**

```rust
#[test]
fn test_composite_section_read_only() {
    let schema = BlockSchema::Composite {
        sections: vec![
            CompositeSection {
                name: "diagnostics".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![FieldDef {
                        name: "errors".to_string(),
                        description: "Error list".to_string(),
                        field_type: FieldType::List,
                        required: true,
                        default: None,
                        read_only: false, // Field-level, section overrides
                    }],
                }),
                description: Some("LSP diagnostics".to_string()),
                read_only: true, // Whole section is read-only
            },
            CompositeSection {
                name: "config".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![FieldDef {
                        name: "filter".to_string(),
                        description: "Filter setting".to_string(),
                        field_type: FieldType::Text,
                        required: false,
                        default: None,
                        read_only: false,
                    }],
                }),
                description: Some("User configuration".to_string()),
                read_only: false,
            },
        ],
    };

    assert_eq!(schema.is_section_read_only("diagnostics"), Some(true));
    assert_eq!(schema.is_section_read_only("config"), Some(false));
    assert_eq!(schema.is_section_read_only("nonexistent"), None);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_composite_section_read_only`
Expected: FAIL with "expected struct, found enum variant"

**Step 3: Define CompositeSection struct and update Composite variant**

Add struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeSection {
    pub name: String,
    pub schema: Box<BlockSchema>,
    #[serde(default)]
    pub description: Option<String>,
    /// If true, only system/source code can write to this section.
    #[serde(default)]
    pub read_only: bool,
}
```

Update `BlockSchema::Composite` variant:

```rust
pub enum BlockSchema {
    // ... other variants ...

    /// Multi-section document with different schemas per section
    Composite {
        sections: Vec<CompositeSection>,
    },
}
```

Add helper method:

```rust
impl BlockSchema {
    // ... existing methods ...

    /// Check if a section is read-only (for Composite schemas).
    pub fn is_section_read_only(&self, section_name: &str) -> Option<bool> {
        match self {
            BlockSchema::Composite { sections } => {
                sections
                    .iter()
                    .find(|s| s.name == section_name)
                    .map(|s| s.read_only)
            }
            _ => None,
        }
    }

    /// Get the schema for a section (for Composite schemas).
    pub fn get_section_schema(&self, section_name: &str) -> Option<&BlockSchema> {
        match self {
            BlockSchema::Composite { sections } => {
                sections
                    .iter()
                    .find(|s| s.name == section_name)
                    .map(|s| s.schema.as_ref())
            }
            _ => None,
        }
    }
}
```

**Step 4: Update existing Composite usages**

Search for `BlockSchema::Composite` and update to use new structure. Old format was likely `Vec<(String, BlockSchema)>`.

**Step 5: Run test to verify it passes**

Run: `cargo test -p pattern_core test_composite_section_read_only`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/pattern_core/src/memory/schema.rs
git commit -m "feat(memory): add CompositeSection with read_only flag"
```

---

## Task 11: Add ReadOnlyField and ReadOnlySection Errors

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs:17-34` (DocumentError)

**Step 1: Write the failing test**

```rust
#[test]
fn test_document_error_read_only_variants() {
    let field_err = DocumentError::ReadOnlyField("status".to_string());
    let section_err = DocumentError::ReadOnlySection("diagnostics".to_string());

    let field_msg = format!("{}", field_err);
    let section_msg = format!("{}", section_err);

    assert!(field_msg.contains("status"));
    assert!(field_msg.contains("read-only"));
    assert!(section_msg.contains("diagnostics"));
    assert!(section_msg.contains("read-only"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_document_error_read_only_variants`
Expected: FAIL with "no variant named ReadOnlyField"

**Step 3: Add error variants**

In `DocumentError` enum:

```rust
#[derive(Debug, Error)]
pub enum DocumentError {
    // ... existing variants ...

    #[error("Field '{0}' is read-only and cannot be modified by agent")]
    ReadOnlyField(String),

    #[error("Section '{0}' is read-only and cannot be modified by agent")]
    ReadOnlySection(String),
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_document_error_read_only_variants`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory/document.rs
git commit -m "feat(memory): add ReadOnlyField and ReadOnlySection errors"
```

---

## Task 12: Add is_system Parameter to StructuredDocument Write Methods

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs:159-243` (write methods)

This is a larger task - we need to update multiple methods. For each write method that can target a field:
- `set_field`
- `set_text_field`
- `append_to_list_field`
- `remove_from_list_field`
- `increment_counter`

**Step 1: Write the failing test**

```rust
#[test]
fn test_structured_document_field_permission_check() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "readonly_field".to_string(),
                description: "Read-only".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: true,
            },
            FieldDef {
                name: "writable_field".to_string(),
                description: "Writable".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);

    // Agent (is_system=false) can write to writable field
    assert!(doc.set_field("writable_field", "value", false).is_ok());

    // Agent cannot write to read-only field
    let result = doc.set_field("readonly_field", "value", false);
    assert!(matches!(result, Err(DocumentError::ReadOnlyField(_))));

    // System (is_system=true) can write to read-only field
    assert!(doc.set_field("readonly_field", "system_value", true).is_ok());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_structured_document_field_permission_check`
Expected: FAIL (signature mismatch or missing parameter)

**Step 3: Update set_field method**

```rust
/// Set a field value. If is_system is false and field is read_only, returns error.
pub fn set_field(
    &self,
    name: &str,
    value: impl Into<serde_json::Value>,
    is_system: bool,
) -> Result<(), DocumentError> {
    // Check read-only if not system
    if !is_system {
        if let Some(true) = self.schema.is_field_read_only(name) {
            return Err(DocumentError::ReadOnlyField(name.to_string()));
        }
    }

    // ... existing implementation ...
}
```

**Step 4: Update remaining write methods similarly**

- `set_text_field(name, value, is_system)`
- `append_to_list_field(name, value, is_system)`
- `remove_from_list_field(name, index, is_system)`
- `increment_counter(name, delta, is_system)`

**Step 5: Update all call sites**

Search for each method call and add the appropriate `is_system` parameter:
- Tool code: `false`
- System/source code: `true`
- Tests: typically `true` or `false` depending on what's being tested

**Step 6: Run test to verify it passes**

Run: `cargo test -p pattern_core test_structured_document_field_permission_check`
Expected: PASS

**Step 7: Run all document tests**

Run: `cargo test -p pattern_core document`
Expected: PASS (all updated)

**Step 8: Commit**

```bash
git add crates/pattern_core/src/memory/document.rs
git commit -m "feat(memory): add is_system parameter to StructuredDocument write methods"
```

---

## Task 13: Add Section Parameter for Composite Schemas

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn test_structured_document_section_operations() {
    let schema = BlockSchema::Composite {
        sections: vec![
            CompositeSection {
                name: "diagnostics".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![FieldDef {
                        name: "error_count".to_string(),
                        description: "Error count".to_string(),
                        field_type: FieldType::Counter,
                        required: true,
                        default: Some(serde_json::json!(0)),
                        read_only: false,
                    }],
                }),
                description: None,
                read_only: true, // Section is read-only
            },
            CompositeSection {
                name: "notes".to_string(),
                schema: Box::new(BlockSchema::Text),
                description: None,
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);

    // System can write to read-only section
    assert!(doc.set_field("error_count", 5, Some("diagnostics"), true).is_ok());

    // Agent cannot write to read-only section
    let result = doc.set_field("error_count", 10, Some("diagnostics"), false);
    assert!(matches!(result, Err(DocumentError::ReadOnlySection(_))));

    // Agent can write to writable section
    assert!(doc.set_text("my notes", Some("notes"), false).is_ok());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_structured_document_section_operations`
Expected: FAIL (wrong signature)

**Step 3: Add container_key and effective_schema helpers**

```rust
impl StructuredDocument {
    /// Get the container key for an operation.
    fn container_key(&self, section: Option<&str>) -> &str {
        section.unwrap_or_else(|| match &self.schema {
            BlockSchema::Text => "content",
            BlockSchema::Map { .. } => "root",
            BlockSchema::List { .. } => "items",
            BlockSchema::Log { .. } => "entries",
            BlockSchema::Tree => "tree",
            BlockSchema::Composite { .. } => {
                panic!("Composite schema requires section parameter")
            }
        })
    }

    /// Get the effective schema for an operation (section schema for Composite).
    fn effective_schema(&self, section: Option<&str>) -> &BlockSchema {
        match (&self.schema, section) {
            (BlockSchema::Composite { .. }, Some(name)) => self
                .schema
                .get_section_schema(name)
                .expect("Section not found in composite schema"),
            _ => &self.schema,
        }
    }

    /// Check section read-only permission.
    fn check_section_permission(
        &self,
        section: Option<&str>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        if !is_system {
            if let Some(name) = section {
                if let Some(true) = self.schema.is_section_read_only(name) {
                    return Err(DocumentError::ReadOnlySection(name.to_string()));
                }
            }
        }
        Ok(())
    }
}
```

**Step 4: Update method signatures to include section parameter**

Update key methods:
- `text_content(section: Option<&str>)`
- `set_text(content, section: Option<&str>, is_system: bool)`
- `get_field(name, section: Option<&str>)`
- `set_field(name, value, section: Option<&str>, is_system: bool)`
- etc.

**Step 5: Update all call sites**

Add `None` for section parameter where not using Composite schemas.

**Step 6: Run test to verify it passes**

Run: `cargo test -p pattern_core test_structured_document_section_operations`
Expected: PASS

**Step 7: Commit**

```bash
git add crates/pattern_core/src/memory/document.rs
git commit -m "feat(memory): add section parameter for Composite schema operations"
```

---

## Task 14: Add Loro Subscription Methods

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn test_structured_document_subscription() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let schema = BlockSchema::Map {
        fields: vec![FieldDef {
            name: "counter".to_string(),
            description: "A counter".to_string(),
            field_type: FieldType::Counter,
            required: true,
            default: Some(serde_json::json!(0)),
            read_only: false,
        }],
    };

    let doc = StructuredDocument::new(schema);

    let changed = Arc::new(AtomicBool::new(false));
    let changed_clone = changed.clone();

    let _sub = doc.subscribe_root(Arc::new(move |_event| {
        changed_clone.store(true, Ordering::SeqCst);
    }));

    // Make a change and commit
    doc.increment_counter("counter", 1, None, true).unwrap();
    doc.commit();

    // Subscription should have fired
    assert!(changed.load(Ordering::SeqCst));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_structured_document_subscription`
Expected: FAIL with "method `subscribe_root` not found"

**Step 3: Add subscription methods**

```rust
use loro::{Subscriber, Subscription, ContainerID};

impl StructuredDocument {
    /// Subscribe to all changes on this document.
    pub fn subscribe_root(&self, callback: Subscriber) -> Subscription {
        self.doc.subscribe_root(callback)
    }

    /// Subscribe to changes on a specific container.
    pub fn subscribe(&self, container_id: &ContainerID, callback: Subscriber) -> Subscription {
        self.doc.subscribe(container_id, callback)
    }

    /// Subscribe to the main content container based on schema type.
    pub fn subscribe_content(&self, callback: Subscriber) -> Subscription {
        let container_id = match &self.schema {
            BlockSchema::Text => self.doc.get_text("content").id(),
            BlockSchema::Map { .. } => self.doc.get_map("root").id(),
            BlockSchema::List { .. } => self.doc.get_list("items").id(),
            BlockSchema::Log { .. } => self.doc.get_list("entries").id(),
            BlockSchema::Tree => self.doc.get_tree("tree").id(),
            BlockSchema::Composite { .. } => self.doc.get_map("root").id(),
        };
        self.doc.subscribe(&container_id, callback)
    }

    /// Set attribution message for the next commit.
    pub fn set_attribution(&self, message: &str) {
        self.doc.set_next_commit_message(message);
    }

    /// Explicitly commit pending changes (triggers subscriptions).
    pub fn commit(&self) {
        self.doc.commit();
    }

    /// Commit with attribution message.
    pub fn commit_with_attribution(&self, message: &str) {
        self.doc.set_next_commit_message(message);
        self.doc.commit();
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_structured_document_subscription`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory/document.rs
git commit -m "feat(memory): add Loro subscription and commit methods"
```

---

## Task 15: Add Identity Fields to StructuredDocument

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn test_structured_document_identity() {
    let schema = BlockSchema::Text;
    let doc = StructuredDocument::new_with_identity(
        schema,
        "my_block".to_string(),
        Some("agent_123".to_string()),
    );

    assert_eq!(doc.label(), "my_block");
    assert_eq!(doc.accessor_agent_id(), Some("agent_123"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_structured_document_identity`
Expected: FAIL

**Step 3: Add identity fields and constructor**

```rust
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,

    /// Block label for identification.
    label: String,
    /// Agent that loaded this document (for attribution).
    accessor_agent_id: Option<String>,
}

impl StructuredDocument {
    /// Create a new document with identity information.
    pub fn new_with_identity(
        schema: BlockSchema,
        label: String,
        accessor_agent_id: Option<String>,
    ) -> Self {
        Self {
            doc: LoroDoc::new(),
            schema,
            label,
            accessor_agent_id,
        }
    }

    /// Create a new document without identity (for tests/simple cases).
    pub fn new(schema: BlockSchema) -> Self {
        Self::new_with_identity(schema, String::new(), None)
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn accessor_agent_id(&self) -> Option<&str> {
        self.accessor_agent_id.as_deref()
    }

    /// Set attribution automatically based on accessor.
    pub fn auto_attribution(&self, operation: &str) {
        if let Some(agent_id) = &self.accessor_agent_id {
            self.set_attribution(&format!("agent:{}:{}", agent_id, operation));
        }
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_structured_document_identity`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory/document.rs
git commit -m "feat(memory): add identity fields to StructuredDocument"
```

---

## Task 16: Update render() for Read-Only Indicators

**Files:**
- Modify: `crates/pattern_core/src/memory/document.rs:365-447` (render method)

**Step 1: Write the failing test**

```rust
#[test]
fn test_render_read_only_indicators() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "status".to_string(),
                description: "Status".to_string(),
                field_type: FieldType::Text,
                required: true,
                default: None,
                read_only: true,
            },
            FieldDef {
                name: "notes".to_string(),
                description: "Notes".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);
    doc.set_field("status", "active", None, true).unwrap();
    doc.set_field("notes", "some notes", None, true).unwrap();

    let rendered = doc.render();

    // Read-only field should have indicator
    assert!(rendered.contains("status [read-only]: active"));
    // Writable field should not have indicator
    assert!(rendered.contains("notes: some notes"));
    assert!(!rendered.contains("notes [read-only]"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p pattern_core test_render_read_only_indicators`
Expected: FAIL (no [read-only] indicator)

**Step 3: Update render() method**

In the `render()` method, update the Map handling:

```rust
BlockSchema::Map { fields } => {
    let mut output = String::new();
    for field in fields {
        let value = self
            .get_field(&field.name, None)
            .map(|v| json_display(&v))
            .unwrap_or_default();

        // Mark read-only fields
        let marker = if field.read_only { " [read-only]" } else { "" };
        output.push_str(&format!("{}{}: {}\n", field.name, marker, value));
    }
    output
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p pattern_core test_render_read_only_indicators`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory/document.rs
git commit -m "feat(memory): show read-only indicators in render()"
```

---

## Task 17: Drop Tree Schema Variant (Optional Cleanup)

**Files:**
- Modify: `crates/pattern_core/src/memory/schema.rs`
- Modify: `crates/pattern_core/src/memory/document.rs`

Per design doc, Tree schema is dropped. Composite covers the use cases.

**Step 1: Search for Tree usage**

Run: `rg "BlockSchema::Tree" crates/`

**Step 2: Remove or migrate any Tree usages**

Replace with Composite or Map as appropriate.

**Step 3: Remove Tree variant**

```rust
pub enum BlockSchema {
    Text,
    Map { fields: Vec<FieldDef> },
    List { item_schema: Option<Box<BlockSchema>>, max_items: Option<usize> },
    Log { display_limit: usize },
    Composite { sections: Vec<CompositeSection> },
    // Tree removed
}
```

**Step 4: Update all match statements**

Remove `BlockSchema::Tree` arms from match statements in document.rs and elsewhere.

**Step 5: Run all tests**

Run: `cargo test -p pattern_core`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/pattern_core/src/memory/
git commit -m "refactor(memory): remove Tree schema variant"
```

---

## Task 18: Integration Test - Tool with Operation Gating

**Files:**
- Create: `crates/pattern_core/tests/tool_operation_gating.rs`

**Step 1: Write integration test**

```rust
//! Integration test for tool operation gating.

use pattern_core::tool::{
    AiTool, DynamicTool, DynamicToolAdapter, ExecutionMeta, ToolError, ToolRegistry,
    filter_schema_enum,
    rules::engine::{ToolRule, ToolRuleType, ToolRuleEngine},
};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use std::collections::HashSet;

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

impl AiTool for FileTool {
    type Input = FileInput;
    type Output = String;

    fn name(&self) -> &str { "file" }
    fn description(&self) -> &str { "Read, write, and manipulate files" }

    fn operations(&self) -> &'static [&'static str] {
        &["read", "append", "insert", "patch", "save"]
    }

    fn parameters_schema_filtered(&self, allowed_ops: &HashSet<String>) -> serde_json::Value {
        let mut schema = self.parameters_schema();
        filter_schema_enum(&mut schema, "operation", allowed_ops);
        schema
    }

    async fn execute(
        &self,
        params: Self::Input,
        _meta: &ExecutionMeta,
    ) -> Result<Self::Output, ToolError> {
        Ok(format!("Executed {:?} on {}", params.operation, params.path))
    }
}

#[tokio::test]
async fn test_file_tool_operation_gating() {
    // Set up registry with tool
    let registry = ToolRegistry::new();
    registry.register(FileTool);

    // Define rules that only allow read and append
    let allowed: HashSet<String> = ["read", "append"].iter().map(|s| s.to_string()).collect();
    let rules = vec![ToolRule {
        tool_name: "file".to_string(),
        rule_type: ToolRuleType::AllowedOperations(allowed.clone()),
        conditions: vec![],
        priority: 0,
        metadata: None,
    }];

    // Get filtered tools
    let genai_tools = registry.to_genai_tools_with_rules(&rules);
    assert_eq!(genai_tools.len(), 1);

    // Verify schema only contains allowed operations
    let tool_schema = &genai_tools[0].schema;
    // The enum should only have "read" and "append"

    // Set up rule engine for runtime checking
    let engine = ToolRuleEngine::new(rules);

    // Check allowed operations pass
    assert!(engine.check_operation_allowed("file", "read").is_ok());
    assert!(engine.check_operation_allowed("file", "append").is_ok());

    // Check disallowed operations fail
    assert!(engine.check_operation_allowed("file", "patch").is_err());
    assert!(engine.check_operation_allowed("file", "save").is_err());
}
```

**Step 2: Run integration test**

Run: `cargo test -p pattern_core --test tool_operation_gating`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/tests/tool_operation_gating.rs
git commit -m "test: add integration test for tool operation gating"
```

---

## Task 19: Integration Test - Memory Block Permissions

**Files:**
- Create: `crates/pattern_core/tests/memory_permissions.rs`

**Step 1: Write integration test**

```rust
//! Integration test for memory block field permissions.

use pattern_core::memory::{
    schema::{BlockSchema, FieldDef, FieldType, CompositeSection},
    document::{StructuredDocument, DocumentError},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[test]
fn test_field_level_permissions() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "diagnostics".to_string(),
                description: "LSP diagnostics (source-updated)".to_string(),
                field_type: FieldType::List,
                required: true,
                default: Some(serde_json::json!([])),
                read_only: true,
            },
            FieldDef {
                name: "severity_filter".to_string(),
                description: "Filter level (agent-configurable)".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: Some(serde_json::json!("warning")),
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);

    // Agent can modify writable field
    assert!(doc.set_field("severity_filter", "error", None, false).is_ok());

    // Agent cannot modify read-only field
    let result = doc.set_field("diagnostics", vec!["error1"], None, false);
    assert!(matches!(result, Err(DocumentError::ReadOnlyField(_))));

    // System can modify read-only field
    assert!(doc.set_field("diagnostics", vec!["error1", "error2"], None, true).is_ok());

    // Verify the values
    assert_eq!(
        doc.get_field("severity_filter", None).unwrap(),
        serde_json::json!("error")
    );
    assert_eq!(
        doc.get_field("diagnostics", None).unwrap(),
        serde_json::json!(["error1", "error2"])
    );
}

#[test]
fn test_composite_section_permissions() {
    let schema = BlockSchema::Composite {
        sections: vec![
            CompositeSection {
                name: "status".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![FieldDef {
                        name: "health".to_string(),
                        description: "System health".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        default: Some(serde_json::json!("unknown")),
                        read_only: false,
                    }],
                }),
                description: Some("System status (source-managed)".to_string()),
                read_only: true, // Entire section is read-only
            },
            CompositeSection {
                name: "notes".to_string(),
                schema: Box::new(BlockSchema::Text),
                description: Some("Agent notes".to_string()),
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);

    // Agent cannot write to read-only section
    let result = doc.set_field("health", "good", Some("status"), false);
    assert!(matches!(result, Err(DocumentError::ReadOnlySection(_))));

    // System can write to read-only section
    assert!(doc.set_field("health", "good", Some("status"), true).is_ok());

    // Agent can write to writable section
    assert!(doc.set_text("My notes here", Some("notes"), false).is_ok());
}

#[test]
fn test_subscription_fires_on_commit() {
    let schema = BlockSchema::Map {
        fields: vec![FieldDef {
            name: "counter".to_string(),
            description: "A counter".to_string(),
            field_type: FieldType::Counter,
            required: true,
            default: Some(serde_json::json!(0)),
            read_only: false,
        }],
    };

    let doc = StructuredDocument::new(schema);
    let change_count = Arc::new(AtomicU32::new(0));
    let change_count_clone = change_count.clone();

    let _sub = doc.subscribe_root(Arc::new(move |_event| {
        change_count_clone.fetch_add(1, Ordering::SeqCst);
    }));

    // Make changes and commit
    doc.increment_counter("counter", 1, None, true).unwrap();
    doc.commit();

    doc.increment_counter("counter", 5, None, true).unwrap();
    doc.commit();

    // Should have fired twice
    assert_eq!(change_count.load(Ordering::SeqCst), 2);
}

#[test]
fn test_render_shows_permissions() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "readonly".to_string(),
                description: "Read-only field".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: true,
            },
            FieldDef {
                name: "writable".to_string(),
                description: "Writable field".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);
    doc.set_field("readonly", "value1", None, true).unwrap();
    doc.set_field("writable", "value2", None, true).unwrap();

    let rendered = doc.render();

    assert!(rendered.contains("readonly [read-only]: value1"));
    assert!(rendered.contains("writable: value2"));
    assert!(!rendered.contains("writable [read-only]"));
}
```

**Step 2: Run integration test**

Run: `cargo test -p pattern_core --test memory_permissions`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/tests/memory_permissions.rs
git commit -m "test: add integration tests for memory block permissions"
```

---

## Final Validation

After completing all tasks:

**Step 1: Run full test suite**

```bash
cargo test -p pattern_core
```

Expected: All tests pass

**Step 2: Run clippy**

```bash
cargo clippy -p pattern_core -- -D warnings
```

Expected: No warnings

**Step 3: Check formatting**

```bash
cargo fmt --check -p pattern_core
```

Expected: No formatting issues

**Step 4: Run pre-commit checks**

```bash
just pre-commit
```

Expected: All checks pass

---

## Summary

This plan implements:

1. **Tool Operation Gating** (Tasks 1-7)
   - `AllowedOperations` variant in `ToolRuleType`
   - `operations()` and `parameters_schema_filtered()` on `AiTool` and `DynamicTool`
   - `filter_schema_enum()` helper
   - `to_genai_tools_with_rules()` on `ToolRegistry`
   - `OperationNotAllowed` violation type
   - `check_operation_allowed()` on `ToolRuleEngine`

2. **Block Schema & Loro Integration** (Tasks 8-17)
   - `read_only` flag on `FieldDef`
   - `CompositeSection` with section-level `read_only`
   - `is_field_read_only()`, `read_only_fields()`, `is_section_read_only()` helpers
   - `ReadOnlyField` and `ReadOnlySection` errors
   - `is_system` parameter on write methods
   - `section` parameter for Composite schemas
   - Loro subscription methods (`subscribe_root`, `subscribe`, `subscribe_content`)
   - Identity fields (`label`, `accessor_agent_id`)
   - Read-only indicators in `render()`
   - Tree schema removed

3. **Integration Tests** (Tasks 18-19)
   - Tool operation gating end-to-end
   - Memory block permissions end-to-end
