// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Unified gate types for hook blocking, permission approval, and
//! any future mechanism that needs to pause an operation and get a verdict.
//!
//! The gate flows through the same path regardless of whether the verdict
//! comes from a hook subscriber, a policy rule, or a human in the TUI.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A request to gate (pause and get a verdict on) an operation.
///
/// Wire-safe for postcard serialization between daemon and TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRequest {
    /// Unique ID for correlating request ↔ response.
    pub id: SmolStr,
    /// What kind of gate this is.
    pub kind: GateKind,
    /// The hook tag or policy rule that triggered this gate.
    pub tag: SmolStr,
    /// Which agent triggered the operation.
    pub agent_id: SmolStr,
    /// Human-readable summary of what's being gated.
    pub description: SmolStr,

    // ---- Common structured context ----
    /// For shell operations: the command being run.
    pub command: Option<SmolStr>,
    /// For file operations: the path being accessed.
    pub path: Option<SmolStr>,
    /// For memory operations: the block label.
    pub block_label: Option<SmolStr>,
    /// For tool operations: the tool/function name.
    pub tool_name: Option<SmolStr>,

    // ---- Freeform extension ----
    /// Additional context that doesn't fit the common fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<SmolStr, SmolStr>,
}

/// What triggered the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GateKind {
    /// A hook subscriber returned `Block` or `Gate`.
    HookBlock,
    /// A policy rule requires approval.
    PolicyApproval,
    /// A hook subscriber wants to run async validation before deciding.
    HookGate,
    /// Config file protection triggered.
    ConfigProtection,
}

/// The verdict from a gate — what should happen to the paused operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GateDecision {
    /// Allow the operation to proceed.
    Allow,
    /// Allow this one invocation only.
    AllowOnce,
    /// Allow all matching invocations for this scope/pattern.
    AllowForScope { scope: SmolStr },
    /// Allow for a duration (seconds).
    AllowForDuration { seconds: u64 },
    /// Deny the operation.
    Deny { reason: SmolStr },
    /// Surface information back to the agent (for after-hooks).
    /// The operation already completed; this adds context to the next turn.
    Surface { content: SmolStr },
    /// Notify the partner (human) about something that happened.
    /// Shows in the TUI as a notification/toast rather than going to the agent.
    NotifyPartner { content: SmolStr },
    /// Modify the operation's payload before proceeding.
    Modify { payload: SmolStr },
}

impl GateRequest {
    /// Construct a gate request with minimal fields.
    pub fn new(
        kind: GateKind,
        tag: impl Into<SmolStr>,
        agent_id: impl Into<SmolStr>,
        description: impl Into<SmolStr>,
    ) -> Self {
        Self {
            id: SmolStr::from(crate::types::ids::new_id()),
            kind,
            tag: tag.into(),
            agent_id: agent_id.into(),
            description: description.into(),
            command: None,
            path: None,
            block_label: None,
            tool_name: None,
            extra: BTreeMap::new(),
        }
    }

    /// Set the command context.
    pub fn with_command(mut self, cmd: impl Into<SmolStr>) -> Self {
        self.command = Some(cmd.into());
        self
    }

    /// Set the path context.
    pub fn with_path(mut self, path: impl Into<SmolStr>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set the block label context.
    pub fn with_block(mut self, label: impl Into<SmolStr>) -> Self {
        self.block_label = Some(label.into());
        self
    }

    /// Set the tool name context.
    pub fn with_tool(mut self, name: impl Into<SmolStr>) -> Self {
        self.tool_name = Some(name.into());
        self
    }

    /// Add a freeform extra field.
    pub fn with_extra(mut self, key: impl Into<SmolStr>, value: impl Into<SmolStr>) -> Self {
        self.extra.insert(key.into(), value.into());
        self
    }
}

/// Wire-safe gate response (TUI → daemon or hook subscriber → bus).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResponse {
    /// Correlates to the `GateRequest.id`.
    pub id: SmolStr,
    /// The verdict.
    pub decision: GateDecision,
}
