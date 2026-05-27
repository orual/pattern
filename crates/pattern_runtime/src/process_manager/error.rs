// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Error type for shell process manager operations.
//!
//! All variants are `#[non_exhaustive]` at the enum level so downstream
//! `match` arms must include a wildcard; this lets Phase 3+ add new variants
//! without breaking callers that handle what they know and fall through the
//! rest.

use std::path::PathBuf;
use std::time::Duration;

use crate::process_manager::types::ShellPermission;

/// Errors produced by `ProcessManager` and `ShellBackend` implementations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellError {
    /// The requested operation requires a higher permission tier than the
    /// agent's `CapabilitySet` grants.
    #[error("permission denied: required {required:?}, granted {granted:?}")]
    PermissionDenied {
        required: ShellPermission,
        granted: ShellPermission,
    },

    /// The command would have accessed a path outside the configured sandbox
    /// root.
    #[error("path outside sandbox: {0}")]
    PathOutsideSandbox(PathBuf),

    /// A policy rule explicitly denied this command.
    ///
    /// The string is the policy rule's `reason` field, if set, or the raw
    /// command otherwise.
    #[error("command denied by policy: {0}")]
    CommandDenied(String),

    /// The command exceeded its timeout and was killed.
    ///
    /// Under the v2-semantics decision recorded in `phase_03.md` (Amendment
    /// 2026-04-26), `LocalPtyBackend::execute` always kills on timeout and
    /// returns this error — there is currently no backgrounding path.
    /// `ExecuteResult::backgrounded_as` is always `None` from the bare backend.
    /// Agents that need long-running execution should use `Shell.Spawn`.
    #[error("command timed out after {0:?}")]
    Timeout(Duration),

    /// The backend failed to launch the underlying shell process.
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[source] std::io::Error),

    /// A PTY-level error occurred (e.g. `openpty(2)` failed, PTY writer broke).
    ///
    /// The string is a human-readable description; the underlying `std::io::Error`
    /// is wrapped when available.
    #[error("PTY error: {0}")]
    PtyError(String),

    /// No running task with the given ID. AC3.8 names `ProcessNotFound`; this
    /// variant satisfies that acceptance criterion — the name is illustrative in
    /// the AC text, not normative.
    #[error("unknown task: {0}")]
    UnknownTask(String),

    /// The task completed before the caller could interact with it (e.g.
    /// `kill` on an already-exited process).
    #[error("task already completed")]
    TaskCompleted,

    /// The shell session has not been initialised yet. This is an internal
    /// state error; callers should not see it in normal operation because the
    /// backend lazily initialises on first `execute` call.
    #[error("session not initialized")]
    SessionNotInitialized,

    /// The shell session's PTY died unexpectedly (EOF on the PTY master, or
    /// the shell process exited). The backend reinitialises on the next call.
    #[error("session died unexpectedly")]
    SessionDied,

    /// The OSC prompt-marker echo could not be found in the command output;
    /// the exit code cannot be determined reliably.
    #[error("could not parse exit code from output")]
    ExitCodeParseFailed,

    /// A raw I/O error from the PTY or log file.
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),

    /// The command string is structurally invalid (e.g. empty, null bytes).
    #[error("invalid command: {0}")]
    InvalidCommand(String),

    /// The command output could not be decoded as UTF-8.
    #[error("encoding error: {0}")]
    EncodingError(String),

    /// The Shell effect is not in the agent's `CapabilitySet`. This is the
    /// coarse-grained gate; `PermissionDenied` is the fine-grained gate.
    #[error("capability denied: Shell effect not in agent's CapabilitySet")]
    CapabilityDenied,
}
