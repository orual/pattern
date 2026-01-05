//! Plugin registry for custom tools.
//!
//! This module provides the infrastructure for registering custom tools
//! that can be instantiated from configuration. Uses the `inventory` crate for
//! distributed static registration.
//!
//! # Example
//!
//! To register a custom tool:
//!
//! ```ignore
//! use pattern_core::tool::{DynamicTool, CustomToolFactory};
//! use pattern_core::runtime::ToolContext;
//! use std::sync::Arc;
//!
//! struct MyCustomTool { /* ... */ }
//! impl DynamicTool for MyCustomTool { /* ... */ }
//!
//! inventory::submit! {
//!     CustomToolFactory {
//!         tool_name: "my_custom_tool",
//!         create: |ctx| {
//!             Box::new(MyCustomTool::new(ctx))
//!         },
//!     }
//! }
//! ```

use std::sync::Arc;

use crate::runtime::ToolContext;

use super::DynamicTool;

/// Factory for creating custom tools.
///
/// Register these using `inventory::submit!` to make them available
/// for instantiation by name.
pub struct CustomToolFactory {
    /// Tool name (must be unique)
    pub tool_name: &'static str,

    /// Factory function that creates a tool with the given context
    pub create: fn(Arc<dyn ToolContext>) -> Box<dyn DynamicTool>,
}

// Make CustomToolFactory collectable by inventory
inventory::collect!(CustomToolFactory);

/// Look up and create a custom tool by name.
///
/// Searches registered `CustomToolFactory` entries for a matching
/// `tool_name` and calls its `create` function with the provided context.
pub fn create_custom_tool(name: &str, ctx: Arc<dyn ToolContext>) -> Option<Box<dyn DynamicTool>> {
    for factory in inventory::iter::<CustomToolFactory> {
        if factory.tool_name == name {
            return Some((factory.create)(ctx));
        }
    }
    None
}

/// List all registered custom tool names.
pub fn available_custom_tools() -> Vec<&'static str> {
    inventory::iter::<CustomToolFactory>
        .into_iter()
        .map(|f| f.tool_name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::builtin::create_test_context_with_agent;

    #[tokio::test]
    async fn test_no_factories_registered_returns_none() {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;
        let result = create_custom_tool("nonexistent", ctx);
        assert!(result.is_none());
    }
}
