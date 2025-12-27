//! Built-in tools for agents
//!
//! This module provides the standard tools that all agents have access to,
//! including memory management and inter-agent communication.

mod calculator;
mod constellation_search;
mod context;
//pub mod data_source;
mod recall;
mod search;
pub mod search_utils;
mod send_message;
mod system_integrity;
#[cfg(test)]
mod test_schemas;
mod web;

pub use calculator::{CalculatorInput, CalculatorOutput, CalculatorTool};
pub use constellation_search::{
    ConstellationSearchDomain, ConstellationSearchInput, ConstellationSearchTool,
};
pub use context::{ContextInput, ContextOutput, ContextTool, CoreMemoryOperationType};
//pub use data_source::{
//    DataSourceInput, DataSourceOutput, DataSourceTool, register_data_source_tool,
//};
pub use recall::{
    ArchivalMemoryOperationType, ArchivalSearchResult, RecallInput, RecallOutput, RecallTool,
};
use schemars::JsonSchema;
pub use search::{SearchDomain, SearchInput, SearchOutput, SearchTool};
pub use send_message::SendMessageTool;
use serde::{Deserialize, Serialize};
pub use system_integrity::{SystemIntegrityInput, SystemIntegrityOutput, SystemIntegrityTool};
pub use web::{WebFormat, WebInput, WebOutput, WebTool};

use crate::{
    runtime::ToolContext,
    tool::{DynamicTool, DynamicToolAdapter, ToolRegistry},
};
use std::sync::Arc;

// Message target types for send_message tool
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub struct MessageTarget {
    pub target_type: TargetType,
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    User,
    Agent,
    Group,
    Channel,
    Bluesky,
}

/// Registry specifically for built-in tools
#[derive(Clone)]
pub struct BuiltinTools {
    recall_tool: Box<dyn DynamicTool>,
    context_tool: Box<dyn DynamicTool>,
    search_tool: Box<dyn DynamicTool>,
    send_message_tool: Box<dyn DynamicTool>,
    web_tool: Box<dyn DynamicTool>,
    calculator_tool: Box<dyn DynamicTool>,
}

impl BuiltinTools {
    /// Create default built-in tools for an agent using ToolContext
    pub fn default_for_agent(ctx: Arc<dyn ToolContext>) -> Self {
        Self {
            recall_tool: Box::new(DynamicToolAdapter::new(RecallTool::new(Arc::clone(&ctx)))),
            context_tool: Box::new(DynamicToolAdapter::new(ContextTool::new(Arc::clone(&ctx)))),
            search_tool: Box::new(DynamicToolAdapter::new(SearchTool::new(Arc::clone(&ctx)))),
            send_message_tool: Box::new(DynamicToolAdapter::new(SendMessageTool::new(Arc::clone(
                &ctx,
            )))),
            web_tool: Box::new(DynamicToolAdapter::new(WebTool::new(Arc::clone(&ctx)))),
            calculator_tool: Box::new(DynamicToolAdapter::new(CalculatorTool::new(Arc::clone(
                &ctx,
            )))),
        }
    }

    /// Alias for default_for_agent (for backwards compatibility)
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self::default_for_agent(ctx)
    }

    /// Register all tools to a registry
    pub fn register_all(&self, registry: &ToolRegistry) {
        registry.register_dynamic(self.recall_tool.clone_box());
        registry.register_dynamic(self.context_tool.clone_box());
        registry.register_dynamic(self.search_tool.clone_box());
        registry.register_dynamic(self.send_message_tool.clone_box());
        registry.register_dynamic(self.web_tool.clone_box());
        registry.register_dynamic(self.calculator_tool.clone_box());

        // Note: DataSourceTool requires external coordinator setup.
        // Use register_data_source_tool() function directly when you have a coordinator.
    }

    /// Builder pattern for customization
    pub fn builder() -> BuiltinToolsBuilder {
        BuiltinToolsBuilder::default()
    }
}

/// Builder for customizing built-in tools
#[derive(Default)]
pub struct BuiltinToolsBuilder {
    recall_tool: Option<Box<dyn DynamicTool>>,
    context_tool: Option<Box<dyn DynamicTool>>,
    search_tool: Option<Box<dyn DynamicTool>>,
    send_message_tool: Option<Box<dyn DynamicTool>>,
    web_tool: Option<Box<dyn DynamicTool>>,
    calculator_tool: Option<Box<dyn DynamicTool>>,
}

impl BuiltinToolsBuilder {
    /// Replace the default recall tool
    pub fn with_recall_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.recall_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default context tool
    pub fn with_context_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.context_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default search tool
    pub fn with_search_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.search_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default send_message tool
    pub fn with_send_message_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.send_message_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default web tool
    pub fn with_web_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.web_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default calculator tool
    pub fn with_calculator_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.calculator_tool = Some(Box::new(tool));
        self
    }

    /// Build the tools for a specific agent using ToolContext
    pub fn build_for_agent(self, ctx: Arc<dyn ToolContext>) -> BuiltinTools {
        let defaults = BuiltinTools::default_for_agent(ctx);
        BuiltinTools {
            recall_tool: self.recall_tool.unwrap_or(defaults.recall_tool),
            context_tool: self.context_tool.unwrap_or(defaults.context_tool),
            search_tool: self.search_tool.unwrap_or(defaults.search_tool),
            send_message_tool: self.send_message_tool.unwrap_or(defaults.send_message_tool),
            web_tool: self.web_tool.unwrap_or(defaults.web_tool),
            calculator_tool: self.calculator_tool.unwrap_or(defaults.calculator_tool),
        }
    }
}

#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use test_utils::{MockToolContext, create_test_agent_in_db, create_test_context_with_agent};
