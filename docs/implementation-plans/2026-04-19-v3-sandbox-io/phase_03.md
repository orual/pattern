# Phase 3: Shell handler + ProcessManager

**Goal:** Replace the `ShellHandler` stub with a real implementation dispatching into a runtime-global `ProcessManager` coordinator. ProcessManager owns a map of `ShellSession` instances (each wrapping a persistent PTY-backed shell with OSC prompt markers for exit-code detection). Operations: `Execute` (sync, returns output + exit code), `Spawn` (async, output via system reminders), `Kill`, `Status`. Process output also written to a per-session log file as a reliability backstop.

**Architecture:** `ShellHandler<SessionContext>` (tightened from the stub's `HasCancelState` — matches `SkillsHandler` and Phase 2's `FileHandler`) dispatches `ShellReq` to `cx.user().process_manager()`. ProcessManager is `Arc<ProcessManager>` held on the `TidepoolRuntime` (one per runtime instance, shared across sessions via Arc — different from Phase 2's per-session FileManager because shell sessions have global semantics: an agent shouldn't lose its bash session across pattern session boundaries).

**ProcessManager is sync at every layer; no tokio.** Each `ShellSession` is a dedicated OS thread (`std::thread::spawn`) owning its `pty_process::Pty` via the crate's sync API. The handler dispatches via `crossbeam_channel`: handler sends `Op::Execute { cmd, timeout, reply: crossbeam::Sender<...> }`, the session thread reads PTY synchronously until the prompt marker, sends the reply, handler does `reply.recv_timeout()`. Spawned processes (`Shell.Spawn`) get an OS reader thread that pushes chunks into a crossbeam channel; a per-spawn bridge thread converts those chunks to `MessageAttachment::ShellOutput { … }` (new top-level variant; see Task 7) and enqueues via `cx.user().record_async_reminder(...)` — the between-turn buffer Phase 2 introduced. Compose-time drain on the next turn splices each attachment onto the first user message; Segment2Pass renders them as `<system-reminder>` blocks.

**Why no tokio in ProcessManager?** PTY operations are sync syscalls at the OS level. Wrapping them in tokio adds nothing and would force the eval worker into a `block_on` it shouldn't be doing (eval worker runs without an ambient runtime; `Handle::block_on` against arbitrary plugin code risks deadlock if the awaited future calls `spawn_blocking` against a saturated pool). Pure std::thread + crossbeam keeps the eval worker isolated from runtime concerns and matches the existing pattern_memory subscriber-worker idiom.

Permission gating via Plan 3's `CapabilitySet` (Shell effect category) + a future "destructive command" policy layer (out of scope for this phase — surface the structural seam, defer the rules).

**Tech Stack:** Rust async (tokio), `pty-process = "0.5"`, `strip-ansi-escapes = "0.2"`, `dashmap`, `uuid` (process IDs and exit-marker nonces), `tokio` (broadcast/oneshot channels), `tokio-util` (CancellationToken), `tracing`, `jiff` (timestamps for log rotation).

**Scope:** Phase 3 of 5. Independent of Phase 1/2 except for the between-turn async-reminder buffer Phase 2 introduces (`SessionContext::record_async_reminder`). Phase 3 adds a `MessageAttachment::ShellOutput { … }` top-level variant in `pattern_core/src/types/message.rs`, a render arm in `Segment2Pass`, and a per-spawn bridge thread that builds/enqueues the variant. Depends on **Plan 3 (v3-multi-agent) Phase 1** for `CapabilitySet`. User noted this; Phase 3 execution parks until Plan 3 lands.

---

## Amendment 2026-04-26 — Q4 resolved: per-session ProcessManager (NOT runtime-global)

The original plan defaulted to runtime-global ProcessManager (`Arc<ProcessManager>` on
`TidepoolRuntime`, shared across sessions). This is reversed: **ProcessManager is
per-session, owned directly by `SessionContext`.**

**Why:** runtime-global means session A's `cd /tmp` would be visible to session B's
next `pwd`, working against agent isolation. The same per-session granularity preference
applies here as in Phase 4's PortRegistry decision. Mirrors Phase 2's per-session
`FileManager` pattern. Cost: shell sessions don't survive across pattern session
restarts — acceptable, agents are between-session-stateless anyway.

**Affected tasks (overrides take precedence over the original task text below):**

- **Task 4 (`ProcessManager` coordinator):** unchanged in shape. Constructor signature
  unchanged.
- **Task 5 (wiring):** ProcessManager goes on `SessionContext`, not `TidepoolRuntime`.
  Constructed at session-open time with the session's initial cwd (runtime cwd for now;
  per-session cwd from persona config is a Phase 4+ concern). `SessionContext::process_manager()`
  returns `&Arc<ProcessManager>` (Arc preserved so the spawn-output bridge thread can
  hold a reference for its lifetime — bridge outlives any single handler call).
  `TidepoolRuntime` does NOT carry ProcessManager. `tokio_handle` still goes on
  `TidepoolRuntime` (Phase 4's PortRegistry needs it; ProcessManager doesn't).

- **All references in Tasks 1-9 to "runtime-global ProcessManager", "shared across sessions",
  or `TidepoolRuntime::process_manager()` are hereby reinterpreted as the per-session
  shape described above.**

**Test fixture impact (Task 9):** tests construct one `SessionContext` with its own
`ProcessManager`, same as Phase 2's FileManager fixtures. No multi-session sharing tests
needed (and would be wrong to write).

---

