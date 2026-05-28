// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Host VCS detection helpers.
//!
//! Provides [`discover_host_vcs`] which walks upward from a starting path to
//! find the nearest host VCS root (git or jj). The result is a [`HostVcs`]
//! variant paired with an optional root path.
//!
//! # Preference rule
//!
//! When both `.jj/` and `.git/` exist at the same level (a colocated jj
//! workspace), [`HostVcs::Jj`] is returned. Pattern's Sidecar mode relies on this
//! colocated layout; always preferring jj avoids accidentally treating a
//! colocated repo as a plain git repo.

use std::path::{Path, PathBuf};

/// The kind of host VCS detected at or above a starting directory.
///
/// `#[non_exhaustive]` allows future VCS types (e.g. Pijul, Sapling) without
/// breaking existing match arms.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostVcs {
    /// The nearest VCS root is a plain git repository (`.git/` present,
    /// no `.jj/` at the same level).
    Git,
    /// The nearest VCS root is a jj repository (`.jj/` present).
    ///
    /// This includes colocated workspaces where both `.jj/` and `.git/`
    /// exist; jj takes precedence in that case.
    Jj,
    /// No VCS root was found walking up to the filesystem root.
    None,
}

/// Walk upward from `start` to find the nearest host VCS root.
///
/// Returns `(vcs_kind, Some(root_path))` on success, or
/// `(HostVcs::None, None)` if no VCS root was found.
///
/// # Jj-first scan
///
/// The first pass walks upward checking for `.jj/` directories. If `.jj/`
/// is found before `.git/`, [`HostVcs::Jj`] is returned — this correctly
/// handles colocated repos where both markers co-exist at the same level.
///
/// # Git detection
///
/// If no `.jj/` is found, `gix_discover::upwards` is used to locate a
/// `.git/` directory or file (bare repos use a file). Its result is
/// normalised to the working-tree root.
pub fn discover_host_vcs(start: &Path) -> (HostVcs, Option<PathBuf>) {
    // First pass: walk upward for .jj/ — explicit, handles colocated repos.
    let mut cur = start;
    loop {
        if cur.join(".jj").is_dir() {
            return (HostVcs::Jj, Some(cur.to_owned()));
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }

    // Second pass: use gix-discover for .git detection.
    // `gix_discover::upwards` walks upward internally; it handles both
    // `.git/` directories (standard repos) and `.git` files (worktrees /
    // submodules).
    if let Ok((repo_path, _trust)) = gix_discover::upwards(start) {
        // Handle all three gix_discover path variants in a single match,
        // avoiding a redundant second upwards() call for the bare-repo case.
        let root = match repo_path {
            // WorkTree(path): path IS the work-tree root (no .git suffix).
            gix_discover::repository::Path::WorkTree(root) => Some(root),
            // LinkedWorkTree: separate git dir; work_dir is the checkout root.
            gix_discover::repository::Path::LinkedWorkTree { work_dir, .. } => Some(work_dir),
            // Repository(path): bare repo; no work-tree, but the repo dir
            // itself is the most useful root to return.
            gix_discover::repository::Path::Repository(repo_dir) => Some(repo_dir),
        };
        return (HostVcs::Git, root);
    }

    (HostVcs::None, None)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn git_init(dir: &Path) {
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .expect("git must be on PATH for VCS tests");
        assert!(status.success(), "git init failed");
        // git init leaves HEAD but no commits; that's fine for detection.
    }

    fn jj_dir(dir: &Path) {
        // Simulate a jj workspace by creating a .jj/ directory — we don't
        // need a real jj repo, just the marker directory the walker checks.
        std::fs::create_dir_all(dir.join(".jj")).expect("create .jj failed");
    }

    #[test]
    fn git_repo_detected() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());
        let (vcs, root) = discover_host_vcs(tmp.path());
        assert_eq!(vcs, HostVcs::Git);
        // gix-discover canonicalizes the path; just check it's Some.
        assert!(root.is_some());
    }

    #[test]
    fn jj_repo_detected() {
        let tmp = TempDir::new().unwrap();
        jj_dir(tmp.path());
        let (vcs, root) = discover_host_vcs(tmp.path());
        assert_eq!(vcs, HostVcs::Jj);
        assert_eq!(root.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn colocated_prefers_jj() {
        let tmp = TempDir::new().unwrap();
        // Both .git/ and .jj/ at the same level — jj should win.
        git_init(tmp.path());
        jj_dir(tmp.path());
        let (vcs, _root) = discover_host_vcs(tmp.path());
        assert_eq!(vcs, HostVcs::Jj);
    }

    #[test]
    fn empty_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        let (vcs, root) = discover_host_vcs(tmp.path());
        assert_eq!(vcs, HostVcs::None);
        assert_eq!(root, None);
    }

    #[test]
    fn nested_subdir_discovers_git_ancestor() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());
        // Create a deep subdirectory; the walk should find .git at the root.
        let deep = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        let (vcs, root) = discover_host_vcs(&deep);
        assert_eq!(vcs, HostVcs::Git);
        assert!(root.is_some());
    }

    #[test]
    fn nested_subdir_discovers_jj_ancestor() {
        let tmp = TempDir::new().unwrap();
        jj_dir(tmp.path());
        let deep = tmp.path().join("x").join("y");
        std::fs::create_dir_all(&deep).unwrap();
        let (vcs, root) = discover_host_vcs(&deep);
        assert_eq!(vcs, HostVcs::Jj);
        assert_eq!(root.as_deref(), Some(tmp.path()));
    }
}
