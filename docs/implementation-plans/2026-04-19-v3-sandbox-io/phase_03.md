# Phase 3: Shell handler + ProcessManager

**Goal:** Replace the `ShellHandler` stub with a real implementation dispatching into a runtime-global `ProcessManager` coordinator. ProcessManager owns a map of `ShellSession` instances (each wrapping a persistent PTY-backed shell with OSC prompt markers for exit-code detection). Operations: `Execute` (sync, returns output + exit code), `Spawn` (async, output via system reminders), `Kill`, `Status`. Process output also written to a per-session log file as a reliability backstop.

**Architecture:** `ShellHandler<SessionContext>` (tightened from the stub's `HasCancelState` — matches `SkillsHandler` and Phase 2's `FileHandler`) dispatches `ShellReq` to `cx.user().process_manager()`. ProcessManager is `Arc<ProcessManager>` held on the `TidepoolRuntime` (one per runtime instance, shared across sessions via Arc — different from Phase 2's per-session FileManager because shell sessions have global semantics: an agent shouldn't lose its bash session across pattern session boundaries). It exposes a `ShellBackend` trait (one impl: `LocalPtyBackend` ported from v2 reference code at `rewrite-staging/runtime_subsystems/data_source/process/`). Spawned processes stream output to a tokio `broadcast::channel`; a per-spawn listener task takes the session's `Arc<MemoryStoreAdapter>` (from `cx.user().adapter()` at `Shell.Spawn` dispatch time) and bridges chunks via `pattern_provider::compose::pseudo_messages::render_shell_output_event(...) -> ChatMessage` + `adapter.record_pseudo_message(msg)` — same canonical pipeline as `Pattern.Skills.Load` and Phase 2's file edits. **No new `MessageAttachment` variant** — the existing pseudo-message pipe carries shell output into segment 2. Permission gating via Plan 3's `CapabilitySet` (Shell effect category) + a future "destructive command" policy layer (out of scope for this phase — surface the structural seam, defer the rules).

**Tech Stack:** Rust async (tokio), `pty-process = "0.5"`, `strip-ansi-escapes = "0.2"`, `dashmap`, `uuid` (process IDs and exit-marker nonces), `tokio` (broadcast/oneshot channels), `tokio-util` (CancellationToken), `tracing`, `jiff` (timestamps for log rotation).

**Scope:** Phase 3 of 5. Independent of Phase 1/2 except for the `MessageAttachment` plumbing (Phase 2 introduces the `FileEdits` variant; Phase 3 adds a `ShellOutput` variant alongside, sharing the same Segment-2 render machinery). Depends on **Plan 3 (v3-multi-agent) Phase 1** for `CapabilitySet`. User noted this; Phase 3 execution parks until Plan 3 lands.

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
- System reminder integration: same canonical pseudo-message pipeline as `Pattern.Skills.Load` and Phase 2 — `adapter.record_pseudo_message(render_shell_output_event(...))`. See `crates/pattern_runtime/src/sdk/handlers/skills.rs` (search for `record_pseudo_message`) for the canonical template established by previous work; `pattern_provider::compose::pseudo_messages` is the renderer module.

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
### Task 1: Types — `ShellError`, `TaskId`, `ExecuteResult`, `OutputChunk`, `ShellPermission`

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/mod.rs` — module root.
- Create: `crates/pattern_runtime/src/process_manager/types.rs` — `TaskId`, `ExecuteResult`, `OutputChunk`, `ShellPermission`.
- Create: `crates/pattern_runtime/src/process_manager/error.rs` — `ShellError`.
- Modify: `crates/pattern_runtime/src/lib.rs` — `pub mod process_manager;`.
- Modify: `crates/pattern_runtime/Cargo.toml` — add `pty-process = { version = "0.5", features = ["async"] }`, `strip-ansi-escapes = "0.2"`, `uuid = { workspace = true, features = ["v4"] }` (verify uuid is workspace).
- Modify: `crates/pattern_core/Cargo.toml:98-99` — **remove** the unused `pty-process` and `strip-ansi-escapes` lines (per `[pattern-core] stays trait-only` rule in CLAUDE.md, these were stale).

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
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
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
### Task 2: `ShellBackend` trait

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/backend.rs` — trait definition.

**Implementation:**