**Codebase verified:** 2026-04-24. Evidence:
- `ShellHandler` stub at `crates/pattern_runtime/src/sdk/handlers/shell.rs:1-79`.
- `ShellReq` enum at `crates/pattern_runtime/src/sdk/requests/shell.rs:1-17` — already has the right four variants (`Execute`, `Spawn`, `Kill`, `Status`); **no enum change required**.
- v2 reference at `rewrite-staging/runtime_subsystems/data_source/process/`:
    - `backend.rs:63-117` — `ShellBackend` trait (`execute`, `spawn_streaming`, `kill`, `running_tasks`, `cwd`).
    - `backend.rs:20-29` — `ExecuteResult { output, exit_code, duration_ms }`.
    - `backend.rs:34-39` — `OutputChunk::{Output(String), Exit { code, duration_ms }}`.
    - `backend.rs:42-50` — `TaskId(String)` newtype with UUID-based constructor.
    - `local_pty.rs:34` — `PROMPT_MARKER = "\x1b]pattern-done\x07"` (OSC escape).
    - `local_pty.rs:66-82` — `LocalPtyBackend` struct: shell, initial_cwd, env, load_rc, running map, session, cached_cwd.
    - `local_pty.rs:106-167` — `new`, `find_default_shell`, `with_shell`, `with_env`, `with_load_rc` builder methods.
    - `local_pty.rs:190-231` — `ensure_session()` PTY init via `pty_process::open()` + `pty_process::Command`.
    - `local_pty.rs:236-290` — `read_until_prompt(timeout)` with prompt marker detection + ANSI strip.
    - `local_pty.rs:293-296` — `generate_exit_marker()` (UUID-based nonce).
    - `local_pty.rs:305-329` — `parse_exit_code(output, marker)`.
    - `local_pty.rs:346-379` — `refresh_cwd` (queries `pwd`, caches).
    - `local_pty.rs:384-444` — `execute` impl wrapping commands with the exit-marker echo.
    - `error.rs:47-92` — `ShellError` enum (`#[non_exhaustive]`, all variants).
- Runtime-global wiring template: `crates/pattern_runtime/src/runtime.rs:32` (`TidepoolRuntime` struct).
- Per-session access: `cx.user()` returns `&SessionContext`; for runtime-global state, accessor on `SessionContext` (`process_manager()`) returns the runtime's `Arc<ProcessManager>`.
- Existing deps: `pty-process = { version = "0.5", features = ["async"] }` and `strip-ansi-escapes = "0.2"` listed in `crates/pattern_core/Cargo.toml:98-99` but unused in source — Phase 3 moves them to `crates/pattern_runtime/Cargo.toml` and removes from pattern_core.
- System reminder integration: same between-turn buffer Phase 2 introduces (`SessionContext::record_async_reminder(MessageAttachment)`). Phase 3 adds a `MessageAttachment::ShellOutput { … }` top-level variant in `pattern_core/src/types/message.rs` next to Phase 2's `FileEdit` variant. Render arm added to `pattern_provider::compose::passes::Segment2Pass`. Bridge thread (per spawned process) is a `std::thread::spawn`, drains the crossbeam Receiver of OutputChunks, builds attachments, enqueues.

---

## Acceptance Criteria Coverage

### v3-sandbox-io.AC3: Shell handler
- **v3-sandbox-io.AC3.1 Success:** `Shell.Execute("echo hello", 30)` returns `{ output: "hello\n", exit_code: 0, duration_ms: ... }`
- **v3-sandbox-io.AC3.2 Success:** `Shell.Execute` auto-spawns a default shell session if none exists; subsequent executions reuse it (cwd/env preserved)
- **v3-sandbox-io.AC3.3 Success:** `Shell.Spawn("long-running-cmd")` returns a `ProcessId`; process runs asynchronously; agent receives output via system reminders
- **v3-sandbox-io.AC3.4 Success:** `Shell.Kill(pid)` terminates the process; subsequent `Shell.Status()` shows it as terminated with exit code
- **v3-sandbox-io.AC3.5 Success:** `Shell.Status()` lists all active sessions/processes with their current state
- **v3-sandbox-io.AC3.6 Success:** Shell session persists cwd across executions: `Execute("cd /tmp")` then `Execute("pwd")` returns `/tmp`
- **v3-sandbox-io.AC3.7 Failure:** `Shell.Execute` with timeout: command exceeding timeout is killed; response indicates timeout
- **v3-sandbox-io.AC3.8 Failure:** `Shell.Kill` with nonexistent `ProcessId` returns `ShellError::ProcessNotFound`
- **v3-sandbox-io.AC3.9 Edge:** Exit code detection uses OSC prompt markers (nonce-based); command output containing exit-code-like text does not confuse the parser
- **v3-sandbox-io.AC3.10 Edge:** Process output logged to file as reliability backstop; log file written even if agent session crashes

---

## Subcomponent layout

- **A (tasks 1-3): Types + `ShellBackend` trait + `LocalPtyBackend` port.** Mechanical port from v2, adapted to v3 conventions.
- **B (tasks 4-5): `ProcessManager` coordinator + `TidepoolRuntime`/`SessionContext` wiring.**
- **C (tasks 6-7): `ShellHandler` impl + spawn-output system-reminder pipeline.**
- **D (tasks 8-9): Process logging + AC3 test suite.**

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: Types — `ShellError`, `TaskId`, `ExecuteResult`, `OutputChunk`, `ShellPermission` + `ShellReq::Execute` timeout arg

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/mod.rs` — module root.
- Create: `crates/pattern_runtime/src/process_manager/types.rs` — `TaskId`, `ExecuteResult`, `OutputChunk`, `ShellPermission`.
- Create: `crates/pattern_runtime/src/process_manager/error.rs` — `ShellError`.
- Modify: `crates/pattern_runtime/src/lib.rs` — `pub mod process_manager;`.
- Modify: `crates/pattern_runtime/Cargo.toml` — add `pty-process = "0.5"` (no `async` feature — sync API; see Task 3), `strip-ansi-escapes = "0.2"`, `uuid = { workspace = true, features = ["v4"] }` (verify uuid is workspace), `crossbeam-channel = { workspace = true }`.
- Modify: `crates/pattern_core/Cargo.toml:98-99` — **remove** the unused `pty-process` and `strip-ansi-escapes` lines (per `[pattern-core] stays trait-only` rule in CLAUDE.md, these were stale).
- Modify: `crates/pattern_runtime/src/sdk/requests/shell.rs:1-17` — change `Execute(String)` variant to `Execute(String, i64)` per AC3.1's literal `Shell.Execute("echo hello", 30)` signature (timeout in seconds; 0 means use SessionContext default). Add corresponding line to the parity table at `crates/pattern_runtime/src/sdk/requests.rs`.
- Modify: `crates/pattern_runtime/haskell/Pattern/Shell.hs` — change the `Execute` GADT constructor to `Execute :: Command -> Int -> Shell Text` and update the `execute` helper signature.

**Implementation:**

Direct port from `rewrite-staging/runtime_subsystems/data_source/process/`. Renames + cleanups for v3 fit:

```rust
// process_manager/types.rs
use std::path::PathBuf;
use std::time::Duration;

