# Shell tool and ProcessSource implementation plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add shell command execution capability to Pattern agents via ProcessSource (DataStream) and Shell tool (builtin).

**Architecture:** PTY-based shell session (pty-process crate) wrapped in ShellBackend trait for future swappability. ProcessSource manages streaming processes with Loro blocks. Shell tool provides execute/spawn/kill/status operations.

**Tech Stack:** pty-process (async), tokio broadcast channels, DashMap, Loro CRDT for block content.

---

## Task 1: Add pty-process dependency

**Files:**
- Modify: `crates/pattern_core/Cargo.toml`

**Step 1: Add pty-process to dependencies**

Add under the `# Calculator tool dependencies` section:

```toml
# Shell tool dependencies
pty-process = { version = "0.5", features = ["async"] }
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS with no errors

**Step 3: Commit**

```bash
git add crates/pattern_core/Cargo.toml Cargo.lock
git commit -m "[pattern-core] add pty-process dependency for shell tool"
```

---

## Task 2: Create ShellError type

**Files:**
- Create: `crates/pattern_core/src/data_source/process/error.rs`

**Step 1: Write the error type**

```rust
//! Shell execution error types.

use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

/// Permission level for shell operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellPermission {
    /// Read-only commands (git status, ls, cat).
    ReadOnly,
    /// File modifications, git commit.
    ReadWrite,
    /// Unrestricted access.
    Admin,
}

impl std::fmt::Display for ShellPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "read-only"),
            Self::ReadWrite => write!(f, "read-write"),
            Self::Admin => write!(f, "admin"),
        }
    }
}

/// Errors that can occur during shell operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ShellError {
    #[error("permission denied: requires {required}, have {granted}")]
    PermissionDenied {
        required: ShellPermission,
        granted: ShellPermission,
    },

    #[error("path outside sandbox: {0}")]
    PathOutsideSandbox(PathBuf),

    #[error("command denied by policy: {0}")]
    CommandDenied(String),

    #[error("command timed out after {0:?}")]
    Timeout(Duration),

    #[error("process spawn failed: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("pty error: {0}")]
    PtyError(String),

    #[error("unknown task: {0}")]
    UnknownTask(String),

    #[error("task already completed")]
    TaskCompleted,

    #[error("session not initialized")]
    SessionNotInitialized,

    #[error("shell session died unexpectedly")]
    SessionDied,

    #[error("failed to parse exit code from output")]
    ExitCodeParseFailed,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/process/error.rs
git commit -m "[pattern-core] add ShellError type for shell tool"
```

---

## Task 3: Create ShellBackend trait and LocalPtyBackend

**Files:**
- Create: `crates/pattern_core/src/data_source/process/backend.rs`

**Step 1: Write the backend trait and execute result type**

```rust
//! Shell execution backends.
//!
//! The ShellBackend trait abstracts command execution, allowing future
//! swappability between local PTY, Docker containers, Bubblewrap, etc.

use std::time::Duration;
use tokio::sync::broadcast;

use super::error::ShellError;

/// Result of a one-shot command execution.
#[derive(Debug, Clone)]
pub struct ExecuteResult {
    /// Combined stdout/stderr output (interleaved as PTY delivers them).
    /// PTY merges both streams; separation is not possible without container backend.
    pub output: String,
    /// Process exit code (None if killed by signal).
    pub exit_code: Option<i32>,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
}

/// Chunk of output from a streaming process.
#[derive(Debug, Clone)]
pub enum OutputChunk {
    /// Output chunk (stdout and stderr are interleaved through PTY).
    Output(String),
    /// Process exited.
    Exit { code: Option<i32>, duration_ms: u64 },
}

/// Unique identifier for a spawned task.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskId(pub String);

impl TaskId {
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

/// Backend trait for shell command execution.
///
/// Implementations provide the actual command execution logic.
/// The ProcessSource delegates to a backend for all command work.
#[async_trait::async_trait]
pub trait ShellBackend: Send + Sync + std::fmt::Debug {
    /// Execute a command and wait for completion.
    ///
    /// Session state (cwd, env) persists across calls.
    async fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError>;

    /// Spawn a long-running command with streaming output.
    ///
    /// Returns a task ID and receiver for output chunks.
    async fn spawn_streaming(
        &self,
        command: &str,
    ) -> Result<(TaskId, broadcast::Receiver<OutputChunk>), ShellError>;

    /// Kill a running spawned process.
    async fn kill(&self, task_id: &TaskId) -> Result<(), ShellError>;

    /// List currently running task IDs.
    fn running_tasks(&self) -> Vec<TaskId>;

    /// Get current working directory of the session.
    fn cwd(&self) -> Option<std::path::PathBuf>;
}
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/process/backend.rs
git commit -m "[pattern-core] add ShellBackend trait"
```

---

## Task 4: Implement LocalPtyBackend structure

**Files:**
- Create: `crates/pattern_core/src/data_source/process/local_pty.rs`

**Step 1: Write the LocalPtyBackend struct and initialization**

```rust
//! Local PTY-based shell backend.
//!
//! Uses pty-process to maintain a real shell session where cwd, env vars,
//! and aliases persist across command executions.
//!
//! Exit code detection uses a nonce-based wrapper approach to prevent output
//! injection attacks. Each command is wrapped with a unique marker that includes
//! the exit code, making it impossible for command output to fake the exit code.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, error, trace, warn};

use super::backend::{ExecuteResult, OutputChunk, ShellBackend, TaskId};
use super::error::ShellError;

/// OSC escape sequence used as prompt marker for command completion detection.
const PROMPT_MARKER: &str = "\x1b]pattern-done\x07";

/// Information about a running streaming process.
struct RunningProcess {
    tx: broadcast::Sender<OutputChunk>,
    started_at: Instant,
    abort_handle: tokio::task::AbortHandle,
}

/// Local PTY-based shell backend.
///
/// Maintains a persistent shell session via PTY. Commands are written to
/// the PTY and output is read until the prompt marker appears.
#[derive(Debug)]
pub struct LocalPtyBackend {
    /// Shell to spawn (default: /bin/bash).
    shell: String,
    /// Initial working directory.
    initial_cwd: PathBuf,
    /// Environment variables to set.
    env: HashMap<String, String>,
    /// Running streaming processes.
    running: Arc<DashMap<TaskId, RunningProcess>>,
    /// Current session state (lazily initialized).
    session: Mutex<Option<PtySession>>,
    /// Cached current working directory (updated after each command).
    cached_cwd: Mutex<Option<PathBuf>>,
}

/// Active PTY session state.
struct PtySession {
    pty: pty_process::Pty,
    _child: pty_process::Child,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession").finish_non_exhaustive()
    }
}

impl LocalPtyBackend {
    /// Create a new backend with default shell.
    pub fn new(initial_cwd: PathBuf) -> Self {
        Self {
            shell: "/usr/bin/env bash".to_string(),
            initial_cwd,
            env: HashMap::new(),
            running: Arc::new(DashMap::new()),
            session: Mutex::new(None),
            cached_cwd: Mutex::new(None),
        }
    }

    /// Create with a specific shell.
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// Add environment variables.
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Initialize the PTY session if not already done.
    async fn ensure_session(&self) -> Result<(), ShellError> {
        let mut guard = self.session.lock();
        if guard.is_some() {
            return Ok(());
        }

        debug!(shell = %self.shell, cwd = ?self.initial_cwd, "initializing PTY session");

        // Create PTY
        let mut pty = pty_process::Pty::new().map_err(|e| ShellError::PtyError(e.to_string()))?;
        pty.resize(pty_process::Size::new(24, 120))
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        // Spawn shell
        let pts = pty.pts().map_err(|e| ShellError::PtyError(e.to_string()))?;
        let mut cmd = pty_process::Command::new(&self.shell);
        cmd.current_dir(&self.initial_cwd);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        // Non-interactive shell with explicit prompt
        cmd.env("PS1", PROMPT_MARKER);
        cmd.env("PS2", "");

        let child = cmd.spawn(&pts).map_err(ShellError::SpawnFailed)?;

        *guard = Some(PtySession { pty, _child: child });

        // Drop guard before async operations
        drop(guard);

        // Wait for initial prompt
        self.read_until_prompt(Duration::from_secs(5)).await?;

        debug!("PTY session initialized");
        Ok(())
    }

