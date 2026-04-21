//! Filesystem serialization for memory blocks.
//!
//! Each block schema maps to a canonical file format:
//! - **Text** → `.md` (passthrough, see [`markdown`])
//! - **Map / List / Composite** → `.kdl` (see [`kdl`])
//! - **Log** → `.jsonl` (see [`jsonl`])
//!
//! All writes go through [`atomic_write`] to prevent partial-write visibility
//! to the `notify` watcher or human editors.

pub mod error;
pub mod jsonl;
pub mod kdl;
pub mod markdown;
pub mod watcher;

pub use error::FsError;

use std::io::Write;
use std::path::Path;

/// Write `content` to `path` atomically: write to a `.tmp` sibling, fsync,
/// then rename over the target.
///
/// This prevents the `notify` watcher (or a human editor) from seeing a
/// partially-written file. On success the `.tmp` file no longer exists.
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<(), FsError> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| FsError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        f.write_all(content).map_err(|e| FsError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        f.sync_all().map_err(|e| FsError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| FsError::Io {
        path: path.to_owned(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");

        atomic_write(&path, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");

        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn atomic_write_tmp_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.kdl");

        atomic_write(&path, b"content").unwrap();

        let tmp = path.with_extension("kdl.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn atomic_write_no_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noext");

        atomic_write(&path, b"data").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");

        let tmp = path.with_extension("tmp.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn atomic_write_invalid_directory() {
        let path = std::path::PathBuf::from("/nonexistent_dir_12345/file.txt");
        let result = atomic_write(&path, b"data");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FsError::Io { .. }));
    }
}