/// Stable identifier for a spawned shell process. Distinct from the OS PID
/// (which can be recycled); this is a UUID prefix unique within a runtime
/// instance's lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string()[..8].to_string())
    }
}

impl Default for TaskId { fn default() -> Self { Self::new() } }

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecuteResult {
    /// Output captured up to the moment the call returned.
    pub output: String,
    /// `Some(code)` when the command finished within the timeout. `None`
    /// when the timeout fired and the command was backgrounded — agent
    /// learns the actual exit code later via the spawn-output stream
    /// (`MessageAttachment::ShellOutput { kind: Exit, .. }`).
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// `Some(task_id)` when the call's `timeout` fired and the running
    /// command was backgrounded rather than killed. The task continues
    /// running; the agent can `Shell.Status` to see it and will receive
    /// further output as `MessageAttachment::ShellOutput` entries on the
    /// next turn(s). Mirrors Claude Code's bash tool behavior — long-running
    /// commands don't get cut off, they just transition to background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backgrounded_as: Option<TaskId>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputChunk {
    Output(String),
    Exit { code: Option<i32>, duration_ms: u64 },
}

/// Permission tier for shell operations. Gated at dispatch time per command.
/// Plan 3's CapabilitySet wraps this — for Phase 3, the field exists on
/// every shell op but enforcement is a no-op until Plan 3 wires the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPermission {
    ReadOnly,    // git status, ls, cat
    ReadWrite,   // file mods, git commit
    Admin,       // unrestricted
}
```

```rust
// process_manager/error.rs
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellError {
    #[error("permission denied: required {required:?}, granted {granted:?}")]
    PermissionDenied { required: super::types::ShellPermission, granted: super::types::ShellPermission },
    #[error("path outside sandbox: {0}")]
    PathOutsideSandbox(PathBuf),
    #[error("command denied by policy: {0}")]
    CommandDenied(String),
    #[error("command timed out after {0:?}")]
    Timeout(Duration),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[source] std::io::Error),
    #[error("PTY error: {0}")]
    PtyError(String),
    #[error("unknown task: {0}")]
    UnknownTask(String),
    #[error("task already completed")]
    TaskCompleted,
    #[error("session not initialized")]
    SessionNotInitialized,
    #[error("session died unexpectedly")]
    SessionDied,
    #[error("could not parse exit code from output")]
    ExitCodeParseFailed,
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("encoding error: {0}")]
    EncodingError(String),
    #[error("capability denied: Shell effect not in agent's CapabilitySet")]
    CapabilityDenied,
}
```

**Note on `ShellError::ProcessNotFound` (AC3.8):** the v2 enum has `UnknownTask(String)` for the same case. Use `UnknownTask` and document AC3.8 as satisfied by it (the variant name in the AC text is illustrative, not normative). Alternatively, add a `ProcessNotFound(TaskId)` variant — defaulted to reusing `UnknownTask` to minimise drift from v2.

**Verifies:** Scaffolding for AC3.7 (Timeout variant), AC3.8 (UnknownTask variant), and the rest.

**Verification:**
- `cargo check -p pattern-runtime`.
- `cargo check -p pattern-core` — confirms the dep removal didn't break anything.

**Commit:** `[pattern-runtime] [pattern-core] ShellError + types; relocate pty-process/strip-ansi deps`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `ShellBackend` trait — sync

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/backend.rs` — trait definition.

**Implementation:**

Sync trait. The v2 reference is async (`async_trait`); this version is sync because the backend's call site is the eval worker (no runtime). Each method blocks the calling thread for at most `timeout` — fine because handler dispatch is one-call-at-a-time, and the calling thread is the eval worker (one thread per session).

```rust
use std::path::PathBuf;
use std::time::Duration;
use crossbeam_channel::Receiver;
use crate::process_manager::error::ShellError;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId};

pub trait ShellBackend: Send + Sync + std::fmt::Debug {
    /// Execute a command. Session state (cwd, env) persists across calls.
    /// Blocks the calling thread until the command finishes OR `timeout`
    /// fires. **On timeout the command is NOT killed** — the backend
    /// transitions it to a background task and returns
    /// `ExecuteResult { exit_code: None, backgrounded_as: Some(task_id), output: <so far>, … }`.
    /// The task keeps running in the same shell session; further output
    /// arrives via the spawn-output `MessageAttachment::ShellOutput` stream. Mirrors Claude
    /// Code's bash tool behavior. Caller can `kill` the backgrounded task
    /// explicitly if needed.
    fn execute(&self, command: &str, timeout: Duration)
        -> Result<ExecuteResult, ShellError>;

    /// Spawn a long-running command with streaming output. Returns the
    /// new task ID + a crossbeam receiver of output chunks. The sender
    /// is owned by the backend's per-task reader thread and stays alive
    /// until the process exits or `kill` is called.
    fn spawn_streaming(&self, command: &str)
        -> Result<(TaskId, Receiver<OutputChunk>), ShellError>;

    /// Kill a running spawned process.
    fn kill(&self, task_id: &TaskId) -> Result<(), ShellError>;

    /// List currently running task IDs.
    fn running_tasks(&self) -> Vec<TaskId>;

    /// Get current working directory of the persistent session.
    /// Returns `None` until the session is initialized (lazy first-`execute`).
    fn cwd(&self) -> Option<PathBuf>;
}
```

**Note: no `async_trait`, no `tokio::sync::broadcast`.** The crossbeam receiver is `Send` and `Sync`; bridge threads (Task 7) consume it directly without going through a runtime.

**Verifies:** Scaffolding only.

