//! `FileManager` — pooled `DirWatcher` + file CRUD + watch lifecycle.
//!
//! One `FileManager` per `SessionContext`. Coordinates:
//! - A shared [`PathFanoutRouter`] session-wide.
//! - Pooled [`DirWatcher`] instances per parent directory (refcounted;
//!   lazily created on first file access, GC'd on last close/unwatch).
//! - Open files as [`LoroSyncedFile`] CRDT docs.
//! - Watch-only subscriptions via [`PathFanoutSubscription`].
//! - Listener threads bridging external-change events into the session's
//!   between-turn async-reminder queue.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use pattern_core::capability::CapabilitySet;
use pattern_core::types::message::{FileEditKind, MessageAttachment};
use pattern_memory::loro_sync::{
    DirWatcher, DirWatcherConfig, ExternalChangeEvent, LoroSyncedFile, PathFanoutRouter,
    PathFanoutSubscription,
};

use crate::file_manager::error::FileError;
use crate::file_manager::policy::FilePolicy;
use crate::file_manager::types::FileInfo;
use crate::permission::PermissionBridge;

/// One per parent directory in the FileManager pool. Refcount lives
/// alongside the watcher Arc so a single DashMap entry guard atomically
/// covers acquire / release / GC decisions — no TOCTOU between a racing
/// ensure and release.
struct PooledDirWatcher {
    watcher: Arc<DirWatcher>,
    refcount: usize,
}

/// Per-session file manager coordinating pooled directory watchers,
/// open CRDT-backed files, watch-only subscriptions, and between-turn
/// async-reminder delivery.
pub struct FileManager {
    policy: FilePolicy,
    router: PathFanoutRouter,
    /// Per-directory pooled watchers with refcounts. The refcount lives
    /// inside the entry value (not in a parallel map) so one DashMap entry
    /// guard atomically gates increment / decrement / decide-to-remove.
    dir_watchers: DashMap<PathBuf, PooledDirWatcher>,
    open_files: DashMap<PathBuf, Arc<LoroSyncedFile>>,
    /// Per-file conflict flags. Set when a `ConflictDetected` event fires
    /// for an open file. Cleared by `reload()` or `force_write()`. When
    /// set, `write()` returns `FileError::FileInConflict`.
    conflict_flags: Arc<DashMap<PathBuf, ()>>,
    watch_only_paths: DashMap<PathBuf, PathFanoutSubscription>,
    /// One listener per open/watched file, bridging SyncedDoc change events
    /// or router subscriptions into the session's between-turn async-reminder
    /// queue. The cancel token lets `close()` signal the listener to stop
    /// before dropping the SyncedDoc's senders.
    edit_listeners: DashMap<PathBuf, (CancellationToken, JoinHandle<()>)>,
    /// Handle to the session's async-reminder queue. Each listener thread
    /// receives a clone so it can push `MessageAttachment` entries that
    /// the next turn's compose drains.
    async_reminder_queue: Arc<Mutex<Vec<MessageAttachment>>>,
    capability_set: Arc<CapabilitySet>,
    permission_bridge: Arc<PermissionBridge>,
    /// Owning agent id — used as the `agent_id` field on emitted
    /// `PermissionRequest`s so the human reviewer sees who is asking.
    agent_id: pattern_core::AgentId,
    cancel: CancellationToken,
}

impl std::fmt::Debug for FileManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileManager")
            .field("agent_id", &self.agent_id)
            .field("open_files", &self.open_files.len())
            .field("watch_only_paths", &self.watch_only_paths.len())
            .field("dir_watchers", &self.dir_watchers.len())
            .finish_non_exhaustive()
    }
}

impl FileManager {
    /// Construct a new file manager for a session.
    pub fn new(
        policy: FilePolicy,
        async_reminder_queue: Arc<Mutex<Vec<MessageAttachment>>>,
        capability_set: Arc<CapabilitySet>,
        permission_bridge: Arc<PermissionBridge>,
        agent_id: pattern_core::AgentId,
    ) -> Self {
        Self {
            policy,
            router: PathFanoutRouter::new(),
            dir_watchers: DashMap::new(),
            open_files: DashMap::new(),
            conflict_flags: Arc::new(DashMap::new()),
            watch_only_paths: DashMap::new(),
            edit_listeners: DashMap::new(),
            async_reminder_queue,
            capability_set,
            permission_bridge,
            agent_id,
            cancel: CancellationToken::new(),
        }
    }

    fn check_capability(&self) -> Result<(), FileError> {
        if !self.capability_set.has_file() {
            return Err(FileError::CapabilityDenied);
        }
        Ok(())
    }

