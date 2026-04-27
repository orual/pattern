//! Mirror of `Pattern.File` (`haskell/Pattern/File.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `File` GADT.
///
/// Phase 2 (v3-sandbox-io) expanded variants:
/// - `ListDir` gained a glob argument (empty string treated as `"*"`).
/// - `Open`/`Close`/`Watch` added for LoroSyncedFile lifecycle.
/// - `Reload`/`ForceWrite` added per Phase 1 amendment: agent recourse on
///   `FileConflict` system reminders from stale-base external writes.
#[derive(Debug, FromCore)]
pub enum FileReq {
    #[core(module = "Pattern.File", name = "Read")]
    Read(String),
    #[core(module = "Pattern.File", name = "Write")]
    Write(String, String),
    /// (path, glob) — empty glob is treated as `"*"` (match all).
    #[core(module = "Pattern.File", name = "ListDir")]
    ListDir(String, String),
    /// Open a file, creating a `LoroSyncedFile` and auto-subscribing to
    /// external change notifications. Returns current file content.
    #[core(module = "Pattern.File", name = "Open")]
    Open(String),
    /// Close an open file, dropping its `LoroSyncedFile` and unsubscribing
    /// from change notifications.
    #[core(module = "Pattern.File", name = "Close")]
    Close(String),
    /// Subscribe to change notifications for a path without creating a
    /// `LoroSyncedFile` (lighter weight than `Open`).
    #[core(module = "Pattern.File", name = "Watch")]
    Watch(String),
    /// Drop the `LoroSyncedFile` memory-doc state and reload from disk.
    /// Returns the reloaded content. Use after a `FileConflict` reminder
    /// to accept the external writer's version.
    #[core(module = "Pattern.File", name = "Reload")]
    Reload(String),
    /// Write through to disk, bypassing `ConflictPolicy`. Use after a
    /// `FileConflict` reminder to overwrite with the agent's version.
    #[core(module = "Pattern.File", name = "ForceWrite")]
    ForceWrite(String, String),
}
