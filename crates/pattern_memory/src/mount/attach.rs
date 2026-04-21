//! Mount attachment logic.
//!
//! The [`attach`] function walks upward to find a `.pattern.kdl` config,
//! parses it, opens the database pair, builds a `MemoryCache` with optional
//! subscriber support, starts a filesystem watcher, and returns a
//! [`MountedStore`] handle.

use std::path::Path;
use std::sync::Arc;

use pattern_db::ConstellationDb;

use super::MountedStore;
use super::error::MountError;
use crate::cache::MemoryCache;
use crate::config::{ModeKind, load_mount_config};
use crate::fs::watcher::{MountWatcher, WatcherConfig};
use crate::modes::StorageMode;
use crate::paths::PatternPaths;
use crate::reembed::ReembedQueue;

/// Attach to the nearest mount at or above `start`.
///
/// Walks upward from `start` to find `.pattern/shared/.pattern.kdl`, parses
/// the config, resolves database paths per mode, opens the databases, builds
/// a [`MemoryCache`] with subscriber support, starts a filesystem watcher,
/// and returns a [`MountedStore`] handle.
///
/// # Errors
///
/// - [`MountError::NotFound`] if no mount is found.
/// - [`MountError::Config`] if the `.pattern.kdl` is invalid.
/// - [`MountError::ModeUnavailable`] if Mode C is requested.
/// - [`MountError::Db`] if the databases cannot be opened.
/// - [`MountError::Watcher`] if the filesystem watcher fails to start.
pub fn attach(start: &Path) -> Result<MountedStore, MountError> {
    let mount_path = super::find_mount(start)?;
    let config = load_mount_config(&mount_path.join(".pattern.kdl"))?;

    // Resolve the Pattern home directory for modes that need it (B uses
    // ~/.pattern/projects/<id>/; A and C use it only for messages.db placement).
    let paths = PatternPaths::default_paths()?;

    // Resolve DB paths per mode.
    let (memory_db_path, messages_db_path, mode) = match config.mount.mode {
        ModeKind::A => {
            // For Mode A, project_root is the ancestor containing `.pattern/`.
            // mount_path = <project>/.pattern/shared
            // project_root = <project>
            let project_root = mount_path
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| MountError::InvalidLayout {
                    path: mount_path.clone(),
                })?
                .to_owned();
            let memory_db = mount_path.join(&config.mount.memory_db);
            let messages_db = paths.mode_a_messages_path(&project_root)?;
            (
                memory_db,
                messages_db,
                StorageMode::A {
                    mount_path: mount_path.clone(),
                    project_root,
                },
            )
        }
        ModeKind::B => {
            let memory_db = mount_path.join(&config.mount.memory_db);
            let messages_db = paths.mode_b_messages_path(&config.project.name);
            (
                memory_db,
                messages_db,
                StorageMode::B {
                    mount_path: mount_path.clone(),
                    project_id: config.project.name.clone(),
                },
            )
        }
        ModeKind::C => {
            // Mode C: sidecar jj inside host git. Layout is the same as Mode A:
            // mount_path = <project>/.pattern/shared
            // project_root = <project>
            let project_root = mount_path
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| MountError::InvalidLayout {
                    path: mount_path.clone(),
                })?
                .to_owned();
            let memory_db = mount_path.join(&config.mount.memory_db);
            let messages_db = paths.mode_a_messages_path(&project_root)?;
            (
                memory_db,
                messages_db,
                StorageMode::C {
                    mount_path: mount_path.clone(),
                },
            )
        }
    };

    // Open the paired databases — runs migrations on both.
    let db = Arc::new(ConstellationDb::open(&memory_db_path, &messages_db_path)?);

    // Build the re-embed queue. When a tokio runtime is available, spawn the
    // queue as a background task so subscriber workers can send without hitting
    // SendError. When no runtime is available (pure-sync tests without a
    // tokio context), fall back to dropping the receiver — workers handle the
    // resulting SendError gracefully (log and continue, no data loss).
    //
    // No embedding provider is configured at attach time; the queue drains
    // requests silently until Phase 8 wires the embedding pipeline.
    // See docs/implementation-plans/2026-04-19-v3-memory-rework/phase_08.md.
    let (reembed_queue, reembed_tx) = match tokio::runtime::Handle::try_current() {
        Ok(_) => {
            let (queue, tx) = ReembedQueue::spawn(None, Arc::clone(&db));
            (Some(queue), tx)
        }
        Err(_) => {
            // No tokio runtime — create a channel pair and drop the receiver.
            // Subscriber workers will see SendError on any reembed attempt,
            // which they handle gracefully.
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            (None, tx)
        }
    };

    let (heartbeat_tx, heartbeat_rx) = crossbeam_channel::bounded(256);
    let cache = Arc::new(MemoryCache::new(db.clone()).with_mount_path(
        mount_path.clone(),
        reembed_tx,
        heartbeat_tx,
        heartbeat_rx,
    ));

    // Start the filesystem watcher for external edits.
    let watcher = MountWatcher::start(WatcherConfig {
        mount_path: mount_path.clone(),
        cache: Arc::clone(&cache),
    })?;

    Ok(MountedStore {
        mount_path,
        config,
        mode,
        cache,
        db,
        watcher: Some(watcher),
        reembed_queue,
    })
}
