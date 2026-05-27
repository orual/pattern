// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Catalog of well-known hook event tags.
//!
//! Each constant is a hierarchical string tag. Emit sites reference these
//! constants; subscribers can match by glob pattern. Raw string literals
//! are reserved for plugin-emitted custom tags.

// ---- Turn lifecycle --------------------------------------------------------
pub const TURN_BEFORE: &str = "turn.before";
pub const TURN_AFTER_SUCCESS: &str = "turn.after.success";
pub const TURN_AFTER_FAILURE: &str = "turn.after.failure";
pub const TURN_STOP: &str = "turn.stop";

// ---- Tool dispatch ---------------------------------------------------------
pub const TOOL_BEFORE: &str = "tool.before";
pub const TOOL_AFTER: &str = "tool.after";
pub const TOOL_FAILED: &str = "tool.failed";

// ---- Memory ----------------------------------------------------------------
pub const MEMORY_READ: &str = "memory.read";
pub const MEMORY_WRITE: &str = "memory.write";
pub const MEMORY_SHARED_READ: &str = "memory.shared.read";

// ---- Shell -----------------------------------------------------------------
pub const SHELL_EXECUTE_BEFORE: &str = "shell.execute.before";
pub const SHELL_EXECUTE_AFTER: &str = "shell.execute.after";
pub const SHELL_SPAWN: &str = "shell.spawn";
pub const SHELL_KILL: &str = "shell.kill";

// ---- Tasks -----------------------------------------------------------------
pub const TASK_CREATED: &str = "task.created";
pub const TASK_TRANSITIONED_DONE: &str = "task.transitioned.done";
pub const TASK_TRANSITIONED_IN_PROGRESS: &str = "task.transitioned.in_progress";
pub const TASK_TRANSITIONED_BLOCKED: &str = "task.transitioned.blocked";
pub const TASK_TRANSITIONED_CANCELED: &str = "task.transitioned.canceled";
pub const TASK_LINKED: &str = "task.linked";
pub const TASK_COMMENTED: &str = "task.commented";

// ---- Search + Recall -------------------------------------------------------
pub const SEARCH_QUERY: &str = "search.query";
pub const RECALL_SEARCH: &str = "recall.search";
pub const RECALL_INSERTED: &str = "recall.inserted";

// ---- File ------------------------------------------------------------------
pub const FILE_OPENED: &str = "file.opened";
pub const FILE_READ: &str = "file.read";
pub const FILE_WRITE: &str = "file.write";
pub const FILE_WATCHED: &str = "file.watched";

// ---- Port ------------------------------------------------------------------
pub const PORT_CALLED: &str = "port.called";
pub const PORT_CALL_AFTER: &str = "port.call.after";
pub const PORT_SUBSCRIBED: &str = "port.subscribed";

// ---- Spawn -----------------------------------------------------------------
pub const SPAWN_EPHEMERAL_START: &str = "spawn.ephemeral.start";
pub const SPAWN_EPHEMERAL_EXIT: &str = "spawn.ephemeral.exit";
pub const SPAWN_SIBLING: &str = "spawn.sibling";
pub const SPAWN_FORK: &str = "spawn.fork";
pub const SPAWN_FORK_OP: &str = "spawn.fork.op";

// ---- Plugin ----------------------------------------------------------------
pub const PLUGIN_INSTALLED: &str = "plugin.installed";
pub const PLUGIN_UNINSTALLED: &str = "plugin.uninstalled";
pub const PLUGIN_REGISTERED: &str = "plugin.registered";
pub const PLUGIN_UNREGISTERED: &str = "plugin.unregistered";

// ---- Message ---------------------------------------------------------------
pub const MESSAGE_SENT: &str = "message.sent";
pub const MESSAGE_RECEIVED: &str = "message.received";

// ---- Session ---------------------------------------------------------------
pub const SESSION_OPENED: &str = "session.opened";
pub const SESSION_CLOSED: &str = "session.closed";

// ---- Fronting --------------------------------------------------------------
pub const FRONTING_CHANGED: &str = "fronting.changed";
pub const FRONTING_ROUTED: &str = "fronting.routed";

// ---- Wake ------------------------------------------------------------------
pub const WAKE_REGISTERED: &str = "wake.registered";
pub const WAKE_UNREGISTERED: &str = "wake.unregistered";
pub const WAKE_FIRED: &str = "wake.fired";
