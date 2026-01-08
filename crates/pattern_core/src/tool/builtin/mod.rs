//! Built-in tools for agents
//!
//! This module provides the standard tools that all agents have access to,
//! including memory management and inter-agent communication.

mod block;
mod block_edit;
mod calculator;
mod constellation_search;
mod file;
mod recall;
mod search;
pub mod search_utils;
mod send_message;
mod shell;
mod shell_types;
mod source;
mod system_integrity;
#[cfg(test)]
mod test_schemas;
pub mod types;
mod web;

pub use block::BlockTool;
pub use block_edit::BlockEditTool;
pub use calculator::{CalculatorInput, CalculatorOutput, CalculatorTool};
pub use constellation_search::{
    ConstellationSearchDomain, ConstellationSearchInput, ConstellationSearchTool,
};
pub use file::FileTool;
pub use recall::RecallTool;
use schemars::JsonSchema;
pub use search::{SearchDomain, SearchInput, SearchOutput, SearchTool};
pub use send_message::SendMessageTool;
use serde::{Deserialize, Serialize};
pub use shell::ShellTool;
pub use shell_types::{ShellInput, ShellOp};
pub use source::SourceTool;
pub use system_integrity::{SystemIntegrityInput, SystemIntegrityOutput, SystemIntegrityTool};
pub use web::{WebFormat, WebInput, WebOutput, WebTool};

// V2 tool types (new tool taxonomy)
use std::sync::Arc;
pub use types::{
    BlockEditInput, BlockEditOp, BlockInput, BlockOp, FileInput, FileOp, RecallInput, RecallOp,
    SourceInput, SourceOp, ToolOutput,
};

use crate::{
    runtime::ToolContext,
    tool::{DynamicTool, DynamicToolAdapter, ToolRegistry},
};

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

impl TargetType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetType::User => "user",
            TargetType::Agent => "agent",
            TargetType::Group => "group",
            TargetType::Channel => "channel",
            TargetType::Bluesky => "bluesky",
        }
    }
}

/// Registry specifically for built-in tools
#[derive(Clone)]
pub struct BuiltinTools {
    // Existing tools
    recall_tool: Box<dyn DynamicTool>,
    search_tool: Box<dyn DynamicTool>,
    send_message_tool: Box<dyn DynamicTool>,
    web_tool: Box<dyn DynamicTool>,
    calculator_tool: Box<dyn DynamicTool>,

    // New v2 tools
    block_tool: Box<dyn DynamicTool>,
    block_edit_tool: Box<dyn DynamicTool>,
    source_tool: Box<dyn DynamicTool>,
    file_tool: Box<dyn DynamicTool>,
    shell_tool: Box<dyn DynamicTool>,
}

impl BuiltinTools {
    /// Create default built-in tools for an agent using ToolContext
    pub fn default_for_agent(ctx: Arc<dyn ToolContext>) -> Self {
        Self {
            // Existing tools
            recall_tool: Box::new(DynamicToolAdapter::new(RecallTool::new(Arc::clone(&ctx)))),
            search_tool: Box::new(DynamicToolAdapter::new(SearchTool::new(Arc::clone(&ctx)))),
            send_message_tool: Box::new(DynamicToolAdapter::new(SendMessageTool::new(Arc::clone(
                &ctx,
            )))),
            web_tool: Box::new(DynamicToolAdapter::new(WebTool::new(Arc::clone(&ctx)))),
            calculator_tool: Box::new(DynamicToolAdapter::new(CalculatorTool::new(Arc::clone(
                &ctx,
            )))),

            // New v2 tools
            block_tool: Box::new(DynamicToolAdapter::new(BlockTool::new(Arc::clone(&ctx)))),
            block_edit_tool: Box::new(DynamicToolAdapter::new(BlockEditTool::new(Arc::clone(
                &ctx,
            )))),
            source_tool: Box::new(DynamicToolAdapter::new(SourceTool::new(Arc::clone(&ctx)))),
            file_tool: Box::new(DynamicToolAdapter::new(FileTool::new(Arc::clone(&ctx)))),
            shell_tool: Box::new(DynamicToolAdapter::new(ShellTool::new(Arc::clone(&ctx)))),
        }
    }

    /// Alias for default_for_agent (for backwards compatibility)
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self::default_for_agent(ctx)
    }

    /// Register all tools to a registry
    pub fn register_all(&self, registry: &ToolRegistry) {
        // Existing tools
        registry.register_dynamic(self.recall_tool.clone_box());
        registry.register_dynamic(self.search_tool.clone_box());
        registry.register_dynamic(self.send_message_tool.clone_box());
        registry.register_dynamic(self.web_tool.clone_box());
        registry.register_dynamic(self.calculator_tool.clone_box());

        // New v2 tools
        registry.register_dynamic(self.block_tool.clone_box());
        registry.register_dynamic(self.block_edit_tool.clone_box());
        registry.register_dynamic(self.source_tool.clone_box());
        registry.register_dynamic(self.file_tool.clone_box());
        registry.register_dynamic(self.shell_tool.clone_box());
    }

    /// Builder pattern for customization
    pub fn builder() -> BuiltinToolsBuilder {
        BuiltinToolsBuilder::default()
    }
}

