//! Error types for storage mode initialization.

use std::path::PathBuf;

/// Errors produced during storage mode initialization (`in_repo::init`,
/// `standalone::init`, `sidecar::init`).
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum ModeError {
    /// An I/O error occurred during mount directory creation or file writes.
    #[error("io error at {path}: {source}")]
    #[diagnostic(code(pattern_memory::modes::io))]
    Io {
        /// The path that triggered the error.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A path resolution error (e.g. no home directory available).
    #[error(transparent)]
    #[diagnostic(transparent)]
    Path(#[from] crate::paths::PathError),

    /// The jj adapter reported an error during repo initialization.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Jj(#[from] crate::jj::JjError),
}