    /// Acquire (creating if needed) the DirWatcher for `parent_dir` and
    /// bump its refcount. Caller (open / watch) MUST pair this with
    /// `release_dir_watcher_ref` on close / unwatch.
    ///
    /// Single DashMap entry guard wraps both the watcher Arc and the
    /// refcount, so increment / decrement / decide-to-remove all happen
    /// atomically per parent_dir. No TOCTOU window.
    fn ensure_dir_watcher(&self, parent_dir: &Path) -> Result<Arc<DirWatcher>, FileError> {
        let canonical = std::fs::canonicalize(parent_dir).unwrap_or_else(|_| parent_dir.to_owned());
        match self.dir_watchers.entry(canonical.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut e) => {
                let v = e.get_mut();
                v.refcount += 1;
                Ok(Arc::clone(&v.watcher))
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let w = DirWatcher::start(
                    DirWatcherConfig::new(canonical.clone()),
                    self.router.clone(),
                )
                .map_err(|err| FileError::Io {
                    path: canonical.clone(),
                    source: std::io::Error::other(err.to_string()),
                })?;
                let arc = Arc::new(w);
                e.insert(PooledDirWatcher {
                    watcher: Arc::clone(&arc),
                    refcount: 1,
                });
                Ok(arc)
            }
        }
    }

    /// Decrement the refcount; remove the entry (and drop its watcher)
    /// when refcount hits zero. Atomic per parent_dir via the entry guard.
    fn release_dir_watcher_ref(&self, parent_dir: &Path) {
        let canonical = std::fs::canonicalize(parent_dir).unwrap_or_else(|_| parent_dir.to_owned());
        if let dashmap::mapref::entry::Entry::Occupied(mut e) =
            self.dir_watchers.entry(canonical.clone())
        {
            let v = e.get_mut();
            v.refcount = v.refcount.saturating_sub(1);
            if v.refcount == 0 {
                e.remove(); // drops the inner Arc<DirWatcher>; ingest thread exits.
            }
            return;
        }
        // Unmatched release — programming error. Log loudly; don't panic
        // since a leaked watcher is preferable to a crashed session.
        tracing::warn!(parent = ?canonical, "release_dir_watcher_ref without prior acquire");
    }

    /// Read a file. If the file is open, reads from the CRDT doc for
    /// consistency. Otherwise reads directly from disk.
    pub fn read(&self, path: &Path) -> Result<Vec<u8>, FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        if let Some(sf) = self.open_files.get(&canonical) {
            Ok(sf.read()?.into_bytes())
        } else {
            std::fs::read(path).map_err(|e| FileError::Io {
                path: path.to_owned(),
                source: e,
            })
        }
    }

    /// Write content to a file. If config-shape detection fires,
    /// escalates to human approval via the permission bridge.
    ///
    /// Returns `FileError::FileInConflict` if the file has an outstanding
    /// conflict. Call `reload()` or `force_write()` first.
    pub fn write(&self, path: &Path, content: &[u8]) -> Result<(), FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        if crate::file_manager::config_detect::is_pattern_config_write(path, content) {
            self.await_human_approval(path, content)?;
        }
        let canonical = canonicalize_best(path);
        if self.conflict_flags.contains_key(&canonical) {
            return Err(FileError::FileInConflict { path: canonical });
        }
        if let Some(sf) = self.open_files.get(&canonical) {
            let s = std::str::from_utf8(content).map_err(|e| FileError::Io {
                path: path.to_owned(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?;
            sf.write(s)?;
            Ok(())
        } else {
            pattern_memory::fs::atomic_write(path, content).map_err(|e| FileError::Io {
                path: path.to_owned(),
                source: std::io::Error::other(e.to_string()),
            })
        }
    }

    /// Get an already-open file or auto-open it. Returns the Arc<LoroSyncedFile>
    /// so callers can perform direct Loro operations (line edits, etc.).
    ///
    /// This is the entry point for line-level edit operations: the handler
    /// calls `get_or_open`, then invokes `insert_lines`/`replace_lines`/
    /// `delete_lines` on the returned `LoroSyncedFile`.
    pub fn get_or_open(
        &self,
        path: &std::path::Path,
    ) -> Result<std::sync::Arc<pattern_memory::loro_sync::text::LoroSyncedFile>, FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        eprintln!("get_or_open: canonical = `{}`", canonical.display());
        // Return existing if open.
        if let Some(sf) = self.open_files.get(&canonical) {
            eprintln!("get_or_open: found existing open file");
            return Ok(sf.value().clone());
        }
        // Not open yet — open it (which creates the LoroSyncedFile, watcher, etc.).
        self.open(path)?;
        // Now it should be in open_files.
        self.open_files
            .get(&canonical)
            .map(|sf| sf.value().clone())
            .ok_or_else(|| FileError::Io {
                path: canonical,
                source: std::io::Error::other("file disappeared from open_files after open()"),
            })
    }

    /// Open a file for CRDT-tracked editing. Returns the current content.
    /// Idempotent: re-opening a file returns its current content without
    /// creating a new watcher or listener.
    ///
    /// Uses the DashMap entry API to atomically check-and-insert, preventing
    /// a TOCTOU race where two concurrent `open()` calls both see the file
    /// as absent and create duplicate watchers/listeners.
    pub fn open(&self, path: &Path) -> Result<Vec<u8>, FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);

        // Check idempotent case first without holding the entry lock (avoids
        // deadlock since read() also accesses open_files).
        if self.open_files.contains_key(&canonical) {
            return self.read(path);
        }

        // Atomic check-and-insert via the entry API.
        match self.open_files.entry(canonical.clone()) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                // Race: another thread opened between our contains_key and
                // entry(). Read from the entry directly.
                let sf = entry.get();
                Ok(sf.read()?.into_bytes())
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let parent = canonical.parent().ok_or_else(|| FileError::Io {
                    path: canonical.clone(),
                    source: std::io::Error::other("path has no parent"),
                })?;
                self.ensure_dir_watcher(parent)?;
                let sf = LoroSyncedFile::open_with_router(&canonical, &self.router)?;
                let content = sf.read()?.into_bytes();
                eprintln!("read content: {}", String::from_utf8_lossy(&content));

                // Per-file cancel token for the listener. Signalled in close()
                // BEFORE dropping the SyncedDoc's senders, preventing the
                // listener from enqueuing attachments after close returns.
                let listener_cancel = CancellationToken::new();

                let rx = sf.subscribe_external_changes();
                let queue = Arc::clone(&self.async_reminder_queue);
                let conflict_flags = Arc::clone(&self.conflict_flags);
                let cancel_clone = listener_cancel.clone();
                let path_owned = canonical.clone();
                let listener = std::thread::Builder::new()
                    .name(format!(
                        "file-listener:{}",
                        canonical
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                    ))
                    .spawn(move || {
                        use crossbeam_channel::RecvTimeoutError;
                        loop {
                            if cancel_clone.is_cancelled() {
                                break;
                            }
                            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                                Ok(evt) => {
                                    if cancel_clone.is_cancelled() {
                                        break;
                                    }
                                    let attachment = match evt {
                                        ExternalChangeEvent::Applied { .. } => {
                                            MessageAttachment::FileEdit {
                                                path: path_owned.clone(),
                                                kind: FileEditKind::Open,
                                                at: jiff::Timestamp::now(),
                                                diff: None,
                                            }
                                        }
                                        ExternalChangeEvent::ConflictDetected { .. } => {
                                            // Mark the file as in conflict. Subsequent
                                            // write() calls will return FileInConflict
                                            // until reload() or force_write() clears it.
                                            conflict_flags.insert(path_owned.clone(), ());
                                            MessageAttachment::FileConflict {
                                                path: path_owned.clone(),
                                                at: jiff::Timestamp::now(),
                                            }
                                        }
                                        _ => continue, // future variants — skip gracefully.
                                    };
                                    queue.lock().unwrap().push(attachment);
                                }
                                Err(RecvTimeoutError::Timeout) => continue,
                                Err(RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    })
                    .map_err(|e| FileError::Io {
                        path: path.to_owned(),
                        source: e,
                    })?;
                self.edit_listeners
                    .insert(canonical.clone(), (listener_cancel, listener));
                entry.insert(Arc::new(sf));
                Ok(content)
            }
        }
    }

    /// Close an open file, releasing its CRDT doc and decrementing the
    /// parent directory's watcher refcount.
    ///
    /// Signals the per-file listener cancel token BEFORE dropping the
    /// SyncedDoc, so the listener exits cleanly without enqueuing
    /// attachments after close returns.
    pub fn close(&self, path: &Path) -> Result<(), FileError> {
        self.check_capability()?;
        let canonical = canonicalize_best(path);

        // Cancel the per-file listener BEFORE removing the SyncedDoc.
        // This prevents the listener from enqueuing attachments during
        // the teardown window.
        if let Some((_, (cancel_token, _handle))) = self.edit_listeners.remove(&canonical) {
            cancel_token.cancel();
            // Join is best-effort; the listener exits within 50ms of cancel.
            let _ = _handle.join();
        }

        let Some((_, sf)) = self.open_files.remove(&canonical) else {
            return Err(FileError::NotOpen(canonical));
        };
        // The Arc should be unique since only open_files holds it. If
        // somehow shared (shouldn't happen per current API), the SyncedDoc
        // will close on final Arc drop.
        match Arc::try_unwrap(sf) {
            Ok(sf) => sf.close(),
            Err(arc) => {
                tracing::debug!(
                    path = ?canonical,
                    ref_count = Arc::strong_count(&arc),
                    "LoroSyncedFile Arc not unique at close; will close on final drop"
                );
            }
        }
        self.conflict_flags.remove(&canonical);
        if let Some(parent) = canonical.parent() {
            self.release_dir_watcher_ref(parent);
        }
        Ok(())
    }

    /// Register a watch-only subscription on a file. External edits
    /// generate `FileEdit { kind: Watch }` reminders without maintaining
    /// a CRDT doc.
    pub fn watch(&self, path: &Path) -> Result<(), FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        if self.watch_only_paths.contains_key(&canonical) {
            return Ok(()); // idempotent.
        }
        let parent = canonical.parent().ok_or_else(|| FileError::Io {
            path: canonical.clone(),
            source: std::io::Error::other("path has no parent"),
        })?;
        self.ensure_dir_watcher(parent)?;

        // Register a subscription on the shared router directly.
        let (tx, rx) = crossbeam_channel::bounded(64);
        let subscription = self.router.subscribe(canonical.clone(), tx);

        let listener_cancel = CancellationToken::new();
        let queue = Arc::clone(&self.async_reminder_queue);
        let cancel_clone = listener_cancel.clone();
        let path_owned = canonical.clone();
        let listener = std::thread::Builder::new()
            .name(format!(
                "file-watch-listener:{}",
                canonical
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
            ))
            .spawn(move || {
                use crossbeam_channel::RecvTimeoutError;
                loop {
                    if cancel_clone.is_cancelled() {
                        break;
                    }
                    match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(_evt) => {
                            if cancel_clone.is_cancelled() {
                                break;
                            }
                            let attachment = MessageAttachment::FileEdit {
                                path: path_owned.clone(),
                                kind: FileEditKind::Watch,
                                at: jiff::Timestamp::now(),
                                diff: None, // watch-only never has diff.
                            };
                            queue.lock().unwrap().push(attachment);
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|e| FileError::Io {
                path: path.to_owned(),
                source: e,
            })?;
        self.edit_listeners
            .insert(canonical.clone(), (listener_cancel, listener));
        self.watch_only_paths.insert(canonical, subscription);
        Ok(())
    }

    /// Unwatch a file, dropping the subscription and releasing the
    /// parent directory's watcher refcount.
    pub fn unwatch(&self, path: &Path) -> Result<(), FileError> {
        self.check_capability()?;
        let canonical = canonicalize_best(path);
        // Cancel the per-file listener before dropping the subscription.
        if let Some((_, (cancel_token, handle))) = self.edit_listeners.remove(&canonical) {
            cancel_token.cancel();
            let _ = handle.join();
        }
        self.watch_only_paths.remove(&canonical); // drop guard unregisters router entry.
        if let Some(parent) = canonical.parent() {
            self.release_dir_watcher_ref(parent);
        }
        Ok(())
    }

    /// List directory entries, optionally filtered by a glob pattern.
    pub fn list(&self, dir: &Path, glob: &str) -> Result<Vec<FileInfo>, FileError> {
        self.check_capability()?;
        self.policy.check_access(dir)?;
        let matcher = if glob.is_empty() || glob == "*" {
            None
        } else {
            Some(
                globset::Glob::new(glob)
                    .map_err(|e| FileError::BadGlob(format!("{glob}: {e}")))?
                    .compile_matcher(),
            )
        };
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| FileError::Io {
            path: dir.to_owned(),
            source: e,
        })? {
            let entry = entry.map_err(|e| FileError::Io {
                path: dir.to_owned(),
                source: e,
            })?;
            let p = entry.path();
            if let Some(m) = &matcher
                && !m.is_match(&p)
            {
                continue;
            }
            let meta = entry.metadata().map_err(|e| FileError::Io {
                path: p.clone(),
                source: e,
            })?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| jiff::Timestamp::try_from(t).ok())
                .unwrap_or(jiff::Timestamp::UNIX_EPOCH);
            entries.push(FileInfo {
                path: p,
                size: meta.len(),
                mtime,
                is_dir: meta.is_dir(),
            });
        }
        Ok(entries)
    }

    /// Reload a file from disk, discarding in-memory CRDT state. Returns
    /// the fresh content. Used after a conflict to accept the external
    /// writer's version. Clears the conflict flag so subsequent writes succeed.
    pub fn reload(&self, path: &Path) -> Result<Vec<u8>, FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        let Some(sf) = self.open_files.get(&canonical) else {
            return Err(FileError::NotOpen(canonical));
        };
        let content = sf.reload()?;
        // Clear the conflict flag — reload resolves the conflict by accepting
        // the disk version.
        self.conflict_flags.remove(&canonical);
        Ok(content.into_bytes())
    }

    /// Force-write the agent's current content to disk, bypassing the
    /// CRDT conflict check. Used after a conflict to overwrite with the
    /// agent's version. Clears the conflict flag so subsequent writes succeed.
    pub fn force_write(&self, path: &Path, content: &[u8]) -> Result<(), FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        let Some(sf) = self.open_files.get(&canonical) else {
            return Err(FileError::NotOpen(canonical));
        };
        sf.apply_external_bytes(content)?;
        // Clear the conflict flag — force_write resolves the conflict by
        // overwriting with the agent's content.
        self.conflict_flags.remove(&canonical);
        Ok(())
    }

    /// Snapshot open file paths for session serialization.
    pub fn open_paths(&self) -> Vec<PathBuf> {
        self.open_files.iter().map(|e| e.key().clone()).collect()
    }

    /// Snapshot watch-only paths (subscriptions that track external edits
    /// without maintaining a CRDT doc). Exposed for tests.
    pub fn watch_only_paths(&self) -> Vec<PathBuf> {
        self.watch_only_paths
            .iter()
            .map(|e| e.key().clone())
            .collect()
    }

    /// Number of pooled directory watchers currently alive. Exposed for
    /// tests to verify pooling and GC behaviour.
    pub fn dir_watcher_count(&self) -> usize {
        self.dir_watchers.len()
    }

    fn await_human_approval(&self, path: &Path, content: &[u8]) -> Result<(), FileError> {
        // Implemented in Task 5.
        crate::file_manager::config_detect::await_approval(
            &self.permission_bridge,
            &self.agent_id,
            path,
            content,
        )
    }
}

