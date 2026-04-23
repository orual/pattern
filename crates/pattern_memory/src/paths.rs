//! Pattern home directory and project-hash path helpers.
//!
//! Provides the stable conventions for where Pattern stores its files,
//! encapsulated in [`PatternPaths`]:
//!
//! - `base()` — `~/.pattern/`
//! - `in_repo_messages_path()` — `<project>/.pattern/transient/messages.db`
//! - `standalone_mount_path()` — `~/.pattern/projects/<id>/shared/`
//! - `standalone_messages_path()` — `~/.pattern/projects/<id>/messages/messages.db`
//!
//! For InRepo and Sidecar modes, messages.db lives inside the project repo at
//! `<project>/.pattern/transient/` (gitignored so it is never committed, but
//! project-adjacent for discoverability). The `.pattern/transient/` entry in
//! the project's `.gitignore` keeps it out of VCS history.
//!
//! [`project_hash`] is a free function because it does not depend on the base
//! directory — it only hashes the project root path.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by path-resolution helpers.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum PathError {
    /// No home directory is available in the current environment.
    ///
    /// On POSIX systems this means neither `$HOME` nor the passwd database
    /// returned a valid home directory. Unusual but possible in containers or
    /// stripped CI environments.
    #[error("no home directory available")]
    #[diagnostic(code(pattern_memory::paths::no_home))]
    NoHome,

    /// `std::fs::canonicalize` failed for the given path.
    ///
    /// The most common cause is that the path does not exist on disk — callers
    /// should create the directory before calling [`project_hash`].
    #[error("failed to canonicalize {path}: {source}")]
    #[diagnostic(code(pattern_memory::paths::canonicalize))]
    Canonicalize {
        /// The path that could not be canonicalized.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// PatternPaths
// ---------------------------------------------------------------------------

/// Resolved path layout for Pattern's home directory.
///
/// Production: `PatternPaths::default_paths()` resolves `~/.pattern/` via
/// [`dirs::home_dir`].
/// Tests: `PatternPaths::with_base(tempdir.path())` uses a custom root,
/// removing the need for `unsafe { std::env::set_var(...) }`.
#[derive(Debug, Clone)]
pub struct PatternPaths {
    base: PathBuf,
}

impl PatternPaths {
    /// Resolve the default Pattern home directory.
    ///
    /// Resolution order:
    /// 1. `$PATTERN_HOME` if set — used by integration tests that spawn the
    ///    CLI as a subprocess (safe: no shared address space involved).
    /// 2. `~/.pattern/` via [`dirs::home_dir`].
    ///
    /// Returns [`PathError::NoHome`] if neither source is available.
    ///
    /// Unit tests should use [`PatternPaths::with_base`] instead of setting
    /// `$PATTERN_HOME`, which avoids any risk of env-var races.
    pub fn default_paths() -> Result<Self, PathError> {
        let base = std::env::var("PATTERN_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".pattern")))
            .ok_or(PathError::NoHome)?;
        Ok(Self { base })
    }

    /// Use a custom base directory.
    ///
    /// Intended for tests — callers pass a `TempDir` path to avoid any writes
    /// to the real `~/.pattern/`. No unsafe env-var manipulation required.
    pub fn with_base(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// The base directory (e.g. `~/.pattern/`).
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Path where InRepo mode (and Sidecar mode) stores `messages.db` for a project.
    ///
    /// Returns `<project_root>/.pattern/transient/messages.db`.
    ///
    /// The file lives inside the project at `.pattern/transient/` — gitignored
    /// so it is never committed, but project-adjacent for discoverability. The
    /// caller is responsible for creating the directory before opening the DB.
    ///
    /// This method does not use `&self` (no `~/.pattern/` path is involved for
    /// InRepo/Sidecar); it is kept as an associated method for symmetry with
    /// `standalone_messages_path`.
    pub fn in_repo_messages_path(project_root: &Path) -> PathBuf {
        project_root
            .join(".pattern")
            .join("transient")
            .join("messages.db")
    }

    /// Path where Standalone mode stores its mount directory for a given project ID.
    ///
    /// Returns `<base>/projects/<id>/shared/`. This is the root of the
    /// Pattern-owned jj repository for Standalone mode mounts.
    pub fn standalone_mount_path(&self, project_id: &str) -> PathBuf {
        self.base.join("projects").join(project_id).join("shared")
    }

    /// Path where Standalone mode stores `messages.db` for a given project ID.
    ///
    /// Returns `<base>/projects/<id>/messages/messages.db`. Stored outside
    /// the jj worktree so that history commits don't include conversation data.
    pub fn standalone_messages_path(&self, project_id: &str) -> PathBuf {
        self.base
            .join("projects")
            .join(project_id)
            .join("messages")
            .join("messages.db")
    }

    /// Directory where `messages.db` snapshots are stored for a given project ID.
    ///
    /// Returns `<base>/backups/<id>/messages/`. Used by Standalone mode, which has no
    /// host repo to put backup files in. Created on first snapshot if it does
    /// not yet exist.
    pub fn backup_dir(&self, project_id: &str) -> PathBuf {
        self.base.join("backups").join(project_id).join("messages")
    }

    /// Directory where InRepo/Sidecar stores `messages.db` snapshots for a project.
    ///
    /// Returns `<project_root>/.pattern/transient/backups/<project_name>/messages/`.
    /// Kept inside `.pattern/transient/` so it is gitignored by the same rule
    /// that covers the live `messages.db`. Created on first snapshot if it does
    /// not yet exist.
    pub fn project_backup_dir(&self, project_root: &Path, project_name: &str) -> PathBuf {
        project_root
            .join(".pattern")
            .join("transient")
            .join("backups")
            .join(project_name)
            .join("messages")
    }

    /// Full path for a snapshot file for the given project ID and timestamp.
    ///
    /// Returns `<backup_dir>/<timestamp>.sqlite` where `<timestamp>` is
    /// formatted per [`crate::backup::snapshot::SNAPSHOT_FILENAME_FORMAT`]
    /// (e.g. `2026-04-19T120000Z.sqlite`).
    pub fn backup_snapshot_path(&self, project_id: &str, ts: &jiff::Timestamp) -> PathBuf {
        let name = crate::backup::snapshot::format_snapshot_name(ts);
        self.backup_dir(project_id).join(format!("{name}.sqlite"))
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Derive a stable 16-character hex project hash from a project repository path.
///
/// The path is canonicalized first so that relative paths, `..` components,
/// and symlinks all resolve to the same hash as their canonical form.
///
/// The hash is `blake3::hash(canonical_path_bytes)[..8]` encoded as hex
/// (16 characters = 8 bytes). This matches the workspace content-hash
/// convention described in `pattern_runtime/CLAUDE.md` and provides
/// collision resistance adequate for Pattern's scale.
///
/// # Errors
///
/// Returns [`PathError::Canonicalize`] if `std::fs::canonicalize` fails,
/// which most commonly means the directory does not exist on disk.
pub fn project_hash(project_root: &Path) -> Result<String, PathError> {
    let canonical = std::fs::canonicalize(project_root).map_err(|e| PathError::Canonicalize {
        path: project_root.to_owned(),
        source: e,
    })?;
    let bytes = canonical.to_string_lossy();
    let hash = blake3::hash(bytes.as_bytes());
    // First 16 hex chars = 8 bytes.
    Ok(hash.to_hex().as_str().chars().take(16).collect())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn default_paths_ends_with_dot_pattern() {
        let paths = PatternPaths::default_paths().expect("home dir must be available in test env");
        let last = paths
            .base()
            .file_name()
            .and_then(|n| n.to_str())
            .expect("base has a file name");
        assert_eq!(last, ".pattern");
    }

    #[test]
    fn with_base_uses_custom_root() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        assert_eq!(paths.base(), tmp.path());
    }

    #[test]
    fn project_hash_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let h1 = project_hash(tmp.path()).unwrap();
        let h2 = project_hash(tmp.path()).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn project_hash_is_16_chars() {
        let tmp = TempDir::new().unwrap();
        let h = project_hash(tmp.path()).unwrap();
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn project_hash_canonicalizes_dot_slash() {
        let tmp = TempDir::new().unwrap();
        // Appending /./ should produce the same hash.
        let with_dot = tmp.path().join(".").join(".");
        // std::fs::canonicalize resolves the trailing ./ segments.
        let h1 = project_hash(tmp.path()).unwrap();
        let h2 = project_hash(&with_dot).unwrap();
        assert_eq!(h1, h2, "trailing ./ should not change the hash");
    }

    #[test]
    fn two_distinct_paths_produce_distinct_hashes() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        // Highly unlikely (though not impossible) for two fresh tempdirs to
        // collide. The probability is 1 / 2^64, well below any reasonable
        // flakiness threshold.
        let h1 = project_hash(tmp1.path()).unwrap();
        let h2 = project_hash(tmp2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn in_repo_messages_path_structure() {
        let project = TempDir::new().unwrap();
        let path = PatternPaths::in_repo_messages_path(project.path());
        // Should be: <project>/.pattern/transient/messages.db
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("messages.db")
        );
        let transient = path.parent().unwrap();
        assert_eq!(
            transient.file_name().and_then(|n| n.to_str()),
            Some("transient")
        );
        let dot_pattern = transient.parent().unwrap();
        assert_eq!(
            dot_pattern.file_name().and_then(|n| n.to_str()),
            Some(".pattern")
        );
        // Verify it's rooted under the project directory, not somewhere in ~/.pattern/.
        assert!(path.starts_with(project.path()));
    }

    #[test]
    fn project_backup_dir_structure() {
        let project = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(base.path());
        let dir = paths.project_backup_dir(project.path(), "my-project");
        // Should be: <project>/.pattern/transient/backups/my-project/messages
        assert_eq!(dir.file_name().and_then(|n| n.to_str()), Some("messages"));
        let project_name_dir = dir.parent().unwrap();
        assert_eq!(
            project_name_dir.file_name().and_then(|n| n.to_str()),
            Some("my-project")
        );
        let backups_dir = project_name_dir.parent().unwrap();
        assert_eq!(
            backups_dir.file_name().and_then(|n| n.to_str()),
            Some("backups")
        );
        let transient = backups_dir.parent().unwrap();
        assert_eq!(
            transient.file_name().and_then(|n| n.to_str()),
            Some("transient")
        );
        // Verify it's inside the project, not the base (~/.pattern/).
        assert!(dir.starts_with(project.path()));
        assert!(!dir.starts_with(base.path()));
    }

    #[test]
    fn standalone_mount_path_structure() {
        let base = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(base.path());
        let path = paths.standalone_mount_path("my-project");
        // Should be: <base>/projects/my-project/shared
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("shared"));
        let id_component = path.parent().unwrap();
        assert_eq!(
            id_component.file_name().and_then(|n| n.to_str()),
            Some("my-project")
        );
        assert!(path.starts_with(base.path()));
    }

    #[test]
    fn standalone_messages_path_structure() {
        let base = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(base.path());
        let path = paths.standalone_messages_path("my-project");
        // Should be: <base>/projects/my-project/messages/messages.db
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("messages.db")
        );
        let messages_dir = path.parent().unwrap();
        assert_eq!(
            messages_dir.file_name().and_then(|n| n.to_str()),
            Some("messages")
        );
        assert!(path.starts_with(base.path()));
    }
}