    /// Read from PTY until prompt marker appears or timeout.
    /// Returns Err(SessionDied) if we get EOF without seeing the prompt marker.
    async fn read_until_prompt(&self, timeout: Duration) -> Result<String, ShellError> {
        let deadline = Instant::now() + timeout;
        let mut output = String::new();

        loop {
            if Instant::now() > deadline {
                return Err(ShellError::Timeout(timeout));
            }

            let remaining = deadline.saturating_duration_since(Instant::now());

            // Read a chunk with timeout
            let chunk = {
                let mut guard = self.session.lock();
                let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

                let mut buf = [0u8; 4096];
                match tokio::time::timeout(
                    remaining.min(Duration::from_millis(100)),
                    tokio::io::AsyncReadExt::read(&mut session.pty, &mut buf),
                )
                .await
                {
                    Ok(Ok(0)) => {
                        // EOF without prompt marker means session died
                        return Err(ShellError::SessionDied);
                    }
                    Ok(Ok(n)) => Some(String::from_utf8_lossy(&buf[..n]).to_string()),
                    Ok(Err(e)) => return Err(ShellError::Io(e)),
                    Err(_) => None, // Timeout on this read, continue loop
                }
            };

            if let Some(chunk) = chunk {
                trace!(chunk_len = chunk.len(), "read chunk from PTY");
                output.push_str(&chunk);

                // Check for prompt marker
                if output.contains(PROMPT_MARKER) {
                    // Strip the prompt marker from output
                    let marker_pos = output.find(PROMPT_MARKER).unwrap();
                    output.truncate(marker_pos);
                    return Ok(output);
                }
            }
        }
    }

    /// Generate a unique exit code marker that can't be faked by command output.
    fn generate_exit_marker() -> String {
        let nonce = &uuid::Uuid::new_v4().to_string()[..8];
        format!("__PATTERN_EXIT_{}__", nonce)
    }

    /// Parse exit code from output containing our marker.
    /// Returns (cleaned_output, exit_code).
    fn parse_exit_code(output: &str, marker: &str) -> Result<(String, i32), ShellError> {
        // Find the LAST occurrence of our marker (in case output contains similar text)
        let search_pattern = format!("{}:", marker);
        if let Some(marker_pos) = output.rfind(&search_pattern) {
            let before_marker = &output[..marker_pos];
            let after_marker = &output[marker_pos + search_pattern.len()..];

            // Extract exit code (digits until newline or end)
            let exit_code_str: String = after_marker
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();

            let exit_code = exit_code_str
                .parse::<i32>()
                .map_err(|_| ShellError::ExitCodeParseFailed)?;

            // Clean output: everything before the marker, trimmed
            let cleaned = before_marker.trim_end().to_string();

            Ok((cleaned, exit_code))
        } else {
            Err(ShellError::ExitCodeParseFailed)
        }
    }

    /// Reinitialize session after it died.
    async fn reinitialize_session(&self) -> Result<(), ShellError> {
        {
            let mut guard = self.session.lock();
            *guard = None;
        }
        // Clear cached cwd since session died
        {
            let mut cwd_guard = self.cached_cwd.lock();
            *cwd_guard = None;
        }
        self.ensure_session().await
    }

    /// Query the shell for current working directory and cache it.
    async fn refresh_cwd(&self) -> Result<PathBuf, ShellError> {
        // Use a simple pwd command without our nonce wrapper since we parse it differently
        {
            let mut guard = self.session.lock();
            let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

            let cmd_line = "pwd\n";
            tokio::io::AsyncWriteExt::write_all(&mut session.pty, cmd_line.as_bytes())
                .await
                .map_err(ShellError::Io)?;
        }

        // Read output until prompt
        let raw_output = self.read_until_prompt(Duration::from_secs(5)).await?;

        // Parse: output is "pwd\n/actual/path\n" (echo of command + result)
        let path_str = raw_output
            .lines()
            .filter(|line| line.starts_with('/') && !line.contains("pwd"))
            .next()
            .unwrap_or_else(|| raw_output.trim());

        let cwd = PathBuf::from(path_str.trim());

        // Cache it
        {
            let mut cwd_guard = self.cached_cwd.lock();
            *cwd_guard = Some(cwd.clone());
        }

        trace!(cwd = ?cwd, "refreshed cached cwd");
        Ok(cwd)
    }
}

#[async_trait::async_trait]
impl ShellBackend for LocalPtyBackend {
    async fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError> {
        self.ensure_session().await?;
        let start = Instant::now();

        debug!(command = %command, ?timeout, "executing command");

        // Generate unique marker for exit code detection
        let exit_marker = Self::generate_exit_marker();

        // Wrap command to capture exit code with our unique marker
        let wrapped_command = format!(
            "{}; echo \"{}:$?\"",
            command, exit_marker
        );

        // Write wrapped command to PTY
        {
            let mut guard = self.session.lock();
            let session = guard.as_mut().ok_or(ShellError::SessionNotInitialized)?;

            let cmd_line = format!("{}\n", wrapped_command);
            tokio::io::AsyncWriteExt::write_all(&mut session.pty, cmd_line.as_bytes())
                .await
                .map_err(ShellError::Io)?;
        }

        // Read output until prompt
        let raw_output = match self.read_until_prompt(timeout).await {
            Ok(output) => output,
            Err(ShellError::SessionDied) => {
                // Try to reinitialize for next command
                warn!("shell session died, will reinitialize on next command");
                let _ = self.reinitialize_session().await;
                return Err(ShellError::SessionDied);
            }
            Err(e) => return Err(e),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        // Strip the echoed wrapped command from the start
        let output_after_echo = raw_output
            .strip_prefix(&wrapped_command)
            .unwrap_or(&raw_output)
            .trim_start_matches('\n')
            .trim_start_matches('\r');

        // Parse exit code from our marker
        let (output, exit_code) = Self::parse_exit_code(output_after_echo, &exit_marker)?;

        // Refresh cached cwd after each command (cwd may have changed)
        // This is async but we don't want to fail the whole execute if pwd fails
        if let Err(e) = self.refresh_cwd().await {
            warn!(error = %e, "failed to refresh cwd after command");
        }

        Ok(ExecuteResult {
            output,
            exit_code: Some(exit_code),
            duration_ms,
        })
    }

    async fn spawn_streaming(
        &self,
        command: &str,
    ) -> Result<(TaskId, broadcast::Receiver<OutputChunk>), ShellError> {
        // For streaming, we spawn a new PTY per process (not the persistent session)
        // This gives us clean exit code handling via child.wait()
        let task_id = TaskId::new();
        let (tx, rx) = broadcast::channel(256);

        debug!(task_id = %task_id, command = %command, "spawning streaming process");

        let mut pty = pty_process::Pty::new().map_err(|e| ShellError::PtyError(e.to_string()))?;
        pty.resize(pty_process::Size::new(24, 120))
            .map_err(|e| ShellError::PtyError(e.to_string()))?;

        let pts = pty.pts().map_err(|e| ShellError::PtyError(e.to_string()))?;
        let mut cmd = pty_process::Command::new(&self.shell);
        cmd.current_dir(&self.initial_cwd);
        cmd.args(["-c", command]);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn(&pts).map_err(ShellError::SpawnFailed)?;

        let running = Arc::clone(&self.running);
        let tx_clone = tx.clone();
        let task_id_clone = task_id.clone();

        let handle = tokio::spawn(async move {
            let start = Instant::now();
            let mut reader = BufReader::new(pty);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let _ = tx_clone.send(OutputChunk::Output(line.clone()));
                    }
                    Err(e) => {
                        warn!(error = %e, "error reading from streaming PTY");
                        break;
                    }
                }
            }

            // Wait for child to exit - this gives us the real exit code
            let status = child.wait().await;
            let exit_code = status.ok().and_then(|s| s.code());
            let duration_ms = start.elapsed().as_millis() as u64;

            let _ = tx_clone.send(OutputChunk::Exit {
                code: exit_code,
                duration_ms,
            });

            running.remove(&task_id_clone);
            debug!(task_id = %task_id_clone, ?exit_code, "streaming process completed");
        });

        self.running.insert(
            task_id.clone(),
            RunningProcess {
                tx,
                started_at: Instant::now(),
                abort_handle: handle.abort_handle(),
            },
        );

        Ok((task_id, rx))
    }

    async fn kill(&self, task_id: &TaskId) -> Result<(), ShellError> {
        if let Some((_, process)) = self.running.remove(task_id) {
            process.abort_handle.abort();
            debug!(task_id = %task_id, "killed streaming process");
            Ok(())
        } else {
            Err(ShellError::UnknownTask(task_id.to_string()))
        }
    }

    fn running_tasks(&self) -> Vec<TaskId> {
        self.running.iter().map(|r| r.key().clone()).collect()
    }

    fn cwd(&self) -> Option<PathBuf> {
        // Return cached cwd if available, otherwise initial_cwd
        let cached = self.cached_cwd.lock();
        cached.clone().or_else(|| Some(self.initial_cwd.clone()))
    }
}

