//! GFS-style retention policy for snapshot rotation.
//!
//! # Policy bands
//!
//! 1. **Recent**: keep the N newest snapshots unconditionally.
//! 2. **Hourly**: within the last `hourly_days` days, keep one per hour.
//! 3. **Daily**: within the last `daily_months * 30` days, keep one per day.
//! 4. **Monthly**: keep one per calendar month indefinitely.
//!
//! Safety invariant: at least one snapshot is always kept, even if every
//! retention band would otherwise delete everything.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::error::BackupError;
use super::snapshot::parse_snapshot_name;
use super::types::{RetentionPolicy, SnapshotInfo};

// ---------------------------------------------------------------------------
// list_snapshots
// ---------------------------------------------------------------------------

/// List all snapshots in the backup directory for `project_id`, newest first.
///
/// Skips files that do not have a `.sqlite` extension or whose filename stem
/// cannot be parsed as a snapshot timestamp. Returns an empty `Vec` if the
/// backup directory does not exist yet.
///
/// The `content_hash` field of each returned [`SnapshotInfo`] is `[0u8; 32]`
/// (not computed — reading every file would be expensive). Use
/// [`crate::backup::snapshot::compute_snapshot_hash`] when the hash matters.
pub fn list_snapshots(
    paths: &crate::PatternPaths,
    project_id: &str,
) -> Result<Vec<SnapshotInfo>, BackupError> {
    let dir = paths.backup_dir(project_id);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| BackupError::Io {
        path: dir.clone(),
        source: e,
    })? {
        let entry = entry.map_err(|e| BackupError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("sqlite") {
            continue;
        }

        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };

        let ts = match parse_snapshot_name(name) {
            Ok(ts) => ts,
            Err(_) => {
                // Skip files whose names don't match the snapshot format.
                // This can happen with temp files or pre-restore safety files.
                continue;
            }
        };

        let metadata = entry.metadata().map_err(|e| BackupError::Io {
            path: path.clone(),
            source: e,
        })?;

        out.push(SnapshotInfo {
            timestamp: ts,
            path,
            size_bytes: metadata.len(),
            // Content hash is populated lazily — reading all files is expensive.
            content_hash: [0u8; 32],
        });
    }

    // Sort newest first.
    out.sort_by_key(|s| Reverse(s.timestamp));
    Ok(out)
}

// ---------------------------------------------------------------------------
// select_deletions
// ---------------------------------------------------------------------------

