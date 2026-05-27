// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Per-tag payload structs for typed hook event deserialization.
//!
//! Subscribers match on `event.tag` first, then call
//! `event.try_payload::<SpecificPayload>()` to get typed data.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Payload for `turn.before` / `turn.after.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnPayload {
    pub batch_id: SmolStr,
    pub turn_id: SmolStr,
    pub agent_id: SmolStr,
}

/// Payload for `tool.before` / `tool.after` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPayload {
    pub call_id: SmolStr,
    pub function_name: SmolStr,
    pub arguments_json: Option<String>,
}

/// Payload for `memory.read` / `memory.write` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPayload {
    pub label: SmolStr,
    pub operation: SmolStr, // "get", "put", "create", "append", "replace"
}

/// Payload for `shell.execute.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellPayload {
    pub command: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

/// Payload for `task.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPayload {
    pub task_id: SmolStr,
    pub block_handle: SmolStr,
    pub status: Option<SmolStr>,
}

/// Payload for `file.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePayload {
    pub path: String,
    pub operation: SmolStr, // "open", "read", "write", "watch"
}

/// Payload for `spawn.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPayload {
    pub spawn_id: SmolStr,
    pub kind: SmolStr, // "ephemeral", "sibling", "fork"
}

/// Payload for `plugin.*` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPayload {
    pub plugin_id: SmolStr,
    pub scope: Option<SmolStr>,
}
