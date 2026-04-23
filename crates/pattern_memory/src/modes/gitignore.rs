//! Helper for appending entries to a `.gitignore` file idempotently.
//!
//! Used by InRepo mode init to ensure `.pattern/transient/` (and similar entries)
//! are excluded from host VCS tracking.

use std::io::Write;
use std::path::Path;

use super::error::ModeError;

/// Append `entry` to the `.gitignore` at `project_root/.gitignore` if it is
/// not already present.
///
/// Creates the file if it does not exist. Ensures a trailing newline after
/// the entry. Idempotent: calling twice with the same entry produces no
/// duplicate lines.
///
/// # Errors
///
/// Returns [`ModeError::Io`] on any I/O failure reading or writing the file.
pub fn append_if_missing(project_root: &Path, entry: &str) -> Result<(), ModeError> {
    let path = project_root.join(".gitignore");
    let current = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ModeError::Io {
                path: path.clone(),
                source: e,
            });
        }
    };

    let needle = entry.trim_end_matches('\n');
    if current.lines().any(|line| line.trim() == needle) {
        return Ok(());
    }

    // Append-only open; atomic for a single write() <= PIPE_BUF on POSIX.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| ModeError::Io {
            path: path.clone(),
            source: e,
        })?;

    // Ensure a newline separator if the file doesn't end with one.
    if !current.is_empty() && !current.ends_with('\n') {
        f.write_all(b"\n").map_err(|e| ModeError::Io {
            path: path.clone(),
            source: e,
        })?;
    }

    f.write_all(entry.as_bytes()).map_err(|e| ModeError::Io {
        path: path.clone(),
        source: e,
    })?;
    f.write_all(b"\n").map_err(|e| ModeError::Io {
        path: path.clone(),
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn creates_gitignore_if_absent() {
        let tmp = TempDir::new().unwrap();
        append_if_missing(tmp.path(), ".pattern/transient/").unwrap();

        let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains(".pattern/transient/"));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn appends_to_existing_gitignore() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();

        append_if_missing(tmp.path(), ".pattern/transient/").unwrap();

        let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains("target/"));
        assert!(content.contains(".pattern/transient/"));
    }

    #[test]
    fn idempotent_does_not_duplicate() {
        let tmp = TempDir::new().unwrap();
        append_if_missing(tmp.path(), ".pattern/transient/").unwrap();
        append_if_missing(tmp.path(), ".pattern/transient/").unwrap();

        let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        let count = content
            .lines()
            .filter(|l| l.trim() == ".pattern/transient/")
            .count();
        assert_eq!(count, 1, "entry should appear exactly once");
    }

    #[test]
    fn handles_missing_trailing_newline() {
        let tmp = TempDir::new().unwrap();
        // Write existing content WITHOUT a trailing newline.
        std::fs::write(tmp.path().join(".gitignore"), "target/").unwrap();

        append_if_missing(tmp.path(), ".pattern/transient/").unwrap();

        let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        // Should have a newline between the existing content and the new entry.
        assert!(
            content.contains("target/\n.pattern/transient/"),
            "content was: {content:?}"
        );
    }
}
