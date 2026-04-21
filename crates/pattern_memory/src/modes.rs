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
//! - [`mode_a`] — Mode A initialization (in-repo, host VCS owns history).
//! - [`mode_b`] — Mode B initialization (separate Pattern-owned jj repo).
//! - [`mode_c`] — Mode C initialization (sidecar jj inside host git project).
//! - [`gitignore`] — Idempotent `.gitignore` append helper.

pub mod error;
pub mod gitignore;
pub mod mode_a;
pub mod mode_b;
pub mod mode_c;

use std::path::{Path, PathBuf};

/// Storage mode for a Pattern mount.
///
/// Controls whether and how Pattern uses `jj` for VCS history, and where
/// the memory files live on disk.
///
/// # Variants
///
/// - **Mode A** — in-repo storage; the user's existing host VCS (git or jj)
///   owns history. Pattern writes files into a subdirectory of the host repo
///   and never invokes `jj` itself.
///
/// - **Mode B** — separate directory (e.g. `~/.pattern/projects/<id>/`) with
///   a dedicated Pattern-owned jj repo. Pattern runs `jj commit` for history.
///   Requires a working `jj` installation (checked by [`JjAdapter::detect`]).
///
/// - **Mode C** — sidecar; Pattern's `.jj/` lives alongside the host `.git/`
///   in the same working-copy directory. Gated on Phase 6 validation spike.
///   Not yet enabled for production use.
///
/// [`JjAdapter::detect`]: crate::jj::JjAdapter::detect
///
/// # Phase status
///
/// Phase 5 (this file) introduces the enum shape. Phase 6 adds the
/// per-mode attach/detach logic and reads the active mode from `.pattern.kdl`.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum StorageMode {
    /// In-repo storage; host VCS owns history. Pattern does not run `jj`.
    A {
        /// Root of the mount — where Pattern writes canonical memory files
        /// (`<project>/.pattern/shared/`).
        mount_path: PathBuf,
        /// The project repository root containing `.pattern/`. Used to derive
        /// the project hash for `messages.db` placement.
        project_root: PathBuf,
    },
    /// Separate Pattern-owned jj repository. Pattern runs `jj commit`.
    B {
        /// Root of the mount — the dedicated pattern directory.
        mount_path: PathBuf,
        /// Stable identifier for this project's jj repository.
        project_id: String,
    },
    /// Sidecar — pattern jj lives alongside host git. Phase 6 validation spike.
    C {
        /// Root of the mount — shares the host working-copy directory.
        mount_path: PathBuf,
    },
}

impl StorageMode {
    /// The root directory where Pattern writes canonical memory files.
    pub fn mount_path(&self) -> &Path {
        match self {
            StorageMode::A { mount_path, .. } => mount_path,
            StorageMode::B { mount_path, .. } => mount_path,
            StorageMode::C { mount_path } => mount_path,
        }
    }

    /// Whether this mode requires a `jj` adapter at attach time.
    ///
    /// Mode A works without `jj` (host VCS owns commits). Modes B and C
    /// require a supported `jj` installation — [`JjAdapter::detect`] must
    /// return `Ok(Some(_))` or attachment will fail with a typed error.
    ///
    /// [`JjAdapter::detect`]: crate::jj::JjAdapter::detect
    pub fn requires_jj(&self) -> bool {
        matches!(self, StorageMode::B { .. } | StorageMode::C { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_a_does_not_require_jj() {
        let mode = StorageMode::A {
            mount_path: PathBuf::from("/tmp/test"),
            project_root: PathBuf::from("/tmp"),
        };
        assert!(!mode.requires_jj());
    }

    #[test]
    fn mode_b_requires_jj() {
        let mode = StorageMode::B {
            mount_path: PathBuf::from("/tmp/test"),
            project_id: "proj-123".into(),
        };
        assert!(mode.requires_jj());
    }

    #[test]
    fn mode_c_requires_jj() {
        let mode = StorageMode::C {
            mount_path: PathBuf::from("/tmp/test"),
        };
        assert!(mode.requires_jj());
    }

    #[test]
    fn mount_path_round_trips() {
        let path = PathBuf::from("/some/mount");
        let mode_a = StorageMode::A {
            mount_path: path.clone(),
            project_root: PathBuf::from("/some"),
        };
        assert_eq!(mode_a.mount_path(), path.as_path());

        let mode_b = StorageMode::B {
            mount_path: path.clone(),
            project_id: "p".into(),
        };
        assert_eq!(mode_b.mount_path(), path.as_path());

        let mode_c = StorageMode::C {
            mount_path: path.clone(),
        };
        assert_eq!(mode_c.mount_path(), path.as_path());
    }
}
