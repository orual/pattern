//! Sidecar mode storage initialization.
//!
//! Sidecar mode creates a "sidecar" jj repository inside `.pattern/shared/` within
//! a host git project. The pattern-jj repo is self-contained: its `.jj/`
//! directory lives at `.pattern/shared/.jj/` and only tracks files within
//! `.pattern/shared/`. Host git tracks the pattern files but NOT `.jj/`
//! (which is appended to `.gitignore`).
//!
//! This is NOT a colocated jj repo. Host git operations (checkout, merge,
//! reset) may change the pattern files on disk; jj sees these as working-copy
//! modifications, which is expected and benign.
//!
//! The mount directory layout after init:
//!
//! ```text
//! project/
//! ├── .git/                    ← host git
//! ├── .gitignore               (`.pattern/shared/.jj/`, `.pattern/transient/`,
//! │                             and WAL sidecars appended)
//! ├── .pattern/
//! │   ├── transient/
//! │   │   └── messages.db      (created at attach time by ConstellationDb)
//! │   └── shared/
//! │       ├── .gitignore       (WAL sidecars — jj reads this)
//! │       ├── .pattern.kdl
//! │       ├── .jj/             ← pattern-jj, gitignored by host
//! │       ├── memory.db        (created at attach time by ConstellationDb)
//! │       ├── blocks/
//! │       │   ├── core/
//! │       │   └── working/
//! │       ├── personas/
//! │       └── lib/
//! └── src/                     ← normal project files
//! ```

use std::path::Path;

use chrono::Utc;

use super::StorageMode;
use super::error::ModeError;
use super::gitignore;
use crate::jj::JjAdapter;