/// Apply the retention policy and return paths of snapshots to **delete**.
///
/// `snapshots` must be sorted newest-first (as returned by [`list_snapshots`]).
/// `now` is the reference point for computing retention windows; pass
/// `&Timestamp::now()` in production and a fixed timestamp in tests.
///
/// # Safety invariant
///
/// At least one snapshot is always kept, even if `policy.keep_recent == 0` and
/// all retention bands would delete everything. This prevents a pathological
/// config from wiping the entire backup history.
pub fn select_deletions(
    snapshots: &[SnapshotInfo],
    policy: &RetentionPolicy,
    now: &Timestamp,
) -> Vec<PathBuf> {
    if snapshots.is_empty() {
        return Vec::new();
    }

    let mut keep: HashSet<&Path> = HashSet::new();

    // (a) Keep the N newest unconditionally.
    for s in snapshots.iter().take(policy.keep_recent) {
        keep.insert(&s.path);
    }

    // (b) Hourly retention: within the last `hourly_days` days, keep one per
    // hour. Iterating newest-first means the first snapshot per bucket is the
    // most recent in that hour.
    //
    // Note: `Timestamp::checked_sub` only supports Span units ≤ hours (calendar
    // units like days require a timezone). We convert days → hours for timestamp
    // arithmetic, then use UTC-zoned buckets for the per-hour classification.
    if policy.hourly_days > 0 {
        let hours = i64::from(policy.hourly_days) * 24;
        let hourly_cutoff = now
            .checked_sub(jiff::Span::new().hours(hours))
            .unwrap_or(*now);
        let mut seen_hours: HashSet<(i16, i8, i8, i8)> = HashSet::new(); // year, month, day, hour
        for s in snapshots.iter().filter(|s| s.timestamp >= hourly_cutoff) {
            let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
            let bucket = (zoned.year(), zoned.month(), zoned.day(), zoned.hour());
            if seen_hours.insert(bucket) {
                keep.insert(&s.path);
            }
        }
    }

    // (c) Daily retention: within the last `daily_months * 30` days, keep one
    // per day. Newest-first iteration retains the most recent snapshot per day.
    //
    // Same note as (b): use hours for Timestamp arithmetic to avoid the
    // calendar-unit restriction.
    if policy.daily_months > 0 {
        let hours = i64::from(policy.daily_months) * 30 * 24;
        let daily_cutoff = now
            .checked_sub(jiff::Span::new().hours(hours))
            .unwrap_or(*now);
        let mut seen_days: HashSet<(i16, i8, i8)> = HashSet::new(); // year, month, day
        for s in snapshots.iter().filter(|s| s.timestamp >= daily_cutoff) {
            let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
            let bucket = (zoned.year(), zoned.month(), zoned.day());
            if seen_days.insert(bucket) {
                keep.insert(&s.path);
            }
        }
    }

    // (d) Monthly retention: keep one per calendar month indefinitely.
    // Newest-first iteration means the first encounter per month is the most
    // recent snapshot in that month.
    if policy.monthly_forever {
        let mut seen_months: HashSet<(i16, i8)> = HashSet::new(); // year, month
        for s in snapshots.iter() {
            let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
            let bucket = (zoned.year(), zoned.month());
            if seen_months.insert(bucket) {
                keep.insert(&s.path);
            }
        }
    }

    // Safety: always keep at least one snapshot regardless of policy.
    if keep.is_empty() {
        keep.insert(&snapshots[0].path);
    }

    // Return the paths NOT in the keep set.
    snapshots
        .iter()
        .filter(|s| !keep.contains(s.path.as_path()))
        .map(|s| s.path.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// apply_rotation
// ---------------------------------------------------------------------------

/// Apply `policy` to the snapshots for `project_id` and delete the selected
/// snapshots from disk.
///
/// Returns the number of snapshots deleted.
///
/// # Errors
///
/// - [`BackupError::Io`] — directory read or file deletion failed.
pub fn apply_rotation(
    paths: &crate::PatternPaths,
    project_id: &str,
    policy: &RetentionPolicy,
) -> Result<usize, BackupError> {
    let snapshots = list_snapshots(paths, project_id)?;
    let to_delete = select_deletions(&snapshots, policy, &Timestamp::now());
    for path in &to_delete {
        std::fs::remove_file(path).map_err(|e| BackupError::Io {
            path: path.clone(),
            source: e,
        })?;
    }
    Ok(to_delete.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jiff::Timestamp;

    use super::*;
    use crate::backup::types::{RetentionPolicy, SnapshotInfo};

    // Helper: build a synthetic SnapshotInfo at a given Unix second offset.
    fn make_snapshot(unix_secs: i64) -> SnapshotInfo {
        SnapshotInfo {
            timestamp: Timestamp::from_second(unix_secs).unwrap(),
            path: PathBuf::from(format!("/fake/backup/{unix_secs}.sqlite")),
            size_bytes: 4096,
            content_hash: [0u8; 32],
        }
    }

    // Helper: build a dense sequence of snapshots every `interval_secs` seconds,
    // starting from `base_unix_secs` and going backwards in time for `count` steps.
    // Returns newest-first (as list_snapshots does).
    fn synthetic_snapshots(
        base_unix_secs: i64,
        interval_secs: i64,
        count: usize,
    ) -> Vec<SnapshotInfo> {
        (0..count)
            .map(|i| make_snapshot(base_unix_secs - (i as i64) * interval_secs))
            .collect()
    }

    // ---------------------------------------------------------------------------
    // Edge cases
    // ---------------------------------------------------------------------------

    #[test]
    fn empty_snapshot_list_produces_no_deletions() {
        let policy = RetentionPolicy::default();
        let now = Timestamp::now();
        let deletions = select_deletions(&[], &policy, &now);
        assert!(deletions.is_empty(), "no deletions from empty list");
    }

    #[test]
    fn single_snapshot_is_always_kept() {
        let policy = RetentionPolicy {
            keep_recent: 0,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: false,
        };
        let now = Timestamp::now();
        let snapshots = vec![make_snapshot(now.as_second() - 3600)];
        let deletions = select_deletions(&snapshots, &policy, &now);
        assert!(
            deletions.is_empty(),
            "safety invariant: single snapshot must not be deleted even with all-zero policy"
        );
    }

    #[test]
    fn all_zero_policy_keeps_at_least_one() {
        let policy = RetentionPolicy {
            keep_recent: 0,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: false,
        };
        // Reference: 2026-04-19T12:00:00Z = 1776340800 (approximate)
        let now = Timestamp::from_second(1_776_340_800).unwrap();
        // Ten snapshots spread over 10 hours.
        let snapshots = synthetic_snapshots(now.as_second() - 3600, 3600, 10);
        let deletions = select_deletions(&snapshots, &policy, &now);
        let kept = snapshots.len() - deletions.len();
        assert!(
            kept >= 1,
            "at least one snapshot must survive all-zero policy"
        );
    }

    // ---------------------------------------------------------------------------
    // Band (a): keep_recent
    // ---------------------------------------------------------------------------

    #[test]
    fn keep_recent_retains_n_newest() {
        let policy = RetentionPolicy {
            keep_recent: 3,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: false,
        };
        let now = Timestamp::from_second(1_776_340_800).unwrap();
        // 10 snapshots every 10 minutes.
        let snapshots = synthetic_snapshots(now.as_second(), 600, 10);
        let deletions = select_deletions(&snapshots, &policy, &now);
        // Should keep the 3 newest; delete the remaining 7.
        assert_eq!(
            deletions.len(),
            7,
            "should delete 7 snapshots leaving 3 recent"
        );
        // The 3 newest (indices 0-2) must not appear in deletions.
        let del_set: HashSet<_> = deletions.iter().collect();
        for s in snapshots.iter().take(3) {
            assert!(
                !del_set.contains(&s.path),
                "newest 3 must be kept: {:?}",
                s.path
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Band (b): hourly
    // ---------------------------------------------------------------------------

    #[test]
    fn hourly_retention_keeps_one_per_hour() {
        let policy = RetentionPolicy {
            keep_recent: 0,
            hourly_days: 1,
            daily_months: 0,
            monthly_forever: false,
        };
        // Reference: midnight UTC on 2026-04-19 = 2026-04-19T00:00:00Z.
        let now = Timestamp::from_second(1_776_297_600).unwrap();
        // 144 snapshots every 10 minutes for the last ~23.8 hours.
        // Snapshot range: from now down to now-143*600 = now-85800s ≈ 2026-04-18T00:10:00Z.
        // Hourly cutoff: now - 24h = 2026-04-18T00:00:00Z.
        // All 144 snapshots are within the window.
        // Distinct hours spanned: hour 0 on Apr 18 (partial) + hours 1-23 on Apr 18 + hour 0 on Apr 19 = 25.
        let snapshots = synthetic_snapshots(now.as_second(), 600, 144);
        let deletions = select_deletions(&snapshots, &policy, &now);
        let kept = snapshots.len() - deletions.len();
        // Each distinct hour must have exactly one representative kept.
        // With 10-minute snapshots over ~24h, there are 24-25 distinct hours.
        assert!(
            kept >= 24,
            "hourly retention must keep at least one per hour, expected ≥24, kept {kept}"
        );
        assert!(
            kept <= 25,
            "hourly retention must keep at most one per hour, expected ≤25, kept {kept}"
        );
    }

    // ---------------------------------------------------------------------------
    // Band (c): daily
    // ---------------------------------------------------------------------------

    #[test]
    fn daily_retention_keeps_one_per_day() {
        let policy = RetentionPolicy {
            keep_recent: 0,
            hourly_days: 0,
            daily_months: 1,
            monthly_forever: false,
        };
        // Reference: 2026-04-19T12:00:00Z.
        let now = Timestamp::from_second(1_776_340_800).unwrap();
        // 30 snapshots, one per day for the last 30 days.
        // Place them at noon UTC each day so they all fall within 30-day window.
        let snapshots: Vec<SnapshotInfo> = (0..30)
            .map(|i| make_snapshot(now.as_second() - i * 86400))
            .collect();
        let deletions = select_deletions(&snapshots, &policy, &now);
        let kept = snapshots.len() - deletions.len();
        // One snapshot per day for 30 days = 30 kept.
        assert_eq!(
            kept, 30,
            "daily retention must keep one per day for 30 distinct days, kept {kept}"
        );
    }

    #[test]
    fn daily_retention_multiple_per_day_keeps_newest() {
        let policy = RetentionPolicy {
            keep_recent: 0,
            hourly_days: 0,
            daily_months: 1,
            monthly_forever: false,
        };
        let now = Timestamp::from_second(1_776_340_800).unwrap();
        // 6 snapshots on the same day, 2 hours apart (newest first).
        let snapshots = synthetic_snapshots(now.as_second(), 7200, 6);
        let deletions = select_deletions(&snapshots, &policy, &now);
        let kept = snapshots.len() - deletions.len();
        // All 6 fall on the same day → keep only 1 (the newest).
        assert_eq!(
            kept, 1,
            "must keep only 1 per day when multiple exist, kept {kept}"
        );
        let del_set: HashSet<_> = deletions.iter().collect();
        // The newest (index 0) must be kept.
        assert!(
            !del_set.contains(&snapshots[0].path),
            "newest snapshot of the day must be kept"
        );
    }

    // ---------------------------------------------------------------------------
    // Band (d): monthly
    // ---------------------------------------------------------------------------

    #[test]
    fn monthly_retention_keeps_one_per_calendar_month() {
        let policy = RetentionPolicy {
            keep_recent: 0,
            hourly_days: 0,
            daily_months: 0,
            monthly_forever: true,
        };
        // Reference: 2026-04-19.
        let now = Timestamp::from_second(1_776_340_800).unwrap();
        // 12 snapshots, one per month for the past year.
        let snapshots: Vec<SnapshotInfo> = (0..12)
            .map(|i| make_snapshot(now.as_second() - i * 30 * 86400))
            .collect();
        let deletions = select_deletions(&snapshots, &policy, &now);
        let kept = snapshots.len() - deletions.len();
        // One per month → all 12 should be kept (each in a different month).
        assert!(
            kept >= 11,
            "monthly retention must keep at least 11 of 12 monthly snapshots, kept {kept}"
        );
    }

    // ---------------------------------------------------------------------------
    // Default policy integration
    // ---------------------------------------------------------------------------

    #[test]
    fn default_policy_on_one_year_of_hourly_data() {
        let policy = RetentionPolicy::default();
        let now = Timestamp::from_second(1_776_340_800).unwrap();
        // 8760 snapshots: one per hour for a year.
        let snapshots = synthetic_snapshots(now.as_second(), 3600, 8760);
        let deletions = select_deletions(&snapshots, &policy, &now);
        let kept = snapshots.len() - deletions.len();
        // With default policy:
        // - 24 recent (covers last 24 snapshots = last 24 hours)
        // - hourly for 1 day = 24 distinct hours → already covered by keep_recent
        // - daily for 30 days = up to 30 distinct days
        // - monthly indefinitely = 12 months
        // The exact count depends on overlap between bands; we assert
        // sane bounds rather than a brittle exact number.
        assert!(
            kept >= 30,
            "must keep at least 30 for daily band (30 days), kept {kept}"
        );
        assert!(
            kept < 200,
            "must prune aggressively; keeping {kept} out of 8760 is suspicious"
        );
    }
}
