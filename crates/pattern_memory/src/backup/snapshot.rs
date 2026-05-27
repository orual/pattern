// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Atomic `messages.db` snapshot creation via rusqlite's `Backup` API.
//!
//! # Atomicity guarantee
//!
//! Snapshots are written to a `NamedTempFile` created in the **same directory**
//! as the destination file, then renamed into place. Keeping the temp file on
//! the same filesystem avoids `EXDEV` (cross-device link) errors on the atomic
//! rename.
//!
//! rusqlite's `Backup::run_to_completion` handles `SQLITE_BUSY` retries
//! transparently, so concurrent writers (including WAL-mode writers) do not
//! corrupt the snapshot.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use rusqlite::backup::Backup;

use super::error::BackupError;
use super::types::SnapshotInfo;

// ---------------------------------------------------------------------------
// Filename format
// ---------------------------------------------------------------------------

/// strftime/strptime format used for snapshot filenames.
///
/// Example output: `2026-04-19T120000Z`.
///
/// The format is:
/// - Windows-safe (no colons — `:` is forbidden in NTFS filenames).
/// - ISO-8601-like and sorts lexicographically by recency.
/// - Parseable back to a UTC timestamp via [`parse_snapshot_name`].
pub const SNAPSHOT_FILENAME_FORMAT: &str = "%Y-%m-%dT%H%M%SZ";

/// Format a [`Timestamp`] as a snapshot filename stem (without `.sqlite`).
///
/// Uses [`SNAPSHOT_FILENAME_FORMAT`]. The timestamp is converted to UTC before
/// formatting, so the output always has the `Z` suffix baked into the format
/// string rather than rendered from timezone state.
pub fn format_snapshot_name(ts: &Timestamp) -> String {
    // Timestamp::strftime returns a lazily-rendered Display implementor.
    // Calling .to_string() materialises it.
    ts.strftime(SNAPSHOT_FILENAME_FORMAT).to_string()
}

/// Parse a snapshot filename stem back into a [`Timestamp`].
///
/// Accepts strings of the form `2026-04-19T120000Z` (no `.sqlite` extension).
/// Returns an error if the string does not match [`SNAPSHOT_FILENAME_FORMAT`].
///
/// # Implementation note
///
/// `Timestamp::strptime` requires an offset directive (`%z`) to produce a
/// `Timestamp`. Since our filenames always have a literal trailing `Z` (UTC),
/// we strip the `Z` suffix and parse the remainder as a civil `DateTime`,
/// then treat it as UTC.
pub fn parse_snapshot_name(name: &str) -> Result<Timestamp, jiff::Error> {
    // Strip the trailing 'Z' if present, then parse as a civil datetime in UTC.
    let without_z = name.strip_suffix('Z').unwrap_or(name);
    // Civil datetime format matching SNAPSHOT_FILENAME_FORMAT without the Z.
    let civil_fmt = "%Y-%m-%dT%H%M%S";
    let dt = jiff::civil::DateTime::strptime(civil_fmt, without_z)?;
    // Treat as UTC — our format always uses UTC, the Z suffix encodes this
    // convention in the filename rather than as a parsed timezone.
    dt.to_zoned(jiff::tz::TimeZone::UTC).map(|z| z.timestamp())
}

// ---------------------------------------------------------------------------
// Snapshot hash helper
// ---------------------------------------------------------------------------

/// Compute the blake3 hash of an existing snapshot file.
///
/// Returned as raw bytes. Use `blake3::Hash::to_hex()` or format the bytes
/// manually for display — the `hex` crate is not a dependency.
///
/// # Errors
///
/// Returns [`BackupError::Io`] if the file cannot be read.
pub fn compute_snapshot_hash(path: &Path) -> Result<[u8; 32], BackupError> {
    let bytes = std::fs::read(path).map_err(|e| BackupError::Io {
        path: path.to_owned(),
        source: e,
    })?;
    Ok(blake3::hash(&bytes).into())
}

// ---------------------------------------------------------------------------
// create_snapshot
// ---------------------------------------------------------------------------

