//! Shared types for the backup subsystem.

use std::path::PathBuf;

use jiff::Timestamp;

// ---------------------------------------------------------------------------
// SnapshotInfo
// ---------------------------------------------------------------------------

/// Metadata for a single `messages.db` snapshot.
///
/// Returned by [`crate::backup::snapshot::create_snapshot`] and
/// [`crate::backup::rotation::list_snapshots`].
///
/// The `content_hash` field is populated lazily:
/// - `create_snapshot` computes it from the written file.
/// - `list_snapshots` leaves it as `[0u8; 32]` (expensive to read all files).
///   Callers that need the hash for integrity verification should call
///   [`crate::backup::snapshot::compute_snapshot_hash`].
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    /// The UTC timestamp embedded in the snapshot filename.
    pub timestamp: Timestamp,
    /// Absolute path to the `.sqlite` snapshot file.
    pub path: PathBuf,
    /// File size in bytes as reported by filesystem metadata.
    pub size_bytes: u64,
    /// Blake3 hash of the snapshot file contents.
    ///
    /// Will be `[0u8; 32]` when populated by `list_snapshots`; use
    /// [`crate::backup::snapshot::compute_snapshot_hash`] if the hash matters.
    pub content_hash: [u8; 32],
}

// ---------------------------------------------------------------------------
// RetentionPolicy
// ---------------------------------------------------------------------------

/// GFS-style retention policy for snapshot rotation.
///
/// Applied by [`crate::backup::rotation::select_deletions`].
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Keep the N most-recent snapshots unconditionally, regardless of age.
    ///
    /// Default: 24 (covers a full day of hourly snapshots).
    pub keep_recent: usize,

    /// Within the last `hourly_days` days, keep one snapshot per hour.
    ///
    /// Default: 1 (keep one-per-hour for the last day).
    pub hourly_days: u32,

    /// Within the last `daily_months * 30` days, keep one snapshot per day.
    ///
    /// Default: 1 (keep one-per-day for the last month).
    pub daily_months: u32,

    /// Keep one snapshot per calendar month indefinitely.
    ///
    /// Default: true.
    pub monthly_forever: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_recent: 24,
            hourly_days: 1,
            daily_months: 1,
            monthly_forever: true,
        }
    }
}
