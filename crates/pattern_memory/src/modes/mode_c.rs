//! Mode C storage initialization.
//!
//! Mode C creates a "sidecar" jj repository inside `.pattern/shared/` within
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
//! ├── .gitignore               (`.pattern/shared/.jj/` appended)
//! ├── .pattern/
//! │   └── shared/
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

/// Initialize a Mode C mount at the given project root.
///
/// Creates the `.pattern/shared/` directory tree, writes a `.pattern.kdl`
/// config with `mode="C"` and `jj enabled=true`, initializes a jj git
/// repository inside `.pattern/shared/`, and appends `.pattern/shared/.jj/`
/// to the project root's `.gitignore`.
///
/// `messages.db` placement follows Mode A's convention: it lives outside the
/// project repo at `~/.pattern/transient/<hash>/messages.db` so that
/// ephemeral conversation data is never committed.
///
/// # Errors
///
/// Returns [`ModeError::Io`] on any filesystem failure, [`ModeError::Jj`] if
/// `jj git init` fails, or [`ModeError::Path`] if path resolution fails.
pub fn init(project_root: &Path, jj_adapter: &JjAdapter) -> Result<StorageMode, ModeError> {
    let mount_path = project_root.join(".pattern").join("shared");

    // Create the directory structure. `create_dir_all` is race-safe per std docs.
    for subdir in ["blocks/core", "blocks/working", "personas", "lib"] {
        std::fs::create_dir_all(mount_path.join(subdir)).map_err(|e| ModeError::Io {
            path: mount_path.join(subdir),
            source: e,
        })?;
    }

    // Derive project name from the directory name.
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("pattern-project");
    let now = Utc::now().to_rfc3339();

    // Scaffold .pattern.kdl with Mode C defaults.
    let kdl = format!(
        r#"mount mode="C" memory-db="memory.db"

personas {{
    default "@pattern-default"
}}

isolate-from-persona policy="none"

jj enabled=true

project name="{project_name}" created-at="{now}"
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

    // Also ensure .pattern/transient/ is gitignored (messages.db lives there,
    // outside the project repo, same convention as Mode A).
    gitignore::append_if_missing(project_root, ".pattern/transient/")?;

    Ok(StorageMode::C { mount_path })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::jj::JjAdapter;

    /// Mode C init requires a real `jj` binary on PATH. These tests are
    /// skipped if `jj` is not available.
    fn skip_if_no_jj() -> Option<JjAdapter> {
        match JjAdapter::detect() {
            Ok(Some(adapter)) => Some(adapter),
            _ => {
                eprintln!("skipping Mode C test: jj not available");
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
        let mode = init(tmp.path(), &adapter).unwrap();

        let mount_path = tmp.path().join(".pattern").join("shared");
        assert!(mount_path.join("blocks/core").is_dir());
        assert!(mount_path.join("blocks/working").is_dir());
        assert!(mount_path.join("personas").is_dir());
        assert!(mount_path.join("lib").is_dir());
        assert!(mount_path.join(".pattern.kdl").is_file());
        // jj should have created a .jj directory.
        assert!(mount_path.join(".jj").is_dir());

        match &mode {
            StorageMode::C { mount_path: mp } => {
                assert_eq!(mp, &mount_path);
            }
            _ => panic!("expected StorageMode::C"),
        }
    }

    #[test]
    fn init_writes_valid_kdl_config() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), &adapter).unwrap();

        let kdl_path = tmp.path().join(".pattern/shared/.pattern.kdl");
        let config = crate::config::load_mount_config(&kdl_path).unwrap();
        assert_eq!(config.mount.mode, crate::config::ModeKind::C);
        assert!(config.jj.enabled);
        assert_eq!(config.mount.memory_db, "memory.db");
    }

    #[test]
    fn init_creates_gitignore_entries() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), &adapter).unwrap();

        let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(
            gitignore.contains(".pattern/shared/.jj/"),
            "gitignore should contain .pattern/shared/.jj/"
        );
        assert!(
            gitignore.contains(".pattern/transient/"),
            "gitignore should contain .pattern/transient/"
        );
    }

    #[test]
    fn init_idempotent_gitignore() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let tmp = TempDir::new().unwrap();
        init(tmp.path(), &adapter).unwrap();
        // Re-init should not duplicate entries (though it will re-create .jj/).
        init(tmp.path(), &adapter).unwrap();

        let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        let count = gitignore
            .lines()
            .filter(|l| l.trim() == ".pattern/shared/.jj/")
            .count();
        assert_eq!(count, 1, ".jj/ entry should appear exactly once");
    }
}
