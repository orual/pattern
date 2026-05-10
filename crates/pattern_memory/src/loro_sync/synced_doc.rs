//! `SyncedDoc<B>` — two-doc CRDT sync with injected event subscription.
//!
//! Owns the per-file machinery: memory_doc (caller-supplied) + disk_doc
//! (internal) + local-update subscription + mtime/hash echo suppression +
//! an ingest thread that handles local updates, external events, and
//! synchronous write requests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, bounded};
use loro::{LoroDoc, VersionVector};
use notify_debouncer_full::DebouncedEvent;
use tokio_util::sync::CancellationToken;

use crate::loro_sync::routers::PathFanoutSubscription;
use crate::loro_sync::{
    DirWatcher, DirWatcherConfig, LoroDocBridge, PathFanoutRouter, SyncedDocError,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How the `SyncedDoc` ingest thread handles an external filesystem change
/// that may be based on a stale view of the file (i.e., the external writer
/// did not see the agent's prior write).
///
/// The default is `RejectAndNotify`: the safe option that surfaces conflicts
/// rather than silently merging them. Callers that want silent automerge must
/// opt in explicitly by passing `ConflictPolicy::AutoMerge`.
///
/// Exception — the block-subscriber path (`open_router_owned`) explicitly
/// passes `AutoMerge` because its external edits arrive via
/// `apply_external_bytes`, which bypasses the watcher-based conflict check
/// entirely. The policy field is still set explicitly so future readers can
/// see the intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Apply external edits via the bridge's `apply_external` regardless of
    /// whether `has_unsaved_edits()` returns true. The block subscriber path uses this
    /// because block-level edits are always delta-based (coming from the
    /// subscriber loop or `apply_external_bytes`, not arbitrary external editors).
    ///
    /// Callers must opt in explicitly; the default is `RejectAndNotify`.
    AutoMerge,
    /// Before applying an external edit, check whether the agent has
    /// uncommitted edits in `memory_doc` beyond `last_saved_frontier`. If
    /// so, emit `ExternalChangeEvent::ConflictDetected` and do NOT apply.
    /// If memory_doc is in sync (no pending edits), apply as in `AutoMerge`.
    ///
    /// This is the default. Phase 2's `FileHandler` relies on this behaviour to
    /// surface conflicts to the user instead of silently applying a Myers-diff
    /// that may discard the agent's prior edits.
    #[default]
    RejectAndNotify,
}

/// Configuration for opening a `SyncedDoc`.
///
/// Prefer the fluent builder API for construction:
///
/// ```ignore
/// let config = SyncedDocConfig::new(path, memory_doc, bridge)
///     .event_channel_bound(256)
///     .conflict_policy(ConflictPolicy::RejectAndNotify)
///     .build();
/// ```
///
/// Struct-literal construction still works for callers that need all fields.
pub struct SyncedDocConfig<B: LoroDocBridge> {
    /// Path to the file on disk.
    pub path: PathBuf,
    /// The caller-supplied memory doc (lives in MemoryCache or equivalent).
    pub doc: LoroDoc,
    /// Schema/format adapter.
    pub bridge: Arc<B>,
    /// Bound on the internal ingest event channel.
    pub event_channel_bound: usize,
    /// How to handle external edits that may be based on a stale file view.
    pub conflict_policy: ConflictPolicy,
}

/// Fluent builder for `SyncedDocConfig`.
///
/// Call `SyncedDocConfig::new(path, memory_doc, bridge)` to start, chain
/// optional setters, then call `.build()` to get the config. Omitted fields
/// take their defaults: `event_channel_bound = 256`, `conflict_policy =
/// ConflictPolicy::default()`.
pub struct SyncedDocConfigBuilder<B: LoroDocBridge> {
    path: PathBuf,
    doc: LoroDoc,
    bridge: Arc<B>,
    event_channel_bound: usize,
    conflict_policy: ConflictPolicy,
}

impl<B: LoroDocBridge> SyncedDocConfig<B> {
    /// Start building a `SyncedDocConfig` with required fields.
    ///
    /// Optional fields default to: `event_channel_bound = 256`,
    /// `conflict_policy = ConflictPolicy::default()`.
    #[allow(clippy::new_ret_no_self)] // Intentional builder: returns SyncedDocConfigBuilder<B>.
    pub fn new(
        path: impl Into<PathBuf>,
        doc: LoroDoc,
        bridge: Arc<B>,
    ) -> SyncedDocConfigBuilder<B> {
        SyncedDocConfigBuilder {
            path: path.into(),
            doc,
            bridge,
            event_channel_bound: 256,
            conflict_policy: ConflictPolicy::default(),
        }
    }
}

impl<B: LoroDocBridge> SyncedDocConfigBuilder<B> {
    /// Override the ingest event channel bound (default: 256).
    pub fn event_channel_bound(mut self, bound: usize) -> Self {
        self.event_channel_bound = bound;
        self
    }

    /// Override the conflict policy (default: `ConflictPolicy::default()`).
    pub fn conflict_policy(mut self, policy: ConflictPolicy) -> Self {
        self.conflict_policy = policy;
        self
    }

