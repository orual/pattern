// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Core value types for the shell process manager.
//!
//! These are pure data; no I/O, no platform code. The backend (`ShellBackend`
//! trait) and its implementations consume and produce them.

/// Stable identifier for a spawned shell process. Distinct from the OS PID,
/// which can be recycled; this is a UUID prefix unique within a runtime
/// instance's lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    /// Mint a fresh `TaskId` from the first 8 hex chars of a UUID v4.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string()[..8].to_string())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The outcome of a `ShellBackend::execute` call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecuteResult {
    /// Output captured up to the moment the call returned.
    pub output: String,
    /// `Some(code)` when the command finished within the timeout. `None` when
    /// the call surfaced a timeout via `Err(ShellError::Timeout)` and was
    /// re-shaped into an `ExecuteResult` by an outer layer (the bare backend
    /// returns the error rather than this struct on timeout — see Phase 3
    /// Amendment 2026-04-26).
    pub exit_code: Option<i32>,
    /// Wall-clock duration of the call in milliseconds.
    pub duration_ms: u64,
    /// **Currently always `None`** under the v2-semantics decision recorded in
    /// `phase_03.md` (Amendment 2026-04-26). On timeout, `LocalPtyBackend::execute`
    /// sends Ctrl-C into the PTY, drains the prompt, and returns
    /// `Err(ShellError::Timeout)` — the running command is killed, not
    /// backgrounded. Agents that need long-running execution should use
    /// `Shell.Spawn`, which has clean per-spawn isolated PTYs and a
    /// bridge-thread streaming surface.
    ///
    /// Retained for forward compatibility: if a future phase re-architects the
    /// backend to a per-execute subshell-in-PTY or fresh-PTY model where
    /// backgrounding-on-timeout is feasible without queue-blocking subsequent
    /// commands, populating this field becomes the natural signal. Until then,
    /// expect `None` from every code path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backgrounded_as: Option<TaskId>,
}

/// Public-facing record describing one currently-running spawned task.
///
/// Returned from `ShellBackend::running_tasks()` and surfaced to agents via
/// `Pattern.Shell.Status` as JSON. The `task_id` is the recycle-safe handle
/// agents pass to `Kill`; the `pid` is the underlying OS process id, useful
/// for native-tool interop (e.g. `ps`, `strace`) but NOT for cross-effect
/// kill operations (use the `task_id` for that — see `Pattern/Shell.hs`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskInfo {
    pub task_id: TaskId,
    pub pid: u32,
    pub command: String,
    /// Wall-clock milliseconds elapsed since the task was spawned, computed
    /// at the moment `status()` was called.
    pub elapsed_ms: u64,
}

/// A chunk of output produced by a spawned shell process.
///
/// The backend's per-process reader thread emits these on a crossbeam channel;
/// the bridge thread (Task 7) converts them to `MessageAttachment::ShellOutput`
/// entries and enqueues them for the next turn.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputChunk {
    /// Captured stdout/stderr text, ANSI-stripped.
    Output(String),
    /// Process exited. The bridge thread sends this as the final chunk and
    /// then closes the channel.
    Exit {
        /// Process exit code. `None` if the process was killed (signal exit).
        code: Option<i32>,
        /// Wall-clock duration of the spawned process in milliseconds.
        duration_ms: u64,
    },
}

/// Permission tier required for a shell operation. Gated at dispatch time per
/// command.
///
/// Plan 3's `CapabilitySet` wraps this — for Phase 3, the field exists on every
/// shell op but enforcement is a no-op until Plan 3 wires the policy. The
/// structural seam is present so the enforcement layer can be added without
/// touching the type system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPermission {
    /// Read-only operations: `git status`, `ls`, `cat`, etc.
    ReadOnly,
    /// Read-write operations: file modifications, `git commit`, etc.
    ReadWrite,
    /// Unrestricted: any command, including those that modify system state.
    Admin,
}
