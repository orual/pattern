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
    pub memory_doc: Arc<LoroDoc>,
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
    memory_doc: Arc<LoroDoc>,
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
        memory_doc: Arc<LoroDoc>,
        bridge: Arc<B>,
    ) -> SyncedDocConfigBuilder<B> {
        SyncedDocConfigBuilder {
            path: path.into(),
            memory_doc,
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
            memory_doc: self.memory_doc,
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

/// Ingest events sent to the per-doc ingest thread.
enum IngestEvent {
    /// Local update bytes from `memory_doc.subscribe_local_update`.
    LocalUpdate(Vec<u8>),
    /// External filesystem event delivered from a watcher subscription.
    External(DebouncedEvent),
    /// Synchronous write request — `reply.send(result)` when done.
    SyncWrite {
        bytes: Vec<u8>,
        reply: Sender<Result<(), SyncedDocError>>,
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

/// Shared mutable state between `SyncedDoc` and the ingest thread.
struct SharedState {
    last_written_mtime: Mutex<Option<SystemTime>>,
    last_written_hash: Mutex<Option<[u8; 32]>>,
    external_subscribers: Mutex<Vec<Sender<ExternalChangeEvent>>>,
    /// Subscribers notified after every successful disk write (local or
    /// external). Used by the block subscriber worker for FTS5/reembed.
    write_subscribers: Mutex<Vec<Sender<WriteNotification>>>,
    /// The oplog version vector of `disk_doc` after the most recent
    /// successful local write (SyncWrite or LocalUpdate that resulted in a
    /// successful `atomic_write`). `None` until the first successful write.
    ///
    /// Used by `has_unsaved_edits()` to answer
    /// "does the current in-memory state differ from what is on disk?". Also
    /// read by Phase 2's `FileHandler` to implement `ConflictPolicy::RejectAndNotify`.
    last_saved_frontier: Mutex<Option<VersionVector>>,
    /// The conflict-handling policy for inbound external edits.
    conflict_policy: ConflictPolicy,
}

/// Per-file two-doc CRDT sync state.
///
/// Owns `memory_doc` (caller-supplied) + `disk_doc` (internal) + echo
/// suppression state + an ingest thread that applies both local updates and
/// external filesystem events.
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
    memory_doc: Arc<LoroDoc>,
    disk_doc: Arc<LoroDoc>,
    bridge: Arc<B>,
    shared: Arc<SharedState>,
    cancel: CancellationToken,
    /// `Option` + `Mutex` so `close()` can take and drop the sender (causing
    /// the thread's `rx.recv()` to unblock) even though `inner` is Arc-shared.
    ingest_tx: Mutex<Option<Sender<IngestEvent>>>,
    /// `Option` so `close()` can `take()` and join the thread even though the
    /// inner is Arc-shared. `Mutex` for interior mutability required by Arc.
    ingest_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The loro local-update subscription guard. The callback holds a clone of
    /// `ingest_tx`; dropping this subscription before joining the ingest thread
    /// is required so the callback's sender clone is released and the channel's
    /// send side is fully closed before the join.
    _local_update_sub: Mutex<Option<loro::Subscription>>,
    /// Keeps the standalone watcher alive (for `open_standalone`).
    _standalone_watcher: Option<DirWatcher>,
    /// Keeps the fanout subscription alive (for `open_with_subscription`).
    _fanout_guard: Option<PathFanoutSubscription>,
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

    /// Write bytes to the file via the ingest thread.
    ///
    /// Blocks until the write has been applied to disk. The return value
    /// guarantees disk has been updated before this returns.
    pub fn write(&self, bytes: &[u8]) -> Result<(), SyncedDocError> {
        let (reply_tx, reply_rx) = bounded::<Result<(), SyncedDocError>>(1);
        let guard = self.inner.ingest_tx.lock().unwrap();
        let tx = guard.as_ref().ok_or(SyncedDocError::Closed)?;
        tx.send(IngestEvent::SyncWrite {
            bytes: bytes.to_vec(),
            reply: reply_tx,
        })
        .map_err(|_| SyncedDocError::Closed)?;
        drop(guard);
        reply_rx.recv().map_err(|_| SyncedDocError::Closed)?
    }

    /// Write pre-rendered bytes to disk, bypassing the bridge.
    ///
    /// For use by the block subscriber worker, which imports Loro update bytes
    /// into `disk_doc` and renders canonical bytes itself (via
    /// `render_canonical_from_disk_doc`), then delegates only the disk-write
    /// and bookkeeping to `SyncedDoc`. This preserves the worker's 50ms
    /// debounce coalescing while moving echo suppression state
    /// (`last_written_mtime`, `last_written_hash`, `last_saved_frontier`) into
    /// `SyncedDoc`.
    ///
    /// Contrast with `write(&[u8])` which goes through the bridge's
    /// `apply_external` path (appropriate for callers that have file-content
    /// bytes but no pre-loaded disk_doc state). Use `write_rendered` when
    /// disk_doc is already up to date and `rendered_bytes` are the output of
    /// `bridge.render(&disk_doc)`.
    pub fn write_rendered(&self, rendered_bytes: &[u8]) -> Result<(), SyncedDocError> {
        let path = &self.inner.path;

        crate::fs::atomic_write(path, rendered_bytes)?;

        // Update echo-suppression state so the watcher doesn't re-apply our
        // own write as an external edit.
        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
        {
            *self.inner.shared.last_written_mtime.lock().unwrap() = Some(mtime);
        }
        let hash: [u8; 32] = *blake3::hash(rendered_bytes).as_bytes();
        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);

        // Record the frontier so Phase 2's `FileHandler` can check for
        // unsaved edits.
        *self.inner.shared.last_saved_frontier.lock().unwrap() =
            Some(self.inner.disk_doc.oplog_vv());

        // Notify write subscribers (block subscriber worker uses this for
        // FTS5 and re-embed triggers). A `Full` channel means the subscriber
        // is briefly busy — drop the event but keep the subscriber alive.
        // Only a `Disconnected` error means the receiver was dropped for real.
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

    /// Read the current content as rendered bytes.
    ///
    /// Renders from `memory_doc` — the live view that includes both agent
    /// writes and CRDT-merged external edits. `disk_doc` is the backing store
    /// for disk I/O; `memory_doc` is the source of truth for callers.
    pub fn read(&self) -> Result<Vec<u8>, SyncedDocError> {
        let (_ext, bytes) = self
            .inner
            .bridge
            .render(&self.inner.memory_doc)
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

    /// Reference to the caller-supplied memory doc.
    pub fn memory_doc(&self) -> &Arc<LoroDoc> {
        &self.inner.memory_doc
    }

    /// Reference to the internal disk doc.
    pub fn disk_doc(&self) -> &Arc<LoroDoc> {
        &self.inner.disk_doc
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

    /// Read the file from disk and compare its bytes against the bridge's
    /// render of `disk_doc`. Returns `Ok(true)` if they match (no external
    /// drift), `Ok(false)` if they differ (stale base — external writer
    /// wrote the file without seeing our last write), or an error if the
    /// read or render fails.
    ///
    /// This is a point-in-time check. The result can become stale immediately
    /// after it returns if an external writer modifies the file concurrently.
    pub fn disk_doc_matches_disk(&self) -> Result<bool, SyncedDocError> {
        let path = &self.inner.path;

        let on_disk = std::fs::read(path).map_err(|e| SyncedDocError::Io {
            path: path.clone(),
            source: e,
        })?;

        let (_ext, rendered) = self
            .inner
            .bridge
            .render(&self.inner.disk_doc)
            .map_err(SyncedDocError::Bridge)?;

        Ok(on_disk == rendered)
    }

    /// Returns `true` if `memory_doc` has edits beyond the last successful
    /// local save (i.e., the agent has pending writes not yet rendered to disk).
    ///
    /// Returns `true` also when no write has ever succeeded (the doc was just
    /// opened) and `memory_doc` is non-empty — the initial seed counts as
    /// "unsaved" because nothing has been written by the agent yet.
    pub fn has_unsaved_edits(&self) -> bool {
        has_unsaved_edits_internal(&self.inner.memory_doc, &self.inner.shared)
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

    /// Discard uncommitted memory_doc edits and replace with current disk content.
    ///
    /// Recovery path from `FileConflict` when the agent decides to take
    /// the disk version. Applies disk content directly to memory_doc via
    /// the bridge (Myers-diff to target state), then syncs disk_doc to
    /// match. After reload, `has_unsaved_edits()` returns `false`.
    ///
    /// Also updates `last_saved_frontier`, `last_written_mtime`, and
    /// `last_written_hash` to reflect the current file state.
    ///
    /// **Note on op-log retention:** the agent's pre-reload ops are not
    /// deleted from memory_doc's op log — Myers-diff produces new ops that
    /// transform the current text to disk content. The discarded edits
    /// remain in history and could potentially be resurrected via Loro's
    /// `checkout`/`travel`-style APIs in a future "undo reload" path.
    /// Op-log growth from repeated reloads will be addressed by snapshot/
    /// trim policy at session restart.
    pub fn reload(&self) -> Result<Vec<u8>, SyncedDocError> {
        let path = &self.inner.path;
        let disk_bytes = std::fs::read(path).map_err(|e| SyncedDocError::Io {
            path: path.clone(),
            source: e,
        })?;

        // Apply disk content to memory_doc. The bridge's `apply_external`
        // uses Myers-diff (`text.update_by_line`) which transforms
        // memory_doc's text to match disk_bytes, regardless of what
        // memory_doc currently contains. This effectively discards all
        // pending agent edits.
        self.inner
            .bridge
            .apply_external(&self.inner.memory_doc, &disk_bytes, path)
            .map_err(SyncedDocError::Bridge)?;
        self.inner.memory_doc.commit();

        // Capture memory_doc's version vector before exporting ops.
        let mem_vv_before_export = self.inner.disk_doc.oplog_vv();

        // Export memory_doc's new ops and import into disk_doc to keep
        // them in sync.
        let update = self
            .inner
            .memory_doc
            .export(loro::ExportMode::updates(&mem_vv_before_export))
            .map_err(|e| SyncedDocError::Watcher {
                path: path.to_owned(),
                message: format!("reload export failed: {e}"),
            })?;
        if let Err(e) = self.inner.disk_doc.import(&update) {
            tracing::debug!(path = ?path, error = %e, "failed to import reload update into disk_doc");
        }

        // Update echo-suppression and frontier state.
        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
        {
            *self.inner.shared.last_written_mtime.lock().unwrap() = Some(mtime);
        }
        let hash: [u8; 32] = *blake3::hash(&disk_bytes).as_bytes();
        *self.inner.shared.last_written_hash.lock().unwrap() = Some(hash);

        // Set last_saved_frontier to memory_doc's current vv — no unsaved edits remain.
        *self.inner.shared.last_saved_frontier.lock().unwrap() =
            Some(self.inner.memory_doc.oplog_vv());

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
    pub fn apply_external_bytes(&self, content: &[u8]) -> Result<(), SyncedDocError> {
        apply_external(
            content,
            &self.inner.path,
            &self.inner.disk_doc,
            &self.inner.memory_doc,
            &self.inner.bridge,
            &self.inner.shared,
        )
    }

    /// Cancel the ingest thread and wait for it to stop.
    ///
    /// Signals cancellation, drops the ingest sender (causing the thread's
    /// blocking `rx.recv()` to unblock with `RecvError`), then joins the thread.
    /// After this returns, all resources owned by the ingest thread have been
    /// released (AC1.5).
    pub fn close(self) {
        self.inner.cancel.cancel();
        // Drop the loro local-update subscription FIRST. Its callback holds a
        // clone of `ingest_tx`; keeping it alive would prevent the channel from
        // closing fully and cause the join below to deadlock.
        if let Ok(mut guard) = self.inner._local_update_sub.lock() {
            guard.take();
        }
        // Drop the main ingest sender so the ingest thread's `rx.recv()`
        // unblocks once the forwarder thread (50ms loop) also drops its clone.
        if let Ok(mut guard) = self.inner.ingest_tx.lock() {
            guard.take();
        }
        // Join the thread. The ingest thread exits when all senders are gone
        // (channel closed) or when the cancel token fires and the thread
        // processes one more event. The forwarder thread (which holds another
        // sender clone) exits within 50ms of cancel. After both senders drop,
        // the ingest thread unblocks from `rx.recv()` and exits cleanly.
        if let Ok(mut guard) = self.inner.ingest_thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Open modes
// ---------------------------------------------------------------------------

enum OpenMode<'a> {
    Pooled(&'a PathFanoutRouter),
    Standalone,
    /// No watcher, no internal local-update subscription. Used by the block
    /// subscriber, which drives local-update coalescing externally via
    /// `write_rendered` and routes external edits via `apply_external_bytes`.
    RouterOwned,
}

fn open_impl<B: LoroDocBridge>(
    cfg: SyncedDocConfig<B>,
    mode: OpenMode<'_>,
) -> Result<SyncedDoc<B>, SyncedDocError> {
    let path = cfg.path;

    // For watcher-backed modes, the file must exist to seed the initial state.
    // For `RouterOwned` mode, the file may not yet exist (the worker creates
    // it on the first write_rendered call). In that case, start from an
    // empty initial state.
    let (bytes, initial_mtime, initial_hash) = if path.exists() {
        let b = std::fs::read(&path).map_err(|e| SyncedDocError::Io {
            path: path.clone(),
            source: e,
        })?;
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let hash: [u8; 32] = *blake3::hash(&b).as_bytes();
        (b, mtime, Some(hash))
    } else if matches!(mode, OpenMode::RouterOwned) {
        // File does not exist yet; disk_doc + memory_doc start empty.
        // The worker will create the file on the first write_rendered call.
        (Vec::new(), None, None)
    } else {
        return Err(SyncedDocError::NotFound(path));
    };

    let memory_doc = cfg.memory_doc;
    let bridge = cfg.bridge;

    // Seed memory_doc from the initial file content (only when non-empty;
    // empty bytes on a fresh-start RouterOwned doc are a no-op seed).
    if !bytes.is_empty() {
        bridge
            .apply_external(&memory_doc, &bytes, &path)
            .map_err(SyncedDocError::Bridge)?;
        memory_doc.commit();
    }

    // Fork memory_doc to create disk_doc. `fork()` creates a new document with
    // the same oplog history as memory_doc but assigns a fresh peer ID — the
    // two docs diverge independently from this point forward. This is how the
    // existing block subscriber creates disk_doc via `doc.inner().fork()` in
    // cache.rs.
    let disk_doc = Arc::new(memory_doc.fork());

    // Initialize last_saved_frontier to the current oplog vv. At open time,
    // memory_doc and disk_doc are in sync (both seeded from disk content).
    // Setting the frontier means `has_unsaved_edits()` returns `false` for
    // a freshly opened file with no agent edits — so external writes apply
    // cleanly under RejectAndNotify instead of being treated as conflicts.
    let initial_frontier = if bytes.is_empty() {
        None
    } else {
        Some(memory_doc.oplog_vv())
    };

    let shared = Arc::new(SharedState {
        last_written_mtime: Mutex::new(initial_mtime),
        last_written_hash: Mutex::new(initial_hash),
        external_subscribers: Mutex::new(Vec::new()),
        write_subscribers: Mutex::new(Vec::new()),
        last_saved_frontier: Mutex::new(initial_frontier),
        conflict_policy: cfg.conflict_policy,
    });

    let (ingest_tx, ingest_rx) = bounded::<IngestEvent>(cfg.event_channel_bound);
    let cancel = CancellationToken::new();

    // Wire watcher → ingest thread.
    let (fanout_guard, standalone_watcher) = wire_watcher(
        &path,
        &mode,
        cfg.event_channel_bound,
        ingest_tx.clone(),
        cancel.clone(),
    )?;

    // Subscribe to local updates — only for modes that own the local-update
    // path. `RouterOwned` skips this: the caller's worker drives local-update
    // coalescing externally and calls `write_rendered` directly.
    let local_update_sub = if matches!(mode, OpenMode::RouterOwned) {
        // No-op callback that keeps the subscription alive as a guard.
        // `RouterOwned` mode does not use the local-update path — the caller's
        // worker drives local-update coalescing externally — but we need a
        // `Subscription` value to store in the struct. Returning `true` (keep
        // alive) is required by loro 1.10's API contract: `false` causes
        // auto-unsubscribe after the first call.
        memory_doc.subscribe_local_update(Box::new(|_| true))
    } else {
        let ingest_tx_local = ingest_tx.clone();
        memory_doc.subscribe_local_update(Box::new(move |bytes: &Vec<u8>| {
            let _ = ingest_tx_local.try_send(IngestEvent::LocalUpdate(bytes.clone()));
            // Return `true` to keep the subscription alive per loro 1.10's
            // API contract. Returning `false` causes auto-unsubscribe after
            // the first callback, which would silently drop all subsequent
            // local-update events (bug C1).
            true
        }))
    };

    // Spawn the ingest thread.
    let path_thread = path.clone();
    let disk_doc_thread = Arc::clone(&disk_doc);
    let memory_doc_thread = Arc::clone(&memory_doc);
    let bridge_thread = Arc::clone(&bridge);
    let shared_thread = Arc::clone(&shared);
    let cancel_thread = cancel.clone();

    let ingest_thread = std::thread::Builder::new()
        .name(format!(
            "synced-doc:{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
        ))
        .spawn(move || {
            run_ingest_thread(
                ingest_rx,
                cancel_thread,
                path_thread,
                disk_doc_thread,
                memory_doc_thread,
                bridge_thread,
                shared_thread,
            );
        })
        .map_err(|e| SyncedDocError::Io {
            path: path.clone(),
            source: e,
        })?;

    let inner = SyncedDocInner {
        path,
        memory_doc,
        disk_doc,
        bridge,
        shared,
        cancel,
        ingest_tx: Mutex::new(Some(ingest_tx)),
        ingest_thread: Mutex::new(Some(ingest_thread)),
        _local_update_sub: Mutex::new(Some(local_update_sub)),
        _standalone_watcher: standalone_watcher,
        _fanout_guard: fanout_guard,
    };

    Ok(SyncedDoc {
        inner: Arc::new(inner),
    })
}

/// Wire the watcher subscription for pool or standalone mode.
///
/// Returns `(fanout_guard, standalone_watcher)`.
///
/// For `Pooled` and `Standalone` modes, exactly one of the two is `Some`.
/// For `RouterOwned` mode, both are `None` — no filesystem subscription.
fn wire_watcher(
    path: &Path,
    mode: &OpenMode<'_>,
    channel_bound: usize,
    ingest_tx: Sender<IngestEvent>,
    cancel: CancellationToken,
) -> Result<(Option<PathFanoutSubscription>, Option<DirWatcher>), SyncedDocError> {
    match mode {
        OpenMode::RouterOwned => {
            // External edits arrive via `apply_external_bytes`; no watcher needed.
            Ok((None, None))
        }
        OpenMode::Pooled(router) => {
            let (ext_tx, ext_rx) = bounded::<DebouncedEvent>(channel_bound);
            let guard = router.subscribe(path.to_path_buf(), ext_tx);

            let cancel2 = cancel.clone();
            let path2 = path.to_path_buf();
            std::thread::Builder::new()
                .name("synced-doc-ext-fwd".into())
                .spawn(move || {
                    // Use recv_timeout so the thread checks cancellation
                    // periodically and exits promptly on close().
                    loop {
                        if cancel2.is_cancelled() {
                            break;
                        }
                        match ext_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok(ev) => {
                                let _ = ingest_tx.try_send(IngestEvent::External(ev));
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                // No event; loop back to check cancellation.
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                break;
                            }
                        }
                    }
                })
                .map_err(|e| SyncedDocError::Io {
                    path: path2,
                    source: e,
                })?;

            Ok((Some(guard), None))
        }
        OpenMode::Standalone => {
            let parent = path
                .parent()
                .ok_or_else(|| SyncedDocError::Watcher {
                    path: path.to_path_buf(),
                    message: "file has no parent directory".into(),
                })?
                .to_path_buf();

            let standalone_router = PathFanoutRouter::new();
            let (ext_tx, ext_rx) = bounded::<DebouncedEvent>(channel_bound);
            // Keep the guard alive via the forwarder thread closure.
            let guard = standalone_router.subscribe(path.to_path_buf(), ext_tx);

            let watcher_cfg = DirWatcherConfig {
                root: parent,
                recursive: notify::RecursiveMode::NonRecursive,
                debounce: std::time::Duration::from_millis(200),
            };
            let watcher = DirWatcher::start(watcher_cfg, standalone_router).map_err(|e| {
                SyncedDocError::Watcher {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                }
            })?;

            let cancel2 = cancel.clone();
            let path2 = path.to_path_buf();
            std::thread::Builder::new()
                .name("synced-doc-ext-fwd".into())
                .spawn(move || {
                    // Keep guard alive so the subscription persists until this
                    // thread exits. Use recv_timeout so the thread checks
                    // cancellation periodically and exits promptly on close().
                    let _guard = guard;
                    loop {
                        if cancel2.is_cancelled() {
                            break;
                        }
                        match ext_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok(ev) => {
                                let _ = ingest_tx.try_send(IngestEvent::External(ev));
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                // No event; loop back to check cancellation.
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                break;
                            }
                        }
                    }
                })
                .map_err(|e| SyncedDocError::Io {
                    path: path2,
                    source: e,
                })?;

            Ok((None, Some(watcher)))
        }
    }
}

// ---------------------------------------------------------------------------
// Ingest thread
// ---------------------------------------------------------------------------

fn run_ingest_thread<B: LoroDocBridge>(
    rx: crossbeam_channel::Receiver<IngestEvent>,
    cancel: CancellationToken,
    path: PathBuf,
    disk_doc: Arc<LoroDoc>,
    memory_doc: Arc<LoroDoc>,
    bridge: Arc<B>,
    shared: Arc<SharedState>,
) {
    while let Ok(event) = rx.recv() {
        if cancel.is_cancelled() {
            break;
        }
        match event {
            IngestEvent::LocalUpdate(bytes) => {
                handle_local_update(&bytes, &path, &disk_doc, &bridge, &shared);
            }
            IngestEvent::External(ev) => {
                handle_external_event(ev, &path, &disk_doc, &memory_doc, &bridge, &shared);
            }
            IngestEvent::SyncWrite { bytes, reply } => {
                // Apply the write to disk_doc via bridge, then write to disk.
                // Update memory_doc via local-update export/import.
                let result =
                    handle_sync_write(&bytes, &path, &disk_doc, &memory_doc, &bridge, &shared);
                let _ = reply.send(result);
            }
        }
    }
}

/// Handle a local update (memory_doc → disk_doc → disk file).
fn handle_local_update<B: LoroDocBridge>(
    bytes: &[u8],
    path: &Path,
    disk_doc: &Arc<LoroDoc>,
    bridge: &Arc<B>,
    shared: &Arc<SharedState>,
) {
    eprintln!("writing local to: {}", path.display());
    // Import the update into the disk doc.
    if let Err(e) = disk_doc.import(bytes) {
        tracing::debug!(path = ?path, error = %e, "failed to import local update into disk_doc");
        return;
    }

    write_disk_doc_to_file(path, disk_doc, bridge, shared);
}

/// Handle a sync write request (direct bytes → disk_doc → disk file → memory_doc).
fn handle_sync_write<B: LoroDocBridge>(
    bytes: &[u8],
    path: &Path,
    disk_doc: &Arc<LoroDoc>,
    memory_doc: &Arc<LoroDoc>,
    bridge: &Arc<B>,
    shared: &Arc<SharedState>,
) -> Result<(), SyncedDocError> {
    // Capture the version vector BEFORE applying the external bytes so that
    // the subsequent export only contains the new ops introduced by this write.
    let oplog_vv_before = disk_doc.oplog_vv();

    // Apply the bytes to disk_doc via the bridge.
    bridge
        .apply_external(disk_doc, bytes, path)
        .map_err(SyncedDocError::Bridge)?;
    disk_doc.commit();
    eprintln!(
        "applied external bytes to disk_doc: {}",
        String::from_utf8_lossy(bytes)
    );

    // Export only the new ops and merge into memory_doc.
    let update = disk_doc
        .export(loro::ExportMode::updates(&oplog_vv_before))
        .map_err(|e| SyncedDocError::Watcher {
            path: path.to_owned(),
            message: format!("export failed: {e}"),
        })?;
    if let Err(e) = memory_doc.import(&update) {
        tracing::debug!(path = ?path, error = %e, "failed to import sync-write update into memory_doc");
    }

    write_disk_doc_to_file(path, disk_doc, bridge, shared);
    Ok(())
}

/// Render disk_doc and atomically write to `path`. Update echo suppression state
/// and `last_saved_frontier`.
fn write_disk_doc_to_file<B: LoroDocBridge>(
    path: &Path,
    disk_doc: &Arc<LoroDoc>,
    bridge: &Arc<B>,
    shared: &Arc<SharedState>,
) {
    let (_ext, rendered) = match bridge.render(disk_doc) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(path = ?path, error = %e, "bridge render failed; skipping disk write");
            return;
        }
    };

    if let Err(e) = crate::fs::atomic_write(path, &rendered) {
        tracing::warn!(path = ?path, error = %e, "atomic_write failed");
        return;
    }

    // Record mtime and hash for self-echo suppression.
    if let Ok(meta) = std::fs::metadata(path)
        && let Ok(mtime) = meta.modified()
    {
        *shared.last_written_mtime.lock().unwrap() = Some(mtime);
    }
    let hash: [u8; 32] = *blake3::hash(&rendered).as_bytes();
    *shared.last_written_hash.lock().unwrap() = Some(hash);

    // Record the frontier so `has_unsaved_edits()` and Phase 2's conflict
    // detection can compare against the last known-good disk state.
    *shared.last_saved_frontier.lock().unwrap() = Some(disk_doc.oplog_vv());

    // Notify write subscribers (block subscriber worker uses this for
    // FTS5/reembed triggers). A `Full` channel means the subscriber is briefly
    // busy — drop the event but keep the subscriber alive. Only a
    // `Disconnected` error means the receiver was dropped for real.
    let notification = WriteNotification { content_hash: hash };
    let mut subs = shared.write_subscribers.lock().unwrap();
    subs.retain(|tx| {
        !matches!(
            tx.try_send(notification.clone()),
            Err(crossbeam_channel::TrySendError::Disconnected(_))
        )
    });
}

/// Handle an external filesystem event. Applies conflict-policy gating.
fn handle_external_event<B: LoroDocBridge>(
    ev: DebouncedEvent,
    path: &Path,
    disk_doc: &Arc<LoroDoc>,
    memory_doc: &Arc<LoroDoc>,
    bridge: &Arc<B>,
    shared: &Arc<SharedState>,
) {
    use notify::EventKind;

    // Only process create/modify.
    match ev.event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {}
        _ => return,
    }

    // Only process if this event involves our file.
    if !ev.event.paths.contains(&path.to_path_buf()) {
        return;
    }

    // mtime echo check.
    if let Ok(meta) = std::fs::metadata(path)
        && let Ok(file_mtime) = meta.modified()
    {
        let last = shared.last_written_mtime.lock().unwrap();
        if Some(file_mtime) == *last {
            return; // Self-echo: we wrote this.
        }
    }

    // Read the file.
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(path = ?path, error = %e, "failed to read changed file");
            return;
        }
    };

    // Content hash echo check.
    let hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
    {
        let last = shared.last_written_hash.lock().unwrap();
        if Some(hash) == *last {
            return; // Same bytes we wrote (handles `touch`).
        }
    }

    // Conflict-policy gating.
    match shared.conflict_policy {
        ConflictPolicy::AutoMerge => {
            // Always apply, regardless of whether the external content is
            // based on a stale view of the file.
            if let Err(e) = apply_external(&bytes, path, disk_doc, memory_doc, bridge, shared) {
                tracing::warn!(path = ?path, error = %e, "apply_external failed in AutoMerge path");
            }
        }
        ConflictPolicy::RejectAndNotify => {
            // Only emit ConflictDetected if the agent has uncommitted
            // memory_doc edits beyond last_saved_frontier. If memory_doc is
            // in sync with disk (no pending edits), the external write is a
            // clean external sync — apply normally and emit Applied.
            if has_unsaved_edits_internal(memory_doc, shared) {
                // Agent has pending edits. External write conflicts.
                // Do NOT apply. Emit ConflictDetected so the caller can decide.
                let frontier = shared.last_saved_frontier.lock().unwrap().clone();
                let ev = ExternalChangeEvent::ConflictDetected {
                    path: path.to_owned(),
                    disk_content: Arc::new(bytes),
                    last_saved_frontier: frontier,
                };
                let mut subs = shared.external_subscribers.lock().unwrap();
                // A `Full` channel means the subscriber is briefly busy — drop
                // the event but keep the subscriber alive. Only `Disconnected`
                // means the receiver was dropped for real.
                subs.retain(|tx| {
                    !matches!(
                        tx.try_send(ev.clone()),
                        Err(crossbeam_channel::TrySendError::Disconnected(_))
                    )
                });
            } else {
                // No pending agent edits. Clean external edit — apply normally.
                if let Err(e) = apply_external(&bytes, path, disk_doc, memory_doc, bridge, shared) {
                    tracing::warn!(
                        path = ?path, error = %e,
                        "apply_external failed in RejectAndNotify clean-edit path"
                    );
                }
            }
        }
    }
}

/// Free helper: returns `true` iff `memory_doc.oplog_vv()` is strictly ahead
/// of `last_saved_frontier`. Reusable by the ingest thread without going
/// through `SyncedDoc::has_unsaved_edits(&self)`.
fn has_unsaved_edits_internal(memory_doc: &LoroDoc, shared: &SharedState) -> bool {
    let frontier = shared.last_saved_frontier.lock().unwrap().clone();
    match frontier {
        None => {
            // No local write has ever succeeded. Treat as unsaved.
            true
        }
        Some(saved_vv) => {
            let current_vv = memory_doc.oplog_vv();
            current_vv != saved_vv
        }
    }
}

/// Core external-edit application logic. Shared by the ingest thread's
/// `AutoMerge` path and `RejectAndNotify`'s clean-edit path, as well as
/// `SyncedDoc::apply_external_bytes`.
///
/// Returns `Err` if the bridge fails or the disk_doc export fails. These are
/// hard failures: the caller should log and propagate rather than silently
/// swallowing them. `memory_doc` import failure is logged at debug level and
/// treated as non-fatal (the CRDT merge is best-effort; the disk write
/// succeeded and the subscriber can reconcile on the next cycle).
fn apply_external<B: LoroDocBridge>(
    content: &[u8],
    path: &Path,
    disk_doc: &Arc<LoroDoc>,
    memory_doc: &Arc<LoroDoc>,
    bridge: &Arc<B>,
    shared: &Arc<SharedState>,
) -> Result<(), SyncedDocError> {
    let oplog_vv_before = disk_doc.oplog_vv();

    bridge
        .apply_external(disk_doc, content, path)
        .map_err(SyncedDocError::Bridge)?;
    disk_doc.commit();

    let update = disk_doc
        .export(loro::ExportMode::updates(&oplog_vv_before))
        .map_err(|e| SyncedDocError::Watcher {
            path: path.to_owned(),
            message: format!("disk_doc export failed: {e}"),
        })?;

    if let Err(e) = memory_doc.import(&update) {
        tracing::debug!(path = ?path, error = %e, "failed to import external update into memory_doc");
    }

    // Advance `last_saved_frontier` to disk_doc's new oplog version vector.
    // Frontier means "we are synced with disk through this version" — both
    // local writes and external apply-bytes leave disk_doc in sync with the
    // file on disk, so both should advance the frontier. This is what
    // `has_unsaved_edits()` compares against to detect agent-side pending
    // edits in memory_doc that haven't reached disk. Echo-suppression state
    // (`last_written_mtime`/`last_written_hash`) is intentionally NOT updated
    // here — those track *our own* writes for self-echo detection; touching
    // them on external apply would suppress legitimate subsequent external
    // edits that race within the debounce window.
    *shared.last_saved_frontier.lock().unwrap() = Some(disk_doc.oplog_vv());

    // Fan out to external subscribers. A `Full` channel means the subscriber
    // is briefly busy — drop the event but keep the subscriber alive. Only
    // `Disconnected` means the receiver was dropped for real.
    let ev = ExternalChangeEvent::Applied {
        path: path.to_owned(),
    };
    let mut subs = shared.external_subscribers.lock().unwrap();
    subs.retain(|tx| {
        !matches!(
            tx.try_send(ev.clone()),
            Err(crossbeam_channel::TrySendError::Disconnected(_))
        )
    });

    Ok(())
}
