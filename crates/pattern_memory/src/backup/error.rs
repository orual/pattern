//! Error types for the backup subsystem.

use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

/// All errors produced by backup snapshot, rotation, and restore operations.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum BackupError {
    /// A path-resolution error when computing backup or snapshot paths.
    #[error("path error: {source}")]
    #[diagnostic(code(pattern_memory::backup::path_error))]
    Path {
        #[from]
        source: crate::paths::PathError,
    },

    /// An I/O error operating on a file or directory.
    #[error("I/O error at {path}: {source}")]
    #[diagnostic(code(pattern_memory::backup::io))]
    Io {
        /// The path being operated on when the error occurred.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to create a temporary file in the backup directory.
    ///
    /// Using `NamedTempFile::new_in` to keep the temp file on the same
    /// filesystem as the destination prevents EXDEV on the rename step.
    #[error("failed to create temporary file in {path}: {source}")]
    #[diagnostic(code(pattern_memory::backup::temp_file))]
    TempFile {
        /// The directory where the temp file was being created.
        path: PathBuf,
        /// Underlying I/O error from `tempfile`.
        #[source]
        source: std::io::Error,
    },

    /// Failed to atomically persist (rename) a temporary file into its
    /// final destination path.
    #[error("failed to persist temporary file to {path}: {source}")]
    #[diagnostic(code(pattern_memory::backup::temp_persist))]
    TempPersist {
        /// The intended destination path.
        path: PathBuf,
        /// Underlying I/O error from `tempfile::PersistError`.
        #[source]
        source: std::io::Error,
    },

    /// Failed to open the source database for backup.
    #[error("failed to open source database: {0}")]
    #[diagnostic(code(pattern_memory::backup::open_source))]
    OpenSource(#[source] rusqlite::Error),

    /// Failed to open the destination database for backup.
    #[error("failed to open destination database: {0}")]
    #[diagnostic(code(pattern_memory::backup::open_dest))]
    OpenDest(#[source] rusqlite::Error),

    /// `rusqlite::backup::Backup::new` failed to initialise the backup handle.
    #[error("failed to initialise rusqlite Backup handle: {0}")]
    #[diagnostic(code(pattern_memory::backup::backup_init))]
    BackupInit(#[source] rusqlite::Error),

    /// `Backup::run_to_completion` failed.
    #[error("rusqlite backup run failed: {0}")]
    #[diagnostic(code(pattern_memory::backup::backup_run))]
    BackupRun(#[source] rusqlite::Error),

    /// A snapshot filename could not be parsed as a valid timestamp.
    #[error("invalid snapshot filename: {path}")]
    #[diagnostic(
        code(pattern_memory::backup::invalid_snapshot_name),
        help(
            "snapshot filenames must match the format YYYY-MM-DDTHHMMSSZ (e.g. 2026-04-19T120000Z)"
        )
    )]
    InvalidSnapshotName {
        /// The path whose filename could not be parsed.
        path: PathBuf,
    },

    /// A requested snapshot file was not found at the given path.
    #[error("snapshot not found: {path}")]
    #[diagnostic(code(pattern_memory::backup::snapshot_not_found))]
    SnapshotNotFound {
        /// The path that was expected to exist.
        path: PathBuf,
    },

    /// No snapshots exist for the given project.
    #[error("no snapshots found for project {project_id}")]
    #[diagnostic(
        code(pattern_memory::backup::no_snapshots),
        help("run `pattern backup create` to create the first snapshot")
    )]
    NoSnapshots {
        /// The project ID that has no snapshots.
        project_id: String,
    },

    /// A snapshot spec (timestamp string or shorthand) did not match any
    /// existing snapshot. The error includes the available timestamps so
    /// the user can correct their input.
    #[error("no snapshot matching {spec:?}")]
    #[diagnostic(
        code(pattern_memory::backup::snapshot_not_found_by_spec),
        help("available snapshots:\n{}", available.join("\n"))
    )]
    SnapshotNotFoundBySpec {
        /// The spec string that was provided.
        spec: String,
        /// All available snapshot timestamp strings (newest first).
        available: Vec<String>,
    },

    /// `PRAGMA integrity_check` on a snapshot file returned a non-ok result.
    #[error("snapshot is corrupt at {path}: {detail}")]
    #[diagnostic(
        code(pattern_memory::backup::corrupt_snapshot),
        help("the snapshot file failed SQLite integrity_check; it cannot safely be restored")
    )]
    CorruptSnapshot {
        /// The snapshot file that failed the check.
        path: PathBuf,
        /// The detail string returned by `PRAGMA integrity_check`.
        detail: String,
    },

    /// `PRAGMA integrity_check` query itself failed (distinct from a
    /// corrupt result).
    #[error("integrity check query failed on {path}: {source}")]
    #[diagnostic(code(pattern_memory::backup::integrity_check))]
    IntegrityCheck {
        /// The snapshot file being checked.
        path: PathBuf,
        /// Underlying rusqlite error.
        #[source]
        source: rusqlite::Error,
    },
}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        // Bare I/O errors without a specific path context. Callers that have a
        // path should construct BackupError::Io directly.
        BackupError::Io {
            path: PathBuf::from("<unknown>"),
            source: e,
        }
    }
}
