// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `ProcessManager` — thin sync coordinator over a `ShellBackend`.
//!
//! ## Design notes
//!
//! `ProcessManager` is a thin wrapper that keeps capability gating and bridge
//! thread management out of the backend itself. It delegates every operation
//! directly to the held `Arc<dyn ShellBackend>` without buffering or
//! reinterpretation.
//!
//! ## Per-session, not runtime-global
//!
//! Per the Q4 resolution (Amendment 2026-04-26), `ProcessManager` lives on
//! `SessionContext` rather than on `TidepoolRuntime`. Session A's `cd /tmp`
//! must not affect session B's `pwd`. This matches the `FileManager` precedent
//! from Phase 2.
//!
//! ## No capability gating here
//!
//! Capability gating (Plan 3's `CapabilitySet`) happens at the handler
//! boundary in `ShellHandler` (Task 6). `ProcessManager` is runtime-internal
//! and deliberately import-free of `pattern_core::CapabilitySet` to keep the
//! dependency cone minimal.
//!
//! ## No tokio
//!
//! `ProcessManager` is entirely sync — no `block_on`, no `spawn`, no
//! `Handle::current()`. All methods block the calling thread, which is the
//! Tidepool eval worker. The eval worker runs without an ambient tokio runtime
//! by design; see `CLAUDE.md` "Eval worker" section.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Receiver;
use pattern_core::types::message::{MessageAttachment, ShellOutputKind};

use crate::process_manager::backend::ShellBackend;
use crate::process_manager::error::ShellError;
use crate::process_manager::local_pty::LocalPtyBackend;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId, TaskInfo};

/// Sync coordinator over a `ShellBackend` instance.
///
/// Wraps a single `Arc<dyn ShellBackend>` and exposes a stable API surface for
/// the `ShellHandler` (Task 6) to dispatch into. The indirection keeps
/// capability-gating code in the handler and backend mechanics in the backend
/// implementation.
///
/// `cache_dir` is the root under which per-task log files are written by
/// `ProcessLogger` (Task 8, AC3.10). Each spawned task gets its own file at
/// `<cache_dir>/shell/<task_id>.log`.
#[derive(Debug)]
pub struct ProcessManager {
    backend: Arc<dyn ShellBackend>,
    /// Root cache directory for shell task logs. Defaults to
    /// `$TMPDIR/pattern/shell` via `ProcessManager::new`.
    cache_dir: PathBuf,
}