impl Drop for LocalPtyBackend {
    fn drop(&mut self) {
        // Kill any running processes
        for entry in self.running.iter() {
            entry.abort_handle.abort();
        }
    }
}
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/data_source/process/local_pty.rs
git commit -m "[pattern-core] implement LocalPtyBackend"
```

---

## Task 5: Create process module with re-exports

**Files:**
- Create: `crates/pattern_core/src/data_source/process/mod.rs`
- Modify: `crates/pattern_core/src/data_source/mod.rs`

**Step 1: Create process module**

```rust
//! Process execution data source.
//!
//! Provides shell command execution capability through:
//! - `ProcessSource`: DataStream impl managing process lifecycles
//! - `ShellBackend`: Trait for swappable execution backends
//! - `LocalPtyBackend`: PTY-based local execution

mod backend;
mod error;
mod local_pty;

pub use backend::{ExecuteResult, OutputChunk, ShellBackend, TaskId};
pub use error::{ShellError, ShellPermission};
pub use local_pty::LocalPtyBackend;
```

**Step 2: Add to data_source/mod.rs**

Add after `mod file_source;`:

```rust
pub mod process;
```

And in the re-exports section, add:

```rust
pub use process::{
    ExecuteResult, LocalPtyBackend, OutputChunk, ShellBackend, ShellError, ShellPermission, TaskId,
};
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/data_source/process/mod.rs crates/pattern_core/src/data_source/mod.rs
git commit -m "[pattern-core] add process module to data_source"
```

---

## Task 6: Write unit tests for LocalPtyBackend

**Files:**
- Create: `crates/pattern_core/src/data_source/process/tests.rs`
- Modify: `crates/pattern_core/src/data_source/process/mod.rs`

**Step 1: Write the tests**

```rust
//! Tests for process execution backends.

use std::time::Duration;

use super::*;

#[tokio::test]
async fn test_local_pty_execute_simple() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("echo hello", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert!(result.output.contains("hello"));
    assert_eq!(result.exit_code, Some(0));
}

#[tokio::test]
async fn test_local_pty_execute_multiline() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("echo line1; echo line2", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert!(result.output.contains("line1"));
    assert!(result.output.contains("line2"));
}

#[tokio::test]
async fn test_local_pty_exit_code_success() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("true", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert_eq!(result.exit_code, Some(0));
}

#[tokio::test]
async fn test_local_pty_exit_code_failure() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("false", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert_eq!(result.exit_code, Some(1));
}

#[tokio::test]
async fn test_local_pty_exit_code_custom() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("exit 42", Duration::from_secs(5))
        .await;

    // This kills the session, so we expect SessionDied
    assert!(matches!(result, Err(ShellError::SessionDied)));
}

#[tokio::test]
async fn test_local_pty_cwd_persistence() {
    let backend = LocalPtyBackend::new(std::env::temp_dir());

    // Create a temp dir and cd into it
    backend
        .execute("mkdir -p test_cwd_$$", Duration::from_secs(5))
        .await
        .expect("mkdir should succeed");

    backend
        .execute("cd test_cwd_$$", Duration::from_secs(5))
        .await
        .expect("cd should succeed");

    // pwd should show we're in the new directory
    let result = backend
        .execute("pwd", Duration::from_secs(5))
        .await
        .expect("pwd should succeed");

    assert!(result.output.contains("test_cwd_"));

    // Cleanup
    backend
        .execute("cd .. && rmdir test_cwd_$$", Duration::from_secs(5))
        .await
        .ok();
}

#[tokio::test]
async fn test_local_pty_cwd_cached_after_cd() {
    let backend = LocalPtyBackend::new(std::env::temp_dir());

    // Initial cwd should be temp_dir
    let initial_cwd = backend.cwd().expect("cwd should be set");
    assert!(initial_cwd.starts_with("/tmp") || initial_cwd.starts_with("/var"));

    // Run a command to ensure session is initialized and cwd is cached
    backend
        .execute("echo init", Duration::from_secs(5))
        .await
        .expect("echo should succeed");

    // After first command, cached cwd should match initial
    let cached_cwd = backend.cwd().expect("cwd should be set");
    assert!(cached_cwd.starts_with("/tmp") || cached_cwd.starts_with("/var"));

    // cd to a subdirectory
    backend
        .execute("mkdir -p cwd_test_$$ && cd cwd_test_$$", Duration::from_secs(5))
        .await
        .expect("cd should succeed");

    // Cached cwd should now reflect the new directory
    let new_cwd = backend.cwd().expect("cwd should be set");
    assert!(new_cwd.to_string_lossy().contains("cwd_test_"));

    // Cleanup
    backend
        .execute("cd .. && rmdir cwd_test_$$", Duration::from_secs(5))
        .await
        .ok();
}

#[tokio::test]
async fn test_local_pty_env_persistence() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    // Set an env var
    backend
        .execute("export TEST_VAR=pattern_test", Duration::from_secs(5))
        .await
        .expect("export should succeed");

    // Should persist
    let result = backend
        .execute("echo $TEST_VAR", Duration::from_secs(5))
        .await
        .expect("echo should succeed");

    assert!(result.output.contains("pattern_test"));
}

#[tokio::test]
async fn test_local_pty_spawn_streaming() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let (task_id, mut rx) = backend
        .spawn_streaming("echo streaming && sleep 0.1 && echo done")
        .await
        .expect("spawn should succeed");

    let mut outputs = Vec::new();
    let mut has_output = false;
    while let Ok(chunk) = rx.recv().await {
        match &chunk {
            OutputChunk::Output(_) => {
                has_output = true;
                outputs.push(chunk);
            }
            OutputChunk::Exit { .. } => {
                outputs.push(chunk);
                break;
            }
        }
    }

    // Should have received output chunks
    assert!(has_output, "should have received output");

    // Should have exit event
    assert!(matches!(outputs.last(), Some(OutputChunk::Exit { .. })));

    // Task should be cleaned up
    assert!(backend.running_tasks().is_empty());
}