**Verification:** `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] ShellBackend trait`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `LocalPtyBackend` — port v2 algorithm, switch async→sync (M18)

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/local_pty.rs`.

**Scope:** algorithmic port from `rewrite-staging/runtime_subsystems/data_source/process/local_pty.rs` (600 lines). PTY mechanics (init, OSC prompt detection, exit-marker nonce, cwd cache via `pwd`) port mostly verbatim. The async layer flips to sync per the architecture decision in this phase header — no tokio in ProcessManager.

**Async→sync substitutions** (the non-mechanical part of the port):

- `pty_process::Pty` async API → sync API (`pty-process` 0.5 supports both; use `std::io::{Read,Write}` impls instead of `AsyncReadExt`/`AsyncWriteExt`).
- `tokio::io::BufReader` → `std::io::BufReader`.
- `tokio::sync::Mutex` (`session`, `cached_cwd`, `running` map) → `std::sync::Mutex`.
- `tokio::sync::broadcast::Sender<OutputChunk>` per running process → `crossbeam_channel::Sender<OutputChunk>` bounded(64) per spawn.
- `tokio::sync::oneshot::Sender<()>` (kill_tx) → `crossbeam_channel::Sender<()>` bounded(1); reader thread `try_recv`s in its loop.
- `tokio::task::AbortHandle` → `Arc<AtomicBool>` cancellation flag the reader thread checks each iteration.
- `tokio::time::timeout(...)` in `read_until_prompt` → `std::time::Instant` deadline polling. Pick a sync-readable PTY pattern at execution time (raw fd + `nix::poll` with timeout, or `pty-process`'s sync nonblocking read with `set_read_timeout`); document choice. v2's chunked-read shape (`min(remaining, 100ms)`) is a fine starting point.

**Things to keep unchanged from v2** (load-bearing decisions verified by v2 tests):
- `PROMPT_MARKER = "\x1b]pattern-done\x07"` — OSC escape literal.
- `STREAMING_READ_TIMEOUT = Duration::from_secs(60)` — stall detection for streaming reads.
- Exit marker shape `__PATTERN_EXIT_<8-char-uuid>__` (AC3.9 nonce).
- `--norc --noprofile` shell args by default.
- `PS1 = PROMPT_MARKER`, `PS2 = ""`.
- ANSI strip via `strip_ansi_escapes::strip` before returning output.
- `read_until_prompt` returns `ShellError::SessionDied` on EOF (raw_os_error == 5 or read 0).
- `reinitialize_session` clears session + cached cwd on death.
- Wrapped command shape `format!("{command}; echo \"{exit_marker}:$?\"")`.
- `find_default_shell` bash-first preference (NixOS-aware: `command -v bash` shellout).

**Things to drop from v2:**
- `ShellPermission::RequestedFor` field (Plan 3 capability gating is at the handler boundary).
- Mount/sandbox path-checking that v2 did internally (Phase 2 owns path policy via FilePolicy).

**Per-session OS thread structure:** `ShellSession` owns a `std::thread::JoinHandle<()>` plus a `crossbeam::Sender<SessionOp>` where `SessionOp` is the request enum (`Execute { cmd, timeout, reply }`, `Spawn { cmd, reply }`, `Kill { task_id, reply }`, `Cwd { reply }`, `Shutdown`). The thread loop owns the PTY, services ops one at a time (PTY is single-stream; can't multiplex commands), and tracks per-spawn reader sub-threads. Dropping the `ShellSession` sends `Shutdown` and joins.

**Tracing decisions:** keep v2's `debug!`/`trace!` calls verbatim — tuned during v2 development.

**Verifies:** AC3.1, AC3.2, AC3.6, AC3.7, AC3.9 mechanism.

**Verification:**
- `cargo check -p pattern-runtime`.
- `cargo nextest run -p pattern-runtime --lib process_manager::local_pty`. Port the v2 LocalPty-direct test cases at `rewrite-staging/runtime_subsystems/data_source/process/tests.rs:1-565` (skip `source.rs` tests — obsolete). Tests no longer need `#[tokio::test]`; plain `#[test]` works because everything is sync.
- Tests use `bash` if available, fall back to `sh`. NixOS devshell + standard CI both have bash.

**Commit:** `[pattern-runtime] LocalPtyBackend — port from v2 + async→sync conversion`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

---

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->

<!-- START_TASK_4 -->
### Task 4: `ProcessManager` coordinator — sync, no tokio

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/manager.rs`.

**Implementation:**

ProcessManager wraps a single `Arc<dyn ShellBackend>` (sync, from Task 2). All methods sync. No tokio runtime, no DashMap of broadcast receivers — `spawn` returns the crossbeam Receiver directly to the caller (handler in Task 6) which wires it into a bridge thread (Task 7). Capability gating happens in the handler, not here — keeps ProcessManager free of Plan 3 imports.

```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use crossbeam_channel::Receiver;
use crate::process_manager::backend::ShellBackend;
use crate::process_manager::local_pty::LocalPtyBackend;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId};
use crate::process_manager::error::ShellError;

#[derive(Debug)]
pub struct ProcessManager {
    backend: Arc<dyn ShellBackend>,
}

impl ProcessManager {
    pub fn new(initial_cwd: PathBuf) -> Self {
        Self { backend: Arc::new(LocalPtyBackend::new(initial_cwd)) }
    }

    /// Constructor for test/alternative-backend usage.
    pub fn with_backend(backend: Arc<dyn ShellBackend>) -> Self {
        Self { backend }
    }

    /// Execute a command and wait for completion or timeout. On timeout
    /// the command is NOT killed — the backend transitions it to a
    /// background task and returns `ExecuteResult { backgrounded_as: Some(task_id), … }`.
    /// The handler emits a sentinel `MessageAttachment::ShellOutput { kind: Backgrounded, .. }` at that transition so
    /// the agent learns the backgrounding happened immediately, then
    /// receives further output via the standard spawn-output bridge.
    /// Mirrors Claude Code's bash tool behavior.
    pub fn execute(&self, command: &str, timeout: Duration)
        -> Result<ExecuteResult, ShellError>
    {
        self.backend.execute(command, timeout)
    }

    /// Spawn a streaming process. Returns the task id and a crossbeam
    /// receiver of OutputChunks. Caller (handler in Task 6) hands the
    /// receiver to a bridge thread (Task 7) that converts chunks to
    /// `MessageAttachment::ShellOutput` entries via `record_async_reminder`.
    pub fn spawn(&self, command: &str)
        -> Result<(TaskId, Receiver<OutputChunk>), ShellError>
    {
        self.backend.spawn_streaming(command)
    }

    pub fn kill(&self, task_id: &TaskId) -> Result<(), ShellError> {
        self.backend.kill(task_id)
    }

    pub fn status(&self) -> Vec<TaskId> { self.backend.running_tasks() }