    /// Consume the builder and produce a `SyncedDocConfig`.
    pub fn build(self) -> SyncedDocConfig<B> {
        SyncedDocConfig {
            path: self.path,
            doc: self.doc,
            bridge: self.bridge,
            event_channel_bound: self.event_channel_bound,
            conflict_policy: self.conflict_policy,
        }
    }
}

/// An event emitted when the `SyncedDoc` ingest thread processes an external
/// filesystem change.
///
/// Subscribe via `SyncedDoc::subscribe_external_changes`. Events are fanned
/// out to all live subscribers via bounded channels; slow subscribers may
/// lose events under high load (`try_send` is used — never blocks).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ExternalChangeEvent {
    /// External edit successfully applied to `disk_doc` and merged into
    /// `memory_doc` via the Loro CRDT. This is the normal path for
    /// `ConflictPolicy::AutoMerge` and for clean external edits under
    /// `ConflictPolicy::RejectAndNotify`.
    Applied {
        /// The watched file that changed.
        path: PathBuf,
    },
    /// Stale-base detected under `ConflictPolicy::RejectAndNotify`. The
    /// external writer's content did not match the bridge's render of
    /// `disk_doc`, indicating the writer was unaware of the agent's prior
    /// write. The ingest thread did NOT apply the change.
    ///
    /// The caller must decide what to do:
    /// - Force-apply via `SyncedDoc::apply_external_bytes` (caller accepts
    ///   the external content as authoritative).
    /// - Reload `memory_doc` from disk (discard agent edits).
    /// - Surface to the user for manual resolution.
    ConflictDetected {
        /// The watched file that changed.
        path: PathBuf,
        /// The raw bytes that were on disk at the time the conflict was
        /// detected (i.e., the external writer's content).
        ///
        /// `Arc<Vec<u8>>` so that fanning out to multiple subscribers does not
        /// require cloning the full byte buffer for each recipient.
        disk_content: Arc<Vec<u8>>,
        /// The `last_saved_frontier` at the time of detection. `None` if no
        /// local write has yet succeeded. Useful for callers that want to
        /// compute what the agent has written since the last save.
        last_saved_frontier: Option<VersionVector>,
    },
}

/// Notification emitted after any disk write (local update, sync write, or
/// external edit application). Subscribers receive the blake3 hash of the
/// rendered bytes that were written. Used by the block subscriber worker to
/// trigger FTS5 updates and re-embedding without owning the render/write
/// machinery itself.
#[derive(Clone, Debug)]
pub struct WriteNotification {
    /// Blake3 hash of the rendered bytes written to disk.
    pub content_hash: [u8; 32],
}

/// Shared mutable state on `SyncedDoc`.
struct SharedState {
    last_written_mtime: Mutex<Option<SystemTime>>,
    last_written_hash: Mutex<Option<[u8; 32]>>,
    external_subscribers: Mutex<Vec<Sender<ExternalChangeEvent>>>,
    /// Subscribers notified after every successful disk write (local or
    /// external). Used by the block subscriber worker for FTS5/reembed.
    write_subscribers: Mutex<Vec<Sender<WriteNotification>>>,
    /// The oplog version vector of `doc` after the most recent successful
    /// local write. `None` until the first successful write.
    ///
    /// Used by `has_unsaved_edits()` to answer
    /// "does the current in-memory state differ from what is on disk?". Also
    /// read by Phase 2's `FileHandler` to implement `ConflictPolicy::RejectAndNotify`.
    last_saved_frontier: Mutex<Option<VersionVector>>,
    /// The conflict-handling policy for inbound external edits.
    conflict_policy: ConflictPolicy,
    /// Held across rebase-import + render + atomic_write to serialize
    /// concurrent local writes and external edits against the single doc.
    write_lock: Mutex<()>,
    /// Set when apply_external_bytes detects a conflict under
    /// `RejectAndNotify` policy. Blocks `write_local` until the agent
    /// resolves via `reload()` or `force_apply_external()`. Without this,
    /// the agent's next op would write_local → overwrite the external
    /// content on disk, silently losing it.
    conflict_pending: Mutex<bool>,
}

/// Per-file single-doc CRDT sync state.
///
/// Owns the single `LoroDoc`, echo-suppression state, and a watcher
/// subscription that feeds external file changes into the doc via
/// `apply_external_bytes`.
pub struct SyncedDoc<B: LoroDocBridge> {
    inner: Arc<SyncedDocInner<B>>,
}

impl<B: LoroDocBridge> std::fmt::Debug for SyncedDoc<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncedDoc")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