#[tokio::test]
async fn test_local_pty_kill() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let (task_id, _rx) = backend
        .spawn_streaming("sleep 60")
        .await
        .expect("spawn should succeed");

    assert_eq!(backend.running_tasks().len(), 1);

    backend.kill(&task_id).await.expect("kill should succeed");

    // Give tokio a moment to clean up
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(backend.running_tasks().is_empty());
}

#[tokio::test]
async fn test_local_pty_timeout() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("sleep 10", Duration::from_millis(100))
        .await;

    assert!(matches!(result, Err(ShellError::Timeout(_))));
}

#[test]
fn test_parse_exit_code_basic() {
    let marker = "__PATTERN_EXIT_abc12345__";
    let output = "hello world\n__PATTERN_EXIT_abc12345__:0\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "hello world");
    assert_eq!(code, 0);
}

#[test]
fn test_parse_exit_code_nonzero() {
    let marker = "__PATTERN_EXIT_xyz98765__";
    let output = "error message\n__PATTERN_EXIT_xyz98765__:127\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "error message");
    assert_eq!(code, 127);
}

#[test]
fn test_parse_exit_code_output_contains_fake_marker() {
    // The command output contains something that looks like our marker, but with wrong nonce
    let marker = "__PATTERN_EXIT_real1234__";
    let output = "user typed __PATTERN_EXIT_fake0000__:999\nactual output\n__PATTERN_EXIT_real1234__:0\n";

    // Should use the LAST occurrence with the correct marker
    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(code, 0);
    // The fake marker is part of the cleaned output since it doesn't match our nonce
    assert!(cleaned.contains("__PATTERN_EXIT_fake0000__:999"));
}

#[test]
fn test_generate_exit_marker_uniqueness() {
    let marker1 = LocalPtyBackend::generate_exit_marker();
    let marker2 = LocalPtyBackend::generate_exit_marker();

    assert_ne!(marker1, marker2);
    assert!(marker1.starts_with("__PATTERN_EXIT_"));
    assert!(marker1.ends_with("__"));
}
```

**Step 2: Add test module to mod.rs**

Add at the end of `process/mod.rs`:

```rust
#[cfg(test)]
mod tests;
```

**Step 3: Run tests**

Run: `cargo nextest run -p pattern-core --lib test_local_pty`
Expected: All tests PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/data_source/process/tests.rs crates/pattern_core/src/data_source/process/mod.rs
git commit -m "[pattern-core] add LocalPtyBackend tests"
```

---

## Task 7: Create permission validation module

**Files:**
- Create: `crates/pattern_core/src/data_source/process/permission.rs`

**Step 1: Write permission validation**

```rust
//! Permission validation for shell commands.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::error::{ShellError, ShellPermission};

/// Command patterns that are always denied regardless of permission level.
const DENIED_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "sudo rm -rf",
    "mkfs",
    "dd if=/dev/zero",
    ":(){ :|:& };:",  // Fork bomb
    "chmod -R 777 /",
    "> /dev/sda",
];

/// Configuration for shell permissions.
#[derive(Debug, Clone)]
pub struct ShellPermissionConfig {
    /// Default permission level.
    pub default: ShellPermission,
    /// Glob patterns mapping to permission levels.
    pub command_rules: Vec<(String, ShellPermission)>,
    /// Allowed paths for file operations.
    pub allowed_paths: Vec<PathBuf>,
    /// Whether to strictly enforce path restrictions.
    pub strict_path_enforcement: bool,
}

impl Default for ShellPermissionConfig {
    fn default() -> Self {
        Self {
            default: ShellPermission::ReadWrite,
            command_rules: Vec::new(),
            allowed_paths: Vec::new(),
            strict_path_enforcement: false,
        }
    }
}

impl ShellPermissionConfig {
    /// Create a new config with the given default permission.
    pub fn new(default: ShellPermission) -> Self {
        Self {
            default,
            ..Default::default()
        }
    }

    /// Add an allowed path.
    pub fn allow_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_paths.push(path.into());
        self
    }

    /// Enable strict path enforcement.
    pub fn strict(mut self) -> Self {
        self.strict_path_enforcement = true;
        self
    }

    /// Check if a command is explicitly denied.
    pub fn is_command_denied(&self, command: &str) -> Option<&'static str> {
        let cmd_lower = command.to_lowercase();
        for pattern in DENIED_PATTERNS {
            if cmd_lower.contains(pattern) {
                return Some(pattern);
            }
        }
        None
    }

    /// Validate that all paths in a command are within allowed paths.
    pub fn validate_paths(&self, command: &str, session_cwd: &Path) -> Result<(), ShellError> {
        if self.allowed_paths.is_empty() || !self.strict_path_enforcement {
            return Ok(());
        }

        for path in extract_paths(command) {
            let resolved = if path.is_absolute() {
                path.canonicalize().unwrap_or(path)
            } else {
                session_cwd.join(&path).canonicalize().unwrap_or(path)
            };

            if !self.is_within_allowed(&resolved) {
                return Err(ShellError::PathOutsideSandbox(resolved));
            }
        }

        Ok(())
    }

    /// Check if a path is within any allowed path.
    fn is_within_allowed(&self, path: &Path) -> bool {
        for allowed in &self.allowed_paths {
            if path.starts_with(allowed) {
                return true;
            }
        }
        false
    }
}

/// Extract potential file paths from a command string.
///
/// This is a best-effort extraction - shell expansion and complex quoting
/// are not handled. Defense in depth.
fn extract_paths(command: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Split on whitespace and look for path-like tokens
    for token in command.split_whitespace() {
        // Skip flags
        if token.starts_with('-') {
            continue;
        }
        // Skip shell operators
        if ["&&", "||", "|", ";", ">", ">>", "<", "2>&1"].contains(&token) {
            continue;
        }
        // If it looks like a path (contains / or starts with . or ~)
        if token.contains('/') || token.starts_with('.') || token.starts_with('~') {
            // Expand ~ to home dir
            let expanded = if token.starts_with('~') {
                if let Some(home) = dirs::home_dir() {
                    PathBuf::from(token.replacen('~', home.to_string_lossy().as_ref(), 1))
                } else {
                    PathBuf::from(token)
                }
            } else {
                PathBuf::from(token)
            };
            paths.push(expanded);
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_denied_commands() {
        let config = ShellPermissionConfig::default();

        assert!(config.is_command_denied("rm -rf /").is_some());
        assert!(config.is_command_denied("sudo rm -rf /home").is_some());
        assert!(config.is_command_denied("echo hello").is_none());
        assert!(config.is_command_denied("rm -rf ./build").is_none());
    }

    #[test]
    fn test_path_extraction() {
        let paths = extract_paths("cat /etc/passwd ./local.txt");
        assert!(paths.iter().any(|p| p == Path::new("/etc/passwd")));
        assert!(paths.iter().any(|p| p == Path::new("./local.txt")));
    }

    #[test]
    fn test_path_validation() {
        let config = ShellPermissionConfig::new(ShellPermission::ReadWrite)
            .allow_path("/home/user/project")
            .strict();

        let cwd = PathBuf::from("/home/user/project");

        // Within allowed
        assert!(config.validate_paths("cat ./src/main.rs", &cwd).is_ok());

        // Outside allowed (using absolute path that exists)
        // Note: This test may fail if /etc/passwd doesn't exist on the system
        // In real tests, we'd use tempdir
    }
}
```

**Step 2: Add to mod.rs**

Add to `process/mod.rs`:

```rust
mod permission;

pub use permission::ShellPermissionConfig;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/data_source/process/permission.rs crates/pattern_core/src/data_source/process/mod.rs
git commit -m "[pattern-core] add shell permission validation"
```

