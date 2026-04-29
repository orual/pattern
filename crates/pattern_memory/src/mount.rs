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

pub use attach::{attach, attach_with_paths};
pub use error::MountError;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pattern_db::ConstellationDb;

use crate::backup::scheduler::BackupScheduler;
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
    /// The backup scheduler task (if the `.pattern.kdl` has a `backup`
    /// section with a `snapshot_interval`). `Option` so `detach` can take and
    /// cancel it.
    pub(crate) backup_scheduler: Option<BackupScheduler>,
}

impl MountedStore {
    /// Cleanly shut down all mount resources.
    ///
    /// 1. Cancels and joins the backup scheduler task (if running).
    /// 2. Stops the filesystem watcher (no more external-edit events).
    /// 3. Drains all subscriber workers (cancels tokens, joins threads).
    /// 4. Drops the re-embed queue handle.
    /// 5. Drops the cache and database pool references.
    ///
    /// This is intentionally synchronous — all teardown operations are sync.
    /// The backup scheduler is an async tokio task; if a tokio runtime is
    /// available, it is cancelled and joined with a 5-second timeout. If no
    /// runtime is available (sync-only test contexts), the cancel signal is
    /// sent and the handle is dropped — the task will be cleaned up when the
    /// runtime itself shuts down.
    pub fn detach(mut self) {
        // Cancel + join the backup scheduler before stopping the watcher,
        // so any in-flight snapshot completes cleanly.
        if let Some(scheduler) = self.backup_scheduler.take() {
            scheduler.cancel();
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    // Block on the join with a short timeout to avoid hanging
                    // on a misbehaving task.
                    //
                    // `handle.block_on()` panics when called from within a
                    // tokio worker thread (e.g. the CLI uses `#[tokio::main]`).
                    // `block_in_place` moves the current worker to a blocking
                    // context first, making `block_on` safe to call from any
                    // tokio multi-thread runtime thread.
                    let _ = tokio::task::block_in_place(|| {
                        handle.block_on(async {
                            tokio::time::timeout(
                                std::time::Duration::from_secs(5),
                                scheduler.join(),
                            )
                            .await
                        })
                    });
                }
                Err(_) => {
                    // No tokio runtime — cancel was already sent above; the
                    // task will be dropped when the runtime shuts down.
                    drop(scheduler);
                }
            }
        }
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
/// On miss, consults the projects registry at
/// `<PATTERN_HOME>/projects.kdl` to resolve a standalone-mode mount.
///
/// Returns the mount path (the directory containing `.pattern.kdl`) or
/// [`MountError::NotFound`] if no mount can be resolved.
pub fn find_mount(start: &Path) -> Result<PathBuf, MountError> {
    let paths = crate::PatternPaths::default_paths()?;
    find_mount_with_paths(start, &paths)
}

/// Like [`find_mount`] but with an explicit [`PatternPaths`] for the
/// registry lookup. Used by tests and by callers that need a custom
/// `PATTERN_HOME` base.
pub fn find_mount_with_paths(
    start: &Path,
    paths: &crate::PatternPaths,
) -> Result<PathBuf, MountError> {
    // 0. Direct mount: `start` IS a mount root (it contains `.pattern.kdl`
    //    directly). Lets `attach()` accept standalone mount paths
    //    handed to it directly — including the global fallback path
    //    `<data_root>/projects/@global/shared/`.
    if start.join(".pattern.kdl").is_file() {
        return Ok(start.to_owned());
    }
    // 1. Walk up looking for an in-repo / sidecar `.pattern/shared/.pattern.kdl`
    //    marker. Primary resolution for InRepo and Sidecar modes.
    if let Some(p) = walk_up_for_in_repo_marker(start) {
        return Ok(p);
    }
    // 2. Consult the projects registry for a standalone mount. Standalone
    //    mode writes nothing into the project repo by design, so the only
    //    way to resolve an arbitrary user path → standalone mount is via
    //    the registry.
    if let Some(p) = resolve_via_registry(start, paths) {
        return Ok(p);
    }
    Err(MountError::NotFound {
        started_at: start.to_owned(),
    })
}

/// Walk upward from `start` for the in-repo / sidecar marker. Returns
/// the mount directory (`<project>/.pattern/shared/`) on hit.
fn walk_up_for_in_repo_marker(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_owned();
    loop {
        let candidate = cur.join(".pattern").join("shared").join(".pattern.kdl");
        if candidate.is_file() {
            return Some(
                candidate
                    .parent()
                    .expect(".pattern.kdl has a parent directory")
                    .to_owned(),
            );
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_owned(),
            _ => return None,
        }
    }
}

/// Consult the projects registry. If `start` (or any registered
/// ancestor) maps to a project ID with an existing standalone mount,
/// return the mount path.
fn resolve_via_registry(start: &Path, paths: &crate::PatternPaths) -> Option<PathBuf> {
    let registry = crate::projects::ProjectRegistry::load(paths).ok()?;
    let project_id = registry.project_id_for_path(start)?;
    let mount_path = paths.standalone_mount_path(project_id);
    if mount_path.join(".pattern.kdl").is_file() {
        Some(mount_path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// Create a minimal mount structure in a tempdir for testing.
    fn setup_in_repo_mount(tmp: &Path) {
        crate::modes::in_repo::init(tmp, "test").expect("InRepo mode init should succeed");
    }

    #[test]
    fn find_mount_at_project_root() {
        let tmp = TempDir::new().unwrap();
        setup_in_repo_mount(tmp.path());

        let found = find_mount(tmp.path()).unwrap();
        assert_eq!(found, tmp.path().join(".pattern").join("shared"));
    }

    #[test]
    fn find_mount_from_subdirectory() {
        let tmp = TempDir::new().unwrap();
        setup_in_repo_mount(tmp.path());

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
    fn attach_in_repo_round_trip() {
        let tmp = TempDir::new().unwrap();
        setup_in_repo_mount(tmp.path());

        let store = attach(tmp.path(), None).unwrap();
        assert!(matches!(store.mode, StorageMode::InRepo { .. }));
        assert_eq!(store.mount_path, tmp.path().join(".pattern").join("shared"));

        // Verify the DB is healthy.
        store.db.health_check().unwrap();

        // Detach cleanly.
        store.detach();
    }

    #[test]
    fn attach_not_found_error() {
        let tmp = TempDir::new().unwrap();
        let err = attach(tmp.path(), None).unwrap_err();
        assert!(
            matches!(err, MountError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
    }

    #[test]
    fn attach_detach_reattach() {
        let tmp = TempDir::new().unwrap();
        setup_in_repo_mount(tmp.path());

        // First attach.
        let store = attach(tmp.path(), None).unwrap();
        store.db.health_check().unwrap();
        store.detach();

        // Re-attach should succeed with identical state.
        let store2 = attach(tmp.path(), None).unwrap();
        store2.db.health_check().unwrap();
        store2.detach();
    }
}