Direct port from v2's `backend.rs:63-117`:

```rust
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::broadcast;
use crate::process_manager::error::ShellError;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId};

#[async_trait::async_trait]
pub trait ShellBackend: Send + Sync + std::fmt::Debug {
    /// Execute a command and wait for completion. Session state (cwd, env)
    /// persists across calls.
    async fn execute(&self, command: &str, timeout: Duration)
        -> Result<ExecuteResult, ShellError>;

    /// Spawn a long-running command with streaming output. Returns the
    /// new task ID + a receiver for output chunks. The sender remains
    /// alive (held by the backend) until the process exits.
    async fn spawn_streaming(&self, command: &str)
        -> Result<(TaskId, broadcast::Receiver<OutputChunk>), ShellError>;

    /// Kill a running spawned process.
    async fn kill(&self, task_id: &TaskId) -> Result<(), ShellError>;

    /// List currently running task IDs.
    fn running_tasks(&self) -> Vec<TaskId>;

    /// Get current working directory of the persistent session.
    /// Returns `None` until the session is initialized (lazy first-`execute`).
    async fn cwd(&self) -> Option<PathBuf>;
}
```

**Verifies:** Scaffolding only.

**Verification:** `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] ShellBackend trait`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `LocalPtyBackend` — port v2 implementation

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/local_pty.rs` — port of `rewrite-staging/runtime_subsystems/data_source/process/local_pty.rs`.

**Scope:** mechanical port. The v2 file is 600 lines; the port preserves the algorithms (PTY init via `pty_process::open()`, OSC prompt marker detection in `read_until_prompt`, exit-marker nonce wrapping, `cwd` cache via `pwd` query, `spawn_streaming` with abort handle + kill channel) and updates only:

- Imports use the new module path (`crate::process_manager::*` instead of `super::*`).
- Error type is the new `ShellError` (same variants, same shape).
- Tracing `target!` strings change from `data_source::process` to `process_manager`.
- The MOVING TO comment header is removed.
- `find_default_shell` retains the bash-first preference (NixOS-aware: `command -v bash` shellout).

**Things to keep unchanged from v2 (load-bearing decisions verified by v2 tests):**
- `PROMPT_MARKER = "\x1b]pattern-done\x07"` — OSC escape, not a regular string.
- `STREAMING_READ_TIMEOUT = Duration::from_secs(60)` — stall detection for spawn_streaming reads.
- Exit marker shape: `__PATTERN_EXIT_<8-char-uuid>__` — nonce avoids output-injection attacks (AC3.9).
- `--norc --noprofile` shell args by default — ensures predictable PS1 / no shell-config interference.
- `PS1` env var set to `PROMPT_MARKER`, `PS2` set to empty — required for prompt detection.
- ANSI strip via `strip_ansi_escapes::strip` before returning output.
- `read_until_prompt` returns `ShellError::SessionDied` on EOF (raw_os_error == 5 or read returns 0).
- `reinitialize_session` clears the session + cached cwd on death.
- Wrapped command shape: `format!("{command}; echo \"{exit_marker}:$?\"")`.

**Things to drop from v2:**
- The `ShellPermission` enum's `RequestedFor` field (was a permission-wrapping experiment in v2; v3 capability gating is at the handler boundary).
- The mount/sandbox path-checking that v2 did internally (Phase 2 owns path policy via FilePolicy; ProcessManager doesn't try to second-guess it).

**Tracing decisions:** keep the v2 `debug!`/`trace!` calls verbatim — they were tuned during v2 development and prevent silent debugging issues.

**Verifies:** AC3.1, AC3.2, AC3.6, AC3.7, AC3.9 mechanism (all via the LocalPtyBackend impl).

**Verification:**
- `cargo check -p pattern-runtime`.
- `cargo nextest run -p pattern-runtime --lib process_manager::local_pty` — port over the v2 test cases at `rewrite-staging/runtime_subsystems/data_source/process/tests.rs` that exercise LocalPtyBackend directly (skip the `source.rs` integration tests; those are obsolete in v3). Estimate: ~12 LocalPty-level tests survive the port.
- Tests must use `bash` if available, fall back to `sh`. CI environment per `crates/pattern_runtime/CLAUDE.md` "Smoke-test procedure" section: NixOS devshell has bash; non-Nix CI has bash via the dependent OSes.

**Commit:** `[pattern-runtime] LocalPtyBackend ported from v2 reference`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

---

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->

<!-- START_TASK_4 -->
### Task 4: `ProcessManager` coordinator

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/manager.rs`.