impl ProcessManager {
    /// Construct with a `LocalPtyBackend` using the given initial working
    /// directory. This is the production constructor — the process manager
    /// spawns a real PTY-backed shell when the first command is executed.
    ///
    /// `initial_cwd` is passed to the backend so the shell session opens with
    /// the expected working directory. In practice, callers supply
    /// `std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))`.
    ///
    /// `cache_dir` is the root for per-task log files (Task 8, AC3.10). Pass
    /// the session's data/cache directory; fall back to
    /// `std::env::temp_dir().join("pattern")` when no explicit path is
    /// configured.
    pub fn new(initial_cwd: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            backend: Arc::new(LocalPtyBackend::new(initial_cwd)),
            cache_dir,
        }
    }

    /// Construct with an explicit backend. Used in tests to inject a stub or
    /// mock backend without a real PTY.
    ///
    /// Uses `std::env::temp_dir().join("pattern")` as the cache directory
    /// since test backends typically don't need real log files. Pass a
    /// different `cache_dir` via `with_backend_and_cache_dir` when the test
    /// exercises the logging path.
    pub fn with_backend(backend: Arc<dyn ShellBackend>) -> Self {
        Self {
            backend,
            cache_dir: std::env::temp_dir().join("pattern"),
        }
    }

    /// Construct with an explicit backend and explicit cache directory.
    ///
    /// Use this variant in tests that verify `ProcessLogger` integration.
    pub fn with_backend_and_cache_dir(backend: Arc<dyn ShellBackend>, cache_dir: PathBuf) -> Self {
        Self { backend, cache_dir }
    }

    /// Root directory under which per-task log files are stored.
    ///
    /// Actual log files live at `<cache_dir>/shell/<task_id>.log`. Exposed
    /// so `ShellHandler::handle` (Task 8) can construct a `ProcessLogger`
    /// for each `Spawn` call without needing a separate accessor path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Execute a command synchronously and return the result.
    ///
    /// Delegates directly to the backend. Under the v2-semantics decision
    /// (Amendment 2026-04-26), **timeout = kill**: if the command exceeds
    /// `timeout`, the backend sends Ctrl-C, drains the prompt, and returns
    /// `Err(ShellError::Timeout)`. There is no backgrounding path at this layer.
    /// Agents that need long-running execution should use `Shell.Spawn` (which
    /// gets its own isolated PTY and bridge-thread streaming).
    pub fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError> {
        self.backend.execute(command, timeout)
    }

    /// Spawn a long-running command with streaming output.
    ///
    /// Returns the task ID, the OS process id of the spawned child, and a
    /// crossbeam receiver of `OutputChunk`s. The caller hands the receiver to
    /// a bridge thread (`spawn_output_bridge`) that converts chunks to
    /// `MessageAttachment::ShellOutput` entries and enqueues them via
    /// `SessionContext::record_async_reminder`.
    pub fn spawn(&self, command: &str) -> Result<(TaskId, u32, Receiver<OutputChunk>), ShellError> {
        self.backend.spawn_streaming(command)
    }

    /// Kill a running spawned process by its handle.
    ///
    /// Returns `Err(ShellError::UnknownTask)` if the handle is unknown (e.g.
    /// the task already exited and was cleaned up).
    pub fn kill(&self, task_id: &TaskId) -> Result<(), ShellError> {
        self.backend.kill(task_id)
    }

    /// List records describing all currently-running spawned processes.
    ///
    /// Each `TaskInfo` carries the recycle-safe handle (`task_id`), the OS
    /// process id, the original command line, and wall-clock elapsed time.
    pub fn status(&self) -> Vec<TaskInfo> {
        self.backend.running_tasks()
    }

    /// Current working directory of the persistent shell session.
    ///
    /// Before the first `execute` call, returns the `initial_cwd` the manager
    /// was constructed with. After each `execute`, the backend caches the
    /// resolved cwd (via `pwd`) and this returns the cached value. Returns
    /// `None` only when an alternative backend explicitly reports no cwd.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.backend.cwd()
    }
}

