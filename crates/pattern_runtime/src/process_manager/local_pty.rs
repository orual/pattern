// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `LocalPtyBackend` — sync PTY-backed `ShellBackend` implementation.
//!
//! Ports the v2 algorithm at `rewrite-staging/runtime_subsystems/data_source/process/local_pty.rs`
//! to `pty_process::blocking` (sync) + `crossbeam_channel` + plain `std::thread`,
//! per the Phase 3 architecture decision (no tokio in the process manager).
//!
//! ## Sync read-with-timeout
//!
//! `pty_process::blocking::Pty` implements `std::io::Read` with the underlying
//! file descriptor in blocking mode. To get a bounded read deadline we use
//! `nix::poll::poll(2)` on the borrowed fd. The two callers use slightly
//! different polling strategies:
//!
//! - **`read_until_prompt`** (persistent-session reader for `execute`): each
//!   iteration computes the actual remaining deadline and polls with that as
//!   the timeout. One `poll(2)` per chunk; the kernel handles the wait. On
//!   `Ok(0)` (poll's own timeout fired), the overall deadline is exhausted —
//!   return `Timeout`. On `EINTR`, sleep 1ms then re-poll with a recomputed
//!   deadline (the brief backoff bounds a pathological signal storm at
//!   ~1000 retries/sec instead of letting the thread syscall-spin).
//!
//! - **`run_spawn_reader`** (per-spawn streaming reader): polls with a fixed
//!   100ms tick because the loop must wake periodically to check the
//!   `kill_flag` and the streaming-stall deadline. The 100ms tick IS the
//!   backoff; no extra sleep is needed on the no-data path.
//!
//! Both readers treat EOF and EIO (errno 5, returned on Linux when the PTY
//! child exits) as `SessionDied` rather than generic I/O errors. Read
//! `WouldBlock` is treated as a kernel anomaly (the fd is blocking-mode by
//! default) and surfaces as a real I/O error rather than a tight retry — the
//! latter would risk hard-spinning if the anomaly persisted.
//!
//! ## Persistent session vs streaming spawn
//!
//! - **Persistent session** (`execute`): one PTY+shell-child pair, lazily
//!   initialised on first call. Commands are written to the master end with an
//!   exit-marker echo wrapper; `read_until_prompt` consumes output until the
//!   OSC `PROMPT_MARKER` reappears. `cd`-style state persists because the
//!   shell process is the same across calls.
//!
//! - **Streaming spawn** (`spawn_streaming`): each spawn gets its own
//!   `(pty, pts)` pair and a freshly-spawned `bash -c '<cmd>'` child. A
//!   dedicated reader thread drains the PTY into a `crossbeam_channel`,
//!   sending `OutputChunk::Output(...)` per chunk and a final
//!   `OutputChunk::Exit { code, duration_ms }` when the child exits. Killed
//!   tasks observe an `Arc<AtomicBool>` flag set by `kill()` and call
//!   `child.kill()` before exiting.
//!
//! ## Timeout semantics (v2)
//!
//! `execute` on timeout sends `0x03` (Ctrl-C) into the PTY, drains output up
//! to a bounded post-kill drain timeout (`POST_KILL_DRAIN_TIMEOUT`, 1s), and
//! returns `Err(ShellError::Timeout(...))`. See `phase_03.md` Amendment
//! 2026-04-26 — backgrounding-on-timeout is deferred to a future per-execute
//! subshell architecture; agents that need long-running execution call
//! `Shell.Spawn` instead.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use dashmap::DashMap;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use pty_process::blocking::{Command, Pty};
use tracing::{debug, trace, warn};
use uuid::Uuid;

use crate::process_manager::backend::ShellBackend;
use crate::process_manager::error::ShellError;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId, TaskInfo};

/// OSC escape sequence used as the prompt marker for command-completion
/// detection. Verbatim from v2 (load-bearing — bash recognises it as a
/// no-op terminal escape and emits it before each prompt).
const PROMPT_MARKER: &str = "\x1b]pattern-done\x07";

/// Maximum wall-clock duration a streaming reader will wait between chunks
/// before declaring the spawn stalled. v2 uses the same value.
const STREAMING_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounded drain window after sending Ctrl-C on `execute` timeout. We try to
/// consume the post-kill output up to the next prompt so the persistent
/// session is ready for the next command; if the prompt doesn't return in
/// time we give up and trust `read_until_prompt`'s next attempt to
/// resynchronise.
const POST_KILL_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Per-iteration poll timeout for `run_spawn_reader`. The streaming reader
/// must wake periodically to check `kill_flag` and the streaming-stall
/// deadline; this tick is its backoff. `read_until_prompt` does NOT use this
/// constant — it polls with the actual remaining deadline, since it has no
/// external state to check.
const POLL_TICK_MS: i32 = 100;

/// Per-spawn streaming output channel capacity. Bounded to limit unbounded
/// memory growth if a chatty process outpaces its consumer; the consumer
/// (bridge thread, Task 7) is expected to drain promptly.
const SPAWN_CHANNEL_CAPACITY: usize = 64;

// ----------------------------------------------------------------------------
// Internal state types
// ----------------------------------------------------------------------------

/// State for one spawned streaming process tracked by the backend.
struct RunningProcess {
    /// Wall-clock start time of the spawn. Used to compute `elapsed_ms` in
    /// `running_tasks()`.
    started_at: Instant,
    /// OS process id of the spawned child. Reported via `TaskInfo` so agents
    /// can cross-reference with native tools (`ps`, `strace`). Not used for
    /// kill dispatch — kill goes through `kill_flag` and the reader thread's
    /// owned `Child` handle (recycle-safe).
    pid: u32,
    /// Original command string, for status reporting.
    command: String,
    /// Set to `true` by `kill()`; the spawn's reader thread observes this on
    /// each iteration and tears down the child.
    kill_flag: Arc<AtomicBool>,
}

