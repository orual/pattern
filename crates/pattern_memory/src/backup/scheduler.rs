//! Tokio interval task that periodically snapshots `messages.db`.
//!
//! The scheduler wakes on a configurable interval, checks whether any new
//! messages have been written since the last snapshot, and if so creates a
//! snapshot and applies rotation. The task is tied to the [`MountedStore`]
//! lifecycle via a [`CancellationToken`]: calling [`BackupScheduler::cancel`]
//! signals the task to stop, and [`BackupScheduler::join`] awaits its
//! completion.
//!
//! # Design decisions
//!
//! - `MissedTickBehavior::Delay` — if a snapshot takes longer than the
//!   interval, the next tick is delayed rather than burst-fired. This prevents
//!   cascading snapshot storms if the process was paused or the disk was slow.
//! - The scheduler opens its own read-only rusqlite connection to check for
//!   new messages, independent of the r2d2 pool. This avoids ATTACH complexity
//!   and keeps the scheduler's reads from blocking the pool.
//! - `spawn_blocking` wraps all rusqlite calls since rusqlite is synchronous.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use super::error::BackupError;
use super::types::RetentionPolicy;
use crate::paths::PatternPaths;

// ---------------------------------------------------------------------------
// BackupPolicy
// ---------------------------------------------------------------------------

/// Combined backup policy: how often to snapshot and how long to keep them.
///
/// Parsed from the `.pattern.kdl` `backup` section (Task 5) and passed to
/// [`BackupScheduler::spawn`].
#[derive(Debug, Clone)]
pub struct BackupPolicy {
    /// How often the scheduler wakes up and potentially creates a snapshot.
    pub snapshot_interval: Duration,

    /// GFS-style retention policy applied after each snapshot.
    pub retention: RetentionPolicy,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            snapshot_interval: Duration::from_secs(3600), // 1 hour
            retention: RetentionPolicy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// BackupScheduler
// ---------------------------------------------------------------------------

/// Handle to the background tokio task that periodically snapshots `messages.db`.
///
/// Spawned by [`BackupScheduler::spawn`]. Cancel via [`cancel`](Self::cancel),
/// then await via [`join`](Self::join) to ensure the task has fully stopped
/// before the caller proceeds (e.g., in `MountedStore::detach`).
pub struct BackupScheduler {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl BackupScheduler {
    /// Spawn the background snapshot task.
    ///
    /// # Parameters
    ///
    /// - `messages_db_path` — path to `messages.db`; opened read-only for the
    ///   "has new messages?" check.
    /// - `project_id` — project identifier used to resolve the backup directory
    ///   via `paths`.
    /// - `policy` — snapshot interval + retention policy.
    /// - `paths` — [`PatternPaths`] used to resolve the backup directory.
    pub fn spawn(
        messages_db_path: Arc<PathBuf>,
        project_id: String,
        policy: Arc<BackupPolicy>,
        paths: Arc<PatternPaths>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let mut tick = interval(policy.snapshot_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            // Consume the immediately-fired first tick so the loop doesn't
            // snapshot on entry before we've checked for new messages.
            tick.tick().await;

            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    _ = tick.tick() => {
                        if should_snapshot(&messages_db_path, &project_id, &paths).await
                            && let Err(e) = try_snapshot(
                                &messages_db_path,
                                &project_id,
                                &policy,
                                &paths,
                            ).await
                        {
                            // Log the error and continue — a snapshot failure
                            // must not crash the scheduler or the agent.
                            tracing::warn!(
                                project_id = %project_id,
                                error = %e,
                                "scheduled snapshot failed; will retry next tick"
                            );
                        }
                    }
                }
            }
        });

        Self { handle, cancel }
    }

    /// Signal the scheduler task to stop.
    ///
    /// This is non-blocking — call [`join`](Self::join) afterward to wait for
    /// the task to actually finish.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Await the scheduler task's completion.
    ///
    /// Should be called after [`cancel`](Self::cancel). Returns the
    /// `JoinHandle` result — an `Err` indicates the task panicked.
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.handle.await
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Check whether any messages have been written since the last snapshot.
///
/// Opens a direct read-only connection to `messages.db` (bypassing the r2d2
/// pool and ATTACH machinery) to run a simple `SELECT EXISTS(...)` query.
/// Returns `true` if a snapshot should be created; `false` if the tick should
/// be skipped.
///
/// A missing or unreadable messages.db returns `false` (safe: we can't
/// snapshot something we can't read, and the scheduler will retry next tick).
async fn should_snapshot(
    messages_db_path: &std::path::Path,
    project_id: &str,
    paths: &Arc<PatternPaths>,
) -> bool {
    // Find the timestamp of the most recent snapshot, if any.
    let last_snapshot_ts = match super::rotation::list_snapshots(paths, project_id) {
        Ok(snapshots) => snapshots
            .into_iter()
            .next()
            .map(|s| s.timestamp)
            .unwrap_or_else(|| Timestamp::from_second(0).unwrap()),
        Err(_) => Timestamp::from_second(0).unwrap(),
    };

    let path = messages_db_path.to_owned();
    // Use a unix timestamp (seconds since epoch) for comparison. This avoids
    // any text-format ambiguity: chrono serialises DateTime<Utc> through
    // rusqlite as "2026-04-19 12:00:00+00:00" (space separator, +00:00
    // suffix), while jiff's strftime produces "2026-04-19T12:00:00Z" (T
    // separator, Z suffix). SQLite's lexicographic TEXT comparison would
    // therefore be unreliable. Using strftime('%s', created_at) normalises
    // both sides to integer seconds, making the comparison format-agnostic.
    let last_ts_secs = last_snapshot_ts.as_second();

    // spawn_blocking because rusqlite is synchronous.
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;

        // messages table is in the default schema when opened directly
        // (not via the pool's ATTACH). The column is `created_at TEXT`.
        // strftime('%s', created_at) converts whatever text format is stored
        // to unix epoch seconds so the comparison is format-agnostic.
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE CAST(strftime('%s', created_at) AS INTEGER) > ?1 LIMIT 1)",
            rusqlite::params![last_ts_secs],
            |r| r.get::<_, bool>(0),
        )
        .ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// Create a snapshot and apply the retention policy.
///
/// Runs on a `spawn_blocking` thread because both operations use synchronous
/// rusqlite calls.
async fn try_snapshot(
    messages_db_path: &std::path::Path,
    project_id: &str,
    policy: &BackupPolicy,
    paths: &Arc<PatternPaths>,
) -> Result<(), BackupError> {
    let path = messages_db_path.to_owned();
    let pid = project_id.to_owned();
    let retention = policy.retention.clone();
    let paths = Arc::clone(paths);

    tokio::task::spawn_blocking(move || {
        super::snapshot::create_snapshot(&path, &paths, &pid)?;
        super::rotation::apply_rotation(&paths, &pid, &retention)?;
        Ok::<_, BackupError>(())
    })
    .await
    .map_err(|e| BackupError::Io {
        path: messages_db_path.to_owned(),
        source: std::io::Error::other(format!("scheduler task panicked: {e}")),
    })?
}
