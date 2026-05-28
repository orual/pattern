// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Error types for mount discovery, attachment, and detachment.

use std::path::PathBuf;

/// Errors produced during mount discovery, attachment, or detachment.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum MountError {
    /// No `.pattern/shared/.pattern.kdl` was found walking upward from the
    /// starting path to the filesystem root.
    #[error("no mount found at or above {started_at}")]
    #[diagnostic(
        code(pattern_memory::mount::not_found),
        help("run `pattern mount init --mode a` to initialize a mount here")
    )]
    NotFound {
        /// The directory where the walk-upward search began.
        started_at: PathBuf,
    },

    /// The mount directory layout is invalid (e.g. can't derive project_root
    /// from the mount path).
    #[error("mount at {path} has invalid directory layout")]
    #[diagnostic(code(pattern_memory::mount::invalid_layout))]
    InvalidLayout {
        /// The mount path with the invalid layout.
        path: PathBuf,
    },

    /// Sidecar mode is not yet available for production use.
    #[error("mode {mode} is unavailable: {reason}")]
    #[diagnostic(code(pattern_memory::mount::mode_unavailable))]
    ModeUnavailable {
        /// The mode that was requested.
        mode: &'static str,
        /// Why the mode is unavailable.
        reason: String,
    },

    /// Config parsing or validation error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Config(#[from] crate::config::ConfigError),

    /// Database open error.
    #[error("database error: {0}")]
    #[diagnostic(code(pattern_memory::mount::db))]
    Db(#[from] pattern_db::DbError),

    /// Path resolution error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Paths(#[from] crate::paths::PathError),

    /// Filesystem watcher error.
    #[error("watcher error: {0}")]
    #[diagnostic(code(pattern_memory::mount::watcher))]
    Watcher(#[from] crate::fs::FsError),

    /// Filesystem I/O error during directory creation or other setup.
    #[error("failed to create directory {path}: {source}")]
    #[diagnostic(code(pattern_memory::mount::io))]
    Io {
        /// The path involved in the failure.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}