    pub fn cwd(&self) -> Option<PathBuf> { self.backend.cwd() }
}

// No Drop impl needed — backend's session threads observe Sender drop
// when the Arc<dyn ShellBackend> hits zero refcount, then exit cleanly
// via their internal Shutdown handling.
```

**Capability gating moved to the handler.** `ShellHandler::handle` (Task 6) checks `cap.has_shell()` once at dispatch before forwarding to ProcessManager. Keeps ProcessManager runtime-internal and policy-free; isolates Plan 3's `CapabilitySet` import in the handler.

**Verifies:** Mechanism for AC3.1, AC3.2, AC3.3, AC3.4, AC3.5, AC3.6.

**Verification:**
- `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] ProcessManager — sync wrapper over ShellBackend`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Wire `ProcessManager` + `tokio::runtime::Handle` into `TidepoolRuntime` + `SessionContext`

**Files:**
- Modify: `crates/pattern_runtime/src/runtime.rs:32-69` — add `process_manager: Arc<ProcessManager>` field; add `tokio_handle: tokio::runtime::Handle` field; thread the handle through `TidepoolRuntime::new` + `with_default_sdk` (explicit caller-supplied param).
- Modify: `crates/pattern_runtime/src/session.rs:40-121` — add `process_manager: Arc<ProcessManager>` and `tokio_handle: tokio::runtime::Handle` fields on `SessionContext`; expose `process_manager()` and `tokio_handle()` accessors.
- Modify: `TidepoolSession::open` and any other call site of `from_persona` — thread the runtime references through (full call-site list per investigator: `session.rs:546-619`; verify with `grep -rn 'from_persona' crates/pattern_runtime/src` at execution time).
- Modify: `crates/pattern_server` and `crates/pattern_cli` — if these construct `TidepoolRuntime`, supply their tokio handle. (Should be one call site each; verify with `grep -rn 'TidepoolRuntime::new\|with_default_sdk' crates/`.)
- Modify: existing test fixtures that construct `TidepoolRuntime` — pass `Handle::current()` (tests run under `#[tokio::test]`).

**Implementation:**

`TidepoolRuntime::new` becomes:
```rust
pub fn new(
    sdk: SdkLocation,
    memory_store: Arc<dyn MemoryStore>,
    provider: Arc<dyn ProviderClient>,
    db: Arc<pattern_db::ConstellationDb>,
    tokio_handle: tokio::runtime::Handle,   // explicit; honest about the dependency
) -> Self {
    let process_manager = Arc::new(ProcessManager::new(
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    ));
    Self { sdk, memory_store, provider, db, tokio_handle, process_manager }
}
```

`with_default_sdk` likewise gains the param. `TidepoolRuntime::tokio_handle() -> &tokio::runtime::Handle` is exposed for Phase 4's PortRegistry to consume at construction.

**Why explicit at `new`?** `Handle::current()` magic-capture is brittle — caller must be in async context at construction time, single-threaded runtimes silently change semantics, etc. Explicit param surfaces the contract in the type signature and documents that the runtime borrows the caller's tokio runtime.

**Why does ProcessManager not need the handle?** ProcessManager is pure std::thread + crossbeam (Task 4). It runs no async code. The handle exists on the runtime + SessionContext for *Phase 4's* PortRegistry actor and any future async-needing subsystem.

`SessionContext` accessors:
```rust
pub fn process_manager(&self) -> &Arc<ProcessManager> { &self.process_manager }
pub fn tokio_handle(&self) -> &tokio::runtime::Handle { &self.tokio_handle }
```

**Note on initial cwd:** Phase 3 takes the runtime's process cwd. Phase 4+ may want per-session cwd from persona config; out of scope here.

**Verifies:** Mechanism for all AC3 — handler can reach ProcessManager via `cx.user().process_manager()`.

**Verification:**
- `cargo check --workspace`. The `Handle` param ripples to every `TidepoolRuntime::new` callsite.
- Existing `session_lifecycle.rs` tests still pass (just gain a `Handle::current()` arg).

**Commit:** `[pattern-runtime] ProcessManager + tokio_handle on TidepoolRuntime + SessionContext`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

---

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: Implement `ShellHandler` — dispatch `ShellReq` to `ProcessManager`

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/shell.rs` — replace stub.

**Implementation:**

Tighten the trait bound from `HasCancelState` to `SessionContext` (matches `SkillsHandler`). All dispatch is **sync** — ProcessManager is sync (Task 4), and the eval worker has no ambient tokio runtime by design (`crates/pattern_runtime/CLAUDE.md` "Eval worker" section: explicit "no nested tokio runtime, no Handle::current().block_on"). Capability check happens once at the top before any dispatch.

```rust
// SAFETY / DESIGN NOTE for future maintainers:
// This handler runs on the Tidepool eval worker — a dedicated OS thread
// with NO ambient tokio runtime. Do NOT introduce `block_on` here, even
// against a Handle stashed on SessionContext. block_on against arbitrary
// plugin code can deadlock if the awaited future calls `spawn_blocking`
// against a saturated pool (or runs on a single-thread runtime). All
// dispatched subsystems exposed at this boundary MUST be sync at the API
// surface; ProcessManager is, and Phase 4's PortRegistry is sync at the
// boundary too (its actor task hides the async work internally).
impl EffectHandler<SessionContext> for ShellHandler {
    type Request = ShellReq;