impl std::fmt::Debug for RunningProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningProcess")
            .field("started_at", &self.started_at)
            .field("pid", &self.pid)
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

/// Persistent shell session: one PTY master + one child.
///
/// Held inside a `Mutex<Option<PtySession>>`; populated lazily on first
/// `execute` call (or `ensure_session`), reset to `None` on `SessionDied`.
struct PtySession {
    pty: Pty,
    /// Kept alive for the lifetime of the session so its file descriptors are
    /// not collected; reaped explicitly in the backend's `Drop` impl.
    child: std::process::Child,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession").finish_non_exhaustive()
    }
}

// ----------------------------------------------------------------------------
// LocalPtyBackend
// ----------------------------------------------------------------------------

/// Local-machine PTY-backed shell backend.
///
/// Owns one persistent shell session and any number of independently-PTY'd
/// streaming spawns. See module docs for sync read-with-timeout strategy and
/// timeout semantics.
#[derive(Debug)]
pub struct LocalPtyBackend {
    /// Shell binary path (e.g. `/run/current-system/sw/bin/bash` on NixOS,
    /// `/bin/bash` elsewhere). Resolved once at construction.
    shell: String,
    /// Initial working directory; passed via `Command::current_dir` at session
    /// init. Updated by `refresh_cwd` after each `execute` call into
    /// `cached_cwd`.
    initial_cwd: PathBuf,
    /// Environment variables to pass to the shell process. Empty by default.
    env: HashMap<String, String>,
    /// Whether to load shell rc files at session init. False by default for
    /// reliable prompt-marker detection (custom PS1/PROMPT_COMMAND can interfere
    /// with the OSC marker).
    load_rc: bool,
    /// Map of running streaming spawns keyed by `TaskId`. Entries self-remove
    /// when the spawn's reader thread exits (after natural EOF, kill, or
    /// stall).
    running: Arc<DashMap<TaskId, RunningProcess>>,
    /// The persistent shell session. `None` until first `execute` call.
    session: Mutex<Option<PtySession>>,
    /// Cached working directory of the persistent session, updated after each
    /// successful `execute`. `None` until at least one `refresh_cwd` succeeds.
    cached_cwd: Mutex<Option<PathBuf>>,
}

impl LocalPtyBackend {
    /// Construct a new backend rooted at `initial_cwd`. Shell is resolved via
    /// `find_default_shell` (bash-preferred). The PTY+shell are not actually
    /// allocated until the first `execute` or `spawn_streaming` call.
    pub fn new(initial_cwd: PathBuf) -> Self {
        Self {
            shell: Self::find_default_shell(),
            initial_cwd,
            env: HashMap::new(),
            load_rc: false,
            running: Arc::new(DashMap::new()),
            session: Mutex::new(None),
            cached_cwd: Mutex::new(None),
        }
    }

    /// Override the shell binary. Used by tests and consumers who need a
    /// non-default shell.
    #[must_use]
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// Replace the environment-variable map.
    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Toggle shell-rc-file loading. Default is `false` (skip rc files via
    /// `--norc --noprofile`) for reliable prompt detection.
    #[must_use]
    pub fn with_load_rc(mut self, load: bool) -> Self {
        self.load_rc = load;
        self
    }

    /// Locate a usable shell binary, bash-preferred.
    ///
    /// Probes in order: `command -v bash` (NixOS-friendly), `/bin/bash`,
    /// `/usr/bin/bash`, `/bin/sh`, `/usr/bin/sh`, `$SHELL`, then a literal
    /// `"bash"` last-resort string (relying on PATH at exec time).
    ///
    /// Public so callers can probe shell availability without constructing a
    /// full backend (e.g. test fixtures that want to skip on shell-less CI
    /// images).
    pub fn find_default_shell() -> String {
        if let Ok(output) = std::process::Command::new("sh")
            .args(["-c", "command -v bash"])
            .output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() && std::path::Path::new(&path).exists() {
                return path;
            }
        }
        for path in ["/bin/bash", "/usr/bin/bash"] {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }
        for path in ["/bin/sh", "/usr/bin/sh"] {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }
        if let Ok(shell) = std::env::var("SHELL")
            && std::path::Path::new(&shell).exists()
        {
            return shell;
        }
        "bash".to_string()
    }

    /// Generate a unique exit-marker nonce that command output cannot
    /// reasonably collide with. Format: `__PATTERN_EXIT_<8-char-uuid>__`.
    pub(crate) fn generate_exit_marker() -> String {
        let nonce = &Uuid::new_v4().to_string()[..8];
        format!("__PATTERN_EXIT_{nonce}__")
    }

    /// Strip ANSI escape sequences from output. Wrapper around the
    /// `strip-ansi-escapes` crate.
    fn strip_ansi(input: &str) -> String {
        String::from_utf8_lossy(&strip_ansi_escapes::strip(input)).to_string()
    }

    /// Parse the trailing exit-marker echo emitted by the wrapped command.
    ///
    /// Searches for the LAST occurrence of `<marker>:` in the output (in case
    /// the command itself echoed something marker-like — the actual marker is
    /// nonce-based per call so spurious matches are improbable but not
    /// impossible). Returns the cleaned output (everything before the marker)
    /// and the parsed exit code.
    pub(crate) fn parse_exit_code(output: &str, marker: &str) -> Result<(String, i32), ShellError> {
        let search_pattern = format!("{marker}:");
        let Some(marker_pos) = output.rfind(&search_pattern) else {
            return Err(ShellError::ExitCodeParseFailed);
        };
        let before_marker = &output[..marker_pos];
        let after_marker = &output[marker_pos + search_pattern.len()..];
        let exit_code_str: String = after_marker
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        let exit_code = exit_code_str
            .parse::<i32>()
            .map_err(|_| ShellError::ExitCodeParseFailed)?;
        let cleaned = before_marker.trim_end().to_string();
        Ok((cleaned, exit_code))
    }

