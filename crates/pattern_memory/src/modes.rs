// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Storage mode for a Pattern mount.
//!
//! The `StorageMode` enum describes *how* Pattern manages VCS history for
//! a given mount. Phase 5 introduced the skeleton; Phase 6 adds per-mode
//! init logic, `.pattern.kdl` config generation, attach/detach, and the
//! gitignore helper.
//!
//! # Submodules
//!
//! - [`error`] — [`ModeError`](error::ModeError) type.
//! - [`in_repo`] — InRepo initialization (host VCS owns history).
//! - [`standalone`] — Standalone initialization (Pattern-owned jj repo).
//! - [`sidecar`] — Sidecar initialization (jj alongside host git).
//! - [`gitignore`] — Idempotent `.gitignore` append helper.

pub mod error;
pub mod gitignore;
pub mod in_repo;
pub mod sidecar;
pub mod standalone;

use std::path::{Path, PathBuf};

/// Storage mode for a Pattern mount.
///
/// Controls whether and how Pattern uses `jj` for VCS history, and where
/// the memory files live on disk.
///
/// # Variants
///
/// - **InRepo** — in-repo storage; the user's existing host VCS (git or jj)
///   owns history. Pattern writes files into a subdirectory of the host repo
///   and never invokes `jj` itself.
///
/// - **Standalone** — separate directory (e.g. `~/.pattern/projects/<id>/`) with
///   a dedicated Pattern-owned jj repo. Pattern runs `jj commit` for history.
///   Requires a working `jj` installation (checked by [`JjAdapter::detect`]).
///
/// - **Sidecar** — Pattern's `.jj/` lives alongside the host `.git/` in the
///   same working-copy directory. Validated by Phase 6 spike; uses
///   `--no-colocate` so the jj-internal git repo stays at `.jj/repo/`.
///
/// [`JjAdapter::detect`]: crate::jj::JjAdapter::detect
///
/// # Phase status
///
/// Phase 5 introduced the enum shape. Phase 6 added the per-mode
/// attach/detach logic and reads the active mode from `.pattern.kdl`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum StorageMode {
    /// In-repo storage; host VCS owns history. Pattern does not run `jj`.
    InRepo {
        /// Root of the mount — where Pattern writes canonical memory files
        /// (`<project>/.pattern/shared/`).
        mount_path: PathBuf,
        /// The project repository root containing `.pattern/`. Used to resolve
        /// the `messages.db` path at `<project_root>/.pattern/transient/messages.db`.
        project_root: PathBuf,
    },
    /// Separate Pattern-owned jj repository. Pattern runs `jj commit`.
    Standalone {
        /// Root of the mount — the dedicated pattern directory.
        mount_path: PathBuf,
        /// Stable identifier for this project's jj repository.
        project_id: String,
    },
    /// Sidecar — pattern jj lives alongside host git.
    Sidecar {
        /// Root of the mount — shares the host working-copy directory.
        mount_path: PathBuf,
    },
}

impl StorageMode {
    /// The root directory where Pattern writes canonical memory files.
    pub fn mount_path(&self) -> &Path {
        match self {
            StorageMode::InRepo { mount_path, .. } => mount_path,
            StorageMode::Standalone { mount_path, .. } => mount_path,
            StorageMode::Sidecar { mount_path } => mount_path,
        }
    }

    /// Whether this mode requires a `jj` adapter at attach time.
    ///
    /// `InRepo` works without `jj` (host VCS owns commits). `Standalone` and
    /// `Sidecar` require a supported `jj` installation — [`JjAdapter::detect`]
    /// must return `Ok(Some(_))` or attachment will fail with a typed error.
    ///
    /// [`JjAdapter::detect`]: crate::jj::JjAdapter::detect
    pub fn requires_jj(&self) -> bool {
        matches!(
            self,
            StorageMode::Standalone { .. } | StorageMode::Sidecar { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_repo_does_not_require_jj() {
        let mode = StorageMode::InRepo {
            mount_path: PathBuf::from("/tmp/test"),
            project_root: PathBuf::from("/tmp"),
        };
        assert!(!mode.requires_jj());
    }

    #[test]
    fn standalone_requires_jj() {
        let mode = StorageMode::Standalone {
            mount_path: PathBuf::from("/tmp/test"),
            project_id: "proj-123".into(),
        };
        assert!(mode.requires_jj());
    }

    #[test]
    fn sidecar_requires_jj() {
        let mode = StorageMode::Sidecar {
            mount_path: PathBuf::from("/tmp/test"),
        };
        assert!(mode.requires_jj());
    }

    #[test]
    fn mount_path_round_trips() {
        let path = PathBuf::from("/some/mount");
        let in_repo = StorageMode::InRepo {
            mount_path: path.clone(),
            project_root: PathBuf::from("/some"),
        };
        assert_eq!(in_repo.mount_path(), path.as_path());

        let standalone = StorageMode::Standalone {
            mount_path: path.clone(),
            project_id: "p".into(),
        };
        assert_eq!(standalone.mount_path(), path.as_path());

        let sidecar = StorageMode::Sidecar {
            mount_path: path.clone(),
        };
        assert_eq!(sidecar.mount_path(), path.as_path());
    }
}
