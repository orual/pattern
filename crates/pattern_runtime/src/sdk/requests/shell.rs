//! Mirror of `Pattern.Shell` (`haskell/Pattern/Shell.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Shell` GADT.
///
/// - `Execute(command, Option<timeout_secs>)`: `None` means "use the
///   `SessionContext` default" (currently 30 s). On timeout the command is
///   killed (Ctrl-C, drain) and an error is surfaced; agents that want
///   long-running execution use `Spawn`.
/// - `Spawn(command)`: returns JSON `{"task_id": "...", "pid": N}`. The
///   `task_id` is an opaque recycle-safe handle for `Kill` / `Status`.
/// - `Kill(task_id)`: kill a spawned task by its handle. Stale handles
///   return `UnknownTask` (the task already exited).
/// - `Status`: returns JSON `[{"task_id": "...", "pid": N, "command": "...",
///   "elapsed_ms": N}, ...]`.
#[derive(Debug, FromCore)]
pub enum ShellReq {
    #[core(module = "Pattern.Shell", name = "Execute")]
    Execute(String, Option<i64>),
    #[core(module = "Pattern.Shell", name = "Spawn")]
    Spawn(String),
    #[core(module = "Pattern.Shell", name = "Kill")]
    Kill(String),
    #[core(module = "Pattern.Shell", name = "Status")]
    Status,
}
