//! Utilities for filtering JSON schemas based on allowed operations.

use serde_json::Value;
use std::collections::BTreeSet;

/// Filter an enum field in a JSON schema to only include allowed values.
///
/// Handles both simple `enum` arrays and `oneOf` patterns for tagged enums.
pub fn filter_schema_enum(schema: &mut Value, field_name: &str, allowed_values: &BTreeSet<String>) {
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

        let allowed: BTreeSet<String> = ["read", "write"].iter().map(|s| s.to_string()).collect();
        filter_schema_enum(&mut schema, "operation", &allowed);

        let enum_values = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
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

        let allowed: BTreeSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
        filter_schema_enum(&mut schema, "operation", &allowed);

        let one_of = schema["properties"]["operation"]["oneOf"]
            .as_array()
            .unwrap();
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
        let allowed: BTreeSet<String> = ["read"].iter().map(|s| s.to_string()).collect();
        filter_schema_enum(&mut schema, "operation", &allowed);

        assert_eq!(schema, original);
    }
}
