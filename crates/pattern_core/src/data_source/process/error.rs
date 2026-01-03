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

    #[error("invalid command: {0}")]
    InvalidCommand(String),

    #[error("encoding error: {0}")]
    EncodingError(String),
}
