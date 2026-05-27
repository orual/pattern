// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `ShellBackend` trait — the sync interface all PTY backends must implement.
//!
//! ## Design: sync-only, no async
//!
//! The backend's call site is the Tidepool eval worker — a dedicated OS thread
//! with no ambient tokio runtime (see `CLAUDE.md` "Eval worker" section). All
//! methods block the calling thread for at most `timeout`. This is intentional:
//! PTY operations are sync syscalls at the OS level and the eval worker services
//! one request at a time, so no async machinery is needed or wanted here.
//!
//! ## Marker traits
//!
//! Every `ShellBackend` implementation must be `Send + Sync + std::fmt::Debug`.
//! - `Send`: backends are moved into session-owned `Arc`s and accessed from
//!   the eval-worker thread (a different thread from the tokio runtime).
//! - `Sync`: the `Arc<dyn ShellBackend>` may be cloned into bridge threads
//!   (Task 7) that read status and deliver output concurrently.
//! - `Debug`: required by the `ProcessManager` derive and useful for tracing.

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::Receiver;

use crate::process_manager::error::ShellError;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId, TaskInfo};

/// Sync interface for a PTY-backed shell backend.
///
/// Implementors own one persistent shell session (a PTY child process). Session
/// state — current working directory, environment variables, shell history — is
/// preserved across calls. The session is initialised lazily on the first
/// `execute` or `spawn_streaming` call.
///
/// ## Thread safety
///
/// The trait is `Send + Sync`, but the session is internally single-stream: a
/// PTY can only run one command at a time. Implementations MUST serialise
/// concurrent calls — typically via `Mutex` (for `LocalPtyBackend`'s session
/// thread) or `crossbeam_channel` dispatch (for the `ShellSession` actor in
/// Task 3).
///
/// ## No async
///
/// All methods are synchronous `fn`, not `async fn`. See module documentation
/// for the design rationale.
pub trait ShellBackend: Send + Sync + std::fmt::Debug {
    /// Execute a command synchronously. Session state (cwd, env) persists
    /// across calls.
    ///
    /// Blocks the calling thread until the command finishes **or** `timeout`
    /// fires. Under v2 semantics (phase_03.md Amendment 2026-04-26), timeout
    /// = kill: the backend sends Ctrl-C into the PTY, drains output up to a
    /// short bounded post-kill drain timeout, and returns
    /// `Err(ShellError::Timeout)`. Agents that need long-running execution
    /// should use `spawn_streaming`.
    fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError>;

    /// Spawn a long-running command with streaming output.
    ///
    /// Returns the new task ID, the OS process id of the spawned child, and a
    /// crossbeam receiver of output chunks. The sender is owned by the
    /// backend's per-task reader thread and stays alive until the process
    /// exits or `kill` is called. The caller is responsible for consuming the
    /// receiver (typically by handing it to a bridge thread).
    fn spawn_streaming(
        &self,
        command: &str,
    ) -> Result<(TaskId, u32, Receiver<OutputChunk>), ShellError>;

    /// Kill a running spawned process by its task handle.
    ///
    /// Returns `Err(ShellError::UnknownTask)` if `task_id` is not a currently
    /// running task. Returns `Err(ShellError::TaskCompleted)` if the process
    /// exited before the kill could land. Both are non-fatal; callers should
    /// log and continue.
    fn kill(&self, task_id: &TaskId) -> Result<(), ShellError>;

    /// List records describing all currently-running spawned processes.
    ///
    /// Each `TaskInfo` carries the recycle-safe handle (`task_id`), the OS
    /// process id (`pid`), the original command line, and the wall-clock
    /// elapsed time since spawn. Does not include the persistent `execute`
    /// session itself — only tasks started via `spawn_streaming`.
    fn running_tasks(&self) -> Vec<TaskInfo>;

    /// Get the current working directory of the persistent shell session.
    ///
    /// Returns `None` until the session is initialised (lazy first `execute`
    /// call). After that, the value is cached and updated on each `cd`
    /// command.
    fn cwd(&self) -> Option<PathBuf>;
}
