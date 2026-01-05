//! Configuration types for agent tool rules
//!
//! These types define serializable configuration for tool execution rules
//! that control how agents can use tools.

use serde::{Deserialize, Serialize};

/// Configuration for tool execution rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRuleConfig {
    /// Name of the tool this rule applies to
    pub tool_name: String,

    /// Type of rule
    pub rule_type: ToolRuleTypeConfig,

    /// Conditions for this rule (tool names, parameters, etc.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,

    /// Priority of this rule (higher numbers = higher priority)
    #[serde(default = "default_rule_priority")]
    pub priority: u8,

    /// Optional metadata for this rule
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Configuration for tool rule types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ToolRuleTypeConfig {
    /// Continue the conversation loop after this tool is called (no heartbeat required)
    ContinueLoop,

    /// Exit conversation loop after this tool is called
    ExitLoop,

    /// This tool must be called after specified tools (ordering dependency)
    RequiresPrecedingTools,

    /// This tool must be called before specified tools
    RequiresFollowingTools,

    /// Multiple exclusive groups - only one tool from each group can be called per conversation
    ExclusiveGroups(Vec<Vec<String>>),

    /// Call this tool at conversation start
    StartConstraint,

    /// This tool must be called before conversation ends
    RequiredBeforeExit,

    /// Required for exit if condition is met
    RequiredBeforeExitIf,

    /// Maximum number of times this tool can be called
    MaxCalls(u32),

    /// Minimum cooldown period between calls (in seconds)
    Cooldown(u64),

    /// Call this tool periodically during long conversations (in seconds)
    Periodic(u64),

    /// Require user consent before executing the tool
    RequiresConsent {
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

fn default_rule_priority() -> u8 {
    5
}