impl Drop for FileManager {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Cancel all per-file listener tokens so they exit promptly.
        for entry in self.edit_listeners.iter() {
            entry.value().0.cancel();
        }
        // Cascade:
        //   1. open_files drops → SyncedDoc drops → router subscription guards drop.
        //   2. watch_only_paths drops → router subscription guards drop.
        //   3. dir_watchers drops → each DirWatcher drops → ingest threads exit.
        //   4. edit_listeners drops → each listener sees cancel + Disconnected.
    }
}

use crate::file_manager::path_util::canonicalize_best;

/// Test-only helpers. Expose internals needed for deterministic conflict-path
/// integration tests without polluting the production API surface.
///
/// Available under `#[cfg(test)]` (in-crate unit tests) and when the
/// `test-support` feature is enabled (integration tests in `tests/`).
#[cfg(any(test, feature = "test-support"))]
impl FileManager {
    /// Get the `LoroSyncedFile` for an open path. Returns `None` if the path
    /// is not open. Used in integration tests to call `clear_saved_frontier_for_test`
    /// and `has_unsaved_edits` directly on the underlying CRDT doc.
    pub fn get_open_file_for_test(&self, path: &Path) -> Option<Arc<LoroSyncedFile>> {
        let canonical = canonicalize_best(path);
        self.open_files.get(&canonical).map(|v| Arc::clone(&*v))
    }