    /// Initialise the persistent PTY session if not already initialised.
    ///
    /// Acquires the session mutex, allocates a `(pty, pts)` pair, spawns
    /// `<shell> --norc --noprofile` (or with rc files if `load_rc` is true)
    /// with `PS1=PROMPT_MARKER` and `PS2=""`, then drops the lock and reads
    /// until the first prompt appears (5s budget).
    fn ensure_session(&self) -> Result<(), ShellError> {
        {
            let guard = self.session.lock().unwrap();
            if guard.is_some() {
                return Ok(());
            }
        }

        debug!(shell = %self.shell, cwd = ?self.initial_cwd, "initializing PTY session");

        let (pty, pts) =
            pty_process::blocking::open().map_err(|e| ShellError::PtyError(e.to_string()))?;
        pty.resize(pty_process::Size::new(24, 120))
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        let mut cmd = Command::new(&self.shell);
        if !self.load_rc {
            cmd = cmd.args(["--norc", "--noprofile"]);
        }
        cmd = cmd.current_dir(&self.initial_cwd);
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }
        cmd = cmd.env("PS1", PROMPT_MARKER);
        cmd = cmd.env("PS2", "");

        let child = cmd
            .spawn(pts)
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        {
            let mut guard = self.session.lock().unwrap();
            *guard = Some(PtySession { pty, child });
        }

