// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Pre-restore safety snapshot + atomic swap of `messages.db`.
//!
//! # Restore flow
//!
//! 1. Verify the snapshot file is a valid SQLite database (PRAGMA integrity_check).
//! 2. Copy the current `messages.db` to `messages.db.pre-restore-<ts>` as a
//!    rollback safety net. The safety copy is fsynced before the swap.
//! 3. Copy the snapshot into a temp file in the same directory as `messages.db`,
//!    fsync it, then atomically rename it into place.
//!
//! Integrity verification happens **before** touching `messages.db`, so a
//! corrupt snapshot leaves the live database unchanged.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::error::BackupError;
use super::rotation;
use super::snapshot::format_snapshot_name;
use super::types::SnapshotInfo;

// ---------------------------------------------------------------------------
// restore_snapshot
// ---------------------------------------------------------------------------

/// Restore `messages.db` from the snapshot at `snapshot_path`.
///
/// Before swapping in the snapshot, the current `messages.db` is copied to
/// `messages.db.pre-restore-<ts>` as a rollback safety net. If `messages.db`
/// does not exist (e.g., first-time restore), the safety copy step is skipped.
///
/// Returns the path of the pre-restore safety copy so the caller can surface
/// it to the user ("if the restored state is wrong, your pre-restore state is
/// at `<path>`").
///
/// # Important: pool must be closed before calling
///
/// `messages.db` must not be open by any active r2d2 pool or WAL-mode
/// connection when this function is called. If the pool remains open, it may
/// recreate the WAL file after our cleanup step, causing the newly restored
/// data to be shadowed by stale WAL entries. In production, `pattern backup
/// restore` runs as a separate one-shot process from the running agents, so
/// this condition is naturally satisfied.
///
/// # Errors
///
/// - [`BackupError::SnapshotNotFound`] — `snapshot_path` does not exist.
/// - [`BackupError::CorruptSnapshot`] — `PRAGMA integrity_check` returned a
///   non-ok result (messages.db is left unchanged).
/// - [`BackupError::IntegrityCheck`] — the integrity check query itself failed.
/// - [`BackupError::Io`] — I/O failure during safety copy or restore.
/// - [`BackupError::TempFile`] / [`BackupError::TempPersist`] — atomic rename
///   failure.
pub fn restore_snapshot(
    messages_db_path: &Path,
    snapshot_path: &Path,
) -> Result<PathBuf, BackupError> {
    if !snapshot_path.is_file() {
        return Err(BackupError::SnapshotNotFound {
            path: snapshot_path.to_owned(),
        });
    }

    // Verify the snapshot BEFORE touching messages.db. A corrupt snapshot
    // must leave the live database unchanged.
    verify_snapshot_integrity(snapshot_path)?;

    // Pre-restore safety copy.
    //
    // We use rusqlite's Backup API rather than std::fs::copy to ensure the
    // safety copy is a fully consistent snapshot even when the source database
    // has an active WAL (which is the case for databases used via r2d2 pools).
    // A raw file copy would miss any data that is in the WAL but not yet
    // checkpointed into the main database file.
    let pre_restore_path = pre_restore_path(messages_db_path);
    if messages_db_path.exists() {
        let src = rusqlite::Connection::open_with_flags(
            messages_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(BackupError::OpenSource)?;
        let mut dst =
            rusqlite::Connection::open(&pre_restore_path).map_err(BackupError::OpenDest)?;
        {
            let backup =
                rusqlite::backup::Backup::new(&src, &mut dst).map_err(BackupError::BackupInit)?;
            backup
                .run_to_completion(100, std::time::Duration::from_millis(5), None)
                .map_err(BackupError::BackupRun)?;
            // Backup dropped here, releasing its mutable borrow of dst.
        }
        // Strip WAL mode from the safety copy — same reasoning as for the
        // main restore destination. The safety copy file should be a clean,
        // self-contained snapshot that opens without creating a -wal file.
        dst.execute_batch("PRAGMA journal_mode = DELETE;")
            .map_err(|e| BackupError::Io {
                path: pre_restore_path.clone(),
                source: std::io::Error::other(e.to_string()),
            })?;
        // fsync the safety copy before doing the swap.
        drop(dst);
        std::fs::File::open(&pre_restore_path)
            .and_then(|f| f.sync_all())
            .map_err(|e| BackupError::Io {
                path: pre_restore_path.clone(),
                source: e,
            })?;
    }

    // Atomic swap: write snapshot into destination via rusqlite Backup API.
    //
    // We use the Backup API (rather than a raw file copy) to ensure the
    // destination is a clean, WAL-checkpointed database. A raw file copy of
    // the snapshot would leave the existing -wal and -shm files in place,
    // and SQLite would replay them on the next open — potentially corrupting
    // the restored state.
    //
    // By writing into a temp file and renaming, we:
    // 1. Keep the operation atomic (no partial-write observed by other readers).
    // 2. Ensure the destination has no associated WAL because it was freshly
    //    created as a new SQLite file (the Backup API produces a WAL-free
    //    checkpoint if the source was checkpointed).
    //
    // We also remove any pre-existing -wal and -shm files BEFORE the rename
    // so that when the file is opened next, SQLite does not apply stale WAL
    // entries to the freshly restored data.
    let messages_dir = messages_db_path
        .parent()
        .expect("messages_db_path always has a parent directory");

    let tmp = tempfile::NamedTempFile::new_in(messages_dir).map_err(|e| BackupError::TempFile {
        path: messages_dir.to_owned(),
        source: e,
    })?;

    // Use the Backup API to produce a clean copy of the snapshot.
    {
        let src = rusqlite::Connection::open_with_flags(
            snapshot_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(BackupError::OpenSource)?;
        let mut dst = rusqlite::Connection::open(tmp.path()).map_err(BackupError::OpenDest)?;
        {
            let backup =
                rusqlite::backup::Backup::new(&src, &mut dst).map_err(BackupError::BackupInit)?;
            backup
                .run_to_completion(100, std::time::Duration::from_millis(5), None)
                .map_err(BackupError::BackupRun)?;
            // Backup dropped here, releasing its mutable borrow of dst.
        }

        // Strip WAL mode from the destination.
        //
        // The Backup API copies page 1 (the database header) from the source,
        // which may have WAL mode set. If we leave the destination in WAL mode,
        // opening it later triggers creation of a new -wal file, which would
        // shadow the freshly restored data when the r2d2 pool re-enables WAL.
        // Converting to DELETE mode here makes the file a clean snapshot;
        // the pool will re-enable WAL on its next open via PRAGMA journal_mode=WAL.
        dst.execute_batch("PRAGMA journal_mode = DELETE;")
            .map_err(|e| BackupError::Io {
                path: tmp.path().to_owned(),
                source: std::io::Error::other(e.to_string()),
            })?;
    }

    std::fs::File::open(tmp.path())
        .and_then(|f| f.sync_all())
        .map_err(|e| BackupError::Io {
            path: tmp.path().to_owned(),
            source: e,
        })?;

    // Remove stale WAL and SHM files so SQLite does not replay them over
    // the freshly restored data. Missing files are ignored (not an error).
    let messages_db_name = messages_db_path
        .file_name()
        .expect("messages_db_path has a filename");
    for suffix in &["-wal", "-shm"] {
        let side_file =
            messages_dir.join(format!("{}{}", messages_db_name.to_string_lossy(), suffix));
        if side_file.exists() {
            std::fs::remove_file(&side_file).map_err(|e| BackupError::Io {
                path: side_file.clone(),
                source: e,
            })?;
        }
    }

    // Atomic rename into place.
    tmp.persist(messages_db_path)
        .map_err(|e| BackupError::TempPersist {
            path: messages_db_path.to_owned(),
            source: e.error,
        })?;

    Ok(pre_restore_path)
}

// ---------------------------------------------------------------------------
// resolve_snapshot
// ---------------------------------------------------------------------------

/// Look up a snapshot for `project_id` by a user-provided spec string.
///
/// Supported spec forms:
/// - `"latest"` — the most recent snapshot.
/// - `"2026-04-19T120000Z"` — exact filename stem match.
/// - `"2026-04-19"` — date prefix; returns the most recent snapshot on that
///   date (since the list is newest-first, this is the first match).
///
/// # Errors
///
/// - [`BackupError::NoSnapshots`] — no snapshots exist for `project_id`.
/// - [`BackupError::SnapshotNotFoundBySpec`] — spec did not match any
///   snapshot; the error includes all available timestamps for the user.
pub fn resolve_snapshot(
    paths: &crate::PatternPaths,
    project_id: &str,
    spec: &str,
) -> Result<SnapshotInfo, BackupError> {
    let snapshots = rotation::list_snapshots(paths, project_id)?;
    if snapshots.is_empty() {
        return Err(BackupError::NoSnapshots {
            project_id: project_id.to_owned(),
        });
    }

    if spec == "latest" {
        return Ok(snapshots.into_iter().next().unwrap());
    }

    // Exact filename stem match (e.g. "2026-04-19T120000Z").
    if let Some(matched) = snapshots
        .iter()
        .find(|s| s.path.file_stem().and_then(|n| n.to_str()) == Some(spec))
    {
        return Ok(matched.clone());
    }

    // Date-prefix match (e.g. "2026-04-19") — returns the most recent snapshot
    // on that calendar day. List is newest-first, so the first match is correct.
    if let Some(matched) = snapshots.iter().find(|s| {
        let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
        let iso_date = format!(
            "{:04}-{:02}-{:02}",
            zoned.year(),
            zoned.month(),
            zoned.day()
        );
        iso_date == spec
    }) {
        return Ok(matched.clone());
    }

    // No match — build a helpful error listing all available timestamps.
    Err(BackupError::SnapshotNotFoundBySpec {
        spec: spec.to_owned(),
        available: snapshots
            .iter()
            .map(|s| format_snapshot_name(&s.timestamp))
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Verify `PRAGMA integrity_check` on `snapshot_path` returns `"ok"`.
///
/// Opens the file read-only so it does not acquire any write locks.
fn verify_snapshot_integrity(snapshot_path: &Path) -> Result<(), BackupError> {
    let conn = rusqlite::Connection::open_with_flags(
        snapshot_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(BackupError::OpenSource)?;

    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| BackupError::IntegrityCheck {
            path: snapshot_path.to_owned(),
            source: e,
        })?;

    if result != "ok" {
        return Err(BackupError::CorruptSnapshot {
            path: snapshot_path.to_owned(),
            detail: result,
        });
    }

    Ok(())
}

/// Compute the pre-restore safety copy path for `messages_db_path`.
///
/// Returns `<parent>/<stem>.pre-restore-<ts_ns>` where `<ts_ns>` is the current
/// UTC nanosecond timestamp as a decimal integer. Nanosecond precision ensures
/// uniqueness even when two restores happen within the same second (e.g., a
/// restore immediately followed by a rollback restore in the same process).
fn pre_restore_path(messages_db_path: &Path) -> PathBuf {
    let ts_ns = Timestamp::now().as_nanosecond();
    let parent = messages_db_path
        .parent()
        .expect("messages_db_path always has a parent directory");
    let stem = messages_db_path
        .file_name()
        .expect("messages_db_path has a filename")
        .to_string_lossy();
    parent.join(format!("{stem}.pre-restore-{ts_ns}"))
}
