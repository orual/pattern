//! Pattern path resolution for the memory subsystem.
//!
//! [`PatternPaths`] wraps [`pattern_core::PatternRoots`] (which owns
//! the cross-crate config/data/cache root resolution) and adds the
//! memory-specific subdirectory conventions: standalone mounts,
//! message databases, backups, and project-local InRepo/Sidecar
//! paths.
//!
//! See `pattern_core::paths` for the root resolution model and the
//! `$PATTERN_HOME` override semantics.

use std::path::{Path, PathBuf};

use pattern_core::paths::PatternRoots;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by path-resolution helpers in pattern_memory.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum PathError {
    /// Forwarded from [`pattern_core::paths::RootsError`].
    #[error(transparent)]
    #[diagnostic(transparent)]
    Roots(#[from] pattern_core::paths::RootsError),

    /// `std::fs::canonicalize` failed for the given path.
    ///
    /// The most common cause is that the path does not exist on disk —
    /// callers should create the directory before calling [`project_hash`].
    #[error("failed to canonicalize {path}: {source}")]
    #[diagnostic(code(pattern_memory::paths::canonicalize))]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// PatternPaths
// ---------------------------------------------------------------------------

/// Memory-subsystem path layout.
///
/// Wraps [`PatternRoots`] (config/data/cache resolution) and exposes
/// path builders for memory-specific files (standalone mounts,
/// messages, backups, project-local InRepo/Sidecar paths).
#[derive(Debug, Clone)]
pub struct PatternPaths {
    roots: PatternRoots,
}

impl PatternPaths {
    /// Resolve roots from the environment via [`PatternRoots::default_paths`].
    pub fn default_paths() -> Result<Self, PathError> {
        Ok(Self {
            roots: PatternRoots::default_paths()?,
        })
    }

    /// Pile all three roots under a single base directory: same
    /// shape as [`PatternRoots::with_base`]. Intended for tests.
    pub fn with_base(base: impl Into<PathBuf>) -> Self {
        Self {
            roots: PatternRoots::with_base(base),
        }
    }

    /// Build from an existing [`PatternRoots`].
    pub fn from_roots(roots: PatternRoots) -> Self {
        Self { roots }
    }

    /// Borrow the underlying roots — useful for handing to other
    /// subsystems that take a `&PatternRoots` directly (e.g.
    /// `pattern_provider::JsonFallbackStore::with_roots`).
    pub fn roots(&self) -> &PatternRoots {
        &self.roots
    }

    /// Root for user-editable configuration.
    pub fn config_root(&self) -> &Path {
        self.roots.config_root()
    }

    /// Root for durable user data.
    pub fn data_root(&self) -> &Path {
        self.roots.data_root()
    }

    /// Root for regenerable caches.
    pub fn cache_root(&self) -> &Path {
        self.roots.cache_root()
    }

    // -----------------------------------------------------------------------
    // Project-local paths (InRepo / Sidecar)
    // -----------------------------------------------------------------------

    /// Path where InRepo and Sidecar modes store `messages.db` for a project.
    ///
    /// Returns `<project_root>/.pattern/transient/messages.db`. The file
    /// lives inside the project at `.pattern/transient/` — gitignored so
    /// it is never committed, but project-adjacent for discoverability.
    pub fn in_repo_messages_path(project_root: &Path) -> PathBuf {
        project_root
            .join(".pattern")
            .join("transient")
            .join("messages.db")
    }

    /// Directory where InRepo/Sidecar stores `messages.db` snapshots
    /// for a project.
    ///
    /// Returns `<project_root>/.pattern/transient/backups/<project_name>/messages/`.
    pub fn project_backup_dir(&self, project_root: &Path, project_name: &str) -> PathBuf {
        project_root
            .join(".pattern")
            .join("transient")
            .join("backups")
            .join(project_name)
            .join("messages")
    }

    // -----------------------------------------------------------------------
    // Standalone-mode paths (under data_root)
    // -----------------------------------------------------------------------

    /// Standalone mount directory for a given project ID.
    ///
    /// Returns `<data_root>/projects/<id>/shared/`.
    pub fn standalone_mount_path(&self, project_id: &str) -> PathBuf {
        self.data_root()
            .join("projects")
            .join(project_id)
            .join("shared")
    }

    /// Standalone `messages.db` for a given project ID.
    ///
    /// Returns `<data_root>/projects/<id>/messages/messages.db`.
    pub fn standalone_messages_path(&self, project_id: &str) -> PathBuf {
        self.data_root()
            .join("projects")
            .join(project_id)
            .join("messages")
            .join("messages.db")
    }

    /// Directory where Standalone mode stores `messages.db` snapshots
    /// for a given project ID.
    ///
    /// Returns `<data_root>/backups/<id>/messages/`.
    pub fn backup_dir(&self, project_id: &str) -> PathBuf {
        self.data_root()
            .join("backups")
            .join(project_id)
            .join("messages")
    }

    /// Full path for a snapshot file for the given project ID and timestamp.
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
/// # Errors
///
/// Returns [`PathError::Canonicalize`] if `std::fs::canonicalize` fails.
pub fn project_hash(project_root: &Path) -> Result<String, PathError> {
    let canonical = std::fs::canonicalize(project_root).map_err(|e| PathError::Canonicalize {
        path: project_root.to_owned(),
        source: e,
    })?;
    let bytes = canonical.to_string_lossy();
    let hash = blake3::hash(bytes.as_bytes());
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
    fn with_base_piles_three_roots_under_one_dir() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        assert_eq!(paths.config_root(), tmp.path().join("config"));
        assert_eq!(paths.data_root(), tmp.path().join("data"));
        assert_eq!(paths.cache_root(), tmp.path().join("cache"));
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
        let with_dot = tmp.path().join(".").join(".");
        let h1 = project_hash(tmp.path()).unwrap();
        let h2 = project_hash(&with_dot).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn two_distinct_paths_produce_distinct_hashes() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let h1 = project_hash(tmp1.path()).unwrap();
        let h2 = project_hash(tmp2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn standalone_mount_path_under_data_root() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let path = paths.standalone_mount_path("my-project");
        assert!(path.starts_with(paths.data_root()));
        assert!(path.ends_with(Path::new("projects/my-project/shared")));
    }

    #[test]
    fn backup_dir_under_data_root() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let dir = paths.backup_dir("my-project");
        assert!(dir.starts_with(paths.data_root()));
        assert!(dir.ends_with(Path::new("backups/my-project/messages")));
    }

    #[test]
    fn project_backup_dir_is_project_local() {
        let project = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(base.path());
        let dir = paths.project_backup_dir(project.path(), "my-project");
        assert!(dir.starts_with(project.path()));
        assert!(!dir.starts_with(base.path()));
    }
}