        // Consume the initial prompt so subsequent `execute` calls see a clean
        // state. 5s is plenty even on a cold cache.
        self.read_until_prompt(Duration::from_secs(5))?;
        debug!("PTY session initialized");
        Ok(())
    }

    /// Read from the persistent PTY until `PROMPT_MARKER` appears or `timeout`
    /// elapses. Output is ANSI-stripped before return.
    ///
    /// One `poll(2)` per chunk: each iteration computes the actual remaining
    /// deadline and asks the kernel to wake on data-available or after the
    /// remaining budget expires. EINTR loops without consuming wall-clock
    /// budget (re-polls with a recomputed deadline). EOF and EIO (errno 5,
    /// returned on Linux when the PTY child exits) both surface as
    /// `SessionDied`. Other I/O errors propagate via `ShellError::Io`.
    ///
    /// The session mutex is held for one poll-then-read cycle at a time,
    /// released between iterations. The persistent session is intrinsically
    /// single-stream — only one `execute` runs at a time per session — so the
    /// mutex is uncontended in practice; the cycle-by-cycle release exists
    /// only to keep the lock-acquisition pattern uniform with the rest of
    /// `LocalPtyBackend` (where `interrupt_and_drain`, `refresh_cwd`, and
    /// `reinitialize_session` all assume they can acquire the lock without
    /// reentrancy from a concurrent `read_until_prompt`).
    fn read_until_prompt(&self, timeout: Duration) -> Result<String, ShellError> {
        let deadline = Instant::now() + timeout;
        let mut output = String::new();

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(ShellError::Timeout(timeout));
            }
            let remaining = deadline.saturating_duration_since(now);
            let poll_ms =
                i32::try_from(remaining.as_millis().min(i32::MAX as u128)).unwrap_or(i32::MAX);

            let chunk_result = {
                let mut guard = self.session.lock().unwrap();
                let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

                let pollfd = PollFd::new(session.pty.as_fd(), PollFlags::POLLIN);
                let mut fds = [pollfd];
                let timeout_obj = PollTimeout::try_from(poll_ms).unwrap_or(PollTimeout::ZERO);
                match poll(&mut fds, timeout_obj) {
                    // poll's own timeout fired — overall deadline exhausted.
                    Ok(0) => return Err(ShellError::Timeout(timeout)),
                    Ok(_) => {
                        let mut buf = [0u8; 4096];
                        match session.pty.read(&mut buf) {
                            Ok(0) => PollOutcome::Eof,
                            Ok(n) => {
                                PollOutcome::Data(String::from_utf8_lossy(&buf[..n]).to_string())
                            }
                            Err(e) if e.raw_os_error() == Some(5) => PollOutcome::Eof,
                            // `pty_process::blocking::Pty` uses a blocking fd by
                            // default — WouldBlock from `read` would be a kernel
                            // anomaly. Treat as a real I/O error rather than a
                            // tight retry; surfaces to the caller instead of
                            // hard-spinning.
                            Err(e) => PollOutcome::Io(e),
                        }
                    }
                    // EINTR: signal interrupted poll. Re-loop with a recomputed
                    // deadline (the top-of-loop check honours that). The 1ms
                    // backoff caps a pathological signal storm at ~1000 retry
                    // iterations per second instead of letting the thread
                    // syscall-spin at full speed; signal latency in this code
                    // path is not time-sensitive.
                    Err(Errno::EINTR) => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => PollOutcome::PollError(e),
                }
            };

            match chunk_result {
                PollOutcome::Data(chunk) => {
                    trace!(chunk_len = chunk.len(), "read chunk from PTY");
                    output.push_str(&chunk);
                    if let Some(pos) = output.find(PROMPT_MARKER) {
                        output.truncate(pos);
                        return Ok(Self::strip_ansi(&output));
                    }
                }
                PollOutcome::Eof => return Err(ShellError::SessionDied),
                PollOutcome::Io(e) => return Err(ShellError::Io(e)),
                PollOutcome::PollError(e) => {
                    return Err(ShellError::PtyError(format!("poll failed: {e}")));
                }
            }
        }
    }

    /// Read from the persistent PTY until the exit marker appears or `timeout`
    /// expires. Unlike `read_until_prompt`, this scans for a per-call nonce
    /// marker rather than `PROMPT_MARKER`, allowing newline-separated command
    /// + echo to work (heredocs, multi-line constructs).
    ///
    /// After finding the marker, continues reading until `PROMPT_MARKER` to
    /// drain the shell's prompt so it doesn't leak into the next operation.
    fn read_until_exit_marker(
        &self,
        marker: &str,
        timeout: Duration,
    ) -> Result<String, ShellError> {
        let deadline = Instant::now() + timeout;
        let mut output = String::new();
        let search_pattern = format!("{marker}:");

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(ShellError::Timeout(timeout));
            }
            let remaining = deadline.saturating_duration_since(now);
            let poll_ms =
                i32::try_from(remaining.as_millis().min(i32::MAX as u128)).unwrap_or(i32::MAX);

            let chunk_result = {
                let mut guard = self.session.lock().unwrap();
                let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

                let pollfd = PollFd::new(session.pty.as_fd(), PollFlags::POLLIN);
                let mut fds = [pollfd];
                let timeout_obj = PollTimeout::try_from(poll_ms).unwrap_or(PollTimeout::ZERO);
                match poll(&mut fds, timeout_obj) {
                    Ok(0) => return Err(ShellError::Timeout(timeout)),
                    Ok(_) => {
                        let mut buf = [0u8; 4096];
                        match session.pty.read(&mut buf) {
                            Ok(0) => PollOutcome::Eof,
                            Ok(n) => {
                                PollOutcome::Data(String::from_utf8_lossy(&buf[..n]).to_string())
                            }
                            Err(e) if e.raw_os_error() == Some(5) => PollOutcome::Eof,
                            Err(e) => PollOutcome::Io(e),
                        }
                    }
                    Err(Errno::EINTR) => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => PollOutcome::PollError(e),
                }
            };

            match chunk_result {
                PollOutcome::Data(chunk) => {
                    trace!(chunk_len = chunk.len(), "read chunk from PTY (marker mode)");
                    output.push_str(&chunk);
                    // Debug: log what we're seeing
                    let has_marker = output.contains(&search_pattern);
                    let has_prompt = output.contains(PROMPT_MARKER);
                    eprintln!(
                        "[read_until_exit_marker] chunk=`{}` output=`{:?}` len={} has_marker={} has_prompt={}",
                        chunk,
                        output.as_bytes(),
                        output.len(),
                        has_marker,
                        has_prompt
                    );
                    if has_marker && has_prompt {
                        let stripped = Self::strip_ansi(&output);
                        return Ok(stripped);
                    }
                }
                PollOutcome::Eof => return Err(ShellError::SessionDied),
                PollOutcome::Io(e) => return Err(ShellError::Io(e)),
                PollOutcome::PollError(e) => {
                    return Err(ShellError::PtyError(format!("poll failed: {e}")));
                }
            }
        }
    }

    /// Drop and re-create the persistent session after a `SessionDied`.
    fn reinitialize_session(&self) -> Result<(), ShellError> {
        {
            let mut guard = self.session.lock().unwrap();
            *guard = None;
        }
        {
            let mut cwd_guard = self.cached_cwd.lock().unwrap();
            *cwd_guard = None;
        }
        self.ensure_session()
    }

    /// Query the persistent shell for its working directory and update the
    /// cache. Best-effort — failure is logged and swallowed by callers.
    fn refresh_cwd(&self) -> Result<PathBuf, ShellError> {
        {
            let mut guard = self.session.lock().unwrap();
            let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;
            session.pty.write_all(b"pwd\n").map_err(ShellError::Io)?;
        }

        let raw_output = self.read_until_prompt(Duration::from_secs(5))?;

        // Output looks like "pwd\n/actual/path\n" — pick the absolute path line.
        let path_str = raw_output
            .lines()
            .find(|line| line.starts_with('/') && !line.contains("pwd"))
            .unwrap_or_else(|| raw_output.trim());
        let cwd = PathBuf::from(path_str.trim());

        {
            let mut cwd_guard = self.cached_cwd.lock().unwrap();
            *cwd_guard = Some(cwd.clone());
        }

        trace!(cwd = ?cwd, "refreshed cached cwd");
        Ok(cwd)
    }

    /// Send Ctrl-C into the persistent PTY and drain output up to the next
    /// prompt. Best-effort — used after `execute` timeout to clear the line so
    /// the next command can run cleanly.
    fn interrupt_and_drain(&self) {
        if let Ok(mut guard) = self.session.lock()
            && let Some(session) = guard.as_mut()
        {
            let _ = session.pty.write_all(&[0x03]); // Ctrl-C.
            let _ = session.pty.flush();
        }
        // Drain to next prompt; ignore the result (we already know the call
        // timed out).
        let _ = self.read_until_prompt(POST_KILL_DRAIN_TIMEOUT);
    }

    /// Run the per-spawn reader loop. Drains output from the spawn's PTY into
    /// the crossbeam channel, removes the entry from `running` once the child
    /// has exited, and finally sends an `Exit` chunk.
    ///
    /// The remove-before-send order is the load-bearing invariant of the
    /// streaming surface: by the time a consumer observes `OutputChunk::Exit`
    /// on the receiver, the task ID is guaranteed to no longer appear in
    /// `running_tasks()`. Callers that want to assert "task is fully gone"
    /// can rely on that ordering without polling.
    fn run_spawn_reader(
        task_id: TaskId,
        mut pty: Pty,
        mut child: std::process::Child,
        tx: Sender<OutputChunk>,
        kill_flag: Arc<AtomicBool>,
        running: Arc<DashMap<TaskId, RunningProcess>>,
    ) {
        let start = Instant::now();
        let mut last_data_at = start;
        let mut buf = [0u8; 4096];
        let mut killed = false;
        let mut stalled = false;

        loop {
            if kill_flag.load(Ordering::SeqCst) {
                killed = true;
                break;
            }
            if last_data_at.elapsed() > STREAMING_READ_TIMEOUT {
                warn!(
                    task_id = %task_id,
                    elapsed = ?last_data_at.elapsed(),
                    "streaming read stalled; aborting spawn"
                );
                let _ = tx.send(OutputChunk::Output(format!(
                    "[timeout: no output for {STREAMING_READ_TIMEOUT:?}]\n"
                )));
                stalled = true;
                break;
            }

            let pollfd = PollFd::new(pty.as_fd(), PollFlags::POLLIN);
            let mut fds = [pollfd];
            let timeout_obj = PollTimeout::try_from(POLL_TICK_MS).unwrap_or(PollTimeout::ZERO);
            match poll(&mut fds, timeout_obj) {
                // 100ms tick with no data — re-loop to check kill_flag and
                // stall deadline. The 100ms timeout is the natural backoff:
                // we cannot hard-spin here.
                Ok(0) => continue,
                Ok(_) => {
                    match pty.read(&mut buf) {
                        Ok(0) => break, // EOF — child closed the slave.
                        Ok(n) => {
                            last_data_at = Instant::now();
                            let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                            let clean = String::from_utf8_lossy(&strip_ansi_escapes::strip(&raw))
                                .to_string();
                            if tx.send(OutputChunk::Output(clean)).is_err() {
                                // Receiver dropped — caller no longer cares.
                                break;
                            }
                        }
                        Err(e) if e.raw_os_error() == Some(5) => break, // EIO.
                        // Blocking-fd anomaly: surface as a warn + break rather
                        // than tight-retry. See `read_until_prompt` for the
                        // matching rationale.
                        Err(e) => {
                            warn!(error = %e, task_id = %task_id, "spawn read error");
                            break;
                        }
                    }
                }
                // Signal interrupted poll. Re-loop after a 1ms backoff so a
                // pathological signal storm can't peg the CPU between
                // kill_flag / stall-deadline checks.
                Err(Errno::EINTR) => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, task_id = %task_id, "spawn poll error");
                    break;
                }
            }
        }

        if (killed || stalled)
            && let Err(e) = child.kill()
        {
            warn!(error = %e, task_id = %task_id, "failed to kill spawn child");
        }
        let status = child.wait();
        let exit_code = status.ok().and_then(|s| s.code());
        let duration_ms = start.elapsed().as_millis() as u64;
        // Remove from the running map BEFORE sending Exit so consumers that
        // observe Exit on the channel can rely on `running_tasks()` no longer
        // listing this task.
        running.remove(&task_id);
        let _ = tx.send(OutputChunk::Exit {
            code: exit_code,
            duration_ms,
        });
        debug!(task_id = %task_id, ?exit_code, "spawned process completed");
    }
}