    /// Returns `true` iff the open file at `path` has unsaved edits in its
    /// memory_doc. Returns `None` if the path is not open.
    pub fn has_unsaved_edits_for_path(&self, path: &Path) -> Option<bool> {
        let canonical = canonicalize_best(path);
        self.open_files
            .get(&canonical)
            .map(|sf| sf.has_unsaved_edits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::capability::EffectCategory;
    use std::time::Duration;

    fn full_caps() -> Arc<CapabilitySet> {
        Arc::new(CapabilitySet::all())
    }

    fn no_file_caps() -> Arc<CapabilitySet> {
        // Start with default (empty) and add only Memory — no File.
        let mut cs = CapabilitySet::default();
        cs.categories.insert(EffectCategory::Memory);
        Arc::new(cs)
    }

    fn allow_all_policy(dir: &Path) -> FilePolicy {
        FilePolicy::from_rules(vec![(
            crate::file_manager::policy::RuleMode::Allow,
            format!("{}/**", dir.display()),
        )])
        .unwrap()
    }

    /// Wait up to `deadline` for `check()` to return true, polling every 25ms.
    fn wait_for(deadline: Duration, check: impl Fn() -> bool) -> bool {
        let end = std::time::Instant::now() + deadline;
        while std::time::Instant::now() < end {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        check()
    }

    /// Open three files in the same directory → assert one DirWatcher is
    /// created (not three); close them one by one → assert watcher GC'd
    /// after last close.
    #[tokio::test]
    async fn pooled_watcher_shared_and_gc() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        let file_c = dir.path().join("c.txt");
        std::fs::write(&file_a, "hello a").unwrap();
        std::fs::write(&file_b, "hello b").unwrap();
        std::fs::write(&file_c, "hello c").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Open first file — one watcher created.
        let content_a = fm.open(&file_a).unwrap();
        assert_eq!(content_a, b"hello a");
        assert_eq!(
            fm.dir_watcher_count(),
            1,
            "one dir watcher after first open"
        );

        // Open second file in same dir — still one watcher.
        let content_b = fm.open(&file_b).unwrap();
        assert_eq!(content_b, b"hello b");
        assert_eq!(
            fm.dir_watcher_count(),
            1,
            "still one dir watcher after second open in same dir"
        );

        // Open third file in same dir — still one watcher.
        let content_c = fm.open(&file_c).unwrap();
        assert_eq!(content_c, b"hello c");
        assert_eq!(
            fm.dir_watcher_count(),
            1,
            "still one dir watcher after third open in same dir"
        );

        // Close first — refcount goes to 2; watcher lives.
        fm.close(&file_a).unwrap();
        assert_eq!(
            fm.dir_watcher_count(),
            1,
            "watcher alive while two files still open"
        );

        // Close second — refcount goes to 1; watcher lives.
        fm.close(&file_b).unwrap();
        assert_eq!(
            fm.dir_watcher_count(),
            1,
            "watcher alive while one file still open"
        );

        // Close third — refcount goes to 0; watcher GC'd.
        fm.close(&file_c).unwrap();
        assert_eq!(
            fm.dir_watcher_count(),
            0,
            "watcher GC'd after last file closed"
        );
    }

    #[tokio::test]
    async fn capability_denied_without_file_effect() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "data").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            no_file_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        let err = fm.read(&file).unwrap_err();
        assert!(
            matches!(err, FileError::CapabilityDenied),
            "expected CapabilityDenied, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn read_write_without_open() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rw.txt");
        std::fs::write(&file, "original").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Read bypasses CRDT (file not open).
        let content = fm.read(&file).unwrap();
        assert_eq!(content, b"original");

        // Write bypasses CRDT (file not open) → atomic write.
        fm.write(&file, b"updated").unwrap();
        let disk = std::fs::read(&file).unwrap();
        assert_eq!(disk, b"updated");
    }

