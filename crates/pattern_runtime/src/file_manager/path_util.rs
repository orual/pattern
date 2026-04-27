//! Shared path utilities for the file manager subsystem.

use std::path::{Component, Path, PathBuf};

/// Best-effort path canonicalization for file operations.
///
/// Strategy (in order):
/// 1. Lexically normalize `..` and `.` components so that even non-existent
///    files are resolved correctly (e.g. write-new case). This prevents
///    `sub/../../etc/passwd` style escapes without requiring filesystem access.
/// 2. Then try `std::fs::canonicalize` on the normalized path to resolve any
///    remaining symlinks. If that fails (file not on disk), use the lexically
///    normalized path.
pub(crate) fn canonicalize_best(path: &Path) -> PathBuf {
    let normalized = lexically_normalize(path);
    std::fs::canonicalize(&normalized).unwrap_or(normalized)
}

/// Lexically normalize a path by resolving `.` and `..` components without
/// touching the filesystem.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    let mut has_root = false;

    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {
                parts.clear();
                parts.push(component.as_os_str().to_owned());
                has_root = true;
            }
            Component::CurDir => {
                // `.` — skip.
            }
            Component::ParentDir => {
                if has_root || parts.last().is_some_and(|p| p != "..") {
                    parts.pop();
                } else {
                    parts.push(component.as_os_str().to_owned());
                }
            }
            Component::Normal(name) => {
                parts.push(name.to_owned());
            }
        }
    }

    let mut result = PathBuf::new();
    for part in &parts {
        result.push(part);
    }
    result
}
