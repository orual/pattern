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
#[non_exhaustive]
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
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - [`ShellError::Timeout`]: Command exceeds the specified timeout duration.
    /// - [`ShellError::SessionDied`]: The underlying shell session terminated unexpectedly.
    /// - [`ShellError::SessionNotInitialized`]: Session hasn't been started yet.
    /// - [`ShellError::SpawnFailed`]: Failed to spawn the command process.
    /// - [`ShellError::ExitCodeParseFailed`]: Could not parse exit code from output.
    /// - [`ShellError::Io`]: An I/O error occurred during execution.
    async fn execute(&self, command: &str, timeout: Duration) -> Result<ExecuteResult, ShellError>;

    /// Spawn a long-running command with streaming output.
    ///
    /// Returns a task ID and receiver for output chunks.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - [`ShellError::SessionNotInitialized`]: Session hasn't been started yet.
    /// - [`ShellError::SpawnFailed`]: Failed to spawn the command process.
    /// - [`ShellError::CommandDenied`]: Command blocked by security policy.
    /// - [`ShellError::Io`]: An I/O error occurred during spawn.
    async fn spawn_streaming(
        &self,
        command: &str,
    ) -> Result<(TaskId, broadcast::Receiver<OutputChunk>), ShellError>;

    /// Kill a running spawned process.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - [`ShellError::UnknownTask`]: No task exists with the given ID.
    /// - [`ShellError::TaskCompleted`]: The task has already finished.
    async fn kill(&self, task_id: &TaskId) -> Result<(), ShellError>;

    /// List currently running task IDs.
    fn running_tasks(&self) -> Vec<TaskId>;

    /// Get current working directory of the session.
    ///
    /// Returns `None` if the session hasn't been initialized yet.
    /// Returns `Some(path)` with the current working directory once the session is running.
    async fn cwd(&self) -> Option<std::path::PathBuf>;
}