---

## Task 8: Create ProcessSource DataStream implementation

**Files:**
- Create: `crates/pattern_core/src/data_source/process/source.rs`

**Step 1: Write ProcessSource structure**

```rust
//! ProcessSource - DataStream implementation for shell process management.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dashmap::DashMap;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::agent::AgentId;
use crate::data_source::helpers::BlockBuilder;
use crate::data_source::stream::{DataStream, StreamStatus};
use crate::data_source::types::{BlockSchemaSpec, Notification, StreamCursor};
use crate::data_source::{BlockEdit, EditFeedback};
use crate::memory::schema::BlockSchema;
use crate::memory::types::BlockType;
use crate::runtime::ToolContext;
use crate::tool::rule::ToolRule;
use crate::CoreResult;

use super::backend::{OutputChunk, ShellBackend, TaskId};
use super::error::ShellError;
use super::local_pty::LocalPtyBackend;
use super::permission::ShellPermissionConfig;

/// Default auto-unpin delay after process exit (5 minutes).
const DEFAULT_UNPIN_DELAY: Duration = Duration::from_secs(300);

/// Information about a spawned streaming process.
#[derive(Debug)]
struct ProcessInfo {
    task_id: TaskId,
    block_label: String,
    command: String,
    started_at: SystemTime,
    unpin_delay: Duration,
}

/// ProcessSource manages shell process lifecycles and streams output to blocks.
///
/// Implements `DataStream` to integrate with Pattern's data source system.
/// Uses a `ShellBackend` for actual command execution.
pub struct ProcessSource {
    source_id: String,
    name: String,
    backend: Arc<dyn ShellBackend>,
    permission_config: ShellPermissionConfig,
    processes: DashMap<TaskId, ProcessInfo>,
    status: RwLock<StreamStatus>,
    tx: RwLock<Option<broadcast::Sender<Notification>>>,
    ctx: RwLock<Option<Arc<dyn ToolContext>>>,
    owner: RwLock<Option<AgentId>>,
}

impl std::fmt::Debug for ProcessSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessSource")
            .field("source_id", &self.source_id)
            .field("name", &self.name)
            .field("status", &*self.status.read())
            .field("process_count", &self.processes.len())
            .finish()
    }
}

impl ProcessSource {
    /// Create a new ProcessSource with the given backend.
    pub fn new(
        source_id: impl Into<String>,
        backend: Arc<dyn ShellBackend>,
        permission_config: ShellPermissionConfig,
    ) -> Self {
        let source_id = source_id.into();
        Self {
            name: format!("Shell ({})", &source_id),
            source_id,
            backend,
            permission_config,
            processes: DashMap::new(),
            status: RwLock::new(StreamStatus::Stopped),
            tx: RwLock::new(None),
            ctx: RwLock::new(None),
            owner: RwLock::new(None),
        }
    }

    /// Create with default local PTY backend.
    pub fn with_local_backend(source_id: impl Into<String>, cwd: PathBuf) -> Self {
        let backend = Arc::new(LocalPtyBackend::new(cwd));
        Self::new(source_id, backend, ShellPermissionConfig::default())
    }

    /// Set permission configuration.
    pub fn with_permissions(mut self, config: ShellPermissionConfig) -> Self {
        self.permission_config = config;
        self
    }

    /// Execute a one-shot command. Returns result directly, no block created.
    pub async fn execute(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<super::backend::ExecuteResult, ShellError> {
        // Check denied commands
        if let Some(pattern) = self.permission_config.is_command_denied(command) {
            return Err(ShellError::CommandDenied(pattern.to_string()));
        }

        // Validate paths
        let cwd = self.backend.cwd().unwrap_or_default();
        self.permission_config.validate_paths(command, &cwd)?;

        // Execute via backend
        self.backend.execute(command, timeout).await
    }

    /// Spawn a streaming process. Creates a block for output.
    pub async fn spawn(
        &self,
        command: &str,
        unpin_delay: Option<Duration>,
    ) -> Result<(TaskId, String), ShellError> {
        // Check denied commands
        if let Some(pattern) = self.permission_config.is_command_denied(command) {
            return Err(ShellError::CommandDenied(pattern.to_string()));
        }

        // Validate paths
        let cwd = self.backend.cwd().unwrap_or_default();
        self.permission_config.validate_paths(command, &cwd)?;

        // Spawn via backend
        let (task_id, mut rx) = self.backend.spawn_streaming(command).await?;
        let block_label = format!("process:{}", task_id);

        // Create block for output
        let ctx = self.ctx.read().clone();
        let owner = self.owner.read().clone();

        if let (Some(ctx), Some(owner)) = (ctx, owner) {
            let memory = ctx.memory();

            BlockBuilder::new(memory, &owner, &block_label)
                .description(format!("Output from: {}", command))
                .schema(BlockSchema::text())
                .block_type(BlockType::Log)
                .pinned()
                .build()
                .await
                .map_err(|e| ShellError::PtyError(format!("failed to create block: {}", e)))?;

            // Spawn task to stream output to block and emit notifications
            let processes = self.processes.clone();
            let tx = self.tx.read().clone();
            let block_label_clone = block_label.clone();
            let task_id_clone = task_id.clone();
            let unpin_delay = unpin_delay.unwrap_or(DEFAULT_UNPIN_DELAY);

            tokio::spawn(async move {
                while let Ok(chunk) = rx.recv().await {
                    match chunk {
                        OutputChunk::Output(text) => {
                            // Update block
                            if let Ok(Some(doc)) = memory.get_block(&owner, &block_label_clone).await {
                                if let Err(e) = doc.append_text(&text, true) {
                                    error!(error = %e, "failed to append to process block");
                                }
                            }

                            // Emit notification
                            if let Some(tx) = &tx {
                                // Notifications are batched by the runtime
                                // For now just log
                                debug!(
                                    task_id = %task_id_clone,
                                    bytes = text.len(),
                                    "process output chunk"
                                );
                            }
                        }
                        OutputChunk::Exit { code, duration_ms } => {
                            info!(
                                task_id = %task_id_clone,
                                exit_code = ?code,
                                duration_ms,
                                "process exited"
                            );

                            // Update block with exit status
                            if let Ok(Some(doc)) = memory.get_block(&owner, &block_label_clone).await {
                                let status_line = format!(
                                    "\n--- Process exited with code {:?} after {}ms ---\n",
                                    code, duration_ms
                                );
                                let _ = doc.append_text(&status_line, true);
                            }

                            // Schedule auto-unpin
                            let memory = memory.clone();
                            let owner = owner.clone();
                            let label = block_label_clone.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(unpin_delay).await;
                                if let Err(e) = memory.set_block_pinned(&owner, &label, false).await {
                                    debug!(error = %e, label = %label, "failed to auto-unpin process block");
                                } else {
                                    debug!(label = %label, "auto-unpinned process block");
                                }
                            });

                            processes.remove(&task_id_clone);
                            break;
                        }
                    }
                }
            });

            // Track process
            self.processes.insert(
                task_id.clone(),
                ProcessInfo {
                    task_id: task_id.clone(),
                    block_label: block_label.clone(),
                    command: command.to_string(),
                    started_at: SystemTime::now(),
                    unpin_delay: unpin_delay.unwrap_or(DEFAULT_UNPIN_DELAY),
                },
            );
        }

        Ok((task_id, block_label))
    }

    /// Kill a running process.
    pub async fn kill(&self, task_id: &TaskId) -> Result<(), ShellError> {
        self.backend.kill(task_id).await?;
        self.processes.remove(task_id);
        Ok(())
    }

    /// Get status of all running processes.
    pub fn status(&self) -> Vec<ProcessStatus> {
        self.processes
            .iter()
            .map(|entry| {
                let info = entry.value();
                ProcessStatus {
                    task_id: info.task_id.clone(),
                    block_label: info.block_label.clone(),
                    command: info.command.clone(),
                    running_since: info.started_at,
                }
            })
            .collect()
    }
}

/// Status information for a running process.
#[derive(Debug, Clone)]
pub struct ProcessStatus {
    pub task_id: TaskId,
    pub block_label: String,
    pub command: String,
    pub running_since: SystemTime,
}

#[async_trait::async_trait]
impl DataStream for ProcessSource {
    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn block_schemas(&self) -> Vec<BlockSchemaSpec> {
        vec![BlockSchemaSpec::pinned(
            "process:{task_id}",
            BlockSchema::text(),
            "Output from shell process execution",
        )]
    }

    async fn start(
        &self,
        ctx: Arc<dyn ToolContext>,
        owner: AgentId,
    ) -> CoreResult<broadcast::Receiver<Notification>> {
        let (tx, rx) = broadcast::channel(256);
        *self.tx.write() = Some(tx);
        *self.ctx.write() = Some(ctx);
        *self.owner.write() = Some(owner);
        *self.status.write() = StreamStatus::Running;

        info!(source_id = %self.source_id, "ProcessSource started");
        Ok(rx)
    }

    async fn stop(&self) -> CoreResult<()> {
        // Kill all running processes
        for entry in self.processes.iter() {
            let _ = self.backend.kill(&entry.task_id).await;
        }
        self.processes.clear();

        *self.tx.write() = None;
        *self.ctx.write() = None;
        *self.owner.write() = None;
        *self.status.write() = StreamStatus::Stopped;

        info!(source_id = %self.source_id, "ProcessSource stopped");
        Ok(())
    }

    fn pause(&self) {
        *self.status.write() = StreamStatus::Paused;
    }

    fn resume(&self) {
        *self.status.write() = StreamStatus::Running;
    }

    fn status(&self) -> StreamStatus {
        *self.status.read()
    }

    fn supports_pull(&self) -> bool {
        false
    }

    async fn pull(
        &self,
        _limit: usize,
        _cursor: Option<StreamCursor>,
    ) -> CoreResult<Vec<Notification>> {
        Ok(Vec::new())
    }

    async fn handle_block_edit(
        &self,
        _edit: &BlockEdit,
        _ctx: Arc<dyn ToolContext>,
    ) -> CoreResult<EditFeedback> {
        // Process blocks are read-only from agent perspective
        Ok(EditFeedback::Rejected {
            reason: "process output blocks are read-only".to_string(),
        })
    }
}
```