struct SyncedDocInner<B: LoroDocBridge> {
    path: PathBuf,
    /// THE doc. Source of truth. Both agent ops and CRDT-merged external
    /// edits land here. Loro is internally Arc'd; no second wrapper needed.
    doc: LoroDoc,
    bridge: Arc<B>,
    shared: Arc<SharedState>,
    cancel: CancellationToken,
    /// Keeps the standalone watcher alive (for `open_standalone`).
    _standalone_watcher: Option<DirWatcher>,
    /// Keeps the fanout subscription alive (for `open_with_subscription`).
    _fanout_guard: Option<PathFanoutSubscription>,
    /// Watcher feeder thread handle. Reads external events from a channel
    /// and calls `apply_external_bytes`. Joined on `close()`.
    watcher_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl<B: LoroDocBridge> SyncedDoc<B> {
    /// Open against an externally-owned `DirWatcher<PathFanoutRouter>`.
    pub fn open_with_subscription(
        cfg: SyncedDocConfig<B>,
        router: &PathFanoutRouter,
    ) -> Result<Self, SyncedDocError> {
        open_impl(cfg, OpenMode::Pooled(router))
    }

    /// Open with a private per-file watcher (standalone / test usage).
    pub fn open_standalone(cfg: SyncedDocConfig<B>) -> Result<Self, SyncedDocError> {
        open_impl(cfg, OpenMode::Standalone)
    }

    /// Open without any filesystem watcher subscription or local-update subscription.
    ///
    /// For the block-subscriber path, where a single mount-wide
    /// `DirWatcher<BlockFanoutRouter>` routes external edits by block_id to
    /// `apply_external_bytes`, and the caller's worker drives local-update
    /// coalescing by calling `write_rendered` after a debounce window.
    ///
    /// Unlike `open_with_subscription` and `open_standalone`, this constructor
    /// does NOT call `memory_doc.subscribe_local_update` — the block subscriber
    /// worker owns the `CommitEvent` channel for local-update delivery. External
    /// edits arrive exclusively via `apply_external_bytes`.
    ///
    /// Choose between the three constructors as follows:
    ///
    /// - `open_with_subscription` — production pool usage; one
    ///   `DirWatcher<PathFanoutRouter>` per directory shared across many
    ///   `SyncedDoc`s (Phase 2 `FileHandler`).
    /// - `open_standalone` — tests and one-off usage; spawns a private
    ///   single-file watcher.
    /// - `open_router_owned` — block subscriber; no internal watcher, no
    ///   internal local-update sub. The block path uses a mount-wide
    ///   `DirWatcher<BlockFanoutRouter>` for external edits and the worker's
    ///   debounce loop for local-update coalescing.
    pub fn open_router_owned(cfg: SyncedDocConfig<B>) -> Result<Self, SyncedDocError> {
        open_impl(cfg, OpenMode::RouterOwned)
    }

    /// Read the current content as rendered bytes from `doc`.
    pub fn read(&self) -> Result<Vec<u8>, SyncedDocError> {
        let (_ext, bytes) = self
            .inner
            .bridge
            .render(&self.inner.doc)
            .map_err(SyncedDocError::Bridge)?;
        Ok(bytes)
    }

    /// Subscribe to write notifications. Each call creates a new bounded
    /// channel; notifications are fanned out to all live subscribers after
    /// every successful disk write (local, sync, or external).
    ///
    /// Used by the block subscriber worker to trigger FTS5 and re-embed
    /// processing without owning the render/write machinery.
    pub fn subscribe_writes(&self) -> Receiver<WriteNotification> {
        let (tx, rx) = bounded(64);
        self.inner.shared.write_subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Subscribe to external change events. Each call creates a new bounded
    /// channel; events are fanned out to all live subscribers.
    pub fn subscribe_external_changes(&self) -> Receiver<ExternalChangeEvent> {
        self.subscribe_external_changes_with_capacity(64)
    }

    /// Subscribe to external change events with a custom channel capacity.
    ///
    /// Useful in tests to create a capacity-1 channel that becomes `Full`
    /// after one event, exercising the C2 retain-on-Full logic without
    /// reaching inside private fields.
    pub fn subscribe_external_changes_with_capacity(
        &self,
        capacity: usize,
    ) -> Receiver<ExternalChangeEvent> {
        let (tx, rx) = bounded(capacity);
        self.inner
            .shared
            .external_subscribers
            .lock()
            .unwrap()
            .push(tx);
        rx
    }

    /// Path to the file on disk.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// The `mtime` recorded after the last successful write by this `SyncedDoc`.
    ///
    /// Used for self-echo suppression in the `BlockFanoutRouter`: if the file
    /// watcher fires with a timestamp equal to `last_written_mtime`, the event
    /// was caused by the agent's own write and should not be re-applied.
    pub fn last_written_mtime(&self) -> Option<SystemTime> {
        *self.inner.shared.last_written_mtime.lock().unwrap()
    }

    /// Reference to THE doc.
    pub fn doc(&self) -> &LoroDoc {
        &self.inner.doc
    }

    /// Return the oplog version vector of `disk_doc` after the last successful
    /// local write. Returns `None` if no local write has succeeded yet (i.e.,
    /// the doc was just opened and has never been written by the agent).
    pub fn last_saved_frontier(&self) -> Option<VersionVector> {
        self.inner
            .shared
            .last_saved_frontier
            .lock()
            .unwrap()
            .clone()
    }

    /// Returns `true` if `memory_doc` has edits beyond the last successful
    /// local save (i.e., the agent has pending writes not yet rendered to disk).
    ///
    /// Returns `true` also when no write has ever succeeded (the doc was just
    /// opened) and `memory_doc` is non-empty — the initial seed counts as
    /// "unsaved" because nothing has been written by the agent yet.
    pub fn has_unsaved_edits(&self) -> bool {
        let cur = self.inner.doc.oplog_vv();
        match &*self.inner.shared.last_saved_frontier.lock().unwrap() {
            Some(saved) => cur != *saved,
            None => !cur.is_empty(),
        }
    }

    /// Force `has_unsaved_edits()` to return `true` by clearing the saved
    /// frontier. Used in tests to deterministically set up the conflict path
    /// (external edit arrives after this call → `ConflictDetected` fires)
    /// without relying on timing between the local-update ingest thread and
    /// the watcher debounce window.
    ///
    /// Available under `#[cfg(test)]` (unit tests) and when the `test-support`
    /// feature is enabled (integration tests in `tests/`).
    /// Never call this in production code.
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_saved_frontier_for_test(&self) {
        *self.inner.shared.last_saved_frontier.lock().unwrap() = None;
    }

    /// Discard uncommitted edits and replace with current disk content.
    ///
    /// Recovery path from `FileConflict` when the agent decides to take
    /// the disk version. The bridge's `apply_external` uses Myers-diff to
    /// transform `doc`'s text to match disk content, effectively discarding
    /// pending agent ops as new ops on top.
    pub fn reload(&self) -> Result<Vec<u8>, SyncedDocError> {
        let _g = self.inner.shared.write_lock.lock().unwrap();
        // Reload resolves any pending conflict by taking the disk version.
        *self.inner.shared.conflict_pending.lock().unwrap() = false;
        let path = &self.inner.path;
        let disk_bytes = std::fs::read(path).map_err(|e| SyncedDocError::Io {
            path: path.clone(),
            source: e,
        })?;

        self.inner
            .bridge
            .apply_external(&self.inner.doc, &disk_bytes, path)
            .map_err(SyncedDocError::Bridge)?;
        self.inner.doc.commit();

        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
        {
            *self.inner.shared.last_written_mtime.lock().unwrap() = Some(mtime);
        }
        let hash: [u8; 32] = *blake3::hash(&disk_bytes).as_bytes();
        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);
        *self.inner.shared.last_saved_frontier.lock().unwrap() =
            Some(self.inner.doc.oplog_vv());

        Ok(disk_bytes)
    }

    /// Apply external bytes directly, bypassing the watcher subscription.
    ///
    /// Used by `BlockFanoutRouter` (Task 7) where a single mount-wide watcher
    /// routes events to the appropriate `SyncedDoc` by block_id. This always
    /// applies the bytes regardless of `ConflictPolicy` — callers using this
    /// method are asserting they have already validated the content and want
    /// to apply it unconditionally (the `BlockFanoutRouter` owns path→block_id
    /// resolution and has already decided to apply the edit).
    ///
    /// # Skill block enforcement contract
    ///
    /// When the bridge is a `BlockSchemaBridge` with a `Skill` schema, this
    /// method passes `content` directly into the bridge's `apply_external`,
    /// which writes the `metadata.trust_tier` from the file as-is. It cannot
    /// enforce provenance-based trust because it lacks `mount_path` and
    /// `first_party_skills_dir`.
    ///
    /// **Do not call this method directly for Skill blocks.** Always route
    /// through `MemoryCache::apply_external_edit`, which enforces the trust
    /// tier from provenance and re-emits corrected bytes before calling this
    /// method. See `crate::subscriber::bridge::apply_block_external_edit` for
    /// the full enforcement contract.
    /// Force-apply external content as the authoritative state. Clears
    /// conflict_pending. Used by the conflict-resolution path
    /// (`FileManager::force_write`) when the agent has decided to overwrite
    /// disk with their version. Bypasses echo + conflict policy checks,
    /// but still does the CRDT merge so doc reflects the new content.
    pub fn force_apply_external_bytes(&self, content: &[u8]) -> Result<(), SyncedDocError> {
        let _g = self.inner.shared.write_lock.lock().unwrap();
        *self.inner.shared.conflict_pending.lock().unwrap() = false;

        let scratch = if let Some(frontier_vv) = self.inner.shared.last_saved_frontier.lock().unwrap().clone() {
            let frontiers = self.inner.doc.vv_to_frontiers(&frontier_vv);
            self.inner.doc.fork_at(&frontiers)
        } else {
            self.inner.doc.fork()
        };
        scratch.set_detached_editing(true);
        let vv_before = scratch.oplog_vv();
        self.inner
            .bridge
            .apply_external(&scratch, content, &self.inner.path)
            .map_err(SyncedDocError::Bridge)?;
        scratch.commit();
        let updates = scratch
            .export(loro::ExportMode::updates(&vv_before))
            .map_err(|e| SyncedDocError::Watcher {
                path: self.inner.path.clone(),
                message: format!("scratch export failed: {e}"),
            })?;
        if let Err(e) = self.inner.doc.import(&updates) {
            tracing::debug!(path = ?self.inner.path, error = %e, "force_apply: import failed");
        }

        // Atomic-write the forced content to disk so disk matches what we
        // just told the doc.
        crate::fs::atomic_write(&self.inner.path, content)?;
        let hash = *blake3::hash(content).as_bytes();
        if let Ok(meta) = std::fs::metadata(&self.inner.path)
            && let Ok(mtime) = meta.modified()
        {
            *self.inner.shared.last_written_mtime.lock().unwrap() = Some(mtime);
        }
        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);
        *self.inner.shared.last_saved_frontier.lock().unwrap() = Some(self.inner.doc.oplog_vv());

        let ev = ExternalChangeEvent::Applied {
            path: self.inner.path.clone(),
        };
        let mut subs = self.inner.shared.external_subscribers.lock().unwrap();
        subs.retain(|tx| {
            !matches!(
                tx.try_send(ev.clone()),
                Err(crossbeam_channel::TrySendError::Disconnected(_))
            )
        });
        Ok(())
    }

