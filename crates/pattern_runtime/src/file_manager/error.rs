// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Error types for the file manager subsystem.

use std::path::PathBuf;

use pattern_memory::loro_sync::LoroSyncError;

/// Errors that can be returned by `FileManager` operations.
///
/// Variants cover the full error surface from AC2.8 (`PermissionDenied`),
/// AC2.9 (`ConfigApprovalRequired`/`ConfigApprovalDenied`), AC2.12
/// (`FileInConflict`), and general I/O and CRDT sync failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    #[error("file not found: {0}")]
    NotFound(PathBuf),

    #[error("permission denied: {path} ({reason})")]
    PermissionDenied { path: PathBuf, reason: String },

    #[error("config-file write requires human approval: {path}")]
    ConfigApprovalRequired {
        path: PathBuf,
        /// Top-level KDL keys that triggered the config-shape detection.
        matched_keys: Vec<String>,
    },

    #[error("config-file write was denied by the human: {path}")]
    ConfigApprovalDenied { path: PathBuf },

    #[error("capability denied: File effect not in agent's CapabilitySet")]
    CapabilityDenied,

    #[error("io on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("loro sync: {0}")]
    LoroSync(#[from] LoroSyncError),

    #[error("glob pattern invalid: {0}")]
    BadGlob(String),

    #[error("file not open: {0}")]
    NotOpen(PathBuf),

    /// The file has an outstanding conflict — an external writer wrote a
    /// whole-file replacement that didn't include the agent's last saved
    /// edit. The agent must call `File.Reload` (take disk's version),
    /// `File.ForceWrite` (overwrite with its own version), or `File.Write`
    /// with a manually composed merge before the conflict is resolved.
    #[error("file is currently in conflict; reload or force-write to recover: {path}")]
    FileInConflict { path: PathBuf },
}

impl FileError {
    /// Format the error as an agent-facing effect message.
    ///
    /// Most errors are prefixed with `"Pattern.File: "` so the agent
    /// can distinguish file-manager errors from other effect errors.
    ///
    /// [`FileError::CapabilityDenied`], [`FileError::PermissionDenied`],
    /// [`FileError::ConfigApprovalDenied`] are prefixed with the
    /// `PERMISSION_DENIED_PREFIX` so tests and the eventual UI can
    /// discriminate denial from I/O or CRDT errors without parsing prose.
    pub fn to_effect_message(&self) -> String {
        use crate::policy::PERMISSION_DENIED_PREFIX;
        match self {
            // Denial-class errors get the permission-denied prefix so
            // tests can discriminate them from I/O / CRDT failures.
            FileError::CapabilityDenied => {
                format!("{PERMISSION_DENIED_PREFIX}Pattern.File: {self}")
            }
            FileError::PermissionDenied { .. } => {
                format!("{PERMISSION_DENIED_PREFIX}Pattern.File: {self}")
            }
            FileError::ConfigApprovalDenied { .. } => {
                format!("{PERMISSION_DENIED_PREFIX}Pattern.File: {self}")
            }
            // All other errors use the plain prefix.
            other => format!("Pattern.File: {other}"),
        }
    }
}
