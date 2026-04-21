//! Mount discovery, attachment, and the [`MountedStore`] runtime handle.
//!
//! A "mount" is a directory containing Pattern-managed memory state. The
//! canonical marker is `.pattern/shared/.pattern.kdl`. The [`attach`]
//! function walks upward from a starting directory to find this marker,
//! parses the config, opens databases, spawns subscribers, and returns a
//! [`MountedStore`] that owns all resources for the mount's lifetime.
//!
//! [`detach`](MountedStore::detach) drains subscribers, stops the filesystem
//! watcher, and drops the database pool.
//!
//! # Module layout
//!
//! - `mount.rs` — this file; re-exports + `MountedStore` + `find_mount`.
//! - `mount/attach.rs` — the [`attach`] function.
//! - `mount/error.rs` — [`MountError`] type.

pub mod attach;
pub mod error;

pub use attach::attach;
pub use error::MountError;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pattern_db::ConstellationDb;

use crate::cache::MemoryCache;
use crate::config::MountConfig;
use crate::fs::watcher::MountWatcher;
use crate::modes::StorageMode;
use crate::reembed::ReembedQueue;

/// Runtime handle for an attached mount.
///
/// Owns the [`MemoryCache`], [`ConstellationDb`] pool, filesystem
/// [`MountWatcher`], and optional [`ReembedQueue`] for the mount's lifetime.
/// Call [`detach`](Self::detach) to cleanly shut down all resources.
///
/// The cache has subscriber support enabled: lazy subscriber spawning and
/// the supervisor task are active while this handle is alive.
pub struct MountedStore {
    /// The mount root directory (e.g. `<project>/.pattern/shared/`).
    pub mount_path: PathBuf,
    /// The parsed `.pattern.kdl` configuration.
    pub config: MountConfig,
    /// The resolved storage mode.
    pub mode: StorageMode,
    /// The in-memory cache with subscriber support.
    pub cache: Arc<MemoryCache>,
    /// The database pool for memory.db + messages.db.
    pub db: Arc<ConstellationDb>,
    /// The filesystem watcher (if started). `Option` so `detach` can take it.
    watcher: Option<MountWatcher>,
    /// The re-embed queue task (if a tokio runtime was available at attach
    /// time). Dropping this does not cancel the task — the task exits
    /// naturally when all senders are dropped (i.e., when the cache and all
    /// subscriber workers are gone). Stored here so `detach` drops it in the
    /// correct order: after draining subscribers, ensuring no new reembed
    /// requests are in-flight before the queue is released.
    pub(crate) reembed_queue: Option<ReembedQueue>,
}

impl MountedStore {
    /// Cleanly shut down all mount resources.
    ///
    /// 1. Stops the filesystem watcher (no more external-edit events).
    /// 2. Drains all subscriber workers (cancels tokens, joins threads).
    /// 3. Drops the re-embed queue handle.
    /// 4. Drops the cache and database pool references.
    ///
    /// This is intentionally synchronous — all teardown operations are sync.
    pub fn detach(mut self) {
        // Stop the watcher first so no new events arrive.
        drop(self.watcher.take());
        // Drain all subscriber workers (cancels tokens, joins OS threads).
        self.cache.drain_subscribers();
        // Release the re-embed queue. The task exits when all senders drop.
        drop(self.reembed_queue);
        // Drop the cache and DB — the Arcs may still have other references
        // but this handle's references are released.
        drop(self.cache);
        drop(self.db);
    }
}

impl std::fmt::Debug for MountedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountedStore")
            .field("mount_path", &self.mount_path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// Walk upward from `start` looking for `.pattern/shared/.pattern.kdl`.
///
/// Returns the mount path (the directory containing `.pattern.kdl`, i.e.
/// `<project>/.pattern/shared/`) or [`MountError::NotFound`] if no mount
/// is found before the filesystem root.
pub fn find_mount(start: &Path) -> Result<PathBuf, MountError> {
    let mut cur = start.to_owned();
    loop {
        let candidate = cur.join(".pattern").join("shared").join(".pattern.kdl");
        if candidate.is_file() {
            // mount_path is the directory containing .pattern.kdl.
            return Ok(candidate
                .parent()
                .expect(".pattern.kdl has a parent directory")
                .to_owned());
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_owned(),
            _ => break,
        }
    }
    Err(MountError::NotFound {
        started_at: start.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// Create a minimal mount structure in a tempdir for testing.
    fn setup_mode_a_mount(tmp: &Path) {
        crate::modes::mode_a::init(tmp).expect("Mode A init should succeed");
    }

    #[test]
    fn find_mount_at_project_root() {
        let tmp = TempDir::new().unwrap();
        setup_mode_a_mount(tmp.path());

        let found = find_mount(tmp.path()).unwrap();
        assert_eq!(found, tmp.path().join(".pattern").join("shared"));
    }

    #[test]
    fn find_mount_from_subdirectory() {
        let tmp = TempDir::new().unwrap();
        setup_mode_a_mount(tmp.path());

        let deep = tmp.path().join("src").join("lib").join("deep");
        std::fs::create_dir_all(&deep).unwrap();

        let found = find_mount(&deep).unwrap();
        assert_eq!(found, tmp.path().join(".pattern").join("shared"));
    }

    #[test]
    fn find_mount_not_found() {
        let tmp = TempDir::new().unwrap();
        let err = find_mount(tmp.path()).unwrap_err();
        assert!(
            matches!(err, MountError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
    }

    #[test]
    fn attach_mode_a_round_trip() {
        let tmp = TempDir::new().unwrap();
        setup_mode_a_mount(tmp.path());

        let store = attach(tmp.path()).unwrap();
        assert!(matches!(store.mode, StorageMode::A { .. }));
        assert_eq!(store.mount_path, tmp.path().join(".pattern").join("shared"));

        // Verify the DB is healthy.
        store.db.health_check().unwrap();

        // Detach cleanly.
        store.detach();
    }

    #[test]
    fn attach_not_found_error() {
        let tmp = TempDir::new().unwrap();
        let err = attach(tmp.path()).unwrap_err();
        assert!(
            matches!(err, MountError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
    }

    #[test]
    fn attach_detach_reattach() {
        let tmp = TempDir::new().unwrap();
        setup_mode_a_mount(tmp.path());

        // First attach.
        let store = attach(tmp.path()).unwrap();
        store.db.health_check().unwrap();
        store.detach();

        // Re-attach should succeed with identical state.
        let store2 = attach(tmp.path()).unwrap();
        store2.db.health_check().unwrap();
        store2.detach();
    }
}