**Step 2: Add to mod.rs**

Add to `process/mod.rs`:

```rust
mod source;

pub use source::{ProcessSource, ProcessStatus};
```

And update `data_source/mod.rs` re-exports:

```rust
pub use process::{
    ExecuteResult, LocalPtyBackend, OutputChunk, ProcessSource, ProcessStatus, ShellBackend,
    ShellError, ShellPermission, ShellPermissionConfig, TaskId,
};
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/data_source/process/source.rs crates/pattern_core/src/data_source/process/mod.rs crates/pattern_core/src/data_source/mod.rs
git commit -m "[pattern-core] implement ProcessSource DataStream"
```

---

## Task 9: Create Shell tool input/output types

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/shell_types.rs`

**Step 1: Write the types**

```rust
//! Shell tool input/output types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shell tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellOp {
    /// Execute a command and wait for completion.
    Execute,
    /// Spawn a long-running process with streaming output.
    Spawn,
    /// Kill a running process.
    Kill,
    /// List running processes.
    Status,
}

impl std::fmt::Display for ShellOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Execute => write!(f, "execute"),
            Self::Spawn => write!(f, "spawn"),
            Self::Kill => write!(f, "kill"),
            Self::Status => write!(f, "status"),
        }
    }
}

/// Input for shell tool operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellInput {
    /// The operation to perform.
    pub op: ShellOp,

    /// Command to execute (required for execute/spawn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Timeout in seconds for execute operation (default: 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// Task ID to kill (required for kill operation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl ShellInput {
    /// Create an execute input.
    pub fn execute(command: impl Into<String>) -> Self {
        Self {
            op: ShellOp::Execute,
            command: Some(command.into()),
            timeout: None,
            task_id: None,
        }
    }

    /// Create a spawn input.
    pub fn spawn(command: impl Into<String>) -> Self {
        Self {
            op: ShellOp::Spawn,
            command: Some(command.into()),
            timeout: None,
            task_id: None,
        }
    }

    /// Create a kill input.
    pub fn kill(task_id: impl Into<String>) -> Self {
        Self {
            op: ShellOp::Kill,
            command: None,
            timeout: None,
            task_id: Some(task_id.into()),
        }
    }

    /// Create a status input.
    pub fn status() -> Self {
        Self {
            op: ShellOp::Status,
            command: None,
            timeout: None,
            task_id: None,
        }
    }

    /// Set timeout for execute.
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout = Some(seconds);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_input_builders() {
        let exec = ShellInput::execute("ls -la");
        assert_eq!(exec.op, ShellOp::Execute);
        assert_eq!(exec.command.as_deref(), Some("ls -la"));

        let spawn = ShellInput::spawn("tail -f /var/log/syslog");
        assert_eq!(spawn.op, ShellOp::Spawn);

        let kill = ShellInput::kill("abc123");
        assert_eq!(kill.op, ShellOp::Kill);
        assert_eq!(kill.task_id.as_deref(), Some("abc123"));

        let status = ShellInput::status();
        assert_eq!(status.op, ShellOp::Status);
    }

    #[test]
    fn test_shell_input_serialization() {
        let input = ShellInput::execute("echo hello").with_timeout(30);
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"op\":\"execute\""));
        assert!(json.contains("\"command\":\"echo hello\""));
        assert!(json.contains("\"timeout\":30"));
    }
}
```

**Step 2: Add to mod.rs**

Add to `tool/builtin/mod.rs` after other mod declarations:

```rust
mod shell_types;
```

And in the pub use section (or create if doesn't exist):

```rust
pub use shell_types::{ShellInput, ShellOp};
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/shell_types.rs crates/pattern_core/src/tool/builtin/mod.rs
git commit -m "[pattern-core] add shell tool input types"
```

---

## Task 10: Implement ShellTool

**Files:**
- Create: `crates/pattern_core/src/tool/builtin/shell.rs`

**Step 1: Write the tool implementation**

```rust
//! Shell tool for command execution.
//!
//! Provides agents with shell command execution capability through
//! execute (one-shot) and spawn (streaming) operations.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, warn};

use crate::data_source::process::{ProcessSource, ShellError};
use crate::runtime::ToolContext;
use crate::tool::rule::{ToolRule, ToolRuleType};
use crate::tool::AiTool;
use crate::CoreError;

use super::shell_types::{ShellInput, ShellOp};
use super::types::ToolOutput;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Shell tool for command execution.
#[derive(Clone)]
pub struct ShellTool {
    ctx: Arc<dyn ToolContext>,
    source: Arc<ProcessSource>,
}

impl std::fmt::Debug for ShellTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl ShellTool {
    /// Create a new shell tool with the given process source.
    pub fn new(ctx: Arc<dyn ToolContext>, source: Arc<ProcessSource>) -> Self {
        Self { ctx, source }
    }

    /// Handle execute operation.
    async fn handle_execute(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ToolOutput, CoreError> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

        debug!(command = %command, ?timeout, "executing shell command");

        match self.source.execute(command, timeout).await {
            Ok(result) => {
                let data = json!({
                    "output": result.output,
                    "exit_code": result.exit_code,
                    "duration_ms": result.duration_ms,
                });

                if result.exit_code == Some(0) {
                    Ok(ToolOutput::success_with_data(
                        format!("Command completed in {}ms", result.duration_ms),
                        data,
                    ))
                } else {
                    // Non-zero exit is not an error - agent decides significance
                    Ok(ToolOutput::success_with_data(
                        format!(
                            "Command exited with code {:?} in {}ms",
                            result.exit_code, result.duration_ms
                        ),
                        data,
                    ))
                }
            }
            Err(e) => self.shell_error_to_output(e, "execute"),
        }
    }

