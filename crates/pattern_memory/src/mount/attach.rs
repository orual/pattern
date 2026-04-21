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
use crate::backup::scheduler::{BackupPolicy, BackupScheduler};
use crate::cache::MemoryCache;
use crate::config::{ModeKind, load_mount_config};
use crate::fs::watcher::{MountWatcher, WatcherConfig};
use crate::modes::StorageMode;
use crate::paths::PatternPaths;
use crate::reembed::ReembedQueue;

/// Attach to the nearest mount at or above `start` using the default
/// [`PatternPaths`] resolution (`~/.pattern/`).
///
/// This is the production entry point. For tests that need a custom base
/// directory, use [`attach_with_paths`].
///
/// # Errors
///
/// - [`MountError::NotFound`] if no mount is found.
/// - [`MountError::Config`] if the `.pattern.kdl` is invalid.
/// - [`MountError::Db`] if the databases cannot be opened.
/// - [`MountError::Watcher`] if the filesystem watcher fails to start.
pub fn attach(start: &Path) -> Result<MountedStore, MountError> {
    let paths = PatternPaths::default_paths()?;
    attach_with_paths(start, &paths)
}

/// Attach to the nearest mount at or above `start` with an explicit
/// [`PatternPaths`] base directory.
///
/// Use [`PatternPaths::with_base`] in tests to avoid writing to the real
/// `~/.pattern/` directory.
pub fn attach_with_paths(start: &Path, paths: &PatternPaths) -> Result<MountedStore, MountError> {
    let mount_path = super::find_mount(start)?;
    let config = load_mount_config(&mount_path.join(".pattern.kdl"))?;

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

    // Spawn the backup scheduler if a `backup` section is configured and a
    // tokio runtime is available. One-shot CLI commands (e.g. `pattern backup
    // create`) don't need the scheduler — they create snapshots directly.
    let backup_scheduler = if let Some(backup_cfg) = &config.backup {
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                let interval = backup_cfg.parse_interval().unwrap_or_else(|e| {
                    tracing::warn!(
                        "invalid snapshot_interval in .pattern.kdl: {e}; using 1h default"
                    );
                    std::time::Duration::from_secs(3600)
                });
                let policy = Arc::new(BackupPolicy {
                    snapshot_interval: interval,
                    retention: crate::backup::types::RetentionPolicy {
                        keep_recent: backup_cfg.keep_recent,
                        hourly_days: backup_cfg.hourly_days,
                        daily_months: backup_cfg.daily_months,
                        monthly_forever: backup_cfg.monthly_forever,
                    },
                });
                Some(BackupScheduler::spawn(
                    Arc::new(messages_db_path.clone()),
                    config.project.name.clone(),
                    policy,
                    Arc::new(paths.clone()),
                ))
            }
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(MountedStore {
        mount_path,
        config,
        mode,
        cache,
        db,
        watcher: Some(watcher),
        reembed_queue,
        backup_scheduler,
    })
}