/// Result of a single poll-then-read cycle in `read_until_prompt`, lifted out
/// of the lock-holding inner block so the outer match can drop the session
/// mutex before taking different actions per branch (e.g. propagating an
/// error, parsing the prompt marker).
enum PollOutcome {
    Data(String),
    Eof,
    Io(std::io::Error),
    PollError(Errno),
}

// ----------------------------------------------------------------------------
// ShellBackend impl
// ----------------------------------------------------------------------------

impl ShellBackend for LocalPtyBackend {
    fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError> {
        self.ensure_session()?;
        let start = Instant::now();
        let exit_marker = Self::generate_exit_marker();
        let wrapped_command = format!("{command}; echo \"{exit_marker}:$?\"");

        debug!(command = %command, ?timeout, "executing command");

        {
            let mut guard = self.session.lock().unwrap();
            let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;
            let cmd_line = format!("{wrapped_command}\n");
            session
                .pty
                .write_all(cmd_line.as_bytes())
                .map_err(ShellError::Io)?;
        }

        let raw_output = match self.read_until_prompt(timeout) {
            Ok(output) => output,
            Err(ShellError::Timeout(t)) => {
                warn!(
                    ?t,
                    "shell execute timed out; sending SIGINT to running command"
                );
                self.interrupt_and_drain();
                return Err(ShellError::Timeout(t));
            }
            Err(ShellError::SessionDied) => {
                warn!("shell session died during execute; reinitializing");
                let _ = self.reinitialize_session();
                return Err(ShellError::SessionDied);
            }
            Err(e) => return Err(e),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        // Strip the echoed wrapped command from the start so the agent sees
        // only the actual command output.
        let output_after_echo = raw_output
            .strip_prefix(&wrapped_command)
            .unwrap_or(&raw_output)
            .trim_start_matches('\n')
            .trim_start_matches('\r');

        let (output, exit_code) = match Self::parse_exit_code(output_after_echo, &exit_marker) {
            Ok(pair) => pair,
            Err(e @ ShellError::ExitCodeParseFailed) => {
                // The session is in an unknown state (e.g. a heredoc left the
                // shell waiting for a delimiter). Reinitialize so subsequent
                // commands don't fail too.
                warn!("exit-code parse failed; reinitializing shell session");
                let _ = self.reinitialize_session();
                return Err(e);
            }
            Err(e) => return Err(e),
        };

        if let Err(e) = self.refresh_cwd() {
            warn!(error = %e, "failed to refresh cwd after command");
        }

        Ok(ExecuteResult {
            output,
            exit_code: Some(exit_code),
            duration_ms,
            backgrounded_as: None,
        })
    }

    fn spawn_streaming(
        &self,
        command: &str,
    ) -> Result<(TaskId, u32, Receiver<OutputChunk>), ShellError> {
        let task_id = TaskId::new();
        let (tx, rx) = bounded(SPAWN_CHANNEL_CAPACITY);
        let kill_flag = Arc::new(AtomicBool::new(false));

        debug!(task_id = %task_id, command = %command, "spawning streaming process");

        let (pty, pts) =
            pty_process::blocking::open().map_err(|e| ShellError::PtyError(e.to_string()))?;
        let mut cmd = Command::new(&self.shell);
        cmd = cmd.current_dir(&self.initial_cwd);
        cmd = cmd.args(["-c", command]);
        for (k, v) in &self.env {
            cmd = cmd.env(k, v);
        }
        let child = cmd
            .spawn(pts)
            .map_err(|e| ShellError::PtyError(e.to_string()))?;
        let pid = child.id();

        // Insert into the running map BEFORE spawning the reader thread so the
        // self-removal in the reader (when the spawn finishes naturally) is
        // guaranteed to find the entry.
        self.running.insert(
            task_id.clone(),
            RunningProcess {
                started_at: Instant::now(),
                pid,
                command: command.to_string(),
                kill_flag: Arc::clone(&kill_flag),
            },
        );

        let running = Arc::clone(&self.running);
        let task_id_for_thread = task_id.clone();
        let thread_result = thread::Builder::new()
            .name(format!("shell-spawn-reader:{task_id_for_thread}"))
            .spawn(move || {
                Self::run_spawn_reader(task_id_for_thread, pty, child, tx, kill_flag, running);
            });

        if let Err(e) = thread_result {
            // Roll back the insert if the thread failed to spawn.
            self.running.remove(&task_id);
            return Err(ShellError::PtyError(format!(
                "failed to spawn reader thread: {e}"
            )));
        }

        Ok((task_id, pid, rx))
    }

    fn kill(&self, task_id: &TaskId) -> Result<(), ShellError> {
        if let Some((_, process)) = self.running.remove(task_id) {
            process.kill_flag.store(true, Ordering::SeqCst);
            // Reader thread observes the flag, calls child.kill(), sends the
            // final Exit chunk, exits. We do not block on join — the receiver
            // is the synchronisation surface.
            debug!(task_id = %task_id, "set kill flag for spawned process");
            Ok(())
        } else {
            Err(ShellError::UnknownTask(task_id.to_string()))
        }
    }

    fn running_tasks(&self) -> Vec<TaskInfo> {
        let now = Instant::now();
        self.running
            .iter()
            .map(|r| {
                let proc = r.value();
                TaskInfo {
                    task_id: r.key().clone(),
                    pid: proc.pid,
                    command: proc.command.clone(),
                    elapsed_ms: now.saturating_duration_since(proc.started_at).as_millis() as u64,
                }
            })
            .collect()
    }

    fn cwd(&self) -> Option<PathBuf> {
        let cached = self.cached_cwd.lock().unwrap();
        cached.clone().or_else(|| Some(self.initial_cwd.clone()))
    }
}

// ----------------------------------------------------------------------------
// Drop
// ----------------------------------------------------------------------------

impl Drop for LocalPtyBackend {
    fn drop(&mut self) {
        // Signal all spawned reader threads to exit. They will call
        // `child.kill()` themselves and emit a final `Exit` chunk.
        for entry in self.running.iter() {
            entry.value().kill_flag.store(true, Ordering::SeqCst);
        }

        // Reap the persistent shell child explicitly. `std::process::Child` does
        // NOT kill its child on drop (unlike `tokio::process::Child`); without
        // this, a long-running foreground command in the persistent session
        // would keep the shell alive as a zombie.
        if let Ok(mut guard) = self.session.lock()
            && let Some(session) = guard.take()
        {
            let PtySession { pty, mut child } = session;
            let _ = child.kill();
            drop(pty);
            let _ = child.wait();
        }
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Test guard: skips the test if no usable shell is available. CI runners
    /// without bash/sh on PATH (e.g. minimal containers) hit this; NixOS
    /// devshell and standard Linux always have bash.
    fn ensure_shell_available() -> bool {
        let shell = LocalPtyBackend::find_default_shell();
        std::path::Path::new(&shell).exists() || shell == "bash"
    }

    fn temp_cwd() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn execute_simple_command_returns_output_and_exit_code() {
        if !ensure_shell_available() {
            eprintln!("skipping: no shell found");
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let result = backend
            .execute("echo hello", Duration::from_secs(5))
            .expect("execute succeeds");
        assert!(
            result.output.contains("hello"),
            "expected 'hello' in output, got: {:?}",
            result.output
        );
        assert_eq!(result.exit_code, Some(0));
        assert!(result.backgrounded_as.is_none());
    }

    #[test]
    fn execute_nonzero_exit_propagates() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let result = backend
            .execute("false", Duration::from_secs(5))
            .expect("execute succeeds");
        assert_eq!(result.exit_code, Some(1));
    }

    #[test]
    fn execute_cwd_persists_across_calls() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let _ = backend
            .execute("cd /tmp", Duration::from_secs(5))
            .expect("cd succeeds");
        let pwd = backend
            .execute("pwd", Duration::from_secs(5))
            .expect("pwd succeeds");
        assert!(
            pwd.output.contains("/tmp"),
            "expected '/tmp' in pwd output, got: {:?}",
            pwd.output
        );
    }

