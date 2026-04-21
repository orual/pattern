//! Universal pre-commit quiesce step.
//!
//! [`quiesce`] prepares the memory subsystem for a VCS commit by ensuring all
//! in-flight writes have landed on disk. It is mode-agnostic:
//!
//! - **Mode A** — the host-VCS caller invokes `quiesce` before its own commit.
//! - **Modes B / C** — `JjAdapter::commit` invokes `quiesce` as its first step.
//!
//! # Order of operations
//!
//! 1. **Pause subscribers** — signal all sync worker threads to flush pending
//!    work and park. Each worker drains its channel, imports all pending updates
//!    into disk_doc, renders the canonical file, then signals pause completion.
//!    Unlike the old drain approach, workers stay alive — subscriptions and
//!    channels remain intact so that writes during the pause window accumulate
//!    in their respective docs and are reconciled on resume.
//!
//! 2. **WAL checkpoint** — run `PRAGMA wal_checkpoint(TRUNCATE)` on `memory.db`
//!    via [`MemoryCache::wal_checkpoint`]. After this the on-disk database file is
//!    canonical with no outstanding WAL frames.
//!
//! 3. **fsync emitted files** — call `File::sync_all()` on each canonical file
//!    the caller supplies. Failures are logged and counted but do not abort the
//!    quiesce — a partial fsync is preferable to a hung pre-commit hook.
//!
//! 4. **Resume subscribers** — signal all paused workers to wake up. Each worker
//!    reconciles writes from the pause window via version-vector diff (catching
//!    both agent writes to memory_doc and external edits to disk_doc), renders
//!    once, then returns to the normal event loop.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::cache::MemoryCache;

/// Outcome of a successful [`quiesce`] call.
#[derive(Debug)]
pub struct QuiesceOutcome {
    /// Wall-clock time spent in `quiesce`.
    pub duration: Duration,
    /// Number of canonical files whose `fsync` failed.
    /// Zero in the happy path. Non-zero indicates a storage warning, but
    /// `quiesce` still returned `Ok` — the caller decides whether to abort
    /// the commit.
    pub fsync_failures: usize,
}

/// Errors that prevent a successful quiesce.
///
/// fsync failures are NOT included here — they are counted in
/// [`QuiesceOutcome::fsync_failures`] rather than aborting the call, because a
/// partial fsync is far better than an indefinitely hung pre-commit step.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum QuiesceError {
    /// The WAL checkpoint failed.
    ///
    /// This is a hard error: without a successful checkpoint the on-disk DB
    /// is not canonical and a VCS commit would capture an incomplete state.
    #[error("WAL checkpoint failed: {source}")]
    #[diagnostic(
        code(pattern_memory::quiesce::wal_checkpoint),
        help(
            "inspect the memory.db file; the database pool may be exhausted or the WAL may be locked"
        )
    )]
    WalCheckpoint {
        /// Underlying memory error (wraps the rusqlite/r2d2 error).
        #[source]
        source: pattern_core::types::memory_types::MemoryError,
    },
}

/// Quiesce the memory subsystem for a VCS commit.
///
/// Drains all sync subscriber workers, checkpoints the WAL on `memory.db`,
/// and fsyncs each path in `emitted_file_paths`. Returns [`QuiesceOutcome`]
/// on success, or [`QuiesceError`] if the WAL checkpoint fails.
///
/// # fsync behaviour
///
/// Per-file fsync errors are non-fatal: they are logged at `WARN` level and
/// counted in [`QuiesceOutcome::fsync_failures`]. This is deliberate — on
/// most filesystems `sync_all()` can fail transiently, and aborting the
/// quiesce loop would leave partially-fsynced files in a worse state than
/// proceeding.
///
/// # Mode A example
///
/// ```no_run
/// use std::path::PathBuf;
/// use pattern_memory::quiesce::quiesce;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let cache = unimplemented!();
/// let outcome = quiesce(&cache, &[PathBuf::from("/path/to/persona.md")])?;
/// println!("quiesce completed in {:?}, fsync failures: {}", outcome.duration, outcome.fsync_failures);
/// # Ok(())
/// # }
/// ```
pub fn quiesce(
    cache: &MemoryCache,
    emitted_file_paths: &[impl AsRef<Path>],
) -> Result<QuiesceOutcome, QuiesceError> {
    let t0 = Instant::now();

    // Step 1: pause all sync subscriber workers. Each worker flushes its
    // in-flight work (drain channel → import into disk_doc → render) and then
    // parks. Unlike drain_subscribers (which kills workers), pause keeps them
    // alive so writes during the pause accumulate in the docs and are
    // reconciled via version-vector diff on resume.
    let pause_outcome = cache.pause_subscribers(Duration::from_secs(5));
    if pause_outcome.timed_out > 0 {
        tracing::warn!(
            timed_out = pause_outcome.timed_out,
            paused = pause_outcome.paused,
            "some subscriber workers did not park within timeout"
        );
    }

    // Step 2: checkpoint the WAL. After this, memory.db is canonical with no
    // outstanding WAL frames. This is a hard error — without a checkpoint the
    // on-disk state is incomplete.
    cache
        .wal_checkpoint()
        .map_err(|e| QuiesceError::WalCheckpoint { source: e })?;

    // Step 3: fsync each emitted canonical file. Failures are non-fatal —
    // logged and counted but do not abort the call.
    let mut fsync_failures: usize = 0;
    for path in emitted_file_paths {
        let p = path.as_ref();
        if let Err(e) = fsync_file(p) {
            tracing::warn!(
                path = %p.display(),
                error = %e,
                "fsync failed for emitted canonical file"
            );
            fsync_failures += 1;
        }
    }

    // Step 4: resume all subscriber workers. They will reconcile any writes
    // that happened during the pause window via version-vector diff.
    cache.resume_subscribers();

    let duration = t0.elapsed();

    if fsync_failures > 0 {
        tracing::warn!(
            fsync_failures,
            ?duration,
            "quiesce completed with fsync failures — storage may be unreliable"
        );
    } else {
        tracing::debug!(?duration, "quiesce completed successfully");
    }

    Ok(QuiesceOutcome {
        duration,
        fsync_failures,
    })
}

/// Open a file and call `sync_all()` to ensure its data and metadata are
/// durably written to the underlying storage device.
fn fsync_file(path: &Path) -> std::io::Result<()> {
    let f = std::fs::File::open(path)?;
    f.sync_all()
}
