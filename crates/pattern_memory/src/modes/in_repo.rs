// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! InRepo mode storage initialization.
//!
//! InRepo mode puts block files inside the project repo at
//! `<project>/.pattern/shared/` and delegates history to the host VCS (git
//! or jj). `messages.db` lives inside the project at
//! `<project>/.pattern/transient/messages.db`, gitignored so it is never
//! committed, but project-adjacent for discoverability.
//!
//! The mount directory layout after init:
//!
//! ```text
//! <project>/
//! ├── .pattern/
//! │   ├── transient/
//! │   │   └── messages.db        (created at attach time by ConstellationDb)
//! │   └── shared/
//! │       ├── .pattern.kdl
//! │       ├── memory.db          (created at attach time by ConstellationDb)
//! │       ├── blocks/
//! │       │   ├── core/
//! │       │   └── working/
//! │       ├── personas/
//! │       └── lib/
//! └── .gitignore                 (`.pattern/transient/` + WAL sidecars appended)
//! ```

use std::path::Path;

use chrono::Utc;

use super::StorageMode;
use super::error::ModeError;
use super::gitignore;

/// Initialize a InRepo mode mount at the given project root.
///
/// Creates the `.pattern/shared/` directory tree, writes a `.pattern.kdl`
/// config, and ensures `.pattern/transient/` is in the project's `.gitignore`.
///
/// Idempotent for directory creation (re-running on an already-initialized
/// project only appends to `.gitignore` if the entry is missing).
///
/// # Errors
///
/// Returns [`ModeError::Io`] on any filesystem failure.
pub fn init(project_root: &Path, project_id: &str) -> Result<StorageMode, ModeError> {
    let mount_path = project_root.join(".pattern").join("shared");

    // Create the directory structure. `create_dir_all` is race-safe per std docs.
    for subdir in ["blocks/core", "blocks/working", "personas", "lib"] {
        std::fs::create_dir_all(mount_path.join(subdir)).map_err(|e| ModeError::Io {
            path: mount_path.join(subdir),
            source: e,
        })?;
    }

    // The id is the caller-resolved canonical handle (passed by the
    // CLI from the projects registry). The display name is the raw
    // directory basename — preserves human-readable form for non-slug
    // names (spaces, non-ASCII, etc.). Falls back to `id` if file_name
    // is unreadable.
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(project_id);
    let now = Utc::now().to_rfc3339();

    // Scaffold .pattern.kdl with InRepo mode defaults.
    let kdl = format!(
        r#"mount mode="in-repo" memory-db="memory.db"

personas {{
    default "@pattern-default"
}}

isolate-from-persona policy="none"

jj enabled=false

project id="{project_id}" name="{project_name}" created-at="{now}"
"#
    );

    let kdl_path = mount_path.join(".pattern.kdl");
    std::fs::write(&kdl_path, kdl).map_err(|e| ModeError::Io {
        path: kdl_path,
        source: e,
    })?;

    // Ensure .pattern/transient/ is gitignored (messages.db lives there,
    // inside the project but outside VCS history).
    gitignore::append_if_missing(project_root, ".pattern/transient/")?;
    // WAL sidecar files appear during SQLite writes and must not be committed.
    gitignore::append_if_missing(project_root, ".pattern/shared/memory.db-wal")?;
    gitignore::append_if_missing(project_root, ".pattern/shared/memory.db-shm")?;

    Ok(StorageMode::InRepo {
        mount_path,
        project_root: project_root.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn init_creates_mount_layout() {
        let tmp = TempDir::new().unwrap();
        let mode = init(tmp.path(), "test-mode").unwrap();

        let mount_path = tmp.path().join(".pattern").join("shared");
        assert!(mount_path.join("blocks/core").is_dir());
        assert!(mount_path.join("blocks/working").is_dir());
        assert!(mount_path.join("personas").is_dir());
        assert!(mount_path.join("lib").is_dir());
        assert!(mount_path.join(".pattern.kdl").is_file());

        match &mode {
            StorageMode::InRepo {
                mount_path: mp,
                project_root: pr,
            } => {
                assert_eq!(mp, &mount_path);
                assert_eq!(pr, tmp.path());
            }
            _ => panic!("expected StorageMode::InRepo"),
        }
    }

    #[test]
    fn init_writes_valid_kdl_config() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test").unwrap();

        let kdl_path = tmp.path().join(".pattern/shared/.pattern.kdl");
        let content = std::fs::read_to_string(&kdl_path).unwrap();

        // Verify key properties are present.
        assert!(content.contains(r#"mode="in-repo""#));
        assert!(content.contains(r#"memory-db="memory.db""#));
        assert!(content.contains("jj enabled=false"));
        assert!(content.contains(r#"project id="test""#));
        assert!(content.contains("name="));
    }

    #[test]
    fn init_creates_gitignore_entry() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test").unwrap();

        let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(
            gitignore.contains(".pattern/transient/"),
            "gitignore should exclude .pattern/transient/"
        );
        assert!(
            gitignore.contains(".pattern/shared/memory.db-wal"),
            "gitignore should exclude WAL sidecar"
        );
        assert!(
            gitignore.contains(".pattern/shared/memory.db-shm"),
            "gitignore should exclude SHM sidecar"
        );
    }

    #[test]
    fn init_idempotent_gitignore() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test").unwrap();
        init(tmp.path(), "test").unwrap();

        let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        let count = gitignore
            .lines()
            .filter(|l| l.trim() == ".pattern/transient/")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn init_kdl_parseable_by_config_loader() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path(), "test").unwrap();

        let kdl_path = tmp.path().join(".pattern/shared/.pattern.kdl");
        let config = crate::config::load_mount_config(&kdl_path).unwrap();
        assert_eq!(config.mount.mode, crate::config::ModeKind::InRepo);
        assert_eq!(config.mount.memory_db, "memory.db");
        assert!(!config.jj.enabled);
    }
}
