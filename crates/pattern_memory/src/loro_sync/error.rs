//! Error types for the `loro_sync` module.

use std::path::PathBuf;

/// All errors that `SyncedDoc` operations can produce.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncedDocError {
    /// The requested file does not exist on disk.
    #[error("file not found: {0}")]
    NotFound(PathBuf),
    /// An I/O operation failed on the given path.
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Watcher setup failed for the given path.
    #[error("watcher setup failed for {path}: {message}")]
    Watcher { path: PathBuf, message: String },
    /// The bridge reported a format or serialization error.
    #[error("bridge failure: {0}")]
    Bridge(#[from] super::bridge::BridgeError),
    /// The `SyncedDoc` has been closed and can no longer be used.
    #[error("doc closed")]
    Closed,
    /// A filesystem-layer error (atomic write, format conversion) from `FsError`.
    #[error("fs error: {0}")]
    Fs(#[from] crate::fs::FsError),
}

/// Type alias kept for call-site readability in `LoroSyncedFile` and other
/// consumers that use the error directly without the struct-path prefix.
pub type LoroSyncError = SyncedDocError;
