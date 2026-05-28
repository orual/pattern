// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Cross-process advisory file locking via `fs4`.
//!
//! Used by:
//! - `auth::codex_storage` — serializes `~/.codex/.auth.json` mutations
//!   between Pattern instances and codex CLI (which doesn't use a lock
//!   itself, but Pattern doing so is still correct: a Pattern crash mid-
//!   write can't corrupt a file codex was about to read, because we
//!   atomic-rename, and Pattern-vs-Pattern races are eliminated).
//! - `creds_store::json_fallback` — same reason, for Pattern's own
//!   keyring-fallback JSON storage.
//!
//! The lock targets a sidecar `*.lock` file so it doesn't interfere with
//! the actual data file's atomic-rename pattern.
//!
//! This module is **not** gated under `subscription-oauth` — it's generic
//! infrastructure consumed by both feature-gated and unconditional code
//! paths.

use std::path::Path;

use thiserror::Error;

/// Errors from `acquire_file_lock`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FileLockError {
    /// Could not create the parent directory of the lock file.
    #[error("could not create lock-file parent directory {path:?}")]
    CreateDir {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Could not open or create the lock file.
    #[error("could not open lock file {path:?}")]
    OpenLock {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Could not acquire the advisory lock (filesystem doesn't support it
    /// or the lock was poisoned).
    #[error("could not acquire exclusive lock on {path:?}")]
    Acquire {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Guard returned by [`acquire_file_lock`]. The underlying advisory lock is
/// released when this guard is dropped (via `fs4` + OS-level handle close).
pub struct FileLockGuard {
    // Holding the File alive keeps the flock alive. Dropping it releases.
    _file: std::fs::File,
}

/// Acquire an exclusive advisory lock on `lock_path`. The lock file is
/// created if it doesn't exist; the file's parent directory is created
/// recursively if needed.
///
/// The returned guard releases the lock on drop. Hold it for the entire
/// duration of the critical section:
///
/// ```ignore
/// let _guard = acquire_file_lock(&lock_path).await?;
/// // ... read-modify-write the protected file ...
/// // guard drops here → lock released.
/// ```
///
/// `fs4`'s `lock_exclusive` is a blocking syscall (`flock(LOCK_EX)`).
/// To avoid stalling the tokio reactor under contention we wrap the
/// acquisition in `spawn_blocking`. The returned guard holds a
/// `std::fs::File` rather than `tokio::fs::File` because the flock
/// lives on the OS-level file handle, not on the async wrapper.
///
/// This is an *advisory* lock; processes that don't call this helper
/// won't be blocked. That's acceptable for Pattern's interop with codex
/// CLI: codex doesn't lock either, but Pattern doing so eliminates
/// Pattern-vs-Pattern races and protects against the worst case
/// (corrupting an in-flight codex write).
pub async fn acquire_file_lock(
    lock_path: impl AsRef<Path>,
) -> Result<FileLockGuard, FileLockError> {
    let lock_path = lock_path.as_ref().to_path_buf();

    if let Some(parent) = lock_path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| FileLockError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
    }

    // Both the open and the flock acquire are blocking — spawn_blocking the
    // whole thing so a contended lock doesn't stall the reactor.
    let lock_path_for_blocking = lock_path.clone();
    let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File, FileLockError> {
        use fs4::fs_std::FileExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path_for_blocking)
            .map_err(|source| FileLockError::OpenLock {
                path: lock_path_for_blocking.clone(),
                source,
            })?;
        file.lock_exclusive()
            .map_err(|source| FileLockError::Acquire {
                path: lock_path_for_blocking,
                source,
            })?;
        Ok(file)
    })
    .await
    .map_err(|join_err| FileLockError::Acquire {
        path: lock_path.clone(),
        source: std::io::Error::other(format!("spawn_blocking join: {join_err}")),
    })??;

    Ok(FileLockGuard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn acquire_creates_lock_file_and_parent_dir() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("file.lock");
        let _guard = acquire_file_lock(&nested).await.expect("acquire");
        assert!(nested.exists(), "lock file should be created");
        assert!(
            nested.parent().unwrap().is_dir(),
            "parent dir should be created"
        );
    }

    #[tokio::test]
    async fn lock_is_exclusive_across_concurrent_acquirers() {
        // Two tasks try to enter a critical section; the second should
        // block on lock acquisition until the first releases. We verify
        // ordering by atomically incrementing a counter inside the section
        // and asserting the second task observes the first's increment.
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("contended.lock");
        let counter = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let lock_path_a = lock_path.clone();
        let counter_a = counter.clone();
        let barrier_a = barrier.clone();
        let task_a = tokio::spawn(async move {
            let _guard = acquire_file_lock(&lock_path_a).await.expect("a acquire");
            barrier_a.wait().await; // sync with b before we extend our hold
            // Hold the lock briefly so b is forced to wait.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let observed = counter_a.fetch_add(1, Ordering::SeqCst);
            assert_eq!(observed, 0, "a should be first");
        });

        let lock_path_b = lock_path.clone();
        let counter_b = counter.clone();
        let barrier_b = barrier.clone();
        let task_b = tokio::spawn(async move {
            barrier_b.wait().await; // ensure a holds the lock first
            let _guard = acquire_file_lock(&lock_path_b).await.expect("b acquire");
            let observed = counter_b.fetch_add(1, Ordering::SeqCst);
            assert_eq!(observed, 1, "b must see a's increment");
        });

        task_a.await.expect("a join");
        task_b.await.expect("b join");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn guard_drop_releases_lock() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("release.lock");
        {
            let _guard = acquire_file_lock(&lock_path).await.expect("acquire");
        } // guard drops here
        // Second acquire on the same path should not block.
        let acquired = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            acquire_file_lock(&lock_path),
        )
        .await
        .expect("did not time out — lock released")
        .expect("re-acquire ok");
        drop(acquired);
    }
}
