//! Tool Rules System for Pattern Agents
//!
//! This module provides sophisticated control over tool execution flow, enabling agents to:
//! - Enforce tool dependencies and ordering
//! - Optimize performance through selective heartbeat management
//! - Control conversation flow (continue/exit loops)
//! - Manage resource limits and cooldowns
//! - Define exclusive tool groups
//! - Require initialization and cleanup tools

pub mod engine;

#[cfg(test)]
pub mod integration_tests;

// Re-export main types
pub use engine::{
    ExecutionPhase, ToolExecution, ToolExecutionState, ToolRule, ToolRuleEngine, ToolRuleType,
    ToolRuleViolation,
};