**Implementation:**

ProcessManager wraps a `ShellBackend` and adds:
- The `running_processes` registry (delegated to the backend's internal map for spawn/kill/status).
- The session lifetime — currently one shared `LocalPtyBackend` per ProcessManager instance, but designed so future variants (per-agent shells, isolated bubblewrap shells, container shells) can swap the backend without touching the manager.
- Optional capability gating (Plan 3 `CapabilitySet` accessor; for Phase 3 this is a stub that always allows when the cap is in the set).
- Per-spawn listener bridge that drains the broadcast receiver and pushes pseudo-messages via `adapter.record_pseudo_message` (Task 7 wires this).

```rust
use std::sync::Arc;
use std::time::Duration;
use dashmap::DashMap;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use pattern_core::capability::CapabilitySet; // Plan 3
use crate::process_manager::backend::ShellBackend;
use crate::process_manager::local_pty::LocalPtyBackend;
use crate::process_manager::types::{ExecuteResult, OutputChunk, TaskId};
use crate::process_manager::error::ShellError;

pub struct ProcessManager {
    backend: Arc<dyn ShellBackend>,
    /// Per-spawned-process broadcast subscribers. Phase 3 owns the
    /// listener bridge that pushes chunks via `adapter.record_pseudo_message`;
    /// see Task 7. Keyed by TaskId.
    spawn_subscribers: DashMap<TaskId, broadcast::Receiver<OutputChunk>>,
    cancel: CancellationToken,
}

impl ProcessManager {
    pub fn new(initial_cwd: std::path::PathBuf) -> Self {
        Self {
            backend: Arc::new(LocalPtyBackend::new(initial_cwd)),
            spawn_subscribers: DashMap::new(),
            cancel: CancellationToken::new(),
        }
    }

    /// Constructor for test/alternative-backend usage.
    pub fn with_backend(backend: Arc<dyn ShellBackend>) -> Self {
        Self {
            backend,
            spawn_subscribers: DashMap::new(),
            cancel: CancellationToken::new(),
        }
    }

    pub async fn execute(&self, capability: &CapabilitySet, command: &str, timeout: Duration)
        -> Result<ExecuteResult, ShellError>
    {
        if !capability.has_shell() { return Err(ShellError::CapabilityDenied); }
        self.backend.execute(command, timeout).await
    }

    pub async fn spawn(&self, capability: &CapabilitySet, command: &str)
        -> Result<TaskId, ShellError>
    {
        if !capability.has_shell() { return Err(ShellError::CapabilityDenied); }
        let (task_id, rx) = self.backend.spawn_streaming(command).await?;
        // Stash the receiver here so the listener bridge (Task 7) can pick
        // it up. Caller of ProcessManager doesn't see the receiver directly —
        // output flows through adapter.record_pseudo_message via the listener.
        self.spawn_subscribers.insert(task_id.clone(), rx);
        Ok(task_id)
    }

    pub async fn kill(&self, capability: &CapabilitySet, task_id: &TaskId)
        -> Result<(), ShellError>
    {
        if !capability.has_shell() { return Err(ShellError::CapabilityDenied); }
        self.backend.kill(task_id).await?;
        self.spawn_subscribers.remove(task_id);
        Ok(())
    }

    pub fn status(&self) -> Vec<TaskId> { self.backend.running_tasks() }

    pub async fn cwd(&self) -> Option<std::path::PathBuf> { self.backend.cwd().await }

    /// Take ownership of a spawn receiver for the listener bridge.
    /// Returns None if not registered (already taken).
    pub(crate) fn take_spawn_receiver(&self, task_id: &TaskId)
        -> Option<broadcast::Receiver<OutputChunk>>
    {
        self.spawn_subscribers.remove(task_id).map(|(_, rx)| rx)
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) { self.cancel.cancel(); }
}
```

**Note on `capability.has_shell()`:** matches the `has_file()` pattern from Phase 2 — Plan 3 provides per-effect-category methods on `CapabilitySet`.

**Verifies:** Mechanism for AC3.1, AC3.2, AC3.3, AC3.4, AC3.5, AC3.6 (delegated to backend).

**Verification:**
- `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] ProcessManager coordinator over ShellBackend`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Wire `ProcessManager` into `TidepoolRuntime` + `SessionContext`

**Files:**
- Modify: `crates/pattern_runtime/src/runtime.rs:32` — add `process_manager: Arc<ProcessManager>` field; construct in `TidepoolRuntime::new`.
- Modify: `crates/pattern_runtime/src/session.rs:40-121` — add `process_manager: Arc<ProcessManager>` field on SessionContext (cloned from runtime at session-open time) + `process_manager()` accessor.

**Implementation:**

In `TidepoolRuntime::new`:
```rust
let process_manager = Arc::new(ProcessManager::new(
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
));
```

The runtime's `process_manager` field flows into each `SessionContext` constructed by `TidepoolSession::open_with_agent_loop` (investigator pointed to `session.rs:546-619`).

`SessionContext` accessor:
```rust
pub fn process_manager(&self) -> &Arc<ProcessManager> { &self.process_manager }
```

**Note on initial cwd:** Phase 3 takes the runtime's process cwd. Phase 4+ may want per-session cwd from persona config; out of scope here. Document for follow-up.

**Verifies:** Mechanism for all AC3 — handler can reach ProcessManager via `cx.user().process_manager()`.

**Verification:**
- `cargo check -p pattern-runtime`.
- Existing `session_lifecycle.rs` tests still pass.

**Commit:** `[pattern-runtime] ProcessManager on TidepoolRuntime + SessionContext`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

---

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: Implement `ShellHandler` — dispatch `ShellReq` to `ProcessManager`

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/shell.rs` — replace stub.

**Implementation:**

Tighten bound from `HasCancelState` to `SessionContext`. The handler dispatch is async-flavoured (PTY is async) but `EffectHandler::handle` is synchronous — bridge via `tokio::runtime::Handle::current().block_on`. Other v3 handlers that need async work do the same; verify the pattern at `cx.user().db().get()` callsites — actually those are sync. The skill handler calls async via the eval worker; for the shell handler, the Tidepool eval worker is what's calling `handle()` — that's a dedicated OS thread (`crates/pattern_runtime/src/agent_loop/eval_worker.rs`), so blocking on a tokio runtime handle there is wrong (no current handle).

**Resolution:** the eval worker doesn't have a current tokio runtime. Two options:
1. Spawn a one-shot tokio runtime per shell call (expensive — each `Execute` pays runtime startup cost).
2. The runtime hands the eval worker a tokio `Handle` at startup, which the handler uses via `handle.block_on(future)`.

Option 2 is the cleaner fit. The `Handle` is cheap to clone, and the runtime already owns a tokio Runtime for provider work. Add a `tokio_handle: tokio::runtime::Handle` field to `ProcessManager` (or to `SessionContext`), populated at construction. The handler:

```rust
impl EffectHandler<SessionContext> for ShellHandler {
    type Request = ShellReq;

    fn handle(&mut self, req: ShellReq, cx: &EffectContext<'_, SessionContext>)
        -> Result<Value, EffectError>
    {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        let pm = cx.user().process_manager().clone();
        let cap = cx.user().capability_set().clone(); // Plan 3 accessor
        let handle = cx.user().tokio_handle().clone();

        match req {
            ShellReq::Execute(cmd) => {
                let result = handle.block_on(pm.execute(
                    &cap, &cmd, Duration::from_secs(30) // TODO Phase 3 follow-up: pass timeout from agent
                )).map_err(|e| EffectError::Handler(format!("Pattern.Shell.Execute: {e}")))?;
                cx.respond(serde_json::to_string(&result).unwrap_or_default())
            }
            ShellReq::Spawn(cmd) => {
                let task_id = handle.block_on(pm.spawn(&cap, &cmd))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Shell.Spawn: {e}")))?;
                // Spawn the listener task; pushes pseudo-messages via the
                // session adapter (Task 7).
                spawn_output_listener(&pm, &task_id, cx.user().adapter().clone(), handle.clone());
                cx.respond(task_id.to_string())
            }
            ShellReq::Kill(pid_int) => {
                let task_id = TaskId(pid_int.to_string());
                handle.block_on(pm.kill(&cap, &task_id))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Shell.Kill: {e}")))?;
                cx.respond(())
            }
            ShellReq::Status(_pid) => {
                // Per AC3.5 the operation lists all sessions; the i64 in the
                // request is unused (legacy Haskell signature placeholder).
                let tasks = pm.status();
                cx.respond(tasks.iter().map(|t| t.to_string()).collect::<Vec<_>>())
            }
        }
    }
}
```

**Note on `ShellReq::Status(i64)`:** the existing enum signature carries an `i64` even though AC3.5 says Status should list *all*. Two options: (a) ignore the i64 (current), (b) change the Haskell signature to take no arg. Defaulted to (a) for compatibility; flag for follow-up to clean up the GADT shape.

**Note on Execute's hardcoded 30s timeout:** the Haskell GADT `Execute :: Command -> Shell Text` doesn't pass a timeout. Two options: (a) hardcode a per-runtime default with a config knob, (b) add a `Execute2 :: Command -> Int -> Shell Text` variant. AC3.1 and AC3.7 imply a timeout is configurable. **Defaulted to (a)** with a 30s default + `RuntimeConfig::shell_default_timeout` knob; flag for follow-up.

**Verifies:** AC3.1, AC3.4, AC3.5, AC3.7.

**Verification:**
- `cargo check -p pattern-runtime`.
- Existing stub test deleted; new tests in Task 9.

**Commit:** `[pattern-runtime] ShellHandler dispatches to ProcessManager`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `render_shell_output_event` + spawn listener

**Files:**
- Modify: `crates/pattern_provider/src/compose/pseudo_messages.rs` — add `render_shell_output_event(task_id, chunk, at) -> ChatMessage` alongside `render_skill_loaded_event` / `render_file_edit_event`.
- Modify: `crates/pattern_runtime/src/process_manager/manager.rs` — add `spawn_output_listener(pm, task_id, adapter, tokio_handle)` that drains the broadcast receiver and pushes a pseudo-message per chunk via the adapter.
- Modify: `crates/pattern_runtime/src/sdk/handlers/shell.rs` (Task 6 callsite) — pass `cx.user().adapter().clone()` into `spawn_output_listener` at `Shell.Spawn` dispatch time.

**Implementation:**

```rust
// pattern_provider/src/compose/pseudo_messages.rs (addition)
pub fn render_shell_output_event(
    task_id: &str,
    chunk: ShellOutputChunkRef<'_>,   // borrowed view; same data as v2 OutputChunk
    at: jiff::Timestamp,
) -> ChatMessage {
    let mut body = format!("<system-reminder>\nshell task {task_id} @ {at}:\n");
    match chunk {
        ShellOutputChunkRef::Output(text) => {
            body.push_str("```\n");
            body.push_str(text);
            body.push_str("\n```");
        }
        ShellOutputChunkRef::Exit { code, duration_ms } => {
            body.push_str(&format!("[exited {code:?} in {duration_ms}ms]"));
        }
    }
    body.push_str("\n</system-reminder>");
    ChatMessage::user(body)
}
```

`ShellOutputChunkRef` is a borrow-compatible mirror of `process_manager::types::OutputChunk`; the renderer takes `&` so the listener doesn't need to clone. Defined in pattern_provider next to the renderer.

```rust
// process_manager/manager.rs — listener
pub(crate) fn spawn_output_listener(
    pm: &Arc<ProcessManager>,
    task_id: &TaskId,
    adapter: Arc<MemoryStoreAdapter>,
    handle: tokio::runtime::Handle,
) {
    let Some(mut rx) = pm.take_spawn_receiver(task_id) else { return };
    let task_id_str = task_id.to_string();
    let cancel = pm.cancel.clone();

    handle.spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => match msg {
                    Ok(OutputChunk::Output(text)) => {
                        let m = pattern_provider::compose::pseudo_messages::
                            render_shell_output_event(
                                &task_id_str,
                                ShellOutputChunkRef::Output(&text),
                                jiff::Timestamp::now(),
                            );
                        adapter.record_pseudo_message(m);
                    }
                    Ok(OutputChunk::Exit { code, duration_ms }) => {
                        let m = pattern_provider::compose::pseudo_messages::
                            render_shell_output_event(
                                &task_id_str,
                                ShellOutputChunkRef::Exit { code, duration_ms },
                                jiff::Timestamp::now(),
                            );
                        adapter.record_pseudo_message(m);
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(task_id = %task_id_str, lagged = n,
                                       "shell output broadcast lagged");
                    }
                }
            }
        }
    });
}
```

**Note on chunking granularity.** Pushing one pseudo-message per output chunk means a chatty process can produce many segment-2 entries. For very chunky processes the Segment2Pass may want to coalesce consecutive shell-output reminders for the same task — out of scope for this phase, but flag for follow-up.

**Verifies:** AC3.3.

**Verification:**
- `cargo check --workspace`.
- Unit test on `render_shell_output_event` — snapshot-test the body for both `Output` and `Exit` chunk variants via `insta`.
- Integration test in Task 9 exercises spawn → wait one turn → assert the spawned task's chunks appear in `most_recent_pseudo_messages` with the right task_id substring.

**Commit:** `[pattern-provider] [pattern-runtime] render_shell_output_event + spawn listener via adapter`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

<!-- START_SUBCOMPONENT_D (tasks 8-9) -->

<!-- START_TASK_8 -->
### Task 8: Process output logging — reliability backstop

**Files:**
- Create: `crates/pattern_runtime/src/process_manager/logger.rs` — `ProcessLogger` writing chunks to `<cache_dir>/shell/<task_id>.log`.
- Modify: `crates/pattern_runtime/src/process_manager/manager.rs` — call logger from inside `spawn_output_listener` (alongside the pending-output push).

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
| 3.3 | `spawn_streams_output_via_pseudo_messages` | Spawn `for i in 1 2 3; do echo line$i; sleep 0.05; done`; wait one turn; assert the next turn's `recent_pseudo_messages` (or `adapter.drain_pending_pseudo_messages()` directly) contains entries whose bodies include `line1`, `line2`, `line3`, and an `[exited` marker. |
| 3.4 | `kill_terminates_running_process` | Spawn `sleep 60`; immediately `kill(task_id)`; verify `status()` no longer lists the task; verify the broadcast `Exit` chunk arrives. |
| 3.5 | `status_lists_running_tasks` | Spawn two long-running processes; assert `status()` returns both task IDs. |
| 3.6 | `cwd_persists_across_executions` | `execute("cd /tmp")` then `execute("pwd")` → output contains `/tmp`. |
| 3.7 | `execute_timeout_kills_command` | `execute("sleep 60", 1s)` → returns `ShellError::Timeout(1s)` quickly (under 2s elapsed). |
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

**Q1: Execute timeout argument.** The Haskell GADT `Execute :: Command -> Shell Text` has no timeout. Phase 3 hardcodes a 30s default with a runtime-config knob. Cleaner: add `Execute2 :: Command -> Int -> Shell Text` (or change the existing) so agents can pass timeout. Defaulted to the hardcoded path; flag if reviewer wants the GADT change.

**Q2: `Status` ignoring its `i64` argument.** The current `ShellReq::Status(i64)` takes an unused arg. Possibilities: (a) ignore (current); (b) drop the arg from the GADT; (c) repurpose as "filter to this task ID". Defaulted to (a); reviewer may prefer (b) or (c).

**Q3: `ShellError::ProcessNotFound` vs `ShellError::UnknownTask`.** AC3.8 names `ProcessNotFound`. v2 (and this plan) use `UnknownTask`. Defaulted to `UnknownTask` (minimises drift from v2); reviewer may prefer renaming to match the AC text exactly.

**Q4: Per-session ProcessManager vs runtime-global.** Plan defaults to runtime-global (one shared ProcessManager across all sessions). This means session A's `cd /tmp` is visible to session B's next `pwd` — not ideal for multi-agent isolation. Alternative: per-session ProcessManager (matches FileManager pattern). Defaulted to runtime-global per the design plan's claim; flag if reviewer wants per-session for isolation.

**Q5: Process log retention.** The plan ships unbounded growth in `<cache_dir>/shell/`. Plan 1 has GFS-style backup rotation; the same hook could apply here. Out of scope this phase; flag whether to add even a basic cap (e.g., delete logs > 30 days).