/// Spawn a background thread that drains the crossbeam `Receiver<OutputChunk>`
/// produced by `ProcessManager::spawn` and enqueues each chunk as a
/// `MessageAttachment::ShellOutput` entry onto the async-reminder queue.
///
/// ## Why std::thread, not tokio::spawn?
///
/// `ProcessManager` is pure `std::thread` + crossbeam by design — the eval
/// worker has no ambient tokio runtime. Introducing a tokio task here would
/// re-introduce the runtime coupling we explicitly avoided (see phase_03.md
/// architecture note). The bridge work is a tight `recv → enqueue` loop;
/// blocking on `rx.iter()` is appropriate for an OS thread.
///
/// ## Thread lifetime
///
/// The bridge thread exits when the sender end of `rx` drops (process exit,
/// `kill`, or `Shutdown`). The `Exit` chunk is the terminal sentinel: the
/// bridge breaks its loop immediately after pushing it so it does not wait
/// for further (never-arriving) chunks.
///
/// ## Logger (Task 8, AC3.10)
///
/// When `logger` is `Some(ProcessLogger)`, each chunk is also written to the
/// log file before being enqueued on the queue. Log writes are best-effort:
/// a write error is reported via `tracing::warn!` but does not abort the
/// bridge — the queue enqueue is the primary output path; logging is a
/// reliability backstop.
pub fn spawn_output_bridge(
    task_id: TaskId,
    rx: Receiver<OutputChunk>,
    queue: Arc<Mutex<Vec<MessageAttachment>>>,
    logger: Option<crate::process_manager::logger::ProcessLogger>,
) {
    let task_id_str = task_id.to_string();
    std::thread::Builder::new()
        .name(format!("shell-output-bridge:{task_id_str}"))
        .spawn(move || {
            for chunk in rx.iter() {
                // Best-effort log write (AC3.10 crash backstop). Errors are
                // warned but do not abort the bridge — queue enqueue is the
                // primary path.
                if let Some(ref log) = logger
                    && let Err(e) = log.append(&chunk)
                {
                    tracing::warn!(
                        task_id = %task_id_str,
                        error = %e,
                        "shell-output-bridge: log write failed (best-effort; continuing)"
                    );
                }

                let kind = match &chunk {
                    OutputChunk::Output(text) => ShellOutputKind::Output(text.clone()),
                    OutputChunk::Exit { code, duration_ms } => ShellOutputKind::Exit {
                        code: *code,
                        duration_ms: *duration_ms,
                    },
                };
                let is_exit = matches!(kind, ShellOutputKind::Exit { .. });
                let attachment = MessageAttachment::ShellOutput {
                    task_id: task_id_str.clone(),
                    kind,
                    at: jiff::Timestamp::now(),
                };
                // Lock briefly per chunk. The queue is drained at turn
                // boundaries by `agent_loop::compose_request_for_turn`; the
                // Mutex contention window is tiny.
                queue.lock().unwrap().push(attachment);
                if is_exit {
                    // Exit is the terminal chunk. The sender side has already
                    // dropped or will drop shortly; exiting here avoids a
                    // spurious recv() that would block until the channel closes.
                    break;
                }
            }
            tracing::debug!(task_id = %task_id_str, "shell-output-bridge: thread exiting");
        })
        .expect("failed to spawn shell-output bridge thread");
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::process_manager::error::ShellError;
    use crate::process_manager::local_pty::LocalPtyBackend;
    use crate::process_manager::types::{OutputChunk, TaskId};

    /// Skip the test if no usable shell is available. Defers to
    /// `LocalPtyBackend::find_default_shell` so the test guard agrees with the
    /// production probe — no chance of the test running against a different
    /// shell-availability definition than the backend itself uses.
    fn ensure_shell_available() -> bool {
        let shell = LocalPtyBackend::find_default_shell();
        std::path::Path::new(&shell).exists() || shell == "bash"
    }

    /// `ProcessManager::new` constructs a real `LocalPtyBackend`; verify
    /// `execute` runs end-to-end through the wrapper.
    #[test]
    fn new_executes_command_through_real_backend() {
        if !ensure_shell_available() {
            eprintln!("skipping: no shell found");
            return;
        }
        let pm = ProcessManager::new(std::env::temp_dir(), std::env::temp_dir().join("pattern"));
        let result = pm
            .execute("echo manager-works", Duration::from_secs(5))
            .expect("execute succeeds");
        assert!(
            result.output.contains("manager-works"),
            "expected 'manager-works' in output, got: {:?}",
            result.output
        );
        assert_eq!(result.exit_code, Some(0));
    }

    /// `status` returns the live task list maintained by the backend. Spawn a
    /// long-running command, observe it appears, then kill and observe it
    /// disappears (the kill removes the entry; observing `Exit` on the
    /// receiver guarantees `running_tasks` no longer lists it — see the
    /// `LocalPtyBackend::run_spawn_reader` ordering invariant).
    #[test]
    fn status_lists_spawned_task_via_real_backend() {
        if !ensure_shell_available() {
            return;
        }
        let pm = ProcessManager::new(std::env::temp_dir(), std::env::temp_dir().join("pattern"));
        let (task_id, _pid, rx) = pm.spawn("sleep 60").expect("spawn succeeds");
        let lists_task = || pm.status().iter().any(|info| info.task_id == task_id);
        assert!(
            lists_task(),
            "expected status to list spawned task, got: {:?}",
            pm.status()
        );
        pm.kill(&task_id).expect("kill succeeds");
        // Drain to Exit; once seen, the entry is removed.
        for _ in 0..50 {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(OutputChunk::Exit { .. }) => break,
                Ok(OutputChunk::Output(_)) => {}
                Err(_) => break,
            }
        }
        assert!(
            !lists_task(),
            "expected status to drop killed task after Exit, got: {:?}",
            pm.status()
        );
    }

    /// `kill` of an unknown task forwards `ShellError::UnknownTask` from the
    /// backend without variant transformation.
    #[test]
    fn kill_unknown_task_returns_unknown_task_error() {
        let pm = ProcessManager::new(std::env::temp_dir(), std::env::temp_dir().join("pattern"));
        let err = pm
            .kill(&TaskId("no-such-id".to_string()))
            .expect_err("kill of bogus id must error");
        assert!(
            matches!(err, ShellError::UnknownTask(_)),
            "expected ShellError::UnknownTask, got: {err:?}"
        );
    }

    /// Before any `execute` call, `cwd` reports the `initial_cwd` the manager
    /// was constructed with (pre-cache fallback).
    #[test]
    fn cwd_returns_initial_before_execute() {
        let pm = ProcessManager::new(
            std::path::PathBuf::from("/tmp"),
            std::env::temp_dir().join("pattern"),
        );
        assert_eq!(pm.cwd(), Some(std::path::PathBuf::from("/tmp")));
    }

    /// After an `execute("cd <new>")`, `cwd` reflects the new working
    /// directory (the backend's post-command `pwd` refresh updated the cache).
    #[test]
    fn cwd_reflects_post_execute_state() {
        if !ensure_shell_available() {
            return;
        }
        let pm = ProcessManager::new(std::env::temp_dir(), std::env::temp_dir().join("pattern"));
        pm.execute("cd /tmp", Duration::from_secs(5))
            .expect("cd succeeds");
        let cwd = pm.cwd().expect("cwd present after execute");
        assert!(
            cwd.starts_with("/tmp"),
            "expected cwd to start with /tmp after `cd /tmp`, got: {cwd:?}"
        );
    }

    // ---- spawn_output_bridge tests ------------------------------------------

    /// `spawn_output_bridge` enqueues an `Output` chunk and then an `Exit`
    /// chunk onto the shared queue, then exits. Verify the queue contains both
    /// and the thread does not hang.
    #[test]
    fn bridge_enqueues_output_and_exit_then_exits() {
        use crossbeam_channel::unbounded;
        use pattern_core::types::message::{MessageAttachment, ShellOutputKind};

        let (tx, rx) = unbounded::<OutputChunk>();
        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let task_id = TaskId("test-bridge-01".to_string());
        spawn_output_bridge(task_id.clone(), rx, Arc::clone(&queue), None);

        // Send some output then an exit.
        tx.send(OutputChunk::Output("hello from bridge\n".to_string()))
            .unwrap();
        tx.send(OutputChunk::Exit {
            code: Some(0),
            duration_ms: 42,
        })
        .unwrap();
        // Drop tx so the bridge's rx.iter() sees the channel closed.
        drop(tx);

        // Give the bridge thread time to flush (it processes synchronously
        // before we even drop tx in the common case, but allow up to 1 s).
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let count = queue.lock().unwrap().len();
            if count >= 2 || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let entries = queue.lock().unwrap().clone();
        assert_eq!(
            entries.len(),
            2,
            "expected exactly 2 entries, got {entries:?}"
        );

        // First entry must be Output, second must be Exit.
        match &entries[0] {
            MessageAttachment::ShellOutput {
                task_id: tid,
                kind: ShellOutputKind::Output(text),
                ..
            } => {
                assert_eq!(tid, "test-bridge-01");
                assert!(text.contains("hello"), "output text mismatch: {text}");
            }
            other => panic!("expected ShellOutput(Output), got {other:?}"),
        }
        match &entries[1] {
            MessageAttachment::ShellOutput {
                task_id: tid,
                kind: ShellOutputKind::Exit { code, duration_ms },
                ..
            } => {
                assert_eq!(tid, "test-bridge-01");
                assert_eq!(*code, Some(0));
                assert_eq!(*duration_ms, 42);
            }
            other => panic!("expected ShellOutput(Exit), got {other:?}"),
        }
    }

    /// Bridge thread exits after the `Exit` chunk even when more chunks
    /// follow (it does not enqueue them). Validates the early-break logic.
    #[test]
    fn bridge_exits_after_exit_chunk_does_not_enqueue_trailing_chunks() {
        use crossbeam_channel::unbounded;
        use pattern_core::types::message::MessageAttachment;

        let (tx, rx) = unbounded::<OutputChunk>();
        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        spawn_output_bridge(
            TaskId("test-bridge-02".to_string()),
            rx,
            Arc::clone(&queue),
            None,
        );

        tx.send(OutputChunk::Exit {
            code: Some(0),
            duration_ms: 1,
        })
        .unwrap();
        // These trailing chunks arrive after the Exit; the bridge should have
        // broken out of its loop and dropped the receiver before processing them.
        // We send them *after* a brief delay to ensure the bridge had time to
        // process the Exit.
        std::thread::sleep(Duration::from_millis(50));
        // Sending after the receiver is dropped will err — that's fine.
        let _ = tx.send(OutputChunk::Output("should not appear".to_string()));
        drop(tx);

        std::thread::sleep(Duration::from_millis(50));
        let entries = queue.lock().unwrap().clone();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly 1 entry (Exit only), got {entries:?}"
        );
    }
}