    /// Reconcile doc with current disk content. Reads disk INSIDE the
    /// write_lock so there's no ordering race with concurrent agent writes:
    /// if the agent's write_local interleaves between event arrival and our
    /// lock acquisition, we read the agent's NEW content (not the stale pre-
    /// write state) and the hash check echo-skips.
    ///
    /// This is the path the watcher feeder thread uses. Test/forced callers
    /// continue to use `apply_external_bytes(&[u8])` directly.
    pub fn reconcile_from_disk(&self) -> Result<(), SyncedDocError> {
        let _g = self.inner.shared.write_lock.lock().unwrap();
        let path = &self.inner.path;
        let content = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(SyncedDocError::Io {
                    path: path.clone(),
                    source: e,
                });
            }
        };
        let hash = *blake3::hash(&content).as_bytes();
        if Some(hash) == *self.inner.shared.last_written_hash.lock().unwrap() {
            return Ok(());  // disk matches our last write — echo or already-reconciled
        }

        if matches!(self.inner.shared.conflict_policy, ConflictPolicy::RejectAndNotify)
            && self.has_unsaved_edits_unlocked()
        {
            *self.inner.shared.conflict_pending.lock().unwrap() = true;
            let last_saved = self.inner.shared.last_saved_frontier.lock().unwrap().clone();
            let ev = ExternalChangeEvent::ConflictDetected {
                path: path.clone(),
                disk_content: Arc::new(content),
                last_saved_frontier: last_saved,
            };
            let mut subs = self.inner.shared.external_subscribers.lock().unwrap();
            subs.retain(|tx| {
                !matches!(
                    tx.try_send(ev.clone()),
                    Err(crossbeam_channel::TrySendError::Disconnected(_))
                )
            });
            return Ok(());
        }

        self.do_external_merge(&content, hash, path)
    }

    /// Same has_unsaved_edits logic but without re-locking write_lock
    /// (caller already holds it).
    fn has_unsaved_edits_unlocked(&self) -> bool {
        let cur = self.inner.doc.oplog_vv();
        match &*self.inner.shared.last_saved_frontier.lock().unwrap() {
            Some(saved) => cur != *saved,
            None => !cur.is_empty(),
        }
    }

    /// Shared merge logic: fork_at(last_saved_frontier), apply bridge,
    /// export+import, update bookkeeping, fire Applied event.
    /// Caller must hold write_lock.
    fn do_external_merge(&self, content: &[u8], hash: [u8; 32], path: &Path) -> Result<(), SyncedDocError> {
        let scratch = if let Some(frontier_vv) = self.inner.shared.last_saved_frontier.lock().unwrap().clone() {
            let frontiers = self.inner.doc.vv_to_frontiers(&frontier_vv);
            self.inner.doc.fork_at(&frontiers)
        } else {
            self.inner.doc.fork()
        };
        scratch.set_detached_editing(true);
        let vv_before = scratch.oplog_vv();
        self.inner
            .bridge
            .apply_external(&scratch, content, path)
            .map_err(SyncedDocError::Bridge)?;
        scratch.commit();
        let updates = scratch
            .export(loro::ExportMode::updates(&vv_before))
            .map_err(|e| SyncedDocError::Watcher {
                path: path.to_owned(),
                message: format!("scratch export failed: {e}"),
            })?;
        if let Err(e) = self.inner.doc.import(&updates) {
            tracing::debug!(path = ?path, error = %e, "import scratch updates into doc failed");
        }

        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);
        *self.inner.shared.last_saved_frontier.lock().unwrap() = Some(self.inner.doc.oplog_vv());

        let ev = ExternalChangeEvent::Applied {
            path: path.to_owned(),
        };
        let mut subs = self.inner.shared.external_subscribers.lock().unwrap();
        subs.retain(|tx| {
            !matches!(
                tx.try_send(ev.clone()),
                Err(crossbeam_channel::TrySendError::Disconnected(_))
            )
        });
        Ok(())
    }

    /// Apply external bytes (e.g. from the watcher) into `doc` as a CRDT merge.
    /// Skips when content matches `last_written_hash` (echo of our own write).
    /// Under `ConflictPolicy::RejectAndNotify`, if the agent has unsaved edits,
    /// fires `ExternalChangeEvent::ConflictDetected` instead of merging.
    ///
    /// **Merge strategy:** fork doc, checkout scratch at `last_saved_frontier`
    /// (= last known disk-side state), apply the bridge to scratch (Myers-diff
    /// from old disk to new disk), export scratch's new ops, import into doc.
    /// CRDT merge preserves any agent-local ops that weren't on disk.
    pub fn apply_external_bytes(&self, content: &[u8]) -> Result<(), SyncedDocError> {
        let hash = *blake3::hash(content).as_bytes();
        if Some(hash) == *self.inner.shared.last_written_hash.lock().unwrap() {
            return Ok(());  // echo of our own write
        }

        if matches!(self.inner.shared.conflict_policy, ConflictPolicy::RejectAndNotify)
            && self.has_unsaved_edits()
        {
            // Mark conflict pending so subsequent write_local calls refuse
            // to overwrite the external content on disk. Cleared by reload()
            // (take disk) or force_apply_external (accept external as base).
            *self.inner.shared.conflict_pending.lock().unwrap() = true;
            let last_saved = self.inner.shared.last_saved_frontier.lock().unwrap().clone();
            let ev = ExternalChangeEvent::ConflictDetected {
                path: self.inner.path.clone(),
                disk_content: Arc::new(content.to_vec()),
                last_saved_frontier: last_saved,
            };
            let mut subs = self.inner.shared.external_subscribers.lock().unwrap();
            subs.retain(|tx| {
                !matches!(
                    tx.try_send(ev.clone()),
                    Err(crossbeam_channel::TrySendError::Disconnected(_))
                )
            });
            return Ok(());
        }

        let _g = self.inner.shared.write_lock.lock().unwrap();

        // Fork doc at the last-known disk-side state. fork_at gives a fresh
        // doc with history truncated to that frontier — no local ops above
        // it. Bridge's Myers-diff then computes "what changed on disk since
        // we last synced," not "what would make this doc equal to disk"
        // (which would squash agent-local ops).
        let scratch = if let Some(frontier_vv) = self.inner.shared.last_saved_frontier.lock().unwrap().clone() {
            let frontiers = self.inner.doc.vv_to_frontiers(&frontier_vv);
            self.inner.doc.fork_at(&frontiers)
        } else {
            // No prior disk state — fork from current head. The bridge
            // will produce ops that bring scratch from current state to
            // the new disk content. With no local ops, this is correct;
            // with local ops, last_saved_frontier should have been set on
            // the first write.
            self.inner.doc.fork()
        };
        // Ensure scratch is editable. fork_at can leave the doc in detached
        // state; set_detached_editing(true) makes it accept ops with a
        // distinct PeerID per checkout.
        scratch.set_detached_editing(true);

        let vv_before = scratch.oplog_vv();
        self.inner
            .bridge
            .apply_external(&scratch, content, &self.inner.path)
            .map_err(SyncedDocError::Bridge)?;
        scratch.commit();

        let updates = scratch
            .export(loro::ExportMode::updates(&vv_before))
            .map_err(|e| SyncedDocError::Watcher {
                path: self.inner.path.clone(),
                message: format!("scratch export failed: {e}"),
            })?;
        if let Err(e) = self.inner.doc.import(&updates) {
            tracing::debug!(path = ?self.inner.path, error = %e, "import scratch updates into doc failed");
        }

        // Update bookkeeping: disk now has `content` (per the external
        // editor), and scratch's new vv reflects the disk-side state.
        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);
        *self.inner.shared.last_saved_frontier.lock().unwrap() = Some(scratch.oplog_vv());

        let ev = ExternalChangeEvent::Applied {
            path: self.inner.path.clone(),
        };
        let mut subs = self.inner.shared.external_subscribers.lock().unwrap();
        subs.retain(|tx| {
            !matches!(
                tx.try_send(ev.clone()),
                Err(crossbeam_channel::TrySendError::Disconnected(_))
            )
        });
        Ok(())
    }

    /// Apply incoming bytes as content (Myers-diff via bridge) AND persist
    /// to disk synchronously. Convenience for the agent-initiated
    /// "set whole-content" path. Does NOT fire ExternalChangeEvent — these
    /// are local writes, not external edits.
    pub fn write_bytes(&self, bytes: &[u8]) -> Result<(), SyncedDocError> {
        let _g = self.inner.shared.write_lock.lock().unwrap();
        self.inner
            .bridge
            .apply_external(&self.inner.doc, bytes, &self.inner.path)
            .map_err(SyncedDocError::Bridge)?;
        self.inner.doc.commit();
        drop(_g);
        self.write_local()
    }

    /// Synchronous local write: render `doc`, rebase against any disk drift,
    /// atomic-write, update bookkeeping. Caller has already mutated `doc`.
    pub fn write_local(&self) -> Result<(), SyncedDocError> {
        let _g = self.inner.shared.write_lock.lock().unwrap();
        if *self.inner.shared.conflict_pending.lock().unwrap() {
            return Err(SyncedDocError::ConflictPending {
                path: self.inner.path.clone(),
            });
        }
        self.rebase_against_disk_if_needed()?;
        let path = &self.inner.path;
        let (_ext, bytes) = self
            .inner
            .bridge
            .render(&self.inner.doc)
            .map_err(SyncedDocError::Bridge)?;
        crate::fs::atomic_write(path, &bytes).map_err(|e| {
            tracing::error!(path = ?path, error = ?e, "write_local: atomic_write failed");
            e
        })?;

        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
        {
            *self.inner.shared.last_written_mtime.lock().unwrap() = Some(mtime);
        }
        let hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);
        *self.inner.shared.last_saved_frontier.lock().unwrap() = Some(self.inner.doc.oplog_vv());

        // Notify write subscribers (FTS5/reembed).
        let notification = WriteNotification { content_hash: hash };
        let mut subs = self.inner.shared.write_subscribers.lock().unwrap();
        subs.retain(|tx| {
            !matches!(
                tx.try_send(notification.clone()),
                Err(crossbeam_channel::TrySendError::Disconnected(_))
            )
        });
        Ok(())
    }

    /// If disk content differs from what we last wrote, import it as a CRDT
    /// update into `doc`. Loro merges; local ops are preserved by CRDT semantics.
    /// Caller must hold `write_lock`.
    fn rebase_against_disk_if_needed(&self) -> Result<(), SyncedDocError> {
        let path = &self.inner.path;
        let disk_bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(SyncedDocError::Io {
                    path: path.clone(),
                    source: e,
                });
            }
        };
        let disk_hash = *blake3::hash(&disk_bytes).as_bytes();
        if Some(disk_hash) == *self.inner.shared.last_written_hash.lock().unwrap() {
            return Ok(());
        }
        self.inner
            .bridge
            .apply_external(&self.inner.doc, &disk_bytes, path)
            .map_err(SyncedDocError::Bridge)?;
        self.inner.doc.commit();
        Ok(())
    }

    /// Cancel the watcher feeder thread. Does NOT join — the feeder is
    /// blocked on `rx.recv()` and won't unblock until its channel disconnects,
    /// which happens when the last `Arc<SyncedDocInner>` drops. Joining here
    /// would deadlock (close holds the last handle but the feeder needs that
    /// handle gone before exiting). Cancel is set so when the thread does
    /// wake, it exits cleanly. The thread is short-lived after that.
    pub fn close(self) {
        self.inner.cancel.cancel();
        // Take the watcher_thread handle so it isn't joined again on Drop.
        // Detach: when this SyncedDoc + any other refs drop, the rx
        // disconnects and the feeder thread exits naturally.
        if let Ok(mut guard) = self.inner.watcher_thread.lock() {
            let _ = guard.take();
        }
    }
}