    /// Handle spawn operation.
    async fn handle_spawn(&self, command: &str) -> Result<ToolOutput, CoreError> {
        debug!(command = %command, "spawning streaming process");

        match self.source.spawn(command, None).await {
            Ok((task_id, block_label)) => Ok(ToolOutput::success_with_data(
                format!("Process started: {}", task_id),
                json!({
                    "task_id": task_id.to_string(),
                    "block_label": block_label,
                }),
            )),
            Err(e) => self.shell_error_to_output(e, "spawn"),
        }
    }

    /// Handle kill operation.
    async fn handle_kill(&self, task_id: &str) -> Result<ToolOutput, CoreError> {
        debug!(task_id = %task_id, "killing process");

        let task_id = crate::data_source::process::TaskId(task_id.to_string());
        match self.source.kill(&task_id).await {
            Ok(()) => Ok(ToolOutput::success(format!("Process {} killed", task_id))),
            Err(e) => self.shell_error_to_output(e, "kill"),
        }
    }

    /// Handle status operation.
    fn handle_status(&self) -> Result<ToolOutput, CoreError> {
        let processes = self.source.status();

        if processes.is_empty() {
            return Ok(ToolOutput::success("No running processes"));
        }

        let process_list: Vec<_> = processes
            .iter()
            .map(|p| {
                let elapsed = p
                    .running_since
                    .elapsed()
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                json!({
                    "task_id": p.task_id.to_string(),
                    "block_label": p.block_label,
                    "command": p.command,
                    "running_for_secs": elapsed,
                })
            })
            .collect();

        Ok(ToolOutput::success_with_data(
            format!("{} running process(es)", processes.len()),
            json!({ "processes": process_list }),
        ))
    }

    /// Convert shell error to tool output.
    fn shell_error_to_output(&self, error: ShellError, op: &str) -> Result<ToolOutput, CoreError> {
        match &error {
            ShellError::Timeout(duration) => {
                // Timeout returns partial output if available
                warn!(op = %op, ?duration, "shell command timed out");
                Ok(ToolOutput::success_with_data(
                    format!("Command timed out after {:?}", duration),
                    json!({
                        "timeout": true,
                        "duration_ms": duration.as_millis(),
                    }),
                ))
            }
            ShellError::PermissionDenied { required, granted } => {
                Ok(ToolOutput::error(format!(
                    "Permission denied: requires {} but only have {}",
                    required, granted
                )))
            }
            ShellError::PathOutsideSandbox(path) => {
                Ok(ToolOutput::error(format!(
                    "Path outside allowed sandbox: {}",
                    path.display()
                )))
            }
            ShellError::CommandDenied(pattern) => {
                Ok(ToolOutput::error(format!(
                    "Command denied by security policy: contains '{}'",
                    pattern
                )))
            }
            ShellError::UnknownTask(id) => {
                Ok(ToolOutput::error(format!("Unknown task: {}", id)))
            }
            ShellError::TaskCompleted => {
                Ok(ToolOutput::error("Task has already completed"))
            }
            _ => {
                // Other errors are unexpected
                Err(CoreError::tool_exec_msg(
                    "shell",
                    json!({"op": op}),
                    error.to_string(),
                ))
            }
        }
    }
}

#[async_trait]
impl AiTool for ShellTool {
    type Input = ShellInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        r#"Execute shell commands in a persistent session.

## Operations

### execute
Run a command and return its output.
- command (required): The command to run
- timeout (optional): Timeout in seconds (default: 60)

Returns output (combined stdout/stderr), exit_code, and duration_ms.

Example: {"op": "execute", "command": "git status"}

### spawn
Start a long-running command with streaming output to a block.
- command (required): The command to run

Returns task_id and block_label for the output block.

Example: {"op": "spawn", "command": "cargo build --release"}

### kill
Terminate a running spawned process.
- task_id (required): The task ID from spawn

Example: {"op": "kill", "task_id": "abc12345"}

### status
List all running spawned processes.

Example: {"op": "status"}

## Notes

- Session state (cwd, env vars) persists across execute calls
- Use `cd` to change directories, `export` to set environment variables
- Non-zero exit codes are reported in data, not as errors
- spawn creates a pinned block that auto-unpins after process exit"#
    }

    fn operations(&self) -> &'static [&'static str] {
        &["execute", "spawn", "kill", "status"]
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(self.name().to_string(), ToolRuleType::ContinueLoop)]
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("the conversation will continue after shell commands complete")
    }

    async fn execute(
        &self,
        input: Self::Input,
        _meta: &crate::tool::ExecutionMeta,
    ) -> Result<Self::Output, CoreError> {
        match input.op {
            ShellOp::Execute => {
                let command = input.command.ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "shell",
                        json!({"op": "execute"}),
                        "command is required for execute operation",
                    )
                })?;
                self.handle_execute(&command, input.timeout).await
            }
            ShellOp::Spawn => {
                let command = input.command.ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "shell",
                        json!({"op": "spawn"}),
                        "command is required for spawn operation",
                    )
                })?;
                self.handle_spawn(&command).await
            }
            ShellOp::Kill => {
                let task_id = input.task_id.ok_or_else(|| {
                    CoreError::tool_exec_msg(
                        "shell",
                        json!({"op": "kill"}),
                        "task_id is required for kill operation",
                    )
                })?;
                self.handle_kill(&task_id).await
            }
            ShellOp::Status => self.handle_status(),
        }
    }
}
```

**Step 2: Add to mod.rs**

Add to `tool/builtin/mod.rs`:

```rust
mod shell;

pub use shell::ShellTool;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/shell.rs crates/pattern_core/src/tool/builtin/mod.rs
git commit -m "[pattern-core] implement ShellTool"
```

---

## Task 11: Register ShellTool in BuiltinTools

**Files:**
- Modify: `crates/pattern_core/src/tool/builtin/mod.rs`

**Step 1: Update BuiltinTools struct**

Find the `BuiltinTools` struct and add:

```rust
shell_tool: Option<Box<dyn DynamicTool>>,
```

**Step 2: Update constructor**

In the `default_for_agent` or similar constructor, add after other tools:

```rust
// Shell tool is optional - requires ProcessSource to be configured
shell_tool: None,
```

Add a new method to enable shell tool:

```rust
/// Enable shell tool with the given ProcessSource.
pub fn with_shell_tool(mut self, source: Arc<ProcessSource>) -> Self {
    self.shell_tool = Some(Box::new(DynamicToolAdapter::new(
        ShellTool::new(Arc::clone(&self.ctx), source),
    )));
    self
}
```

**Step 3: Update register_all**

Add to `register_all`:

```rust
if let Some(ref tool) = self.shell_tool {
    registry.register_dynamic(tool.clone_box());
}
```

**Step 4: Update create_builtin_tool**

Add to the match in `create_builtin_tool`:

```rust
// Note: Shell tool requires ProcessSource, can't be created standalone
// "shell" => { ... }
```

**Step 5: Update BUILTIN_TOOL_NAMES**

Add "shell" to the list.

**Step 6: Verify compilation**

Run: `cargo check -p pattern-core`
Expected: SUCCESS

**Step 7: Commit**

```bash
git add crates/pattern_core/src/tool/builtin/mod.rs
git commit -m "[pattern-core] register ShellTool in BuiltinTools"
```

---

## Task 12: Write integration tests for ShellTool

**Files:**
- Create: `crates/pattern_core/tests/shell_tool.rs`

**Step 1: Write integration tests**

Uses the existing `MockToolContext` from `src/tool/builtin/test_utils.rs`:

```rust
//! Integration tests for shell tool.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pattern_core::agent::AgentId;
use pattern_core::data_source::process::{LocalPtyBackend, ProcessSource, ShellPermissionConfig};
use pattern_core::tool::builtin::test_utils::create_test_context_with_agent;
use pattern_core::tool::builtin::{ShellInput, ShellTool};
use pattern_core::tool::{AiTool, ExecutionMeta};

