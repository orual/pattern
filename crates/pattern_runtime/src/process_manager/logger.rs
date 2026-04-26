//! Per-task process output logger — reliability backstop.
//!
//! ## Purpose
//!
//! `ProcessLogger` writes each `OutputChunk` from a spawned shell process to
//! an append-only log file at `<cache_dir>/shell/<task_id>.log`. The log
//! exists as a crash backstop (AC3.10): if the agent session terminates
//! unexpectedly, the full output is preserved on disk and can be recovered by
//! an operator or a restart path.
//!
//! The log is **NOT** exposed through any effect handler — agents cannot read
//! it via `Shell.*` or any other SDK call. It is purely a reliability surface
//! for human operators.
//!
//! ## Format
//!
//! One chunk per line, ISO-8601 timestamp prefix:
//!
//! ```text
//! 2026-04-24T17:42:00.123Z OUT  hello world
//! 2026-04-24T17:42:00.234Z OUT  another line
//! 2026-04-24T17:42:01.456Z EXIT code=Some(0) duration_ms=1233
//! ```
//!
//! Multi-line `Output` chunks have embedded newlines replaced with `\n` so
//! each chunk occupies exactly one log line. `Exit` records use the `EXIT`
//! prefix.
//!
//! ## Flush semantics
//!
//! `append` flushes on every write so that a mid-stream session crash does not
//! lose buffered output. The per-write flush cost is acceptable because spawned
//! process output is typically modest (log-style output at kilobytes/second at
//! most).
//!
//! ## Log retention
//!
//! Log files accumulate indefinitely in `<cache_dir>/shell/`. Rotation policy
//! is out of scope for Phase 3; the directory should be treated as ephemeral
//! scratch space that can be cleared between runtime restarts without data
//! loss (the canonical output path is the agent's async-reminder queue, not
//! this log).
//!
//! TODO(future): GFS-style rotation hook (plan Q5). When this lands, the
//! `open` path is the natural place to wire age-based cleanup of stale logs
//! before opening a fresh one.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::process_manager::types::{OutputChunk, TaskId};

/// Append-only log file for a single spawned shell task.
///
/// `append` writes each `OutputChunk` as one line with a jiff timestamp
/// prefix. The file handle is wrapped in a `Mutex` so the logger can be
/// shared across a limited concurrency boundary (e.g., if the bridge thread
/// and a test both hold a reference) without races.
///
/// Cloning a `ProcessLogger` shares the same underlying file handle (both
/// sides write to the same file under the same lock). If independent file
/// handles are needed, construct separate loggers.
pub struct ProcessLogger {
    file: Mutex<File>,
    path: PathBuf,
}

impl std::fmt::Debug for ProcessLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessLogger")
            .field("path", &self.path)
            .finish()
    }
}