    #[test]
    fn execute_timeout_kills_and_returns_timeout_error() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let start = Instant::now();
        let result = backend.execute("sleep 5", Duration::from_millis(500));
        let elapsed = start.elapsed();
        assert!(matches!(result, Err(ShellError::Timeout(_))));
        // Should return roughly within the timeout + post-kill drain budget.
        assert!(
            elapsed < Duration::from_secs(3),
            "execute took too long after timeout: {elapsed:?}"
        );
        // Subsequent execute should work cleanly — interrupt_and_drain
        // resynchronises the session.
        let next = backend
            .execute("echo recovered", Duration::from_secs(5))
            .expect("execute after timeout succeeds");
        assert!(next.output.contains("recovered"));
    }

    #[test]
    fn exit_marker_resists_collision_in_command_output() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        // The command echoes a fake-marker-looking string that should NOT be
        // confused with our nonce-based marker.
        let result = backend
            .execute(
                "echo '__PATTERN_EXIT_deadbeef__:1'; true",
                Duration::from_secs(5),
            )
            .expect("execute succeeds");
        // The command exits 0 (`true`); the spurious string in output should
        // not be parsed as the exit code.
        assert_eq!(result.exit_code, Some(0));
        assert!(
            result.output.contains("__PATTERN_EXIT_deadbeef__:1"),
            "spurious marker should appear verbatim in output, got: {:?}",
            result.output
        );
    }

    #[test]
    fn parse_exit_code_finds_last_marker_occurrence() {
        let marker = "__PATTERN_EXIT_abc12345__";
        let output = format!("echo {marker}:1\n{marker}:1\nreal command output\n{marker}:42\n");
        let (cleaned, code) =
            LocalPtyBackend::parse_exit_code(&output, marker).expect("parse succeeds");
        assert_eq!(code, 42);
        assert!(cleaned.contains("real command output"));
    }

    #[test]
    fn spawn_streams_output_chunks_and_exit() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let (task_id, _pid, rx) = backend
            .spawn_streaming("for i in 1 2 3; do echo line$i; done")
            .expect("spawn succeeds");

        // Drain the receiver with a per-chunk timeout. Collect all Output
        // chunks and the terminating Exit chunk.
        let mut output_chunks = Vec::new();
        let mut exit_chunk = None;
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(OutputChunk::Output(s)) => output_chunks.push(s),
                Ok(OutputChunk::Exit { code, .. }) => {
                    exit_chunk = Some(code);
                    break;
                }
                Err(_) => break,
            }
        }
        let combined: String = output_chunks.join("");
        for line in &["line1", "line2", "line3"] {
            assert!(
                combined.contains(line),
                "expected {line} in combined output, got: {combined:?}"
            );
        }
        assert_eq!(exit_chunk, Some(Some(0)));
        // After exit, the running map should not list the task.
        assert!(
            !backend
                .running_tasks()
                .iter()
                .any(|info| info.task_id == task_id)
        );
    }

    #[test]
    fn kill_terminates_running_spawn() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let (task_id, _pid, rx) = backend.spawn_streaming("sleep 60").expect("spawn succeeds");
        backend.kill(&task_id).expect("kill succeeds");

        // Drain until we see Exit. Killed tasks may have non-zero or None exit
        // codes; we only assert that we get to Exit within a reasonable time.
        let mut got_exit = false;
        for _ in 0..50 {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(OutputChunk::Exit { .. }) => {
                    got_exit = true;
                    break;
                }
                Ok(OutputChunk::Output(_)) => {}
                Err(_) => break,
            }
        }
        assert!(got_exit, "expected Exit chunk after kill within 10s");
    }

    #[test]
    fn kill_unknown_task_returns_error() {
        let backend = LocalPtyBackend::new(temp_cwd());
        let bogus = TaskId("not-a-real-id".to_string());
        let result = backend.kill(&bogus);
        assert!(matches!(result, Err(ShellError::UnknownTask(_))));
    }

    #[test]
    fn cwd_returns_initial_before_first_execute() {
        let backend = LocalPtyBackend::new(PathBuf::from("/tmp"));
        assert_eq!(backend.cwd(), Some(PathBuf::from("/tmp")));
    }

    // ── v2 ports ──────────────────────────────────────────────────────────────
    //
    // The following tests are ported from
    // `rewrite-staging/runtime_subsystems/data_source/process/tests.rs`
    // (v2 async → v3 sync API), covering distinct behaviour not already
    // tested by the 10 tests above.

    /// Multi-line output: commands separated by `;` both appear in output.
    /// (v2: `test_local_pty_execute_multiline`)
    #[test]
    fn execute_multiline_output_contains_all_lines() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let result = backend
            .execute("echo line1; echo line2", Duration::from_secs(5))
            .expect("execute succeeds");
        assert!(
            result.output.contains("line1"),
            "expected 'line1' in output, got: {:?}",
            result.output
        );
        assert!(
            result.output.contains("line2"),
            "expected 'line2' in output, got: {:?}",
            result.output
        );
    }

    /// Custom exit code via subshell: `(exit 42)` must exit 42 without killing
    /// the persistent session (`exit 42` without the subshell would).
    /// (v2: `test_local_pty_exit_code_custom`)
    #[test]
    fn execute_custom_exit_code_via_subshell() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        let result = backend
            .execute("(exit 42)", Duration::from_secs(5))
            .expect("execute succeeds");
        assert_eq!(
            result.exit_code,
            Some(42),
            "expected exit_code Some(42), got: {:?}",
            result.exit_code
        );
        // Session must still be usable — the subshell exited, not the parent.
        let next = backend
            .execute("echo session-alive", Duration::from_secs(5))
            .expect("session must survive subshell exit");
        assert!(
            next.output.contains("session-alive"),
            "session must remain alive after subshell exit"
        );
    }

    /// Running `exit <N>` kills the persistent session: the PTY child exits,
    /// and the next read returns `SessionDied`. This is distinct from
    /// `(exit N)` in a subshell above.
    /// (v2: `test_local_pty_exit_kills_session`)
    #[test]
    fn exit_command_kills_session() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        // Prime the session with a successful command first.
        backend
            .execute("echo ready", Duration::from_secs(5))
            .expect("initial execute succeeds");
        // Now kill the session shell.
        let result = backend.execute("exit 42", Duration::from_secs(5));
        assert!(
            matches!(
                result,
                Err(ShellError::SessionDied) | Err(ShellError::Timeout(_))
            ),
            "expected SessionDied or Timeout after 'exit 42', got: {result:?}"
        );
    }

    /// Environment variables set in one `execute` call persist in subsequent
    /// calls through the same persistent session.
    /// (v2: `test_local_pty_env_persistence`)
    #[test]
    fn execute_env_export_persists_across_calls() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());
        // Set the variable.
        backend
            .execute(
                "export PATTERN_TEST_VAR=hello_from_v3",
                Duration::from_secs(5),
            )
            .expect("export succeeds");
        // Read it back in a subsequent call.
        let result = backend
            .execute("echo $PATTERN_TEST_VAR", Duration::from_secs(5))
            .expect("echo succeeds");
        assert!(
            result.output.contains("hello_from_v3"),
            "exported variable must persist in next call; got: {:?}",
            result.output
        );
    }

    /// Two independent `LocalPtyBackend` instances have isolated shell
    /// sessions — setting an env var in one does NOT affect the other.
    /// (v2: `test_multiple_backends_isolated`)
    #[test]
    fn two_backends_are_isolated_from_each_other() {
        if !ensure_shell_available() {
            return;
        }
        let b1 = LocalPtyBackend::new(temp_cwd());
        let b2 = LocalPtyBackend::new(temp_cwd());

        // Set a unique variable in backend1.
        b1.execute("export ISOLATED_BACKEND1=b1_value", Duration::from_secs(5))
            .expect("export in backend1 succeeds");

        // Backend2 must NOT see the variable (different shell process).
        let result = b2
            .execute("echo ${ISOLATED_BACKEND1:-unset}", Duration::from_secs(5))
            .expect("echo in backend2 succeeds");
        assert!(
            !result.output.contains("b1_value"),
            "backend2 must not see backend1's variable; got: {:?}",
            result.output
        );

        // Backend1 must still have it.
        let check = b1
            .execute("echo $ISOLATED_BACKEND1", Duration::from_secs(5))
            .expect("echo in backend1 succeeds");
        assert!(
            check.output.contains("b1_value"),
            "backend1 must still have its variable; got: {:?}",
            check.output
        );
    }

    /// `with_env` injects environment variables into the session at init time.
    /// The backend builder accepts the map; the variable is accessible in the
    /// persistent shell.
    #[test]
    fn with_env_injects_variables_into_session() {
        if !ensure_shell_available() {
            return;
        }
        let mut env = std::collections::HashMap::new();
        env.insert(
            "PATTERN_INJECTED_VAR".to_string(),
            "injected_value".to_string(),
        );
        let backend = LocalPtyBackend::new(temp_cwd()).with_env(env);

        let result = backend
            .execute("echo $PATTERN_INJECTED_VAR", Duration::from_secs(5))
            .expect("execute succeeds");
        assert!(
            result.output.contains("injected_value"),
            "injected env var must be visible in session; got: {:?}",
            result.output
        );
    }

    /// `with_load_rc(true/false)` controls whether `--norc --noprofile` args
    /// are passed at session init. The default `false` skips rc files so
    /// `PS1 = PROMPT_MARKER` is never overridden. `true` loads rc files;
    /// because those often redefine PS1, execution is unreliable and is NOT
    /// recommended for production use — but the builder must not panic.
    ///
    /// This test only verifies that the builder compiles and constructs the
    /// backend without panicking. Execution with `load_rc=true` is intentionally
    /// NOT tested because `.bashrc`/`/etc/bash.bashrc` on most systems redefines
    /// PS1, breaking the OSC prompt marker that `read_until_prompt` depends on
    /// — timeout is the expected outcome, which is exactly why `load_rc=false`
    /// is the safe default.
    #[test]
    fn with_load_rc_builder_constructs_without_panic() {
        // load_rc=false (default): matches the production path.
        let b_default = LocalPtyBackend::new(temp_cwd());
        let _ = b_default; // just verify construction

        // load_rc=true: builder should not panic even though execution is
        // unreliable in most environments.
        let b_rc = LocalPtyBackend::new(temp_cwd()).with_load_rc(true);
        let _ = b_rc; // just verify construction; do NOT call execute()
    }

    /// `find_default_shell` returns a path that exists on disk OR the literal
    /// `"bash"` last-resort string. On NixOS devshell and standard Linux CI,
    /// it must return an absolute path to a real bash or sh binary.
    #[test]
    fn find_default_shell_returns_executable_path() {
        let shell = LocalPtyBackend::find_default_shell();
        // Either an absolute path to a real binary or the last-resort literal.
        if shell != "bash" {
            assert!(
                std::path::Path::new(&shell).exists(),
                "find_default_shell returned non-existent path: {shell:?}"
            );
        }
        // Must at least start with '/' (absolute path) or equal "bash".
        assert!(
            shell.starts_with('/') || shell == "bash",
            "expected absolute path or 'bash', got: {shell:?}"
        );
    }

    /// `cwd()` is updated after a `cd` + subsequent command because
    /// `execute` calls `refresh_cwd()` after each successful command.
    /// This port of v2's `test_local_pty_cwd_cached_after_cd` validates
    /// the cache-update path in more detail than the existing
    /// `execute_cwd_persists_across_calls` (which only checks `pwd` output).
    #[test]
    fn cwd_cache_reflects_post_cd_state() {
        if !ensure_shell_available() {
            return;
        }
        let backend = LocalPtyBackend::new(temp_cwd());

        // Before any execute, cwd returns initial_cwd.
        let initial = backend.cwd().expect("cwd present before execute");

        // Issue a cd and an echo (the echo triggers refresh_cwd).
        let test_subdir = format!("pattern_cwd_test_{}", std::process::id());
        backend
            .execute(
                &format!("mkdir -p /tmp/{test_subdir} && cd /tmp/{test_subdir}"),
                Duration::from_secs(5),
            )
            .expect("mkdir+cd must succeed");

        // The cached cwd should now reflect the new directory.
        let new_cwd = backend.cwd().expect("cwd present after cd");
        assert!(
            new_cwd.to_string_lossy().contains(&test_subdir),
            "expected cwd to contain '{test_subdir}' after cd, got: {new_cwd:?}"
        );
        assert_ne!(
            initial, new_cwd,
            "cwd must differ from initial after cd into subdir"
        );

        // Cleanup.
        let _ = backend.execute(
            &format!("cd /tmp && rmdir /tmp/{test_subdir}"),
            Duration::from_secs(5),
        );
    }
}
