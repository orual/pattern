//! Shell process manager — sync, PTY-backed shell session coordination.
//!
//! ## Architecture summary
//!
//! The process manager is entirely sync (no tokio). Each `ShellSession` runs
//! as a dedicated OS thread owning a `pty_process::Pty` handle. Commands are
//! dispatched via crossbeam channels from the eval-worker thread (which must
//! not block a tokio runtime). This mirrors the `pattern_memory` subscriber-
//! worker idiom.
//!
//! ## Module layout
//!
//! - `types` — `TaskId`, `ExecuteResult`, `OutputChunk`, `ShellPermission`.
//! - `error` — `ShellError` with all variants required by AC3.
//! - `backend` — `ShellBackend` trait (sync, `Send + Sync + Debug`).
//! - `local_pty` — `LocalPtyBackend`: PTY-backed backend (Task 3).
//! - `manager` — `ProcessManager`: thin coordinator over a `ShellBackend` (Task 4).
//! - `logger` — `ProcessLogger`: per-task append-only log (Task 8, AC3.10).
//!
//! ## Amendment note (2026-04-26)
//!
//! Per the Q4 resolution, `ProcessManager` is per-session (on `SessionContext`),
//! NOT runtime-global on `TidepoolRuntime`. No global singleton assumptions
//! are made anywhere in this module tree.

pub mod backend;
pub mod error;
pub mod local_pty;
pub mod logger;
pub mod manager;
pub mod types;

pub use backend::ShellBackend;
pub use error::ShellError;
pub use local_pty::LocalPtyBackend;
pub use logger::ProcessLogger;
pub use manager::ProcessManager;
pub use types::{ExecuteResult, OutputChunk, ShellPermission, TaskId, TaskInfo};
