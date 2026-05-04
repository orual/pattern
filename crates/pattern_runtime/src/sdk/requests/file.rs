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
    /// (path, glob) - empty glob is treated as `"*"` (match all).
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
    /// Insert content after line `n` (1-indexed). Line 0 = insert at top.
    #[core(module = "Pattern.File", name = "InsertLines")]
    InsertLines(String, i64, String),
    /// Replace lines `from`..`to` (1-indexed, inclusive) with new content.
    #[core(module = "Pattern.File", name = "ReplaceLines")]
    ReplaceLines(String, i64, i64, String),
    /// Delete lines `from`..`to` (1-indexed, inclusive).
    #[core(module = "Pattern.File", name = "DeleteLines")]
    DeleteLines(String, i64, i64),
    /// Read a line range from a file. `start` is 1-indexed, `count` is
    /// how many lines to return. Response includes a header with line
    /// numbers and total line count for navigation context.
    #[core(module = "Pattern.File", name = "ReadLines")]
    ReadLines(String, i64, i64),
    /// Find and replace a string in a file. Returns the number of
    /// replacements made as a string (e.g. "2").
    #[core(module = "Pattern.File", name = "Replace")]
    Replace(String, String, String),
}