/// Builder for customizing built-in tools
#[derive(Default)]
pub struct BuiltinToolsBuilder {
    // Existing tools
    recall_tool: Option<Box<dyn DynamicTool>>,
    search_tool: Option<Box<dyn DynamicTool>>,
    send_message_tool: Option<Box<dyn DynamicTool>>,
    web_tool: Option<Box<dyn DynamicTool>>,
    calculator_tool: Option<Box<dyn DynamicTool>>,

    // New v2 tools
    block_tool: Option<Box<dyn DynamicTool>>,
    block_edit_tool: Option<Box<dyn DynamicTool>>,
    source_tool: Option<Box<dyn DynamicTool>>,
    file_tool: Option<Box<dyn DynamicTool>>,
    shell_tool: Option<Box<dyn DynamicTool>>,
}

impl BuiltinToolsBuilder {
    /// Replace the default recall tool
    pub fn with_recall_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.recall_tool = Some(Box::new(tool));
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

    /// Replace the default block tool
    pub fn with_block_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.block_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default block_edit tool
    pub fn with_block_edit_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.block_edit_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default source tool
    pub fn with_source_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.source_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default file tool
    pub fn with_file_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.file_tool = Some(Box::new(tool));
        self
    }

    /// Replace the default shell tool
    pub fn with_shell_tool(mut self, tool: impl DynamicTool + 'static) -> Self {
        self.shell_tool = Some(Box::new(tool));
        self
    }

    /// Build the tools for a specific agent using ToolContext
    pub fn build_for_agent(self, ctx: Arc<dyn ToolContext>) -> BuiltinTools {
        let defaults = BuiltinTools::default_for_agent(ctx);
        BuiltinTools {
            // Existing tools
            recall_tool: self.recall_tool.unwrap_or(defaults.recall_tool),
            search_tool: self.search_tool.unwrap_or(defaults.search_tool),
            send_message_tool: self.send_message_tool.unwrap_or(defaults.send_message_tool),
            web_tool: self.web_tool.unwrap_or(defaults.web_tool),
            calculator_tool: self.calculator_tool.unwrap_or(defaults.calculator_tool),

            // New v2 tools
            block_tool: self.block_tool.unwrap_or(defaults.block_tool),
            block_edit_tool: self.block_edit_tool.unwrap_or(defaults.block_edit_tool),
            source_tool: self.source_tool.unwrap_or(defaults.source_tool),
            file_tool: self.file_tool.unwrap_or(defaults.file_tool),
            shell_tool: self.shell_tool.unwrap_or(defaults.shell_tool),
        }
    }
}

/// List of all available built-in tool names.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "recall",
    "search",
    "send_message",
    "web",
    "calculator",
    "block",
    "block_edit",
    "source",
    "file",
    "shell",
    "emergency_halt",
];

/// Create a built-in tool by name.
///
/// Returns `Some(tool)` if the name matches a built-in tool, `None` otherwise.
/// For custom tools, use the inventory-based lookup.
pub fn create_builtin_tool(name: &str, ctx: Arc<dyn ToolContext>) -> Option<Box<dyn DynamicTool>> {
    match name {
        "recall" => Some(Box::new(DynamicToolAdapter::new(RecallTool::new(
            Arc::clone(&ctx),
        )))),
        "search" => Some(Box::new(DynamicToolAdapter::new(SearchTool::new(
            Arc::clone(&ctx),
        )))),
        "send_message" => Some(Box::new(DynamicToolAdapter::new(SendMessageTool::new(
            Arc::clone(&ctx),
        )))),
        "web" => Some(Box::new(DynamicToolAdapter::new(WebTool::new(Arc::clone(
            &ctx,
        ))))),
        "calculator" => Some(Box::new(DynamicToolAdapter::new(CalculatorTool::new(
            Arc::clone(&ctx),
        )))),
        "block" => Some(Box::new(DynamicToolAdapter::new(BlockTool::new(
            Arc::clone(&ctx),
        )))),
        "block_edit" => Some(Box::new(DynamicToolAdapter::new(BlockEditTool::new(
            Arc::clone(&ctx),
        )))),
        "source" => Some(Box::new(DynamicToolAdapter::new(SourceTool::new(
            Arc::clone(&ctx),
        )))),
        "file" => Some(Box::new(DynamicToolAdapter::new(FileTool::new(
            Arc::clone(&ctx),
        )))),
        "shell" => Some(Box::new(DynamicToolAdapter::new(ShellTool::new(
            Arc::clone(&ctx),
        )))),
        "emergency_halt" => Some(Box::new(DynamicToolAdapter::new(SystemIntegrityTool::new(
            Arc::clone(&ctx),
        )))),
        _ => None,
    }
}

/// Create a tool by name, checking builtins first, then custom registry.
///
/// This is the preferred function for tool instantiation - it handles both
/// built-in tools and custom tools registered via inventory.
pub fn create_tool_by_name(name: &str, ctx: Arc<dyn ToolContext>) -> Option<Box<dyn DynamicTool>> {
    // Try builtin first
    if let Some(tool) = create_builtin_tool(name, Arc::clone(&ctx)) {
        return Some(tool);
    }

    // Fall back to custom tool registry
    crate::tool::create_custom_tool(name, ctx)
}

/// List all available tool names (builtin + custom).
pub fn all_available_tools() -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = BUILTIN_TOOL_NAMES.to_vec();
    tools.extend(crate::tool::available_custom_tools());
    tools
}

#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use test_utils::{MockToolContext, create_test_agent_in_db, create_test_context_with_agent};