    fn handle(&mut self, req: ShellReq, cx: &EffectContext<'_, SessionContext>)
        -> Result<Value, EffectError>
    {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        let pm = cx.user().process_manager().clone();
        let cap = cx.user().capability_set();
        if !cap.has_shell() {
            return Err(EffectError::Handler(
                "Pattern.Shell: capability denied (Shell effect not in agent's CapabilitySet)".to_string()
            ));
        }
        let queue = Arc::clone(cx.user().async_reminder_queue());
        let default_timeout = cx.user().shell_default_timeout(); // SessionContext config knob

        match req {
            ShellReq::Execute(cmd, timeout_secs) => {
                let timeout = if timeout_secs > 0 {
                    Duration::from_secs(timeout_secs as u64)
                } else {
                    default_timeout
                };
                let result = pm.execute(&cmd, timeout)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Shell.Execute: {e}")))?;

                // Timeout-backgrounded transition: enqueue a sentinel
                // ShellOutput attachment (kind = Backgrounded) so the agent
                // learns the backgrounding happened on its next turn, and
                // start the spawn-output bridge so subsequent output flows
                // through the same async-reminder queue.
                if let Some(task_id) = &result.backgrounded_as {
                    queue.lock().unwrap().push(MessageAttachment::ShellOutput {
                        task_id: task_id.to_string(),
                        kind: ShellOutputKind::Backgrounded { partial_output: result.output.clone() },
                        at: jiff::Timestamp::now(),
                    });
                    // The backend stashed the spawn receiver under this
                    // task_id when it transitioned the Execute to background;
                    // ProcessManager exposes it via take_backgrounded_receiver.
                    // The bridge thread (Task 7) takes ownership and pushes
                    // each subsequent OutputChunk as a ShellOutput attachment.
                    if let Some(rx) = pm.take_backgrounded_receiver(task_id) {
                        spawn_output_bridge(task_id.clone(), rx, Arc::clone(&queue));
                    }
                }
                cx.respond(serde_json::to_string(&result).unwrap_or_default())
            }
            ShellReq::Spawn(cmd) => {
                let (task_id, rx) = pm.spawn(&cmd)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Shell.Spawn: {e}")))?;
                // Bridge thread (std::thread::spawn) drains the crossbeam
                // receiver and enqueues each chunk as a ShellOutput attachment.
                spawn_output_bridge(task_id.clone(), rx, Arc::clone(&queue));
                cx.respond(task_id.to_string())
            }
            ShellReq::Kill(pid_int) => {
                let task_id = TaskId(pid_int.to_string());
                pm.kill(&task_id)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Shell.Kill: {e}")))?;
                cx.respond(())
            }
            ShellReq::Status(_pid) => {
                // AC3.5: list all running tasks. The i64 arg is currently
                // unused; see open question Q2 for the GADT cleanup.
                let tasks = pm.status();
                cx.respond(tasks.iter().map(|t| t.to_string()).collect::<Vec<_>>())
            }
        }
    }
}
```

**Capability check:** Done once at the top of `handle`. `cap.has_shell()` is the Plan 3 method; same shape as Phase 2's `cap.has_file()`.

**Execute timeout signature (I10 resolution):** Per AC3.1's literal example `Shell.Execute("echo hello", 30)`, the GADT takes a timeout argument. Phase 3 Task 1 changes `ShellReq::Execute(String)` → `Execute(String, i64)` and the Haskell `Pattern.Shell` GADT to match. Default (when agent passes 0 or omits) comes from `SessionContext::shell_default_timeout()` — runtime config knob, default 30s.

**Status arg (Q2):** `Status(i64)` keeps the unused `i64` for now to avoid touching the Haskell GADT a second time. Cleanup is a follow-up.

**Verifies:** AC3.1, AC3.2, AC3.4, AC3.5, AC3.7.

**Verification:**
- `cargo check -p pattern-runtime`.
- Existing stub test deleted; new tests in Task 9.

**Commit:** `[pattern-runtime] ShellHandler — sync dispatch, capability check, timeout-background sentinel`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `MessageAttachment::ShellOutput` variant + Segment2Pass render arm + spawn-output bridge

**Files:**
- Modify: `crates/pattern_core/src/types/message.rs` — add `MessageAttachment::ShellOutput { task_id, kind, at }` variant alongside Phase 2's `FileEdit`. Define `ShellOutputKind { Output(String), Exit { code: Option<i32>, duration_ms: u64 }, Backgrounded { partial_output: String } }` next to it.
- Modify: `crates/pattern_provider/src/compose/passes/segment_2.rs` — add a render arm for `MessageAttachment::ShellOutput` alongside Phase 2's `FileEdit` arm.
- Modify: `crates/pattern_runtime/src/process_manager/manager.rs` — add `pub fn spawn_output_bridge(task_id: TaskId, rx: Receiver<OutputChunk>, queue: Arc<Mutex<Vec<MessageAttachment>>>)` that spawns a `std::thread` to drain the crossbeam receiver and enqueue ShellOutput attachments. Also add `pub fn take_backgrounded_receiver(&self, task_id) -> Option<Receiver<OutputChunk>>` for the timeout-backgrounded path (LocalPtyBackend stashes the receiver under that task_id when it transitions an Execute to background).

**Implementation:**

```rust
// pattern_core/src/types/message.rs (additions)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShellOutputKind {
    /// Streaming output chunk from a spawned process.
    Output(String),
    /// Process exited; final delivery on the bridge.
    Exit { code: Option<i32>, duration_ms: u64 },
    /// Sentinel emitted at the moment a `Shell.Execute` call's timeout
    /// fires and the running command transitions to background. Agent
    /// learns the transition; subsequent chunks arrive as `Output` /
    /// `Exit` variants.
    Backgrounded { partial_output: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessageAttachment {
    BatchOpeningSnapshot { /* existing */ },
    FileEdit { /* Phase 2 */ },
    /// One spawned-shell event. Bridge thread enqueues one of these per
    /// OutputChunk arriving from the PTY; compose-time drain splices
    /// them onto the next turn's first user message.
    ShellOutput {
        task_id: String,
        kind: ShellOutputKind,
        at: jiff::Timestamp,
    },
    // (Phase 4 adds PortEvent.)
}
```

```rust
// pattern_provider/src/compose/passes/segment_2.rs (addition)
match attachment {
    // … existing arms …
    MessageAttachment::ShellOutput { task_id, kind, at } => {
        let body = match kind {
            ShellOutputKind::Output(text) => format!(
                "<system-reminder>\nshell task {task_id} @ {at}:\n```\n{text}\n```\n</system-reminder>"
            ),
            ShellOutputKind::Exit { code, duration_ms } => format!(
                "<system-reminder>\nshell task {task_id} @ {at}: [exited {code:?} in {duration_ms}ms]\n</system-reminder>"
            ),
            ShellOutputKind::Backgrounded { partial_output } => format!(
                "<system-reminder>\n\
                 Shell.Execute timed out and was backgrounded as task {task_id} @ {at}.\n\
                 Output captured before backgrounding (more will follow as it arrives):\n\
                 ```\n{partial_output}\n```\n\
                 </system-reminder>"
            ),
        };
        push_user_block(message, body);
    }
}
```

```rust
// process_manager/manager.rs — bridge thread
pub fn spawn_output_bridge(
    task_id: TaskId,
    rx: crossbeam_channel::Receiver<OutputChunk>,
    queue: Arc<Mutex<Vec<MessageAttachment>>>,
) {
    // std::thread::spawn — NOT a tokio task. ProcessManager has no tokio
    // runtime by design (see Phase 3 architecture note). Bridge runs as
    // long as the receiver yields; exits when the backend's sender drops
    // (process exit / kill / Shutdown).
    let task_id_str = task_id.to_string();
    std::thread::Builder::new()
        .name(format!("shell-output-bridge:{task_id_str}"))
        .spawn(move || {
            for chunk in rx.iter() {
                let attachment = MessageAttachment::ShellOutput {
                    task_id: task_id_str.clone(),
                    kind: match &chunk {
                        OutputChunk::Output(text) => ShellOutputKind::Output(text.clone()),
                        OutputChunk::Exit { code, duration_ms } => ShellOutputKind::Exit {
                            code: *code, duration_ms: *duration_ms,
                        },
                    },
                    at: jiff::Timestamp::now(),
                };
                queue.lock().unwrap().push(attachment);
                if matches!(chunk, OutputChunk::Exit { .. }) { break; }
            }
            // rx.iter() returns None when the sender drops; thread exits.
        })
        .expect("failed to spawn shell-output bridge thread");
}
```

**Why std::thread, not handle.spawn?** Two reasons:
1. ProcessManager is sync top-to-bottom (Phase 3 design). Introducing a tokio task here would re-introduce the runtime coupling we explicitly avoided.
2. The bridge work is a tight `recv → enqueue` loop. It blocks on `rx.iter()` — appropriate for an OS thread, wasteful for a tokio worker. Hundreds of bridge threads would still be cheap; the agent typically has at most a handful of background processes at any time.

**Note on chunking granularity.** Pushing one attachment per output chunk means a chatty process can produce many segment-2 entries. Coalescing consecutive shell-output reminders for the same task at compose time is a follow-up.

**Note on autonomous activation:** the design plan calls out backgrounded-exec completion as the canonical first hook for autonomous activation. When the bridge enqueues the final `Exit` attachment for a backgrounded task, a future plan can subscribe to that signal and trigger an autonomous turn so the agent sees the completion immediately rather than waiting for the next human-driven turn.

**Verifies:** AC3.3 (output streams via the async-reminder buffer); the Backgrounded variant verifies the AC3.7 update.

**Verification:**
- `cargo check --workspace`.
- Unit tests on the Segment2Pass render arm — snapshot-test bodies for `Output` / `Exit` / `Backgrounded` via `insta`.
- Integration test in Task 9 exercises spawn → wait one turn → assert the spawned task's chunks appear as `MessageAttachment::ShellOutput` on the next turn's first user message with the right task_id substring.

**Commit:** `[pattern-core] [pattern-provider] [pattern-runtime] ShellOutput attachment variant + std::thread bridge`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

<!-- START_SUBCOMPONENT_D (tasks 8-9) -->

<!-- START_TASK_8 -->
### Task 8: Process output logging — reliability backstop

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/logger.rs` — `ProcessLogger` writing chunks to `<cache_dir>/shell/<task_id>.log`.
- Modify: `crates/pattern_runtime/src/process_manager/manager.rs` — call logger from inside `spawn_output_bridge` (alongside the queue enqueue).

**Implementation:**

Per AC3.10, process output is written to a log file as a reliability backstop — the agent can't see it directly (it's not surfaced through any handler), but if the agent session crashes or output is lost in the broadcast lag path, the log is the recovery surface.

Log location: `<runtime_cache_dir>/shell/<task_id>.log`. The runtime cache dir is wherever pattern stores transient state — investigator findings indicated the project uses `PatternPaths` (in `pattern_memory::paths::PatternPaths`) for path discovery. ProcessManager takes a `cache_dir: PathBuf` constructor argument; runtime supplies it from `PatternPaths`.

Format: append-only, one chunk per line, jiff-formatted timestamp prefix:

```
2026-04-24T17:42:00.123Z OUT  hello world
2026-04-24T17:42:00.234Z OUT  another line
2026-04-24T17:42:01.456Z EXIT code=0 duration_ms=1233
```

```rust
// process_manager/logger.rs
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use crate::process_manager::types::{OutputChunk, TaskId};

pub struct ProcessLogger {
    file: Mutex<File>,
    path: PathBuf,
}

impl ProcessLogger {
    pub fn open(cache_dir: &Path, task_id: &TaskId) -> std::io::Result<Self> {
        let dir = cache_dir.join("shell");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{task_id}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { file: Mutex::new(file), path })
    }

    pub fn append(&self, chunk: &OutputChunk) -> std::io::Result<()> {
        let mut f = self.file.lock().unwrap();
        let ts = jiff::Timestamp::now();
        match chunk {
            OutputChunk::Output(s) => writeln!(f, "{ts} OUT  {}", s.replace('\n', "\\n"))?,
            OutputChunk::Exit { code, duration_ms } => {
                writeln!(f, "{ts} EXIT code={code:?} duration_ms={duration_ms}")?;
            }
        }
        f.flush()?;  // flush on each write so a crash mid-output doesn't lose data
        Ok(())
    }

    pub fn path(&self) -> &Path { &self.path }
}
```

The `flush()` per write is a deliberate cost: AC3.10 says the log "is written even if agent session crashes" — write-buffer flush is the only way to guarantee that. Output volume per spawned process is typically modest (kilobytes/second at most for log-style output); the sync flush cost is acceptable.

**Log retention:** out of scope. The directory grows unbounded across runtime restarts. Same rotation policy hook as Plan 1's message backup (jiff timestamps, GFS-style rotation) is a reasonable future direction; flag for follow-up.

**Verifies:** AC3.10.

**Verification:**
- `cargo check -p pattern-runtime`.
- Unit tests in `logger.rs`:
    - `appends_output_lines` — open logger, append three Output chunks, read file, verify three lines.
    - `appends_exit_record` — append Exit chunk; verify the EXIT line shape.
    - `flush_persists_after_drop` — write, drop logger, re-read file from disk; content present.
    - `concurrent_appends_dont_interleave` — spawn 4 threads each writing 50 chunks; verify line count = 200, no interleaved lines (Mutex contract).

**Commit:** `[pattern-runtime] ProcessLogger — per-task output log as crash backstop`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: AC3 test suite

**Files:**
- Create: `crates/pattern_runtime/tests/shell_handler.rs` — full-handler integration tests.
- Expand `crates/pattern_runtime/src/process_manager/local_pty.rs` tests with the v2 ports.

**Tests (one per AC case):**

| AC | Test name | Mechanism |
|----|-----------|-----------|
| 3.1 | `execute_returns_output_and_exit_code` | `pm.execute(cap, "echo hello", 30s)` → output `"hello\n"`, exit_code `Some(0)`, duration_ms reasonable. |
| 3.2 | `execute_auto_spawns_then_reuses_session` | First execute initialises session (cwd unset → set after); second execute reuses (cwd cache hit). Verify with two `pwd` calls in a row. |
| 3.3 | `spawn_streams_output_via_attachments` | Spawn `for i in 1 2 3; do echo line$i; sleep 0.05; done`; wait one turn boundary; assert the next turn's first user message has `MessageAttachment::ShellOutput` entries with `kind = ShellOutputKind::Output` matching `line1`/`line2`/`line3` and one `kind = ShellOutputKind::Exit { code: Some(0), .. }`. |
| 3.4 | `kill_terminates_running_process` | Spawn `sleep 60`; immediately `kill(task_id)`; verify `status()` no longer lists the task; verify the broadcast `Exit` chunk arrives. |
| 3.5 | `status_lists_running_tasks` | Spawn two long-running processes; assert `status()` returns both task IDs. |
| 3.6 | `cwd_persists_across_executions` | `execute("cd /tmp")` then `execute("pwd")` → output contains `/tmp`. |
| 3.7 | `execute_timeout_backgrounds_not_kills` | `execute("sleep 2 && echo done", 1s)` → returns `ExecuteResult { exit_code: None, backgrounded_as: Some(task_id), output: <empty>, … }` quickly (under 2s elapsed). Then `pm.status()` lists the task as still running. After ~2s, the spawn-output bridge enqueues `MessageAttachment::ShellOutput` entries containing `done` + an `Exit` variant; assert they appear on the next turn boundary. Mirrors Claude Code's bash tool behavior. |
| 3.7b | `execute_timeout_emits_backgrounded_sentinel` | `execute("sleep 5", 1s)`; immediately call `session.drain_async_reminders()` after the call returns; assert one entry is `MessageAttachment::ShellOutput { kind: ShellOutputKind::Backgrounded { partial_output }, .. }` matching the returned task id. |
| 3.8 | `kill_unknown_task_returns_error` | `kill(TaskId("not-a-real-id"))` → `ShellError::UnknownTask("not-a-real-id")`. |
| 3.9 | `exit_code_parser_resists_injection` | Run command whose output contains `__PATTERN_EXIT_deadbeef__:1`. The actual exit-marker nonce is unique per call, so the spurious string doesn't match. Verify exit_code is the actual command exit code, not 1. |
| 3.10 | `process_output_logged_to_file` | Spawn process with known output; wait for completion; read `<cache_dir>/shell/<task_id>.log`; verify each output line + the EXIT line are present. |

**LocalPty test suite ports:** The v2 file `rewrite-staging/runtime_subsystems/data_source/process/tests.rs` (565 lines) has roughly 12 LocalPty-direct tests. Port them; they don't require the new ProcessManager and validate the lower-level PTY machinery independently.

**Capability stubbing:** tests construct a `CapabilitySet` with Shell enabled (Plan 3 helper).

**CI considerations per `crates/pattern_runtime/CLAUDE.md`:** NixOS devshell has bash on PATH. Tests should `command -v bash` first; if absent (e.g., Alpine CI), fall back to `sh` — `LocalPtyBackend::find_default_shell` already handles this. No live-model dependency; nothing new for CI.

**Verifies:** AC3.1, AC3.2, AC3.3, AC3.4, AC3.5, AC3.6, AC3.7, AC3.8, AC3.9, AC3.10.

**Verification:**
- `cargo nextest run -p pattern-runtime --test shell_handler`.
- `cargo nextest run -p pattern-runtime --lib process_manager`.
- All 646 existing tests still pass.

**Commit:** `[pattern-runtime] AC3 tests for ShellHandler + ProcessManager`
<!-- END_TASK_9 -->

<!-- END_SUBCOMPONENT_D -->

---

## Open questions for human review (foreground at end of plan-write)

**Q1 [resolved 2026-04-24]:** `ShellReq::Execute(String)` → `Execute(String, i64)` per AC3.1's literal signature. Haskell GADT updated. Timeout `0` means use SessionContext default (`shell_default_timeout`, default 30s). Per the additional design discussion (this phase header), timeout fires → command is **backgrounded, not killed** — see `ExecuteResult.backgrounded_as`.

**Q2: `Status` ignoring its `i64` argument.** The current `ShellReq::Status(i64)` takes an unused arg. Possibilities: (a) ignore (current); (b) drop the arg from the GADT; (c) repurpose as "filter to this task ID". Defaulted to (a); reviewer may prefer (b) or (c).

**Q3: `ShellError::ProcessNotFound` vs `ShellError::UnknownTask`.** AC3.8 names `ProcessNotFound`. v2 (and this plan) use `UnknownTask`. Defaulted to `UnknownTask` (minimises drift from v2); reviewer may prefer renaming to match the AC text exactly.

**Q4: Per-session ProcessManager vs runtime-global.** Plan defaults to runtime-global (one shared ProcessManager across all sessions). This means session A's `cd /tmp` is visible to session B's next `pwd` — not ideal for multi-agent isolation. Alternative: per-session ProcessManager (matches FileManager pattern). Defaulted to runtime-global per the design plan's claim; flag if reviewer wants per-session for isolation.

**Q5: Process log retention.** The plan ships unbounded growth in `<cache_dir>/shell/`. Plan 1 has GFS-style backup rotation; the same hook could apply here. Out of scope this phase; flag whether to add even a basic cap (e.g., delete logs > 30 days).
