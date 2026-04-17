// MOVING TO: pattern_runtime/src/sdk/schema_simplifier.rs
// ORIGIN: crates/pattern_core/src/tool/schema_simplifier.rs
// PHASE: 3 + plugin-system
// RESHAPE: Trait surface extracted in phase 2; concrete tool impls reshape in plugin-system plan
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Schema simplifier for Gemini compatibility
//!
//! Gemini's function calling API only supports a subset of JSON Schema.
//! This module provides utilities to convert complex schemas to Gemini-compatible ones.

use serde_json::{json, Value};

/// Simplify a JSON Schema for Gemini compatibility
pub fn simplify_for_gemini(schema: Value) -> Value {
    match schema {
        Value::Object(mut obj) => {
            let mut simplified = obj.clone();
            
            // Simplify type if it's an array (nullable)
            if let Some(v) = simplified.get_mut("type") {
                *v = simplify_type(v.clone());
            }
            
            // Handle properties recursively
            if let Some(Value::Object(props)) = simplified.get_mut("properties") {
                for (_key, value) in props.iter_mut() {
                    *value = simplify_for_gemini(value.clone());
                }
            }
            
            // Handle items recursively
            if let Some(v) = simplified.get_mut("items") {
                *v = simplify_for_gemini(v.clone());
            }
            
            // Handle oneOf by converting to a simpler structure
            if let Some(Value::Array(_one_of)) = obj.get("oneOf") {
                // For MessageTarget, we'll use a simpler approach
                // Convert the oneOf to a single object with all possible properties
                simplified = json!({
                    "type": "object",
                    "properties": {
                        "type": {
                            "type": "string",
                            "description": "The type of target: 'user', 'agent', 'group', or 'channel'",
                            "enum": ["user", "agent", "group", "channel"]
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "The agent ID (required when type is 'agent')"
                        },
                        "group_id": {
                            "type": "string",
                            "description": "The group ID (required when type is 'group')"
                        },
                        "channel_id": {
                            "type": "string",
                            "description": "The channel ID (required when type is 'channel')"
                        }
                    },
                    "required": ["type"]
                });
                
                // Copy description from original if it exists
                if let Some(desc) = obj.get("description") {
                    simplified["description"] = desc.clone();
                }
            }
            
            Value::Object(simplified.as_object().unwrap().clone())
        }
        other => other,
    }
}

/// Simplify type definitions
fn simplify_type(type_value: Value) -> Value {
    match type_value {
        // Convert array types like ["string", "null"] to just "string"
        Value::Array(arr) => {
            if let Some(first) = arr.into_iter().find(|v| v != "null") {
                first
            } else {
                json!("string")
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_simplify_nullable_type() {
        let schema = json!({
            "type": ["string", "null"],
            "description": "A nullable string"
        });
        
        let simplified = simplify_for_gemini(schema);
        assert_eq!(simplified["type"], "string");
        assert_eq!(simplified["description"], "A nullable string");
    }
    
    #[test]
    fn test_simplify_oneof() {
        let schema = json!({
            "oneOf": [
                {"type": "string", "enum": ["user"]},
                {"type": "object", "properties": {"agent_id": {"type": "string"}}}
            ]
        });
        
        let simplified = simplify_for_gemini(schema);
        assert_eq!(simplified["type"], "object");
        assert!(simplified["properties"].is_object());
    }
}