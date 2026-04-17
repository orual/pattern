// MOVING TO: pattern_runtime/src/sdk/rules/mod.rs
// ORIGIN: crates/pattern_core/src/tool/rules/mod.rs
// PHASE: 3 + plugin-system
// RESHAPE: Trait surface extracted in phase 2; concrete tool impls reshape in plugin-system plan
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

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