/// Initialize a Sidecar mode mount at the given project root.
///
/// Creates the `.pattern/shared/` directory tree, writes a `.pattern.kdl`
/// config with `mode="sidecar"` and `jj enabled=true`, initializes a jj git
/// repository inside `.pattern/shared/`, and appends `.pattern/shared/.jj/`
/// to the project root's `.gitignore`.
///
/// `messages.db` placement follows InRepo mode's convention: it lives inside the
/// project repo at `<project>/.pattern/transient/messages.db`, gitignored so
/// that ephemeral conversation data is never committed.
///
/// # Errors
///
/// Returns [`ModeError::Io`] on any filesystem failure, [`ModeError::Jj`] if
/// `jj git init` fails, or [`ModeError::Path`] if path resolution fails.
pub fn init(
    project_root: &Path,
    project_id: &str,
    jj_adapter: &JjAdapter,
) -> Result<StorageMode, ModeError> {
    let mount_path = project_root.join(".pattern").join("shared");

    // Create the directory structure. `create_dir_all` is race-safe per std docs.
    for subdir in ["blocks/core", "blocks/working", "personas", "lib"] {
        std::fs::create_dir_all(mount_path.join(subdir)).map_err(|e| ModeError::Io {
            path: mount_path.join(subdir),
            source: e,
        })?;
    }

    // The id is the caller-resolved canonical handle. The display name
    // is the raw directory basename so non-slug names (spaces,
    // non-ASCII) round-trip into the kdl unchanged.
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(project_id);
    let now = Utc::now().to_rfc3339();

    // Scaffold .pattern.kdl with Sidecar mode defaults.
    let kdl = format!(
        r#"mount mode="sidecar" memory-db="memory.db"

personas {{
    default "@pattern-default"
}}

isolate-from-persona policy="none"

jj enabled=true

project id="{project_id}" name="{project_name}" created-at="{now}"
"#
    );

    let kdl_path = mount_path.join(".pattern.kdl");
    std::fs::write(&kdl_path, kdl).map_err(|e| ModeError::Io {
        path: kdl_path,
        source: e,
    })?;

    // Initialize a jj git repository inside the mount if not already present.
    // Re-running init on an existing repo would fail with "target repo already
    // exists", so we skip the call when `.jj/` is already there.
    if !mount_path.join(".jj").is_dir() {
        jj_adapter.init_repo(&mount_path)?;
    }

    // Ensure .pattern/shared/.jj/ is gitignored by the host so that git never
    // touches jj's internal state. Because we use `--no-colocate`, the backing
    // git repo lives inside `.jj/repo/` (no top-level `.git/` is created).
    gitignore::append_if_missing(project_root, ".pattern/shared/.jj/")?;

    // Ensure .pattern/transient/ is gitignored (messages.db lives there,
    // inside the project but outside VCS history).
    gitignore::append_if_missing(project_root, ".pattern/transient/")?;

    // WAL sidecar files appear during SQLite writes and must not be committed
    // by the host git repo.
    gitignore::append_if_missing(project_root, ".pattern/shared/memory.db-wal")?;
    gitignore::append_if_missing(project_root, ".pattern/shared/memory.db-shm")?;

    // Also write a .gitignore inside .pattern/shared/ so that jj (which reads
    // gitignore files) excludes WAL sidecars from sidecar-jj commits as well.
    gitignore::append_if_missing(&mount_path, "memory.db-wal")?;
    gitignore::append_if_missing(&mount_path, "memory.db-shm")?;

    Ok(StorageMode::Sidecar { mount_path })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::jj::JjAdapter;

    /// Sidecar mode init requires a real `jj` binary on PATH. These tests are
    /// skipped if `jj` is not available.
    fn skip_if_no_jj() -> Option<JjAdapter> {
        match JjAdapter::detect() {
            Ok(Some(adapter)) => Some(adapter),
            _ => {
                eprintln!("skipping Sidecar mode test: jj not available");
                None
            }
        }
    }

    #[test]
    fn init_creates_mount_layout() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        let mode = init(tmp.path(), "test", &adapter).unwrap();

        let mount_path = tmp.path().join(".pattern").join("shared");
        assert!(mount_path.join("blocks/core").is_dir());
        assert!(mount_path.join("blocks/working").is_dir());
        assert!(mount_path.join("personas").is_dir());
        assert!(mount_path.join("lib").is_dir());
        assert!(mount_path.join(".pattern.kdl").is_file());
        // jj should have created a .jj directory.
        assert!(mount_path.join(".jj").is_dir());

        match &mode {
            StorageMode::Sidecar { mount_path: mp } => {
                assert_eq!(mp, &mount_path);
            }
            _ => panic!("expected StorageMode::Sidecar"),
        }
    }

    #[test]
    fn init_writes_valid_kdl_config() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test", &adapter).unwrap();

        let kdl_path = tmp.path().join(".pattern/shared/.pattern.kdl");
        let config = crate::config::load_mount_config(&kdl_path).unwrap();
        assert_eq!(config.mount.mode, crate::config::ModeKind::Sidecar);
        assert!(config.jj.enabled);
        assert_eq!(config.mount.memory_db, "memory.db");
    }

    #[test]
    fn init_creates_gitignore_entries() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test", &adapter).unwrap();

        let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(
            gitignore.contains(".pattern/shared/.jj/"),
            "gitignore should contain .pattern/shared/.jj/"
        );
        assert!(
            gitignore.contains(".pattern/transient/"),
            "gitignore should contain .pattern/transient/"
        );
        assert!(
            gitignore.contains(".pattern/shared/memory.db-wal"),
            "gitignore should contain WAL sidecar entry"
        );
        assert!(
            gitignore.contains(".pattern/shared/memory.db-shm"),
            "gitignore should contain SHM sidecar entry"
        );
    }

    #[test]
    fn init_creates_shared_gitignore_for_jj() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test", &adapter).unwrap();

        // jj reads .gitignore files in the working-copy directories. The shared
        // .gitignore ensures WAL sidecars are excluded from jj commits.
        let shared_gitignore =
            std::fs::read_to_string(tmp.path().join(".pattern/shared/.gitignore")).unwrap();
        assert!(
            shared_gitignore.contains("memory.db-wal"),
            "shared .gitignore should exclude memory.db-wal"
        );
        assert!(
            shared_gitignore.contains("memory.db-shm"),
            "shared .gitignore should exclude memory.db-shm"
        );
    }

    #[test]
    fn init_idempotent_gitignore() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test", &adapter).unwrap();
        // Re-init should not duplicate entries (though it will re-create .jj/).
        init(tmp.path(), "test", &adapter).unwrap();

        let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        let count = gitignore
            .lines()
            .filter(|l| l.trim() == ".pattern/shared/.jj/")
            .count();
        assert_eq!(count, 1, ".jj/ entry should appear exactly once");
    }
}
