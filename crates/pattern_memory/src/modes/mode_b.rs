//! Mode B storage initialization.
//!
//! Mode B creates a separate Pattern-owned jj repository at
//! `<paths.base()>/projects/<id>/shared/`. Pattern runs `jj commit` for
//! history. `messages.db` lives at
//! `<paths.base()>/projects/<id>/messages/messages.db`.
//!
//! The mount directory layout after init:
//!
//! ```text
//! ~/.pattern/projects/<id>/
//! ├── shared/
//! │   ├── .pattern.kdl
//! │   ├── .jj/                   (created by jj git init)
//! │   ├── memory.db              (created at attach time by ConstellationDb)
//! │   ├── blocks/
//! │   │   ├── core/
//! │   │   └── working/
//! │   ├── personas/
//! │   └── lib/
//! └── messages/
//!     └── messages.db            (created at attach time by ConstellationDb)
//! ```

use chrono::Utc;

use super::StorageMode;
use super::error::ModeError;
use crate::jj::JjAdapter;
use crate::paths::PatternPaths;

/// Initialize a Mode B mount for the given project ID.
///
/// Creates the mount directory tree at `<paths.base()>/projects/<id>/shared/`,
/// writes a `.pattern.kdl` config, ensures the messages directory exists,
/// and initializes a jj git repository in the mount.
///
/// The [`PatternPaths`] argument controls where files are written. Use
/// `PatternPaths::default_paths()?` in production and
/// `PatternPaths::with_base(tempdir.path())` in tests.
///
/// # Errors
///
/// Returns [`ModeError`] on any filesystem or jj failure.
pub fn init(
    project_id: &str,
    jj_adapter: &JjAdapter,
    paths: &PatternPaths,
) -> Result<StorageMode, ModeError> {
    let mount_path = paths.mode_b_mount_path(project_id);

    // Create the directory structure.
    for subdir in ["blocks/core", "blocks/working", "personas", "lib"] {
        std::fs::create_dir_all(mount_path.join(subdir)).map_err(|e| ModeError::Io {
            path: mount_path.join(subdir),
            source: e,
        })?;
    }

    // Ensure the messages directory exists.
    let msgs_path = paths.mode_b_messages_path(project_id);
    if let Some(parent) = msgs_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ModeError::Io {
            path: parent.to_owned(),
            source: e,
        })?;
    }

    let now = Utc::now().to_rfc3339();

    // Scaffold .pattern.kdl with Mode B defaults.
    let kdl = format!(
        r#"mount mode="B" memory-db="memory.db"

personas {{
    default "@pattern-default"
}}

isolate-from-persona policy="none"

jj enabled=true

project name="{project_id}" created-at="{now}"
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

    Ok(StorageMode::B {
        mount_path,
        project_id: project_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// Mode B init requires a real `jj` binary on PATH. These tests are
    /// skipped if `jj` is not available (CI may not have it).
    fn skip_if_no_jj() -> Option<JjAdapter> {
        match JjAdapter::detect() {
            Ok(Some(adapter)) => Some(adapter),
            _ => {
                eprintln!("skipping Mode B test: jj not available");
                None
            }
        }
    }

    #[test]
    fn init_creates_mount_layout() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let home = TempDir::new().expect("tempdir for PatternPaths base");
        let paths = PatternPaths::with_base(home.path());

        // Use a short stable project ID — the tempdir provides isolation.
        let project_id = format!("test-mode-b-{}", uuid::Uuid::new_v4().simple());
        let mount_path = paths.mode_b_mount_path(&project_id);

        let mode = init(&project_id, &adapter, &paths).unwrap();

        assert!(mount_path.join("blocks/core").is_dir());
        assert!(mount_path.join("blocks/working").is_dir());
        assert!(mount_path.join("personas").is_dir());
        assert!(mount_path.join("lib").is_dir());
        assert!(mount_path.join(".pattern.kdl").is_file());
        // jj should have created a .jj directory.
        assert!(mount_path.join(".jj").is_dir());

        match &mode {
            StorageMode::B {
                mount_path: mp,
                project_id: pid,
            } => {
                assert_eq!(mp, &mount_path);
                assert_eq!(pid, &project_id);
            }
            _ => panic!("expected StorageMode::B"),
        }

        // home drops here, deleting the tempdir and all Mode B state.
    }

    #[test]
    fn init_writes_valid_kdl_config() {
        let Some(adapter) = skip_if_no_jj() else {
            return;
        };

        let home = TempDir::new().expect("tempdir for PatternPaths base");
        let paths = PatternPaths::with_base(home.path());

        let project_id = format!("test-mode-b-kdl-{}", uuid::Uuid::new_v4().simple());
        let mount_path = paths.mode_b_mount_path(&project_id);

        init(&project_id, &adapter, &paths).unwrap();

        let kdl_path = mount_path.join(".pattern.kdl");
        let config = crate::config::load_mount_config(&kdl_path).unwrap();
        assert_eq!(config.mount.mode, crate::config::ModeKind::B);
        assert!(config.jj.enabled);
        assert_eq!(config.project.name, project_id);

        // home drops here, deleting the tempdir and all Mode B state.
    }
}