// ---------------------------------------------------------------------------
// Open modes
// ---------------------------------------------------------------------------

enum OpenMode<'a> {
    Pooled(&'a PathFanoutRouter),
    Standalone,
    /// No watcher subscription. The caller routes external edits via
    /// `apply_external_bytes` (used by `BlockFanoutRouter`).
    RouterOwned,
}

fn open_impl<B: LoroDocBridge>(
    cfg: SyncedDocConfig<B>,
    mode: OpenMode<'_>,
) -> Result<SyncedDoc<B>, SyncedDocError> {
    let path = cfg.path;

    // For watcher-backed modes, the file must exist to seed initial state.
    // For `RouterOwned`, the file may not yet exist (caller creates it on first write).
    let (bytes, initial_mtime, initial_hash) = if path.exists() {
        let b = std::fs::read(&path).map_err(|e| SyncedDocError::Io {
            path: path.clone(),
            source: e,
        })?;
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let hash: [u8; 32] = *blake3::hash(&b).as_bytes();
        (b, mtime, Some(hash))
    } else if matches!(mode, OpenMode::RouterOwned) {
        (Vec::new(), None, None)
    } else {
        return Err(SyncedDocError::NotFound(path));
    };

    let doc = cfg.doc;
    let bridge = cfg.bridge;

    // Seed `doc` from initial file content ONLY if doc is empty.
    //
    // For LoroSyncedFile (file API): doc is `LoroDoc::new()` — empty.
    //   Seeding from disk is the cold-start "adopt the file's content" path.
    //
    // For block subscribers (lazy-spawn): doc is the cache-hydrated
    //   StructuredDocument's inner LoroDoc, already populated from
    //   DB snapshot+deltas. The disk file (from a previous daemon run) is
    //   stale relative to in-memory state. Seeding from disk would Myers-
    //   diff stale-disk over the live doc, REVERTING any not-yet-flushed
    //   ops the agent just applied. The block path expects doc to be
    //   canonical and disk to be downstream — write_local will catch
    //   disk up on first flush.
    let doc_already_populated = !doc.oplog_vv().is_empty();
    if !bytes.is_empty() && !doc_already_populated {
        bridge
            .apply_external(&doc, &bytes, &path)
            .map_err(SyncedDocError::Bridge)?;
        doc.commit();
    }

    let initial_frontier = if doc.oplog_vv().is_empty() { None } else { Some(doc.oplog_vv()) };

    let shared = Arc::new(SharedState {
        last_written_mtime: Mutex::new(initial_mtime),
        last_written_hash: Mutex::new(initial_hash),
        external_subscribers: Mutex::new(Vec::new()),
        write_subscribers: Mutex::new(Vec::new()),
        last_saved_frontier: Mutex::new(initial_frontier),
        conflict_policy: cfg.conflict_policy,
        write_lock: Mutex::new(()),
        conflict_pending: Mutex::new(false),
    });

    let cancel = CancellationToken::new();

    // Wire watcher: receive DebouncedEvent, deliver bytes to apply_external_bytes.
    let (event_rx, fanout_guard, standalone_watcher) = wire_watcher(
        &path,
        &mode,
        cfg.event_channel_bound,
        cancel.clone(),
    )?;

    // Construct the inner first; then, if we have a watcher, spawn a feeder thread
    // that owns a weak ref and calls apply_external_bytes via SyncedDoc.
    let inner = Arc::new(SyncedDocInner {
        path: path.clone(),
        doc,
        bridge,
        shared,
        cancel: cancel.clone(),
        _standalone_watcher: standalone_watcher,
        _fanout_guard: fanout_guard,
        watcher_thread: Mutex::new(None),
    });

    if let Some(rx) = event_rx {
        let weak = Arc::downgrade(&inner);
        let path_thread = path.clone();
        let cancel_thread = cancel.clone();
        let handle = std::thread::Builder::new()
            .name(format!(
                "synced-doc-watcher:{}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown")
            ))
            .spawn(move || run_watcher_feeder::<B>(rx, weak, path_thread, cancel_thread))
            .map_err(|e| SyncedDocError::Io {
                path: path.clone(),
                source: e,
            })?;
        *inner.watcher_thread.lock().unwrap() = Some(handle);
    }

    Ok(SyncedDoc { inner })
}