impl ProcessLogger {
    /// Open (or create) the log file for `task_id` under `<cache_dir>/shell/`.
    ///
    /// Creates intermediate directories automatically. Returns an `io::Error`
    /// if the directory cannot be created or the file cannot be opened.
    pub fn open(cache_dir: &Path, task_id: &TaskId) -> std::io::Result<Self> {
        let dir = cache_dir.join("shell");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{task_id}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            file: Mutex::new(file),
            path,
        })
    }

    /// Append one `OutputChunk` to the log, then flush the file buffer.
    ///
    /// Multi-line `Output` strings have their embedded newlines replaced with
    /// the two-character sequence `\n` (backslash + n) so each chunk occupies
    /// exactly one log line.
    ///
    /// Errors from `writeln!` or `flush` are returned to the caller (the
    /// bridge thread). The bridge treats these as non-fatal: it logs the
    /// error via `tracing::warn!` and continues processing remaining chunks
    /// (best-effort logging).
    pub fn append(&self, chunk: &OutputChunk) -> std::io::Result<()> {
        let mut f = self.file.lock().unwrap();
        let ts = jiff::Timestamp::now();
        match chunk {
            OutputChunk::Output(s) => {
                // Replace embedded newlines so one chunk = one log line.
                let escaped = s.replace('\n', "\\n");
                writeln!(f, "{ts} OUT  {escaped}")?;
            }
            OutputChunk::Exit { code, duration_ms } => {
                writeln!(f, "{ts} EXIT code={code:?} duration_ms={duration_ms}")?;
            }
        }
        // Flush per-write: crash-safety guarantee from AC3.10.
        f.flush()?;
        Ok(())
    }

    /// Path of the log file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Open a logger, append three `Output` chunks, read the file, verify
    /// that three lines are present and each contains the expected text.
    #[test]
    fn appends_output_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_id = TaskId("test-log-01".to_string());
        let logger = ProcessLogger::open(dir.path(), &task_id).expect("open logger");

        logger
            .append(&OutputChunk::Output("first line\n".to_string()))
            .expect("append 1");
        logger
            .append(&OutputChunk::Output("second line\n".to_string()))
            .expect("append 2");
        logger
            .append(&OutputChunk::Output("third line\n".to_string()))
            .expect("append 3");

        let content = fs::read_to_string(logger.path()).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 lines, got:\n{content}");
        assert!(
            lines[0].contains("OUT") && lines[0].contains("first line"),
            "line 0 mismatch: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("OUT") && lines[1].contains("second line"),
            "line 1 mismatch: {}",
            lines[1]
        );
        assert!(
            lines[2].contains("OUT") && lines[2].contains("third line"),
            "line 2 mismatch: {}",
            lines[2]
        );
    }

    /// Append an `Exit` chunk and verify the EXIT line format.
    #[test]
    fn appends_exit_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_id = TaskId("test-log-02".to_string());
        let logger = ProcessLogger::open(dir.path(), &task_id).expect("open logger");

        logger
            .append(&OutputChunk::Exit {
                code: Some(0),
                duration_ms: 1234,
            })
            .expect("append exit");

        let content = fs::read_to_string(logger.path()).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "expected 1 line, got:\n{content}");
        let line = lines[0];
        assert!(line.contains("EXIT"), "expected EXIT prefix, got: {line}");
        assert!(
            line.contains("code=Some(0)"),
            "expected code=Some(0), got: {line}"
        );
        assert!(
            line.contains("duration_ms=1234"),
            "expected duration_ms=1234, got: {line}"
        );
    }

    /// Write to the logger then drop it. Re-read the file from disk and
    /// verify the content is still present (flush-persists guarantee).
    #[test]
    fn flush_persists_after_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_id = TaskId("test-log-03".to_string());
        let path = {
            let logger = ProcessLogger::open(dir.path(), &task_id).expect("open logger");
            logger
                .append(&OutputChunk::Output("persistent\n".to_string()))
                .expect("append");
            logger.path().to_path_buf()
        }; // logger dropped here

        let content = fs::read_to_string(&path).expect("read log after drop");
        assert!(
            content.contains("persistent"),
            "expected 'persistent' in log after drop, got:\n{content}"
        );
    }

    /// Spawn four threads each writing 50 chunks. After all threads finish,
    /// verify the total line count is exactly 200 and that no lines are
    /// interleaved (each line is a complete, well-formed log entry).
    ///
    /// This validates the `Mutex<File>` invariant: writes from concurrent
    /// threads do not produce partial/interleaved lines.
    #[test]
    fn concurrent_appends_dont_interleave() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let task_id = TaskId("test-log-04".to_string());
        let logger = Arc::new(ProcessLogger::open(dir.path(), &task_id).expect("open logger"));
        let path = logger.path().to_path_buf();

        let threads: Vec<_> = (0..4)
            .map(|thread_id| {
                let logger = Arc::clone(&logger);
                std::thread::spawn(move || {
                    for i in 0..50 {
                        logger
                            .append(&OutputChunk::Output(format!(
                                "thread {thread_id} chunk {i}\n"
                            )))
                            .expect("append");
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().expect("thread joined");
        }

        let content = fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            200,
            "expected exactly 200 lines from 4 threads × 50 chunks, got {}:\n{}",
            lines.len(),
            &content[..content.len().min(500)]
        );

        // Every line must contain the OUT prefix and one of the thread IDs
        // (no partial/interleaved lines that would lack these markers).
        for (i, line) in lines.iter().enumerate() {
            assert!(line.contains("OUT"), "line {i} missing OUT prefix: {line}");
            // Each line should contain "thread N chunk M" — verify it at
            // least contains "thread" and "chunk".
            assert!(
                line.contains("thread") && line.contains("chunk"),
                "line {i} does not look like a complete log entry: {line}"
            );
        }
    }
}