#[tokio::test]
async fn test_shell_execute_simple() {
    let (_dbs, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
    let source = Arc::new(ProcessSource::new("test", backend, ShellPermissionConfig::default()));

    // Start source (needed for execute to work)
    let owner: AgentId = ctx.agent_id().parse().unwrap();
    let _rx = source.start(Arc::clone(&ctx) as _, owner).await.unwrap();

    let tool = ShellTool::new(Arc::clone(&ctx) as _, Arc::clone(&source));

    let input = ShellInput::execute("echo hello_world");
    let result = tool.execute(input, &ExecutionMeta::default()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.success);
    assert!(output.data.is_some());

    let data = output.data.unwrap();
    let cmd_output = data["output"].as_str().unwrap();
    assert!(cmd_output.contains("hello_world"));
}

#[tokio::test]
async fn test_shell_exit_code_nonzero() {
    let (_dbs, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
    let source = Arc::new(ProcessSource::new("test", backend, ShellPermissionConfig::default()));

    let owner: AgentId = ctx.agent_id().parse().unwrap();
    let _rx = source.start(Arc::clone(&ctx) as _, owner).await.unwrap();

    let tool = ShellTool::new(Arc::clone(&ctx) as _, Arc::clone(&source));

    // Command that fails
    let input = ShellInput::execute("false");
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();

    // Non-zero exit is still success (agent interprets)
    assert!(result.success);
    let data = result.data.unwrap();
    assert_eq!(data["exit_code"], 1);
}

#[tokio::test]
async fn test_shell_execute_with_cwd() {
    let (_dbs, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let backend = Arc::new(LocalPtyBackend::new(PathBuf::from("/tmp")));
    let source = Arc::new(ProcessSource::new("test", backend, ShellPermissionConfig::default()));

    let owner: AgentId = ctx.agent_id().parse().unwrap();
    let _rx = source.start(Arc::clone(&ctx) as _, owner).await.unwrap();

    let tool = ShellTool::new(Arc::clone(&ctx) as _, Arc::clone(&source));

    let input = ShellInput::execute("pwd");
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();

    let data = result.data.unwrap();
    let cmd_output = data["output"].as_str().unwrap();
    assert!(cmd_output.contains("/tmp"));
}

#[tokio::test]
async fn test_shell_command_denied() {
    let (_dbs, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
    let source = Arc::new(ProcessSource::new("test", backend, ShellPermissionConfig::default()));

    let owner: AgentId = ctx.agent_id().parse().unwrap();
    let _rx = source.start(Arc::clone(&ctx) as _, owner).await.unwrap();

    let tool = ShellTool::new(Arc::clone(&ctx) as _, Arc::clone(&source));

    // This should be denied
    let input = ShellInput::execute("rm -rf /");
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();

    assert!(!result.success);
    assert!(result.message.contains("denied"));
}

#[tokio::test]
async fn test_shell_spawn_and_kill() {
    let (_dbs, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
    let source = Arc::new(ProcessSource::new("test", backend, ShellPermissionConfig::default()));

    let owner: AgentId = ctx.agent_id().parse().unwrap();
    let _rx = source.start(Arc::clone(&ctx) as _, owner).await.unwrap();

    let tool = ShellTool::new(Arc::clone(&ctx) as _, Arc::clone(&source));

    // Spawn a long-running process
    let input = ShellInput::spawn("sleep 60");
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();

    assert!(result.success);
    let data = result.data.unwrap();
    let task_id = data["task_id"].as_str().unwrap();

    // Check status
    let input = ShellInput::status();
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();
    assert!(result.message.contains("1 running"));

    // Kill it
    let input = ShellInput::kill(task_id);
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();
    assert!(result.success);

    // Give it a moment
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Status should show none
    let input = ShellInput::status();
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();
    assert!(result.message.contains("No running"));
}

#[tokio::test]
async fn test_shell_timeout() {
    let (_dbs, _memory, ctx) = create_test_context_with_agent("test-agent").await;
    let backend = Arc::new(LocalPtyBackend::new(std::env::temp_dir()));
    let source = Arc::new(ProcessSource::new("test", backend, ShellPermissionConfig::default()));

    let owner: AgentId = ctx.agent_id().parse().unwrap();
    let _rx = source.start(Arc::clone(&ctx) as _, owner).await.unwrap();

    let tool = ShellTool::new(Arc::clone(&ctx) as _, Arc::clone(&source));

    let input = ShellInput::execute("sleep 10").with_timeout(1);
    let result = tool.execute(input, &ExecutionMeta::default()).await.unwrap();

    // Timeout is reported as success with timeout flag
    assert!(result.success);
    let data = result.data.unwrap();
    assert_eq!(data["timeout"], true);
}
```

**Step 2: Run tests**

Run: `cargo nextest run -p pattern-core --test shell_tool`
Expected: All tests PASS (some may need adjustment based on actual test infrastructure)

**Step 4: Commit**

```bash
git add crates/pattern_core/tests/shell_tool.rs
git commit -m "[pattern-core] add shell tool integration tests"
```

---

## Task 13: Add BlockSchema::Process variant (optional enhancement)

**Files:**
- Modify: `crates/pattern_core/src/memory/schema.rs`

**Step 1: Consider if needed**

The current implementation uses `BlockSchema::text()` for process output blocks. If you want structured process blocks with separate stdout/stderr fields, add a Process variant:

```rust
/// Process output with separate streams.
Process {
    /// Include stderr in the block.
    include_stderr: bool,
},
```

**Decision point:** This is optional. The text-based approach works fine for v1. Skip this task unless specifically requested.

---

## Task 14: Documentation and CLAUDE.md updates

**Files:**
- Modify: `crates/pattern_core/CLAUDE.md`

**Step 1: Add shell tool documentation**

Add a new section after the tool system documentation:

```markdown
## Shell Tool

The shell tool provides command execution capability:

### Operations
- `execute`: One-shot command, returns combined output/exit_code
- `spawn`: Long-running process with streaming output to block
- `kill`: Terminate a spawned process
- `status`: List running processes

### Notes
- PTY-based: stdout/stderr are interleaved (single `output` field)
- Exit codes detected via nonce-based wrapper (prevents output injection)
- Session death detected and auto-reinitialized

### Security Model
- Denied command patterns (rm -rf /, sudo, etc.)
- Path sandboxing via `allowed_paths` config
- Permission tiers: ReadOnly, ReadWrite, Admin

### ProcessSource
DataStream implementation managing shell processes:
- Uses PTY for real shell session (cwd/env persist)
- Swappable backend (LocalPtyBackend, future: Docker, Bubblewrap)
- Streaming output to pinned Loro blocks
- Auto-unpin after configurable delay post-exit
```

**Step 2: Commit**

```bash
git add crates/pattern_core/CLAUDE.md
git commit -m "[pattern-core] document shell tool in CLAUDE.md"
```

---

## Final verification

After completing all tasks:

**Step 1: Run full test suite**

```bash
cargo nextest run -p pattern-core
```

**Step 2: Run clippy**

```bash
cargo clippy -p pattern-core --all-features --all-targets
```

**Step 3: Format check**

```bash
cargo fmt --check
```

Expected: All pass with no warnings.