/// Create an atomic snapshot of the database at `source_db_path`.
///
/// The snapshot is placed in the backup directory for `project_id` under
/// `paths`, which resolves to `<base>/backups/<id>/messages/<timestamp>.sqlite`.
/// The directory is created automatically if it does not exist.
///
/// # Atomicity
///
/// The snapshot is written to a `NamedTempFile` in the **same directory** as
/// the destination (so the atomic rename never crosses filesystem boundaries),
/// then renamed into place. rusqlite's `Backup::run_to_completion` retries on
/// `SQLITE_BUSY` transparently, so concurrent writers do not corrupt the
/// snapshot.
///
/// # Errors
///
/// - [`BackupError::Io`] — directory creation, fsync, or read failures.
/// - [`BackupError::TempFile`] — temp file creation failed.
/// - [`BackupError::TempPersist`] — atomic rename failed.
/// - [`BackupError::OpenSource`] / [`BackupError::OpenDest`] — database open
///   failed.
/// - [`BackupError::BackupInit`] / [`BackupError::BackupRun`] — rusqlite
///   backup API failures.
pub fn create_snapshot(
    source_db_path: &Path,
    paths: &crate::PatternPaths,
    project_id: &str,
) -> Result<SnapshotInfo, BackupError> {
    let now = Timestamp::now();
    let dest_path = paths.backup_snapshot_path(project_id, &now);
    let dest_dir = dest_path
        .parent()
        .expect("backup_snapshot_path always has a parent directory");

    std::fs::create_dir_all(dest_dir).map_err(|e| BackupError::Io {
        path: dest_dir.to_owned(),
        source: e,
    })?;

    // Write to a temp file in the SAME directory as the destination to avoid
    // EXDEV on cross-filesystem rename.
    let tmp = tempfile::NamedTempFile::new_in(dest_dir).map_err(|e| BackupError::TempFile {
        path: dest_dir.to_owned(),
        source: e,
    })?;

    {
        let src = rusqlite::Connection::open_with_flags(
            source_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(BackupError::OpenSource)?;
        let mut dst = rusqlite::Connection::open(tmp.path()).map_err(BackupError::OpenDest)?;

        {
            let backup = Backup::new(&src, &mut dst).map_err(BackupError::BackupInit)?;
            // run_to_completion handles SQLITE_BUSY retries transparently.
            backup
                .run_to_completion(
                    /* pages_per_step */ 100,
                    /* pause_between_steps */ Duration::from_millis(5),
                    /* progress_callback */ None,
                )
                .map_err(BackupError::BackupRun)?;
            // Backup is dropped here, releasing the mutable borrow of dst.
        }

        // Strip WAL mode from the snapshot.
        //
        // The Backup API copies page 1 (the database header) from the source,
        // which may have WAL mode enabled. A snapshot with WAL mode will create
        // a fresh -wal file when opened later, shadowing its data. Converting to
        // DELETE journal mode makes each snapshot a clean, self-contained file.
        dst.execute_batch("PRAGMA journal_mode = DELETE;")
            .map_err(|e| BackupError::Io {
                path: tmp.path().to_owned(),
                source: std::io::Error::other(e.to_string()),
            })?;

        // dst Connection is dropped here, flushing WAL and releasing locks.
    }

    // fsync the temp file before rename for durability.
    {
        let f = std::fs::File::open(tmp.path()).map_err(|e| BackupError::Io {
            path: tmp.path().to_owned(),
            source: e,
        })?;
        f.sync_all().map_err(|e| BackupError::Io {
            path: tmp.path().to_owned(),
            source: e,
        })?;
    }

    // Compute blake3 hash + size before rename.
    let bytes = std::fs::read(tmp.path()).map_err(|e| BackupError::Io {
        path: tmp.path().to_owned(),
        source: e,
    })?;
    let content_hash: [u8; 32] = blake3::hash(&bytes).into();
    let size_bytes = bytes.len() as u64;

    // Atomic rename into place (within same filesystem).
    tmp.persist(&dest_path)
        .map_err(|e| BackupError::TempPersist {
            path: dest_path.clone(),
            source: e.error,
        })?;

    Ok(SnapshotInfo {
        timestamp: now,
        path: dest_path,
        size_bytes,
        content_hash,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_roundtrip() {
        let ts = Timestamp::now();
        // Truncate to seconds — the format has second resolution.
        let ts_sec = Timestamp::from_second(ts.as_second()).unwrap();
        let name = format_snapshot_name(&ts_sec);
        let parsed = parse_snapshot_name(&name).unwrap();
        assert_eq!(
            ts_sec, parsed,
            "roundtrip must be lossless at second resolution"
        );
    }

    #[test]
    fn format_snapshot_name_no_colons() {
        let ts = Timestamp::now();
        let name = format_snapshot_name(&ts);
        assert!(
            !name.contains(':'),
            "snapshot filename must not contain colons (Windows-safe): {name}"
        );
    }

    #[test]
    fn format_snapshot_name_ends_with_z() {
        let ts = Timestamp::now();
        let name = format_snapshot_name(&ts);
        assert!(
            name.ends_with('Z'),
            "snapshot filename must end with Z (UTC marker): {name}"
        );
    }

    #[test]
    fn parse_snapshot_name_rejects_garbage() {
        assert!(
            parse_snapshot_name("not-a-timestamp").is_err(),
            "garbage input must not parse"
        );
        assert!(
            parse_snapshot_name("").is_err(),
            "empty string must not parse"
        );
    }
}