/// Wire the watcher subscription. Returns the receiver for raw debounced
/// events plus the appropriate guard for the open mode. `RouterOwned`
/// returns `(None, None, None)` — caller routes external edits manually.
fn wire_watcher(
    path: &Path,
    mode: &OpenMode<'_>,
    channel_bound: usize,
    _cancel: CancellationToken,
) -> Result<(Option<Receiver<DebouncedEvent>>, Option<PathFanoutSubscription>, Option<DirWatcher>), SyncedDocError> {
    match mode {
        OpenMode::RouterOwned => Ok((None, None, None)),
        OpenMode::Pooled(router) => {
            let (tx, rx) = bounded::<DebouncedEvent>(channel_bound);
            let sub = router.subscribe(path.to_owned(), tx);
            Ok((Some(rx), Some(sub), None))
        }
        OpenMode::Standalone => {
            let parent = path.parent().ok_or_else(|| SyncedDocError::Watcher {
                path: path.to_owned(),
                message: "path has no parent directory".into(),
            })?.to_owned();
            // Build a private fanout router, subscribe our path, hand the
            // router off to DirWatcher (which owns and runs it). The
            // PathFanoutSubscription holds an Arc<PathFanoutInner> so it
            // stays valid after the router handle is moved.
            let private_router = PathFanoutRouter::new();
            let (tx, rx) = bounded::<DebouncedEvent>(channel_bound);
            let _sub = private_router.subscribe(path.to_owned(), tx);
            let cfg = DirWatcherConfig::new(parent);
            let watcher = DirWatcher::start(cfg, private_router)?;
            // Both _sub (subscription guard) and watcher need to live. Box
            // _sub into the standalone slot so it tags along; we encode this
            // by returning Some(_sub) in the fanout-guard slot too.
            Ok((Some(rx), Some(_sub), Some(watcher)))
        }
    }
}

/// Feeder thread: receive watcher events, read disk, call apply_external_bytes.
fn run_watcher_feeder<B: LoroDocBridge>(
    rx: Receiver<DebouncedEvent>,
    weak: std::sync::Weak<SyncedDocInner<B>>,
    path: PathBuf,
    cancel: CancellationToken,
) {
    while let Ok(_ev) = rx.recv() {
        if cancel.is_cancelled() { break; }
        let Some(inner) = weak.upgrade() else { break; };
        // Read-under-lock via reconcile_from_disk: avoids the
        // ordering race where the feeder reads disk BEFORE the
        // agent's write_local advances state, then applies stale
        // content as if authoritative — which would compute Myers-
        // diff ops that revert the agent's write.
        let handle = SyncedDoc { inner };
        if let Err(e) = handle.reconcile_from_disk() {
            tracing::debug!(path = ?path, error = %e, "reconcile_from_disk failed in watcher feeder");
        }
    }
}