    #[tokio::test]
    async fn close_not_open_returns_error() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not_open.txt");

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        let err = fm.close(&file).unwrap_err();
        assert!(
            matches!(err, FileError::NotOpen(_)),
            "expected NotOpen, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn open_idempotent() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("idem.txt");
        std::fs::write(&file, "contents").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        let first = fm.open(&file).unwrap();
        let second = fm.open(&file).unwrap();
        assert_eq!(first, second, "re-open should return same content");
        assert_eq!(fm.dir_watcher_count(), 1, "still only one watcher");
    }

    #[tokio::test]
    async fn external_edit_produces_reminder() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("watched.txt");
        std::fs::write(&file, "initial").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        fm.open(&file).unwrap();

        // Give the watcher time to register.
        std::thread::sleep(Duration::from_millis(100));

        // External edit: write directly to disk.
        std::fs::write(&file, "external change").unwrap();

        // Wait for the listener to pick up the event.
        let got = wait_for(Duration::from_secs(5), || !queue.lock().unwrap().is_empty());

        // Clean up before asserting.
        fm.close(&file).unwrap();
        drop(fm);

        assert!(
            got,
            "expected at least one async reminder from external edit"
        );
        let reminders = queue.lock().unwrap();
        assert!(
            reminders.iter().any(|a| matches!(
                a,
                MessageAttachment::FileEdit {
                    kind: FileEditKind::Open,
                    ..
                }
            )),
            "expected FileEdit(Open) reminder (no pending edits → Applied), got: {reminders:?}"
        );
    }

    // ==========================================================================
    // AC2 unit tests
    // ==========================================================================

    /// AC2.1 — `read` does not open a Loro-backed file.
    ///
    /// After `fm.read(path)`, an external edit should produce NO async reminder
    /// (the file is not open, so no listener is registered).
    #[tokio::test]
    async fn read_does_not_open_loro() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("read_only.txt");
        std::fs::write(&file, "initial").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Read the file — must NOT open a loro doc or start a listener.
        let content = fm.read(&file).unwrap();
        assert_eq!(content, b"initial");

        // No listener should be registered — no open files.
        assert!(
            fm.open_files.is_empty(),
            "read must not populate open_files"
        );

        // Give the watcher time to register (it shouldn't since no open/watch).
        std::thread::sleep(Duration::from_millis(100));

        // External edit.
        std::fs::write(&file, "external change").unwrap();

        // Wait 5s — no reminder should arrive because the file was only read.
        let got_reminder = wait_for(Duration::from_secs(5), || !queue.lock().unwrap().is_empty());
        assert!(
            !got_reminder,
            "read-only access must not produce async reminders on external edit"
        );
    }

    /// AC2.2 — `open` returns current content AND subscribes the file.
    ///
    /// After `fm.open(path)`, the returned bytes match disk; a subsequent
    /// external edit produces a `FileEdit { kind: Open }` reminder in the queue.
    #[tokio::test]
    async fn open_returns_content_and_subscribes() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("open_test.txt");
        std::fs::write(&file, "hello open").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Open: returned content must match disk.
        let content = fm.open(&file).unwrap();
        assert_eq!(
            content, b"hello open",
            "open must return current disk content"
        );

        // Give watcher time to register.
        std::thread::sleep(Duration::from_millis(100));

        // External edit.
        std::fs::write(&file, "external change to open file").unwrap();

        // Listener should deliver a reminder.
        let got = wait_for(Duration::from_secs(5), || !queue.lock().unwrap().is_empty());
        fm.close(&file).unwrap();

        assert!(
            got,
            "expected async reminder after external edit on open file"
        );
        let reminders = queue.lock().unwrap();
        assert!(
            reminders.iter().any(|a| matches!(
                a,
                MessageAttachment::FileEdit {
                    kind: FileEditKind::Open,
                    ..
                }
            )),
            "expected FileEdit(Open) (no pending edits → Applied), got: {reminders:?}"
        );
    }

    /// AC2.3 — `write` on an open file goes through the CRDT; on an
    /// un-opened file it falls back to `atomic_write`.
    ///
    /// The CRDT path is verified by reading back through the FileManager
    /// (which reads from the LoroDoc when open). The direct-write path is
    /// verified by reading back from disk without opening.
    #[tokio::test]
    async fn write_on_open_file_goes_through_loro() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("loro_write.txt");
        let file_b = dir.path().join("direct_write.txt");
        std::fs::write(&file_a, "initial a").unwrap();
        std::fs::write(&file_b, "initial b").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Open file_a → write through Loro.
        fm.open(&file_a).unwrap();
        fm.write(&file_a, b"written via loro").unwrap();

        // Read back via FileManager (reads from LoroDoc when open).
        let read_back = fm.read(&file_a).unwrap();
        assert_eq!(
            read_back,
            b"written via loro",
            "write on open file must go through Loro; read_back: {:?}",
            String::from_utf8_lossy(&read_back)
        );

        // file_b was never opened → write goes directly to disk.
        fm.write(&file_b, b"written direct").unwrap();
        let disk_content = std::fs::read(&file_b).unwrap();
        assert_eq!(
            disk_content, b"written direct",
            "write on un-opened file must write directly to disk"
        );
        // No LoroDoc should have been created for file_b.
        let canonical_b = canonicalize_best(&file_b);
        assert!(
            !fm.open_files.contains_key(&canonical_b),
            "un-opened file must not create a Loro doc"
        );

        fm.close(&file_a).unwrap();
    }

    /// AC2.4 — `close` drops the watcher; subsequent external edits
    /// produce no async reminders.
    #[tokio::test]
    async fn close_drops_watcher() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("close_test.txt");
        std::fs::write(&file, "initial").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Open + close immediately.
        fm.open(&file).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        fm.close(&file).unwrap();

        // Drain any reminder that may have fired during setup.
        queue.lock().unwrap().clear();

        // Give the old listener time to terminate.
        std::thread::sleep(Duration::from_millis(100));

        // External edit after close — no listener should fire.
        std::fs::write(&file, "post-close external change").unwrap();

        let got_reminder = wait_for(Duration::from_secs(2), || !queue.lock().unwrap().is_empty());
        assert!(
            !got_reminder,
            "external edit after close must not produce async reminder"
        );
        assert_eq!(
            fm.dir_watcher_count(),
            0,
            "dir watcher must be GC'd after file is closed"
        );
    }

    /// AC2.5 — `list` with a glob pattern returns only matching entries.
    #[tokio::test]
    async fn list_with_glob() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let a_rs = dir.path().join("a.rs");
        let b_py = dir.path().join("b.py");
        let c_rs = dir.path().join("c.rs");
        std::fs::write(&a_rs, "fn main() {}").unwrap();
        std::fs::write(&b_py, "print('hello')").unwrap();
        std::fs::write(&c_rs, "fn other() {}").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        // The policy must allow both the directory itself (for listing) and
        // paths under it (for file operations). Two rules cover both cases.
        let dir_glob = dir.path().display().to_string();
        let files_glob = format!("{}/**", dir_glob);
        let policy = FilePolicy::from_rules(vec![
            (crate::file_manager::policy::RuleMode::Allow, dir_glob),
            (crate::file_manager::policy::RuleMode::Allow, files_glob),
        ])
        .unwrap();

        let fm = FileManager::new(
            policy,
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        let entries = fm.list(dir.path(), "*.rs").unwrap();
        assert_eq!(
            entries.len(),
            2,
            "*.rs glob must return exactly 2 entries, got: {entries:?}"
        );
        // Both returned paths must end with .rs.
        for entry in &entries {
            assert!(
                entry.path.extension().is_some_and(|ext| ext == "rs"),
                "all entries must have .rs extension, got: {:?}",
                entry.path
            );
        }
        // Sizes and mtimes must be populated (not epoch).
        assert!(
            entries
                .iter()
                .all(|e| e.mtime != jiff::Timestamp::UNIX_EPOCH),
            "mtime must be populated for all entries"
        );
    }

    /// AC2.6 — `watch` subscribes without creating a Loro doc.
    ///
    /// After `fm.watch(path)`:
    /// - `open_paths()` does NOT contain the path.
    /// - `watch_only_paths()` contains the path.
    /// - An external edit fires a `FileEdit { kind: Watch }` reminder.
    #[tokio::test]
    async fn watch_does_not_create_loro() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("watched.txt");
        std::fs::write(&file, "initial").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        fm.watch(&file).unwrap();

        let canonical = canonicalize_best(&file);

        // Must NOT be in open_files (no Loro doc).
        assert!(
            !fm.open_files.contains_key(&canonical),
            "watch must not create a Loro doc"
        );
        // Must BE in watch_only_paths.
        let watched = fm.watch_only_paths();
        assert!(
            watched.contains(&canonical),
            "watch must register in watch_only_paths, got: {watched:?}"
        );

        // Give watcher time to register.
        std::thread::sleep(Duration::from_millis(100));

        // External edit → Watch reminder.
        std::fs::write(&file, "watched external change").unwrap();

        let got = wait_for(Duration::from_secs(5), || !queue.lock().unwrap().is_empty());
        fm.unwatch(&file).unwrap();

        assert!(got, "expected async reminder from watch subscription");
        let reminders = queue.lock().unwrap();
        assert!(
            reminders.iter().any(|a| matches!(
                a,
                MessageAttachment::FileEdit {
                    kind: FileEditKind::Watch,
                    ..
                }
            )),
            "expected FileEdit(Watch) reminder, got: {reminders:?}"
        );
    }

    // AC2.6b — covered by `pooled_watcher_shared_and_gc` above (3 files,
    // 1 DirWatcher, GC on last close).

    /// AC2.8 — `write` to a path outside the policy rules returns
    /// `FileError::PermissionDenied` with "no matching rule (default deny)".
    #[tokio::test]
    async fn write_outside_rules_denied() {
        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let project_dir = tempfile::tempdir().unwrap();
        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        // Allow only paths under project_dir.
        let fm = FileManager::new(
            allow_all_policy(project_dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        );

        // Attempt to write outside the allowed directory. We use a
        // tempdir-derived path we know doesn't match the policy.
        let outside_dir = tempfile::tempdir().unwrap();
        let forbidden = outside_dir.path().join("forbidden.txt");
        let err = fm.write(&forbidden, b"should be denied").unwrap_err();

        match &err {
            FileError::PermissionDenied { reason, .. } => {
                assert!(
                    reason.contains("no matching rule"),
                    "expected 'no matching rule (default deny)', got: {reason}"
                );
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
    }

    /// AC2.9 — `write` to a file that looks like a Pattern config KDL
    /// escalates through the permission bridge.
    ///
    /// Sub-scenario (a): broker auto-approves → write succeeds.
    /// Sub-scenario (b): broker denies → `FileError::ConfigApprovalDenied`.
    #[tokio::test]
    async fn config_write_triggers_broker() {
        use pattern_core::permission::PermissionDecisionKind;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join(".pattern.kdl");
        std::fs::write(&config_path, b"").unwrap(); // create the file first.

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        // -- Sub-scenario (a): broker auto-approves --
        {
            let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
            let mut rx = broker.subscribe();
            let broker_clone = broker.clone();

            // Responder task: automatically approve the first request.
            let responder = tokio::spawn(async move {
                if let Ok(req) = rx.recv().await {
                    broker_clone
                        .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                        .await;
                }
            });

            let bridge = Arc::new(PermissionBridge::spawn(broker));
            let fm = FileManager::new(
                allow_all_policy(dir.path()),
                Arc::clone(&queue),
                full_caps(),
                bridge,
                pattern_core::AgentId::from("test-agent"),
            );

            // Write config KDL content through the FileManager.
            // Uses spawn_blocking because request_sync blocks a thread.
            let fm_arc = Arc::new(fm);
            let config_path_clone = config_path.clone();
            let fm_clone = fm_arc.clone();
            let result = tokio::task::spawn_blocking(move || {
                fm_clone.write(
                    &config_path_clone,
                    b"capabilities {\n  effects { memory }\n}\n",
                )
            })
            .await
            .expect("blocking task");

            assert!(
                result.is_ok(),
                "broker-approved config write must succeed, got: {result:?}"
            );
            responder.await.unwrap();
        }

        // -- Sub-scenario (b): broker denies --
        {
            let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
            let mut rx = broker.subscribe();
            let broker_clone = broker.clone();

            let responder = tokio::spawn(async move {
                if let Ok(req) = rx.recv().await {
                    broker_clone
                        .resolve(&req.id, PermissionDecisionKind::Deny)
                        .await;
                }
            });

            let bridge = Arc::new(PermissionBridge::spawn(broker));
            let fm = FileManager::new(
                allow_all_policy(dir.path()),
                Arc::clone(&queue),
                full_caps(),
                bridge,
                pattern_core::AgentId::from("test-agent"),
            );

            let fm_arc = Arc::new(fm);
            let config_path_clone = config_path.clone();
            let fm_clone = fm_arc.clone();
            let err = tokio::task::spawn_blocking(move || {
                fm_clone.write(
                    &config_path_clone,
                    b"capabilities {\n  effects { memory }\n}\n",
                )
            })
            .await
            .expect("blocking task")
            .unwrap_err();

            assert!(
                matches!(err, FileError::ConfigApprovalDenied { .. }),
                "broker-denied config write must return ConfigApprovalDenied, got: {err:?}"
            );
            responder.await.unwrap();
        }
    }

    /// AC2.4 close-race regression — `close_no_emit_after_close_under_concurrent_external_writes`
    ///
    /// Opens a file, floods it with external writes from a background thread,
    /// calls `fm.close()`, and asserts that the queue does NOT grow after close
    /// returns. Verifies that the per-file cancel token + `recv_timeout` shutdown
    /// sequence prevents post-close reminders from arriving after close returns.
    ///
    /// If this test fails (post_close_count > pre_close_count), the per-file
    /// cancel mechanism is broken and reminders leak after close. That is a real
    /// bug — not a reason to relax the assertion.
    #[tokio::test]
    async fn close_no_emit_after_close_under_concurrent_external_writes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let broker = Arc::new(pattern_core::permission::PermissionBroker::new());
        let bridge = Arc::new(PermissionBridge::spawn(broker));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("race.txt");
        std::fs::write(&file, "0").unwrap();

        let queue: Arc<Mutex<Vec<MessageAttachment>>> = Arc::new(Mutex::new(Vec::new()));

        let fm = Arc::new(FileManager::new(
            allow_all_policy(dir.path()),
            Arc::clone(&queue),
            full_caps(),
            bridge,
            pattern_core::AgentId::from("test-agent"),
        ));

        fm.open(&file).unwrap();

        // Give the watcher time to register before starting the flood.
        std::thread::sleep(Duration::from_millis(100));

        // Flood the file with writes from a background thread. The stop flag
        // lets the test signal the writer before join so it terminates promptly.
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let file_clone = file.clone();
        let writer = std::thread::spawn(move || {
            let mut i: u32 = 1;
            while !stop_clone.load(Ordering::Relaxed) {
                let _ = std::fs::write(&file_clone, format!("{i}"));
                i = i.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        // Let the flood run for ~300ms so the queue starts filling and the
        // watcher pipeline is exercised under load.
        std::thread::sleep(Duration::from_millis(300));

        // Call close(). The per-file cancel token is set inside close() before
        // the listener's recv_timeout window expires. The listener exits within
        // ~50ms of the cancel signal.
        fm.close(&file).unwrap();

        // Snapshot the queue immediately after close returns.
        let pre_close_count = queue.lock().unwrap().len();

        // Give any in-flight events 300ms to arrive. If the cancel worked, the
        // listener is already dead and no new reminders will appear. If it
        // didn't work, reminders will keep arriving during this window.
        std::thread::sleep(Duration::from_millis(300));

        // Stop the writer thread and wait for it to exit.
        stop.store(true, Ordering::Relaxed);
        writer.join().expect("writer thread must not panic");

        // One more settle window after the writer stops.
        std::thread::sleep(Duration::from_millis(100));

        let post_close_count = queue.lock().unwrap().len();

        // The queue must not grow after close() returns. post_close_count ==
        // pre_close_count means the cancel worked and no reminders leaked.
        // If post_close_count > pre_close_count, the per-file cancel mechanism
        // is broken — surface as a real bug, not a timing quirk.
        assert_eq!(
            post_close_count, pre_close_count,
            "queue must not grow after close(): \
             pre_close={pre_close_count}, post_close={post_close_count}. \
             Reminders leaked after close — per-file cancel mechanism is broken."
        );
    }
}
