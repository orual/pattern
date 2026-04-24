//! In-memory cache of StructuredDocument instances.
//!
//! The v3 refactor replaced the previous `ConstellationDatabases` wrapper
//! (which bundled `pattern_db` + `pattern_auth`) with direct `pattern_db::ConstellationDb`
//! access. Memory operations don't need the auth DB; consumers that require
//! both wire them separately.

use crate::db_bridge::{DbResultExt, core_search_type_to_db, db_search_result_to_core};
use crate::subscriber::SubscriberHandle;
use crate::subscriber::event::{Heartbeat, ReembedRequest};
use crate::subscriber::supervisor::{SupervisorState, run_supervisor};
use crate::types_internal::CachedBlock;
use chrono::Utc;
use dashmap::DashMap;
use pattern_core::memory::StructuredDocument;
use pattern_core::traits::EmbeddingProvider;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, BlockSchema, MemoryError,
    MemoryPermission, MemoryResult, MemorySearchResult, MemorySearchScope, SearchMode,
    SearchOptions, SharedBlockInfo, UndoRedoDepth, UndoRedoOp,
};
use pattern_db::ConstellationDb;
use pattern_db::Json;
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use pattern_core::types::memory_types::DEFAULT_MEMORY_CHAR_LIMIT;

/// In-memory cache of LoroDoc instances with lazy loading.
///
/// Each cached document may have an associated sync subscriber (OS thread) that
/// keeps the canonical file and FTS5 indexes in sync. The subscriber registry
/// tracks active workers so that [`MemoryCache::drop_doc`] can cancel and join
/// them before evicting the document from the cache.
///
/// Subscribers are lazily spawned on the first successful persist if
/// [`with_mount_path`](MemoryCache::with_mount_path) has been configured.
#[derive(Debug)]
pub struct MemoryCache {
    /// Constellation database for persistence.
    db: Arc<ConstellationDb>,

    /// Optional embedding provider for vector/hybrid search.
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,

    /// Cached blocks: block_id -> CachedBlock.
    ///
    /// Arc-wrapped so the respawn closure in `with_mount_path` can hold a
    /// reference to the live map without requiring `MemoryCache` to be
    /// Arc-shared itself.
    blocks: Arc<DashMap<String, CachedBlock>>,

    /// Per-doc sync subscriber registry: block_id -> SubscriberHandle.
    ///
    /// Wrapped in Arc so the supervisor task can hold a reference to the same
    /// map without requiring `MemoryCache` itself to be Arc-shared.
    /// Subscribers are lazily spawned on the first write to a doc and
    /// cancelled + joined on [`drop_doc`] or cache shutdown.
    subscribers: Arc<DashMap<String, SubscriberHandle>>,

    /// Default character limit for new memory blocks.
    default_char_limit: usize,

    /// Base path for canonical file output. When `Some`, subscribers are
    /// lazily spawned on the first successful persist for each block.
    /// When `None`, the subscriber machinery is disabled (backward-compat for
    /// tests and embedded usage that don't need file emission).
    mount_path: Option<Arc<PathBuf>>,

    /// Sender for re-embed requests from subscriber workers to the async
    /// re-embed queue. Must be set alongside `mount_path`.
    reembed_tx: Option<tokio::sync::mpsc::UnboundedSender<ReembedRequest>>,

    /// Sender for subscriber heartbeats to the supervisor task.
    /// Must be set alongside `mount_path`.
    heartbeat_tx: Option<crossbeam_channel::Sender<Heartbeat>>,

    /// Cancellation token for the supervisor tokio task.
    /// Cancelled when the cache is dropped.
    supervisor_cancel: CancellationToken,

    /// Shared supervisor state (heartbeat tracking).
    supervisor_state: Arc<SupervisorState>,

    /// Join handle for the supervisor tokio task, if spawned.
    supervisor_task: Option<tokio::task::JoinHandle<()>>,
}

/// Outcome of [`MemoryCache::pause_subscribers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseOutcome {
    /// Number of workers that successfully flushed and parked.
    pub paused: usize,
    /// Number of workers that did not park within the timeout.
    pub timed_out: usize,
}

impl MemoryCache {
    /// Create a new memory cache without embedding support.
    pub fn new(db: Arc<ConstellationDb>) -> Self {
        Self {
            db,
            embedding_provider: None,
            blocks: Arc::new(DashMap::new()),
            subscribers: Arc::new(DashMap::new()),
            default_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
            mount_path: None,
            reembed_tx: None,
            heartbeat_tx: None,
            supervisor_cancel: CancellationToken::new(),
            supervisor_state: Arc::new(SupervisorState::new()),
            supervisor_task: None,
        }
    }

    /// Create a new memory cache with an embedding provider for vector/hybrid search.
    pub fn with_embedding_provider(
        db: Arc<ConstellationDb>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            db,
            embedding_provider: Some(provider),
            blocks: Arc::new(DashMap::new()),
            subscribers: Arc::new(DashMap::new()),
            default_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
            mount_path: None,
            reembed_tx: None,
            heartbeat_tx: None,
            supervisor_cancel: CancellationToken::new(),
            supervisor_state: Arc::new(SupervisorState::new()),
            supervisor_task: None,
        }
    }

    /// Set a custom default character limit for new memory blocks
    pub fn with_default_char_limit(mut self, limit: usize) -> Self {
        self.default_char_limit = limit;
        self
    }

    /// Enable subscriber file emission by setting the mount path and the
    /// channels needed to communicate with the re-embed queue and supervisor.
    ///
    /// Once configured, subscribers are lazily spawned on the first successful
    /// persist for each block. Blocks with no content (freshly created) do not
    /// get a subscriber until they have been written and persisted at least
    /// once.
    ///
    /// This also spawns the supervisor tokio task if a tokio runtime is
    /// available. The supervisor watches heartbeats from subscriber workers
    /// and restarts any that become unresponsive. If no runtime is available
    /// (e.g., in pure-sync tests), the supervisor is skipped with a warning.
    ///
    /// If `mount_path` is not called, no subscribers are spawned — this is the
    /// backward-compatible default for tests and embedded usage.
    pub fn with_mount_path(
        mut self,
        path: impl Into<PathBuf>,
        reembed_tx: tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
        heartbeat_tx: crossbeam_channel::Sender<Heartbeat>,
        heartbeat_rx: crossbeam_channel::Receiver<Heartbeat>,
    ) -> Self {
        self.mount_path = Some(Arc::new(path.into()));
        self.reembed_tx = Some(reembed_tx.clone());
        self.heartbeat_tx = Some(heartbeat_tx.clone());

        // Spawn the supervisor as a tokio task if a runtime is available.
        // The supervisor needs: heartbeat_rx, subscribers map, cancel token,
        // state, and a respawn callback.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let subscribers = Arc::clone(&self.subscribers);
                let cancel = self.supervisor_cancel.clone();
                let state = self.supervisor_state.clone();

                // Capture everything the respawn closure needs to re-spawn a
                // crashed worker. We capture Arc clones so the closure can be
                // called from the supervisor task without a reference to `self`.
                let respawn_blocks = Arc::clone(&self.blocks);
                let respawn_subscribers = Arc::clone(&self.subscribers);
                let respawn_db = Arc::clone(&self.db);
                let respawn_mount_path = Arc::clone(
                    self.mount_path
                        .as_ref()
                        .expect("mount_path is set just above"),
                );
                let respawn_reembed_tx = reembed_tx;
                let respawn_heartbeat_tx = heartbeat_tx;

                let respawn_fn: Arc<dyn Fn(&str) + Send + Sync> =
                    Arc::new(move |block_id: &str| {
                        // Look up the live doc and schema from the cache. If
                        // the block has been evicted we skip the respawn — a
                        // future persist() will re-spawn when it's needed.
                        let (doc, schema) = {
                            let Some(cached) = respawn_blocks.get(block_id) else {
                                tracing::warn!(
                                    block_id = %block_id,
                                    "supervisor respawn: block not in cache, skipping"
                                );
                                return;
                            };
                            (cached.doc.clone(), cached.doc.schema().clone())
                        }; // DashMap lock released here.

                        // Guard against a race where another thread already
                        // respawned this subscriber between the supervisor's
                        // remove() and this closure running.
                        if respawn_subscribers.contains_key(block_id) {
                            tracing::debug!(
                                block_id = %block_id,
                                "supervisor respawn: subscriber already exists, skipping"
                            );
                            return;
                        }

                        spawn_subscriber_for_block(
                            block_id,
                            schema,
                            &doc,
                            respawn_reembed_tx.clone(),
                            respawn_heartbeat_tx.clone(),
                            Arc::clone(&respawn_mount_path),
                            Arc::clone(&respawn_db),
                            Arc::clone(&respawn_subscribers),
                        );
                    });

                let task = handle.spawn(run_supervisor(
                    heartbeat_rx,
                    subscribers,
                    cancel,
                    state,
                    respawn_fn,
                ));
                self.supervisor_task = Some(task);
            }
            Err(_) => {
                tracing::warn!(
                    "no tokio runtime available when configuring mount path; \
                     supervisor will not run — subscriber heartbeat timeouts will not be detected"
                );
            }
        }

        self
    }

    /// Get the default character limit
    pub fn default_char_limit(&self) -> usize {
        self.default_char_limit
    }

    /// Get or load a block owned by agent_id.
    /// Returns a cloned StructuredDocument (cheap - LoroDoc internally Arc'd).
    /// For owned blocks, the effective permission is the block's inherent permission.
    pub fn get(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        // 1. Check access FIRST (always) - DB is source of truth.
        let access_result = pattern_db::queries::check_block_access(
            &*self.db.get().mem()?,
            agent_id, // requester
            agent_id, // owner (same for owned blocks)
            label,
        )
        .mem()?;

        tracing::debug!(
            "Access Result: {:?}, agent: {}, label: {}",
            access_result,
            agent_id,
            label
        );
        let (block_id, permission) = match access_result {
            Some((id, perm)) => (id, perm),
            None => {
                return Err(MemoryError::NotFound {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                });
            } // Block doesn't exist or no access.
        };

        // 2. Check cache using block_id.
        if self.blocks.contains_key(&block_id) {
            // Extract data we need without holding the lock.
            let last_seq = {
                let entry = self.blocks.get(&block_id).unwrap();
                entry.last_seq
            };

            // Check for new updates from DB since we last synced.
            let updates =
                pattern_db::queries::get_updates_since(&*self.db.get().mem()?, &block_id, last_seq)
                    .mem()?;

            // Re-acquire mutable lock to apply updates and update permission from DB.
            {
                let mut entry = self.blocks.get_mut(&block_id).unwrap();
                if !updates.is_empty() {
                    for update in &updates {
                        entry.doc.apply_updates(&update.update_blob)?;
                    }
                    entry.last_seq = updates.last().unwrap().seq;
                }

                // DB permission overrides cached permission (in metadata).
                entry.doc.metadata_mut().permission = permission;
                entry.last_accessed = Utc::now();
            }

            // Get the document with updated permission.
            let entry = self.blocks.get(&block_id).unwrap();
            let mut doc = entry.doc.clone();
            doc.set_permission(permission);
            return Ok(Some(doc));
        }

        // 3. Load from database with effective permission.
        let block = self.load_from_db(agent_id, label, permission)?;

        match block {
            Some(cached) => {
                let doc = cached.doc.clone();
                self.blocks.insert(block_id, cached);
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    /// Load a block from database, reconstructing StructuredDocument from snapshot + deltas.
    /// The permission parameter is the effective permission for this access (already calculated).
    fn load_from_db(
        &self,
        agent_id: &str,
        label: &str,
        effective_permission: MemoryPermission,
    ) -> MemoryResult<Option<CachedBlock>> {
        // Get block from database.
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;

        let block = match block {
            Some(b) if b.is_active => b,
            _ => {
                return Err(MemoryError::NotFound {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                });
            }
        };

        // Build BlockMetadata from DB block.
        let mut metadata = db_block_to_metadata(&block);
        // Override with effective permission (may differ for shared blocks).
        metadata.permission = effective_permission;

        // Get and apply any updates since the snapshot.
        let (_checkpoint, updates) =
            pattern_db::queries::get_checkpoint_and_updates(&*self.db.get().mem()?, &block.id)
                .mem()?;

        // Create StructuredDocument from snapshot with metadata.
        let doc = if block.loro_snapshot.is_empty() {
            StructuredDocument::new_with_metadata(metadata.clone(), Some(agent_id.to_string()))
        } else {
            StructuredDocument::from_snapshot_with_metadata(
                &block.loro_snapshot,
                metadata.clone(),
                Some(agent_id.to_string()),
            )?
        };

        for update in &updates {
            doc.apply_updates(&update.update_blob)?;
        }

        let last_seq = updates.last().map(|u| u.seq).unwrap_or(block.last_seq);
        let frontier = doc.current_version();

        Ok(Some(CachedBlock {
            doc,
            last_seq,
            last_persisted_frontier: Some(frontier),
            dirty: false,
            last_accessed: Utc::now(),
        }))
    }

    /// Persist changes for a block (export delta, write to DB).
    pub fn persist(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Get block_id from DB first.
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;
        let block_id = match block {
            Some(b) => b.id,
            None => {
                return Err(MemoryError::NotFound {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                });
            }
        };

        let entry = self
            .blocks
            .get(&block_id)
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        // Extract data we need before releasing the entry lock.
        let doc = entry.doc.clone();
        let last_frontier = entry.last_persisted_frontier.clone();

        // Release the entry lock before doing work.
        drop(entry);

        // Skip persist only when the doc's version vector equals the last
        // persisted frontier — meaning no operations have been applied since
        // the last persist. This is always correct: we do not rely on the
        // `dirty` flag, which callers may have forgotten to set.
        //
        // Even when skipping the write, still attempt to spawn a subscriber so
        // that "warm-up" persist calls (e.g. after `create_block`) register the
        // subscriber before content arrives. The spawn is idempotent.
        if let Some(ref frontier) = last_frontier
            && doc.current_version() == *frontier
        {
            self.maybe_spawn_subscriber_for_block(&block_id);
            return Ok(());
        }

        // Now work with the doc (LoroDoc is already thread-safe).
        let update_blob = match &last_frontier {
            Some(frontier) => doc.export_updates_since(frontier),
            None => doc.export_snapshot(),
        };

        let new_frontier = doc.current_version();
        let preview = doc.render();

        // Only persist if there's actual data.
        let mut new_seq = None;
        if let Ok(blob) = update_blob
            && !blob.is_empty()
        {
            // Encode the frontier for storage (enables undo to this exact state).
            let frontier_bytes = new_frontier.encode();
            let seq = pattern_db::queries::store_update(
                &mut *self.db.get().mem()?,
                &block_id,
                &blob,
                Some(&frontier_bytes),
                Some("agent"),
            )
            .mem()?;

            new_seq = Some(seq);
        }

        // Update the content preview in the main block.
        let preview_str = if preview.is_empty() {
            None
        } else {
            Some(preview.as_str())
        };

        // Only update the preview, don't touch loro_snapshot.
        pattern_db::queries::update_block_preview(&*self.db.get().mem()?, &block_id, preview_str)
            .mem()?;

        // Now re-acquire the lock to update the cache entry.
        let mut entry = self
            .blocks
            .get_mut(&block_id)
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        if let Some(seq) = new_seq {
            entry.last_seq = seq;
        }
        entry.last_persisted_frontier = Some(new_frontier);
        entry.dirty = false;

        // Release the mutable lock before spawning the subscriber, which
        // needs to acquire its own read lock on `self.blocks`.
        drop(entry);

        // Lazily spawn a subscriber on the first successful persist.
        // A freshly created block with no content doesn't need a subscriber
        // until it has real data to emit — this is that moment.
        self.maybe_spawn_subscriber_for_block(&block_id);

        Ok(())
    }

    /// Helper to get block_id from agent_id and label.
    fn get_block_id(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;
        Ok(block.map(|b| b.id))
    }

    /// Mark a block as dirty (has unpersisted changes).
    pub fn mark_dirty(&self, agent_id: &str, label: &str) {
        // This is a synchronous method, so we can't query DB here.
        // Instead, we'll iterate through cache to find the block.
        let block_id = self
            .blocks
            .iter()
            .find(|entry| entry.doc.agent_id() == agent_id && entry.doc.label() == label)
            .map(|entry| entry.doc.id().to_string());

        if let Some(id) = block_id
            && let Some(mut cached) = self.blocks.get_mut(&id)
        {
            cached.dirty = true;
        }
    }

    /// Check if a block is cached.
    pub fn is_cached(&self, agent_id: &str, label: &str) -> bool {
        if let Ok(Some(block_id)) = self.get_block_id(agent_id, label) {
            self.blocks.contains_key(&block_id)
        } else {
            false
        }
    }

    /// Drop a document from the cache, persisting it first if dirty.
    ///
    /// If a sync subscriber is running for this doc, cancels it and joins the
    /// worker thread before removing the block from the cache. This ensures
    /// no in-flight writes after the doc is evicted.
    pub fn drop_doc(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Persist first if dirty.
        self.persist(agent_id, label)?;

        if let Some(block_id) = self.get_block_id(agent_id, label)? {
            // Cancel and join the subscriber before removing from cache.
            // The join is bounded: the cancellation token causes the worker
            // to exit on the next DEBOUNCE_MS (50ms) timeout iteration, so
            // this join completes within ~50ms in the normal case.
            if let Some((_, handle)) = self.subscribers.remove(&block_id) {
                handle.cancel.cancel();
                if let Err(e) = handle.thread.join() {
                    tracing::warn!(
                        block_id = %block_id,
                        "subscriber thread panicked during drop_doc join: {e:?}"
                    );
                }
            }
            self.blocks.remove(&block_id);
        }
        Ok(())
    }

    /// Cancel and join all active sync subscribers, draining in-flight work.
    ///
    /// Used by [`quiesce`] before WAL checkpoint + fsync to ensure all pending
    /// file writes have landed. Blocks are NOT removed from the cache — only
    /// their subscriber workers are stopped. A subsequent `persist()` call will
    /// lazily re-spawn subscribers if `mount_path` is configured.
    ///
    /// Each worker exits within one debounce window (~50ms) after its cancel
    /// token fires, so the total drain time is bounded by `max(worker_count) *
    /// 50ms` in the common case (threads join concurrently after all tokens
    /// are cancelled).
    pub fn drain_subscribers(&self) {
        // Phase 1: cancel all tokens without joining yet. This lets workers
        // begin their shutdown concurrently rather than sequentially.
        let block_ids: Vec<String> = self.subscribers.iter().map(|e| e.key().clone()).collect();
        for block_id in &block_ids {
            if let Some(entry) = self.subscribers.get(block_id) {
                entry.cancel.cancel();
            }
        }

        // Phase 2: remove and join each worker thread.
        for block_id in &block_ids {
            if let Some((_, handle)) = self.subscribers.remove(block_id)
                && let Err(e) = handle.thread.join()
            {
                tracing::warn!(
                    block_id = %block_id,
                    "subscriber thread panicked during drain: {e:?}"
                );
            }
        }

        tracing::debug!(count = block_ids.len(), "drained all subscribers");
    }

    /// Flush all pending subscriber work and pause workers.
    ///
    /// Each worker: drains its channel, does a final render, then parks.
    /// Returns when all workers have confirmed they're parked (or the
    /// timeout expires).
    ///
    /// Subscriptions and channels remain alive — writes during the pause
    /// accumulate in their respective docs (memory_doc for agent writes,
    /// disk_doc for external edits via watcher) and are reconciled on
    /// resume via version-vector diff.
    pub fn pause_subscribers(&self, timeout: std::time::Duration) -> PauseOutcome {
        let block_ids: Vec<String> = self.subscribers.iter().map(|e| e.key().clone()).collect();

        if block_ids.is_empty() {
            return PauseOutcome {
                paused: 0,
                timed_out: 0,
            };
        }

        // Phase 1: set the paused flag on all subscribers. Workers will
        // enter their pause loop on the next iteration.
        for block_id in &block_ids {
            if let Some(entry) = self.subscribers.get(block_id) {
                entry
                    .paused
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }

        // Phase 2: wait for each worker to signal pause_complete.
        let deadline = std::time::Instant::now() + timeout;
        let mut paused_count: usize = 0;
        let mut timed_out_count: usize = 0;

        for block_id in &block_ids {
            let Some(entry) = self.subscribers.get(block_id) else {
                continue;
            };
            let (lock, cvar) = entry.pause_complete.as_ref();
            let mut complete = lock.lock().unwrap();
            while !*complete {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    tracing::warn!(
                        block_id = %block_id,
                        "pause_subscribers: worker did not park within timeout"
                    );
                    timed_out_count += 1;
                    break;
                }
                let (guard, result) = cvar.wait_timeout(complete, remaining).unwrap();
                complete = guard;
                if result.timed_out() && !*complete {
                    tracing::warn!(
                        block_id = %block_id,
                        "pause_subscribers: worker did not park within timeout"
                    );
                    timed_out_count += 1;
                    break;
                }
            }
            if *complete {
                paused_count += 1;
            }
        }

        tracing::debug!(
            paused = paused_count,
            timed_out = timed_out_count,
            "pause_subscribers complete"
        );

        PauseOutcome {
            paused: paused_count,
            timed_out: timed_out_count,
        }
    }

    /// Resume all paused subscribers.
    ///
    /// Each worker: reconciles any writes that happened during the pause
    /// via version-vector diff, does one render, then resumes normal
    /// operation. Returns immediately — workers wake up and reconcile
    /// asynchronously.
    pub fn resume_subscribers(&self) {
        for entry in self.subscribers.iter() {
            let handle = entry.value();
            let (lock, cvar) = handle.resume_signal.as_ref();
            let mut resumed = lock.lock().unwrap();
            *resumed = true;
            cvar.notify_one();
        }

        tracing::debug!("resume_subscribers: all workers signaled");
    }

    /// Checkpoint the WAL file on the backing `memory.db`.
    ///
    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)` which forces all WAL frames to be
    /// written back into the main database file, then truncates the WAL to zero
    /// bytes. After this call the on-disk `memory.db` is canonical and can be
    /// committed by the host VCS without any WAL frames outstanding.
    ///
    /// Called by [`quiesce`](crate::quiesce) after [`pause_subscribers`](Self::pause_subscribers)
    /// to ensure the DB is in a fully-flushed state before a VCS commit. Does not
    /// touch `messages.db` — messages are not VCS-tracked.
    pub fn wal_checkpoint(&self) -> MemoryResult<()> {
        self.db
            .checkpoint()
            .map_err(|e| MemoryError::Other(format!("wal_checkpoint failed: {e}")))
    }

    /// Spawn a sync subscriber for the given block if one isn't already running.
    ///
    /// Creates a `disk_doc` by forking the memory_doc, then wires
    /// `subscribe_local_update` on memory_doc to push raw Loro update bytes
    /// into the worker's event channel. The worker imports those bytes into
    /// disk_doc and renders it to the canonical file on disk.
    ///
    /// `schema` determines the output file format:
    /// - `Text` → `.md`
    /// - `Map` / `List` / `Composite` → `.kdl`
    /// - `Log` → `.jsonl`
    pub(crate) fn spawn_subscriber(
        &self,
        block_id: &str,
        schema: BlockSchema,
        doc: &StructuredDocument,
        reembed_tx: tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
        heartbeat_tx: crossbeam_channel::Sender<Heartbeat>,
        mount_path: Arc<PathBuf>,
    ) {
        spawn_subscriber_for_block(
            block_id,
            schema,
            doc,
            reembed_tx,
            heartbeat_tx,
            mount_path,
            Arc::clone(&self.db),
            Arc::clone(&self.subscribers),
        );
    }

    /// Apply an externally-edited file's content into the cached LoroDoc.
    ///
    /// Called by the filesystem watcher when it detects a change to a block
    /// file that was not written by our own `atomic_write` (i.e., a human
    /// editor changed the file).
    ///
    /// ## Two-doc merge flow
    ///
    /// 1. Parse the file content according to the block's schema.
    /// 2. Apply the parsed content to `disk_doc` via Loro text operations.
    ///    This generates Loro update operations on disk_doc.
    /// 3. Export disk_doc's updates and import them into memory_doc.
    ///    Loro CRDT merge preserves both the agent's and human's edits.
    /// 4. Mark the block dirty for the next persist.
    ///
    /// If the block is not currently loaded in the cache or has no subscriber,
    /// the edit is silently skipped.
    pub(crate) fn apply_external_edit(&self, block_id: &str, content: &[u8]) {
        // Look up the block in the cache; if not loaded, skip.
        let Some(cached) = self.blocks.get(block_id) else {
            tracing::debug!(
                block_id = %block_id,
                "external edit for unloaded block; skipping merge"
            );
            return;
        };

        let doc = cached.doc.clone();
        drop(cached); // Release the DashMap lock before doing work.

        // Get the subscriber's disk_doc. Without a subscriber there's no
        // disk_doc to apply the external edit to.
        let Some(subscriber) = self.subscribers.get(block_id) else {
            tracing::debug!(
                block_id = %block_id,
                "external edit for block without subscriber; skipping merge"
            );
            return;
        };

        let disk_doc = Arc::clone(&subscriber.disk_doc);
        drop(subscriber); // Release the DashMap lock.

        let schema = doc.schema().clone();

        // Capture disk_doc's version before applying the external edit,
        // so we can export only the new operations afterward.
        let disk_vv_before = disk_doc.oplog_vv();

        let result: Result<(), String> = (|| {
            match &schema {
                pattern_core::types::memory_types::BlockSchema::Text { .. } => {
                    // Text blocks: file content is the raw markdown, import as text.
                    let text = String::from_utf8(content.to_vec())
                        .map_err(|e| format!("UTF-8 decode failed: {e}"))?;
                    let stripped = crate::fs::markdown::markdown_to_text(&text);
                    let disk_text = disk_doc.get_text("content");
                    disk_text
                        .update(&stripped, Default::default())
                        .map_err(|e| format!("disk_doc text update failed: {e}"))?;
                    disk_doc.commit();
                }
                pattern_core::types::memory_types::BlockSchema::Map { .. }
                | pattern_core::types::memory_types::BlockSchema::Composite { .. } => {
                    // Map/Composite blocks: parse KDL with Map shape, import via JSON.
                    let text = String::from_utf8(content.to_vec())
                        .map_err(|e| format!("UTF-8 decode failed: {e}"))?;
                    let kdl_doc = crate::fs::kdl::parse_kdl(&text)
                        .map_err(|e| format!("KDL parse failed: {e}"))?;
                    let loro_value =
                        crate::fs::kdl::kdl_to_loro_value(&kdl_doc, crate::fs::kdl::TopShape::Map)
                            .map_err(|e| format!("KDL→LoroValue failed: {e}"))?;
                    let json = crate::fs::kdl::loro_value_to_json(&loro_value)
                        .ok_or_else(|| "LoroValue→JSON conversion failed".to_string())?;
                    // Apply to disk_doc via JSON import. Since disk_doc doesn't
                    // have a StructuredDocument wrapper, we use the LoroDoc
                    // JSON import mechanism directly.
                    apply_json_to_loro_doc(&disk_doc, &json, &schema)
                        .map_err(|e| format!("disk_doc JSON import failed: {e}"))?;
                    disk_doc.commit();
                }
                pattern_core::types::memory_types::BlockSchema::List { .. } => {
                    // List blocks: parse KDL with List shape, import via JSON.
                    let text = String::from_utf8(content.to_vec())
                        .map_err(|e| format!("UTF-8 decode failed: {e}"))?;
                    let kdl_doc = crate::fs::kdl::parse_kdl(&text)
                        .map_err(|e| format!("KDL parse failed: {e}"))?;
                    let loro_value =
                        crate::fs::kdl::kdl_to_loro_value(&kdl_doc, crate::fs::kdl::TopShape::List)
                            .map_err(|e| format!("KDL→LoroValue failed: {e}"))?;
                    let json = crate::fs::kdl::loro_value_to_json(&loro_value)
                        .ok_or_else(|| "LoroValue→JSON conversion failed".to_string())?;
                    apply_json_to_loro_doc(&disk_doc, &json, &schema)
                        .map_err(|e| format!("disk_doc JSON import failed: {e}"))?;
                    disk_doc.commit();
                }
                pattern_core::types::memory_types::BlockSchema::Log { .. } => {
                    // Log blocks: parse JSONL entries and import.
                    let text = String::from_utf8(content.to_vec())
                        .map_err(|e| format!("UTF-8 decode failed: {e}"))?;
                    let entries = crate::fs::jsonl::jsonl_to_log_entries(&text)
                        .map_err(|e| format!("JSONL parse failed: {e}"))?;
                    let arr = serde_json::Value::Array(entries);
                    apply_json_to_loro_doc(&disk_doc, &arr, &schema)
                        .map_err(|e| format!("disk_doc JSON import failed: {e}"))?;
                    disk_doc.commit();
                }
                pattern_core::types::memory_types::BlockSchema::TaskList { .. } => {
                    // TaskList blocks: parse KDL with TaskList shape, import via JSON.
                    let text = String::from_utf8(content.to_vec())
                        .map_err(|e| format!("UTF-8 decode failed: {e}"))?;
                    let kdl_doc = crate::fs::kdl::parse_kdl(&text)
                        .map_err(|e| format!("KDL parse failed: {e}"))?;
                    let loro_value = crate::fs::kdl::kdl_to_loro_value(
                        &kdl_doc,
                        crate::fs::kdl::TopShape::TaskList,
                    )
                    .map_err(|e| format!("KDL→LoroValue failed: {e}"))?;
                    let json = crate::fs::kdl::loro_value_to_json(&loro_value)
                        .ok_or_else(|| "LoroValue→JSON conversion failed".to_string())?;
                    apply_json_to_loro_doc(&disk_doc, &json, &schema)
                        .map_err(|e| format!("disk_doc JSON import failed: {e}"))?;
                    disk_doc.commit();
                }
                pattern_core::types::memory_types::BlockSchema::Skill { .. } => {
                    // Skill blocks parse via YAML-frontmatter + markdown body.
                    // The `markdown_skill` converter is implemented in Task 7
                    // (Phase 4, Subcomponent C). Until then, external edits to
                    // Skill block files cannot be imported — return a typed error
                    // so the caller can log and skip without silent data loss.
                    return Err(format!(
                        "{}",
                        crate::fs::FsError::ConverterNotYetAvailable(
                            pattern_core::types::memory_types::BlockSchemaKind::Skill
                        )
                    ));
                }
                // NOTE: `_ =>` covers future non_exhaustive additions beyond
                // currently-known variants. Keep this list current.
                _ => {
                    return Err(format!("unsupported schema: {schema:?}"));
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                // Export the updates that disk_doc generated and import them
                // into memory_doc. This is the CRDT merge: memory_doc will
                // reconcile its own operations with the disk_doc operations.
                match disk_doc.export(loro::ExportMode::updates(&disk_vv_before)) {
                    Ok(update_bytes) if !update_bytes.is_empty() => {
                        if let Err(e) = doc.inner().import(&update_bytes) {
                            tracing::error!(
                                block_id = %block_id,
                                error = %e,
                                "failed to import disk_doc updates into memory_doc"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            block_id = %block_id,
                            error = %e,
                            "failed to export disk_doc updates"
                        );
                    }
                    _ => {} // Empty update bytes — no-op.
                }

                // Update the FTS5 preview column so external edits are
                // visible to search. The worker does this on every subscriber
                // cycle; we mirror that here for the external-edit path.
                let preview = doc.render();
                match self.db.get() {
                    Ok(conn) => {
                        let preview_str = if preview.is_empty() {
                            None
                        } else {
                            Some(preview.as_str())
                        };
                        if let Err(e) =
                            pattern_db::queries::update_block_preview(&conn, block_id, preview_str)
                        {
                            metrics::counter!("memory.external_edit.fts_update_failed")
                                .increment(1);
                            tracing::error!(
                                block_id = %block_id,
                                error = %e,
                                "FTS5 update failed after external edit merge"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "DB pool get failed during external edit FTS update"
                        );
                    }
                }

                // Mark the block dirty so the next persist stores the update.
                if let Some(mut cached) = self.blocks.get_mut(block_id) {
                    cached.dirty = true;
                }
                tracing::debug!(
                    block_id = %block_id,
                    "external edit imported via two-doc CRDT merge"
                );
                metrics::counter!("memory.external_edit.crdt_merged").increment(1);
            }
            Err(e) => {
                tracing::error!(
                    block_id = %block_id,
                    error = %e,
                    "external edit import failed"
                );
                metrics::counter!("memory.external_edit.import_failed").increment(1);
            }
        }
    }

    /// Get a reference to a subscriber handle by block_id.
    ///
    /// Used by the watcher for self-echo suppression (mtime comparison).
    pub(crate) fn subscriber_handle(
        &self,
        block_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, SubscriberHandle>> {
        self.subscribers.get(block_id)
    }

    /// Lazily spawn a subscriber for a cached block using the cache's own
    /// mount_path, reembed_tx, and heartbeat_tx.
    ///
    /// Does nothing if:
    /// - `mount_path` was not configured (subscriber machinery disabled).
    /// - The block is not currently loaded in the in-memory cache.
    /// - A subscriber for this block is already running.
    fn maybe_spawn_subscriber_for_block(&self, block_id: &str) {
        let (Some(mount_path), Some(reembed_tx), Some(heartbeat_tx)) = (
            self.mount_path.clone(),
            self.reembed_tx.clone(),
            self.heartbeat_tx.clone(),
        ) else {
            return;
        };

        // Don't double-spawn — checked again inside spawn_subscriber, but skip
        // the lock on blocks if we can bail out early.
        if self.subscribers.contains_key(block_id) {
            return;
        }

        let Some(cached) = self.blocks.get(block_id) else {
            return;
        };

        let doc = cached.doc.clone();
        let schema = doc.schema().clone();
        drop(cached); // Release DashMap lock before spawning.

        self.spawn_subscriber(block_id, schema, &doc, reembed_tx, heartbeat_tx, mount_path);
    }

    /// Internal search implementation shared by agent-scoped and
    /// constellation-scoped variants.
    fn search_impl(
        &self,
        agent_id_filter: Option<&str>,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        // Embedding generation requires async; for now we do a blocking
        // call via the provider's runtime if available. Since the trait
        // is sync post-Phase-3, and the embedding provider is still async,
        // we need to handle this carefully.
        let query_embedding = if options.mode.needs_embedding() {
            if let Some(provider) = &self.embedding_provider {
                // Use a one-shot runtime to drive the async embed call.
                // This is acceptable because embedding generation is
                // inherently I/O-bound and infrequent.
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        match std::thread::scope(|s| {
                            let provider = provider.clone();
                            let query = query.to_string();
                            s.spawn(move || handle.block_on(provider.embed_query(&query)))
                                .join()
                        }) {
                            Ok(Ok(embedding)) => Some(embedding),
                            Ok(Err(e)) => {
                                tracing::warn!(
                                    "Failed to generate embedding for query, falling back to FTS: {}",
                                    e
                                );
                                None
                            }
                            Err(_) => {
                                tracing::warn!("Embedding thread panicked, falling back to FTS");
                                None
                            }
                        }
                    }
                    Err(_) => {
                        tracing::warn!(
                            "No tokio runtime available for embedding generation, falling back to FTS"
                        );
                        None
                    }
                }
            } else {
                tracing::warn!(
                    "Vector/Hybrid search requested but no embedding provider configured, falling back to FTS"
                );
                None
            }
        } else {
            None
        };

        // Determine effective mode based on what's available.
        let effective_mode = match options.mode {
            SearchMode::Auto => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::Hybrid
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
            SearchMode::Fts => pattern_db::search::SearchMode::FtsOnly,
            SearchMode::Vector => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::VectorOnly
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
            SearchMode::Hybrid => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::Hybrid
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
        };

        // Build search with pattern_db.
        let search_conn = self.db.get().mem()?;
        let mut builder = pattern_db::search::search(&search_conn)
            .text(query)
            .mode(effective_mode)
            .limit(options.limit as i64);

        // Add embedding if available.
        if let Some(ref embedding) = query_embedding {
            builder = builder.embedding(embedding);
        }

        // If content types is empty, search all types.
        if options.content_types.is_empty() {
            builder = builder.filter(pattern_db::search::ContentFilter {
                content_type: None,
                agent_id: agent_id_filter.map(String::from),
            });
        } else if options.content_types.len() == 1 {
            let db_content_type = core_search_type_to_db(options.content_types[0]);
            builder = builder.filter(pattern_db::search::ContentFilter {
                content_type: Some(db_content_type),
                agent_id: agent_id_filter.map(String::from),
            });
        } else {
            // Multiple content types - execute separate queries and combine results.
            drop(builder);
            let mut all_results = Vec::new();

            for content_type in &options.content_types {
                let db_content_type = core_search_type_to_db(*content_type);
                let mut type_builder = pattern_db::search::search(&search_conn)
                    .text(query)
                    .mode(effective_mode)
                    .limit(options.limit as i64)
                    .filter(pattern_db::search::ContentFilter {
                        content_type: Some(db_content_type),
                        agent_id: agent_id_filter.map(String::from),
                    });

                if let Some(ref embedding) = query_embedding {
                    type_builder = type_builder.embedding(embedding);
                }

                let results = type_builder.execute().mem()?;
                all_results.extend(results);
            }

            // Sort by score and limit.
            all_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            all_results.truncate(options.limit);

            return Ok(all_results
                .into_iter()
                .map(db_search_result_to_core)
                .collect());
        }

        // Execute search.
        let results = builder.execute().mem()?;

        Ok(results.into_iter().map(db_search_result_to_core).collect())
    }
}

impl Drop for MemoryCache {
    fn drop(&mut self) {
        // Cancel the supervisor task when the cache is dropped.
        self.supervisor_cancel.cancel();
        if let Some(task) = self.supervisor_task.take() {
            // The task will notice the cancellation on its next tick.
            // We do not block on it here — fire and forget is sufficient
            // because the supervisor only holds soft references.
            task.abort();
        }
    }
}

/// Spawn a sync subscriber worker for a block, inserting the resulting handle
/// into `subscribers`.
///
/// This is the core spawning logic extracted from `MemoryCache::spawn_subscriber`
/// so that both the method and the supervisor respawn closure can call the same
/// code without either holding `&self`.
///
/// Does nothing if a subscriber for `block_id` is already present in
/// `subscribers` (double-spawn guard).
///
/// # Note on argument count
/// The eight parameters represent distinct, non-composable dependencies — each
/// is an independent `Arc`-wrapped resource that must be provided separately.
/// Grouping them into a helper struct would add indirection without reducing
/// the caller's need to supply each piece individually.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_subscriber_for_block(
    block_id: &str,
    schema: BlockSchema,
    doc: &StructuredDocument,
    reembed_tx: tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
    heartbeat_tx: crossbeam_channel::Sender<Heartbeat>,
    mount_path: Arc<PathBuf>,
    db: Arc<ConstellationDb>,
    subscribers: Arc<DashMap<String, SubscriberHandle>>,
) {
    // Don't double-spawn.
    if subscribers.contains_key(block_id) {
        return;
    }

    let (event_tx, event_rx) = crossbeam_channel::bounded(64);
    let cancel = CancellationToken::new();

    // Fork the memory_doc to create the disk_doc. The fork starts with
    // the same state as memory_doc at this point in time.
    let disk_doc = Arc::new(doc.inner().fork());
    let last_written_mtime: Arc<Mutex<Option<SystemTime>>> = Arc::new(Mutex::new(None));

    // Shared pause state for flush-pause-resume quiesce.
    let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pause_complete = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let resume_signal = Arc::new((Mutex::new(false), std::sync::Condvar::new()));

    // Wire subscribe_local_update on memory_doc: when the agent writes
    // to memory_doc, capture the raw Loro update bytes and forward them
    // to the worker thread for import into disk_doc and file rendering.
    // When paused, skip try_send — writes accumulate in memory_doc and
    // are reconciled via version-vector diff on resume.
    let block_id_owned = block_id.to_string();
    let tx_clone = event_tx.clone();
    let paused_flag = Arc::clone(&paused);
    let subscription = doc
        .inner()
        .subscribe_local_update(Box::new(move |update_bytes| {
            if !paused_flag.load(std::sync::atomic::Ordering::Acquire) {
                let _ = tx_clone.try_send(crate::subscriber::event::CommitEvent {
                    block_id: block_id_owned.clone(),
                    update_bytes: update_bytes.clone(),
                });
            }
            true // Keep subscription active.
        }));

    // Spawn the worker OS thread.
    let config = crate::subscriber::worker::WorkerConfig {
        block_id: block_id.to_string(),
        schema,
        rx: event_rx,
        cancel: cancel.clone(),
        db,
        reembed_tx,
        heartbeat_tx,
        mount_path,
        disk_doc: Arc::clone(&disk_doc),
        doc: doc.clone(),
        last_written_mtime: Arc::clone(&last_written_mtime),
        paused: Arc::clone(&paused),
        pause_complete: Arc::clone(&pause_complete),
        resume_signal: Arc::clone(&resume_signal),
    };

    let thread = match std::thread::Builder::new()
        .name(format!("sync-sub-{}", block_id))
        .spawn(move || {
            crate::subscriber::worker::run_subscriber(config);
        }) {
        Ok(t) => t,
        Err(e) => {
            // Thread spawn failed (OS resource limits, etc.). Log the
            // error and return without registering the subscriber. The
            // cache continues to function; the block simply won't have a
            // backing file until the next persist attempt.
            tracing::error!(
                block_id = %block_id,
                error = %e,
                "failed to spawn subscriber thread; file sync disabled for this block"
            );
            metrics::counter!("memory.sync_worker.spawn_failed").increment(1);
            return;
        }
    };

    subscribers.insert(
        block_id.to_string(),
        SubscriberHandle {
            cancel,
            thread,
            event_tx,
            _subscription: subscription,
            disk_doc,
            last_written_mtime,
            paused,
            pause_complete,
            resume_signal,
        },
    );
}

/// Apply a JSON value to a raw LoroDoc (without StructuredDocument wrapper).
///
/// Convert a `serde_json::Value` to a `loro::LoroValue`.
///
/// Used when importing JSON task items into a `LoroMovableList` so that
/// the render path (`task_item_to_kdl_node`) receives `LoroValue::Map`
/// rather than opaque serialized JSON strings.
fn json_to_loro_value(value: &serde_json::Value) -> loro::LoroValue {
    match value {
        serde_json::Value::Null => loro::LoroValue::Null,
        serde_json::Value::Bool(b) => loro::LoroValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                loro::LoroValue::I64(i)
            } else if let Some(f) = n.as_f64() {
                loro::LoroValue::Double(f)
            } else {
                loro::LoroValue::Null
            }
        }
        serde_json::Value::String(s) => loro::LoroValue::String(s.clone().into()),
        serde_json::Value::Array(arr) => {
            let items: Vec<loro::LoroValue> = arr.iter().map(json_to_loro_value).collect();
            loro::LoroValue::List(items.into())
        }
        serde_json::Value::Object(obj) => {
            let map: std::collections::HashMap<String, loro::LoroValue> = obj
                .iter()
                .map(|(k, v)| (k.clone(), json_to_loro_value(v)))
                .collect();
            loro::LoroValue::Map(map.into())
        }
    }
}

/// This is used by `apply_external_edit` to apply parsed file content to
/// the disk_doc. For text blocks, use `LoroText::update` directly instead
/// of this function. For structured blocks (Map/List/Log/Composite), this
/// function handles the JSON import using the correct container names.
///
/// Container names must match StructuredDocument's conventions exactly:
/// - Map: `"fields"` (LoroMap)
/// - Composite: `"root"` (LoroMap)
/// - List: `"items"` (LoroList)
/// - Log: `"entries"` (LoroList)
fn apply_json_to_loro_doc(
    doc: &loro::LoroDoc,
    json: &serde_json::Value,
    schema: &pattern_core::types::memory_types::BlockSchema,
) -> Result<(), String> {
    // Import JSON by applying it to the appropriate containers.
    // Container names mirror StructuredDocument's conventions exactly —
    // mismatches here cause silent data loss as writes go to an orphan container.
    use pattern_core::types::memory_types::BlockSchema;
    match (json, schema) {
        (serde_json::Value::Object(map), BlockSchema::Map { .. }) => {
            let loro_map = doc.get_map("fields");
            for (key, value) in map {
                let json_str = serde_json::to_string(value)
                    .map_err(|e| format!("JSON serialize failed: {e}"))?;
                loro_map
                    .insert(key, json_str)
                    .map_err(|e| format!("LoroMap insert failed: {e}"))?;
            }
            Ok(())
        }
        (serde_json::Value::Object(map), BlockSchema::Composite { .. }) => {
            let loro_map = doc.get_map("root");
            for (key, value) in map {
                let json_str = serde_json::to_string(value)
                    .map_err(|e| format!("JSON serialize failed: {e}"))?;
                loro_map
                    .insert(key, json_str)
                    .map_err(|e| format!("LoroMap insert failed: {e}"))?;
            }
            Ok(())
        }
        (serde_json::Value::Array(entries), BlockSchema::List { .. }) => {
            let loro_list = doc.get_list("items");
            // Clear existing entries and re-insert.
            let len = loro_list.len();
            if len > 0 {
                loro_list
                    .delete(0, len)
                    .map_err(|e| format!("LoroList delete failed: {e}"))?;
            }
            for entry in entries {
                let json_str = serde_json::to_string(entry)
                    .map_err(|e| format!("JSON serialize failed: {e}"))?;
                loro_list
                    .push(json_str)
                    .map_err(|e| format!("LoroList push failed: {e}"))?;
            }
            Ok(())
        }
        (serde_json::Value::Array(entries), BlockSchema::Log { .. }) => {
            let loro_list = doc.get_list("entries");
            // Clear existing entries and re-insert.
            let len = loro_list.len();
            if len > 0 {
                loro_list
                    .delete(0, len)
                    .map_err(|e| format!("LoroList delete failed: {e}"))?;
            }
            for entry in entries {
                let json_str = serde_json::to_string(entry)
                    .map_err(|e| format!("JSON serialize failed: {e}"))?;
                loro_list
                    .push(json_str)
                    .map_err(|e| format!("LoroList push failed: {e}"))?;
            }
            Ok(())
        }
        (serde_json::Value::Object(map), BlockSchema::TaskList { .. }) => {
            // TaskList: items are in a movable list. Extract the "items" array
            // from the JSON (which comes from the KDL round-trip discriminator map).
            // The items key must be present and must be an array; silent
            // substitution of a missing key would silently discard all items.
            let items = map
                .get("items")
                .ok_or_else(|| "TaskList JSON is missing required 'items' key".to_string())?;
            let items = items
                .as_array()
                .ok_or_else(|| format!("TaskList JSON 'items' must be an array, got: {}", items))?;
            let loro_list = doc.get_movable_list("items");
            let len = loro_list.len();
            if len > 0 {
                loro_list
                    .delete(0, len)
                    .map_err(|e| format!("LoroMovableList delete failed: {e}"))?;
            }
            // Push each item as a nested LoroMap CONTAINER (not a value-map
            // snapshot). This preserves field-level CRDT merge semantics for
            // concurrent mutations — an agent updating `status` and another
            // adding a comment on the same item merge correctly instead of
            // LWW-stomping each other (review finding I3).
            //
            // The render path (`task_item_to_kdl_node`) and subscriber
            // reconcile (`reconcile_task_list`) both consume `get_deep_value()`
            // which materializes containers back into `LoroValue::Map` values,
            // so downstream shape is unchanged.
            for entry in items {
                let entry_obj = entry
                    .as_object()
                    .ok_or_else(|| format!("TaskList item must be a JSON object, got: {entry}"))?;
                let item_map = loro_list
                    .push_container(loro::LoroMap::new())
                    .map_err(|e| format!("LoroMovableList push_container failed: {e}"))?;
                for (key, value) in entry_obj {
                    match (key.as_str(), value) {
                        // `comments` and `blocks` are nested lists. Keep them
                        // as LoroList containers so future in-place mutations
                        // (add_comment, link/unlink) produce CRDT ops rather
                        // than wholesale replacements.
                        ("comments" | "blocks", serde_json::Value::Array(arr)) => {
                            let nested = item_map
                                .insert_container(key, loro::LoroList::new())
                                .map_err(|e| {
                                    format!("LoroMap insert_container({key}) failed: {e}")
                                })?;
                            for elem in arr {
                                nested
                                    .push(json_to_loro_value(elem))
                                    .map_err(|e| format!("LoroList push in {key} failed: {e}"))?;
                            }
                        }
                        _ => {
                            item_map
                                .insert(key, json_to_loro_value(value))
                                .map_err(|e| format!("LoroMap insert({key}) failed: {e}"))?;
                        }
                    }
                }
            }
            Ok(())
        }
        // Skill blocks are not imported via JSON — they use the YAML-frontmatter +
        // markdown-body pipeline in `markdown_skill` (Task 7, Phase 4). Reaching
        // this arm would mean the inbound watcher dispatched a Skill block edit to
        // the JSON import path, which is a logic error in the caller. Return a
        // typed error rather than silently doing nothing wrong.
        (_, pattern_core::types::memory_types::BlockSchema::Skill { .. }) => Err(format!(
            "{}",
            crate::fs::FsError::ConverterNotYetAvailable(
                pattern_core::types::memory_types::BlockSchemaKind::Skill
            )
        )),
        // NOTE: `_ =>` covers future non_exhaustive additions beyond
        // currently-known variants. Keep this list current.
        _ => Err(format!(
            "unexpected JSON shape for schema {:?}: expected object for Map/Composite/TaskList, array for List/Log",
            schema
        )),
    }
}

/// Helper function to convert DB MemoryBlock to BlockMetadata.
fn db_block_to_metadata(block: &pattern_db::models::MemoryBlock) -> BlockMetadata {
    let schema = block
        .metadata
        .as_ref()
        .and_then(|m| m.get("schema"))
        .and_then(|s| serde_json::from_value::<BlockSchema>(s.clone()).ok())
        .unwrap_or_default();

    BlockMetadata {
        id: block.id.clone(),
        agent_id: block.agent_id.clone(),
        label: block.label.clone(),
        description: block.description.clone(),
        block_type: block.block_type,
        schema,
        char_limit: block.char_limit as usize,
        permission: block.permission,
        pinned: block.pinned,
        created_at: block.created_at,
        updated_at: block.updated_at,
    }
}

/// Helper function to convert DB ArchivalEntry to our ArchivalEntry.
fn db_archival_to_archival(entry: &pattern_db::models::ArchivalEntry) -> ArchivalEntry {
    ArchivalEntry {
        id: entry.id.clone(),
        agent_id: entry.agent_id.clone(),
        content: entry.content.clone(),
        metadata: entry.metadata.as_ref().map(|j| j.0.clone()),
        created_at: entry.created_at,
    }
}

impl MemoryStore for MemoryCache {
    fn create_block(
        &self,
        agent_id: &str,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let BlockCreate {
            label,
            description,
            block_type,
            schema,
            char_limit,
            permission,
            ..
        } = create;

        // Use default char limit if 0 is passed.
        let effective_char_limit = if char_limit == 0 {
            self.default_char_limit
        } else {
            char_limit
        };

        // Generate block ID.
        let block_id = format!("mem_{}", Uuid::new_v4().simple());
        let now = Utc::now();

        // Build BlockMetadata.
        let block_metadata = BlockMetadata {
            id: block_id.clone(),
            agent_id: agent_id.to_string(),
            label: label.clone(),
            description: description.clone(),
            block_type,
            schema: schema.clone(),
            char_limit: effective_char_limit,
            permission,
            pinned: false,
            created_at: now,
            updated_at: now,
        };

        // Create new StructuredDocument with metadata.
        let doc = StructuredDocument::new_with_metadata(
            block_metadata.clone(),
            Some(agent_id.to_string()),
        );

        // Store schema in DB metadata JSON.
        let mut db_metadata = serde_json::Map::new();
        db_metadata.insert(
            "schema".to_string(),
            serde_json::to_value(&schema).map_err(|e| MemoryError::Other(e.to_string()))?,
        );
        let metadata_json = JsonValue::Object(db_metadata);
        let loro_snapshot = doc.export_snapshot()?;
        let frontier = doc.current_version().get_frontiers();

        // Create MemoryBlock for DB.
        let db_block = pattern_db::models::MemoryBlock {
            id: block_id.clone(),
            agent_id: agent_id.to_string(),
            label,
            description,
            block_type,
            char_limit: effective_char_limit as i64,
            permission,
            pinned: false,
            loro_snapshot,
            content_preview: None,
            metadata: Some(Json(metadata_json)),
            embedding_model: None,
            is_active: true,
            frontier: Some(frontier.encode()),
            last_seq: 0,
            created_at: now,
            updated_at: now,
        };

        // Store in DB.
        pattern_db::queries::create_block(&*self.db.get().mem()?, &db_block).mem()?;

        // Add to cache (metadata is embedded in doc).
        //
        // `last_persisted_frontier` is set to `None` rather than `Some(vv)`
        // so that the first `persist()` call always performs a full snapshot
        // export instead of attempting a delta. This is a defensive choice:
        // callers typically mutate the returned doc (e.g. `import_from_json`)
        // before calling `persist_block`, and the full-snapshot path
        // guarantees the content reaches the DB regardless of version-vector
        // comparison subtleties with the empty initial doc.
        let cached_block = CachedBlock {
            doc: doc.clone(),
            last_seq: 0,
            last_persisted_frontier: None,
            dirty: false,
            last_accessed: now,
        };

        self.blocks.insert(block_id, cached_block);

        Ok(doc)
    }

    fn get_block(&self, agent_id: &str, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        // Delegate to existing get method.
        self.get(agent_id, label)
    }

    fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        // Query DB for block metadata without loading full document.
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;

        Ok(block.as_ref().map(db_block_to_metadata))
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        // Fetch the broadest applicable base set from DB, then narrow
        // in-memory for combinations the DB queries don't directly support.
        let base = if let Some(ref agent) = filter.agent_id {
            if let Some(bt) = filter.block_type {
                // Optimized path: agent + type.
                pattern_db::queries::list_blocks_by_type(&*self.db.get().mem()?, agent, bt).mem()?
            } else {
                pattern_db::queries::list_blocks(&*self.db.get().mem()?, agent).mem()?
            }
        } else if let Some(ref prefix) = filter.label_prefix {
            pattern_db::queries::list_blocks_by_label_prefix(&*self.db.get().mem()?, prefix)
                .mem()?
        } else {
            // No agent, no prefix — all blocks.
            pattern_db::queries::list_blocks_by_label_prefix(&*self.db.get().mem()?, "").mem()?
        };

        let mut results: Vec<BlockMetadata> = base.iter().map(db_block_to_metadata).collect();

        // Apply in-memory filters for fields that weren't part of the DB query.
        if let Some(bt) = filter.block_type {
            // If we didn't use the optimized by_type query (i.e., no agent_id),
            // apply the type filter now.
            if filter.agent_id.is_none() {
                results.retain(|m| m.block_type == bt);
            }
        }
        if let Some(ref prefix) = filter.label_prefix {
            // If we fetched by agent (not by prefix), apply prefix filter now.
            if filter.agent_id.is_some() {
                results.retain(|m| m.label.starts_with(prefix.as_str()));
            }
        }

        Ok(results)
    }

    fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Get block ID first.
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;

        if let Some(block) = block {
            // Drop from cache first (will persist if dirty and cancel subscriber).
            if self.blocks.contains_key(&block.id) {
                self.drop_doc(agent_id, label)?;
            }

            // Soft-delete in DB.
            pattern_db::queries::deactivate_block(&*self.db.get().mem()?, &block.id).mem()?;
        }

        Ok(())
    }

    fn get_rendered_content(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        // Get doc, call doc.render().
        let doc = self.get(agent_id, label)?;
        Ok(doc.map(|d| d.render()))
    }

    fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Delegate to existing persist method.
        self.persist(agent_id, label)
    }

    fn mark_dirty(&self, agent_id: &str, label: &str) {
        // Delegate to existing method.
        MemoryCache::mark_dirty(self, agent_id, label);
    }

    fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        // Generate archival entry ID.
        let entry_id = format!("arch_{}", Uuid::new_v4().simple());

        // Create archival entry.
        let entry = pattern_db::models::ArchivalEntry {
            id: entry_id.clone(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            metadata: metadata.map(pattern_db::Json),
            chunk_index: 0,
            parent_entry_id: None,
            created_at: Utc::now(),
        };

        // Store in DB.
        pattern_db::queries::create_archival_entry(&*self.db.get().mem()?, &entry).mem()?;

        Ok(entry_id)
    }

    fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        // Use rich search with FTS mode.
        let search_conn = self.db.get().mem()?;
        let results = pattern_db::search::search(&search_conn)
            .text(query)
            .mode(pattern_db::search::SearchMode::FtsOnly)
            .limit(limit as i64)
            .filter(pattern_db::search::ContentFilter::archival(Some(agent_id)))
            .execute()
            .mem()?;

        // Convert search results to ArchivalEntry.
        let mut entries = Vec::new();
        for result in results {
            if let Some(entry) =
                pattern_db::queries::get_archival_entry(&search_conn, &result.id).mem()?
            {
                entries.push(db_archival_to_archival(&entry));
            }
        }

        Ok(entries)
    }

    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        pattern_db::queries::delete_archival_entry(&*self.db.get().mem()?, id).mem()?;
        Ok(())
    }

    fn search(
        &self,
        query: &str,
        options: SearchOptions,
        scope: MemorySearchScope,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        match scope {
            MemorySearchScope::Agent(ref agent_id) => {
                self.search_impl(Some(agent_id.as_str()), query, options)
            }
            MemorySearchScope::Constellation => self.search_impl(None, query, options),
            _ => Err(MemoryError::Other(
                "unsupported search scope variant".into(),
            )),
        }
    }

    fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        let shared =
            pattern_db::queries::get_shared_blocks(&*self.db.get().mem()?, agent_id).mem()?;

        Ok(shared
            .into_iter()
            .map(|(block, permission, owner_name)| SharedBlockInfo {
                block_id: block.id,
                owner_agent_id: block.agent_id,
                owner_agent_name: owner_name,
                label: block.label,
                description: block.description,
                block_type: block.block_type,
                permission,
            })
            .collect())
    }

    fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // 1. Check access FIRST - DB is source of truth.
        let access_result = pattern_db::queries::check_block_access(
            &*self.db.get().mem()?,
            requester_agent_id,
            owner_agent_id,
            label,
        )
        .mem()?;

        let (block_id, shared_permission) = match access_result {
            Some((id, perm)) => (id, perm),
            None => return Ok(None), // No access.
        };

        // 2. Check cache using block_id.
        if self.blocks.contains_key(&block_id) {
            let last_seq = {
                let entry = self.blocks.get(&block_id).unwrap();
                entry.last_seq
            };

            // Check for new updates from DB since we last synced.
            let updates =
                pattern_db::queries::get_updates_since(&*self.db.get().mem()?, &block_id, last_seq)
                    .mem()?;

            // Re-acquire mutable lock to apply updates.
            let mut entry = self.blocks.get_mut(&block_id).unwrap();
            if !updates.is_empty() {
                for update in &updates {
                    entry.doc.apply_updates(&update.update_blob)?;
                }
                entry.last_seq = updates.last().unwrap().seq;
            }
            entry.last_accessed = Utc::now();

            // Clone the doc with the shared permission.
            let mut doc = entry.doc.clone();
            doc.set_permission(shared_permission);
            return Ok(Some(doc));
        }

        // 3. Load from DB with shared permission.
        let block = self.load_from_db(owner_agent_id, label, shared_permission)?;

        match block {
            Some(cached) => {
                let doc = cached.doc.clone();
                self.blocks.insert(block_id, cached);
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    fn update_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        if patch.is_empty() {
            return Ok(());
        }

        // Get block from DB.
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;

        let block = block.ok_or_else(|| MemoryError::NotFound {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
        })?;

        // Apply pinned update.
        if let Some(pinned) = patch.pinned {
            pattern_db::queries::update_block_pinned(&*self.db.get().mem()?, &block.id, pinned)
                .mem()?;
            if let Some(mut cached) = self.blocks.get_mut(&block.id) {
                cached.doc.metadata_mut().pinned = pinned;
                cached.last_accessed = Utc::now();
            }
        }

        // Apply block_type update.
        if let Some(bt) = patch.block_type {
            pattern_db::queries::update_block_type(&*self.db.get().mem()?, &block.id, bt).mem()?;
            if let Some(mut cached) = self.blocks.get_mut(&block.id) {
                cached.doc.metadata_mut().block_type = bt;
                cached.last_accessed = Utc::now();
            }
        }

        // Apply schema update.
        if let Some(ref schema) = patch.schema {
            // Parse existing schema to validate compatibility.
            let existing_schema = block
                .metadata
                .as_ref()
                .and_then(|m| m.get("schema"))
                .and_then(|s| serde_json::from_value::<BlockSchema>(s.clone()).ok())
                .unwrap_or_default();

            // Validate schema compatibility (same variant type).
            if std::mem::discriminant(&existing_schema) != std::mem::discriminant(schema) {
                return Err(MemoryError::Other(format!(
                    "Cannot change schema type from {:?} to {:?}",
                    existing_schema, schema
                )));
            }

            // Build updated metadata.
            let mut db_meta = block
                .metadata
                .as_ref()
                .and_then(|m| m.as_object().cloned())
                .unwrap_or_default();
            db_meta.insert(
                "schema".to_string(),
                serde_json::to_value(schema).map_err(|e| MemoryError::Other(e.to_string()))?,
            );
            let metadata_json = serde_json::Value::Object(db_meta);

            pattern_db::queries::update_block_metadata(
                &*self.db.get().mem()?,
                &block.id,
                &metadata_json,
            )
            .mem()?;

            if let Some(mut cached) = self.blocks.get_mut(&block.id) {
                cached.doc.set_schema(schema.clone());
                cached.last_accessed = Utc::now();
            }
        }

        // Apply description update.
        if let Some(ref description) = patch.description {
            pattern_db::queries::update_block_config(
                &mut *self.db.get().mem()?,
                &block.id,
                None,
                None,
                Some(description.as_str()),
                None,
                None,
            )
            .mem()?;

            if let Some(mut cached) = self.blocks.get_mut(&block.id) {
                cached.doc.metadata_mut().description = description.clone();
                cached.last_accessed = Utc::now();
            }
        }

        Ok(())
    }

    fn undo_redo(&self, agent_id: &str, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        // Get block ID from DB.
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;

        let block = block.ok_or_else(|| MemoryError::NotFound {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
        })?;

        match op {
            UndoRedoOp::Undo => {
                let deactivated_seq = pattern_db::queries::deactivate_latest_update(
                    &*self.db.get().mem()?,
                    &block.id,
                )
                .mem()?;

                if deactivated_seq.is_none() {
                    return Ok(false); // Nothing to undo.
                }

                // Update the block's frontier to the new latest active update's frontier.
                let new_latest =
                    pattern_db::queries::get_latest_update(&*self.db.get().mem()?, &block.id)
                        .mem()?;

                if let Some(update) = new_latest {
                    if let Some(frontier_bytes) = &update.frontier {
                        pattern_db::queries::update_block_frontier(
                            &*self.db.get().mem()?,
                            &block.id,
                            frontier_bytes,
                        )
                        .mem()?;
                    }
                } else {
                    // No active updates left - clear frontier to initial state.
                    pattern_db::queries::update_block_frontier(
                        &*self.db.get().mem()?,
                        &block.id,
                        &[],
                    )
                    .mem()?;
                }

                // Evict from cache - next access will load the undone state from DB.
                self.blocks.remove(&block.id);
                Ok(true)
            }
            UndoRedoOp::Redo => {
                let reactivated_seq =
                    pattern_db::queries::reactivate_next_update(&*self.db.get().mem()?, &block.id)
                        .mem()?;

                if reactivated_seq.is_none() {
                    return Ok(false); // Nothing to redo.
                }

                // Update the block's frontier to the new latest active update's frontier.
                let new_latest =
                    pattern_db::queries::get_latest_update(&*self.db.get().mem()?, &block.id)
                        .mem()?;

                if let Some(update) = new_latest
                    && let Some(frontier_bytes) = &update.frontier
                {
                    pattern_db::queries::update_block_frontier(
                        &*self.db.get().mem()?,
                        &block.id,
                        frontier_bytes,
                    )
                    .mem()?;
                }

                // Evict from cache - next access will load the redone state from DB.
                self.blocks.remove(&block.id);
                Ok(true)
            }
            _ => Err(MemoryError::Other(
                "unsupported undo/redo operation variant".into(),
            )),
        }
    }

    fn history_depth(&self, agent_id: &str, label: &str) -> MemoryResult<UndoRedoDepth> {
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, agent_id, label)
                .mem()?;

        let block = block.ok_or_else(|| MemoryError::NotFound {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
        })?;

        let undo = pattern_db::queries::count_undo_steps(&*self.db.get().mem()?, &block.id).mem()?
            as usize;
        let redo = pattern_db::queries::count_redo_steps(&*self.db.get().mem()?, &block.id).mem()?
            as usize;

        Ok(UndoRedoDepth { undo, redo })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::memory_types::MemoryBlockType;
    use pattern_db::models::MemoryBlock;

    fn test_dbs() -> (tempfile::TempDir, Arc<ConstellationDb>) {
        let dir = tempfile::tempdir().unwrap();
        let dbs = Arc::new(ConstellationDb::open_in_memory().unwrap());
        (dir, dbs)
    }

    /// Create a test agent in the database with sensible defaults.
    fn create_test_agent(dbs: &ConstellationDb, agent_id: &str) -> String {
        let agent = pattern_db::models::Agent {
            id: agent_id.to_string(),
            name: format!("Test Agent {}", agent_id),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: pattern_db::Json(serde_json::json!({})),
            enabled_tools: pattern_db::Json(vec![]),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(&dbs.get().unwrap(), &agent)
            .expect("Failed to create test agent");
        agent_id.to_string()
    }

    /// Create test databases and a default test agent ("agent_1").
    fn test_dbs_with_agent() -> (tempfile::TempDir, Arc<ConstellationDb>) {
        let (dir, dbs) = test_dbs();
        create_test_agent(&dbs, "agent_1");
        (dir, dbs)
    }

    #[test]
    fn test_cache_load_empty_block() {
        let (_dir, dbs) = test_dbs_with_agent();

        // Create a block in DB.
        let block = MemoryBlock {
            id: "mem_1".to_string(),
            agent_id: "agent_1".to_string(),
            label: "persona".to_string(),
            description: "Agent personality".to_string(),
            block_type: MemoryBlockType::Core,
            char_limit: 5000,
            permission: MemoryPermission::ReadWrite,
            pinned: true,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_block(&dbs.get().unwrap(), &block).unwrap();

        // Create cache and load.
        let cache = MemoryCache::new(dbs);
        let doc = cache.get("agent_1", "persona").unwrap();

        assert!(doc.is_some());
        assert!(cache.is_cached("agent_1", "persona"));
    }

    #[test]
    fn test_cache_miss() {
        let (_dir, dbs) = test_dbs();
        let cache = MemoryCache::new(dbs);

        let doc = cache.get("agent_1", "nonexistent");
        assert!(doc.is_err());
    }

    #[test]
    fn test_cache_persist() {
        let (_dir, dbs) = test_dbs_with_agent();

        // Create a block.
        let block = MemoryBlock {
            id: "mem_2".to_string(),
            agent_id: "agent_1".to_string(),
            label: "scratch".to_string(),
            description: "Working memory".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 5000,
            permission: MemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_block(&dbs.get().unwrap(), &block).unwrap();

        let cache = MemoryCache::new(dbs.clone());

        // Load and modify.
        let doc = cache.get("agent_1", "scratch").unwrap().unwrap();
        doc.set_text("Hello, world!", true).unwrap();

        cache.mark_dirty("agent_1", "scratch");

        // Persist.
        cache.persist("agent_1", "scratch").unwrap();

        // Verify update was stored.
        let (_, updates) =
            pattern_db::queries::get_checkpoint_and_updates(&dbs.get().unwrap(), "mem_2").unwrap();

        assert!(!updates.is_empty());
    }

    /// Regression test: `persist` must write data even when `mark_dirty` was
    /// never called. Previously the dirty-flag check would silently skip the
    /// write, causing data loss.
    #[test]
    fn test_persist_without_mark_dirty_still_writes() {
        let (_dir, dbs) = test_dbs_with_agent();

        let block = MemoryBlock {
            id: "mem_nodirty".to_string(),
            agent_id: "agent_1".to_string(),
            label: "nodirty".to_string(),
            description: "Block for no-dirty persist test".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 5000,
            permission: MemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_block(&dbs.get().unwrap(), &block).unwrap();

        let cache = MemoryCache::new(dbs.clone());

        // Mutate via set_text — intentionally do NOT call mark_dirty.
        let doc = cache.get("agent_1", "nodirty").unwrap().unwrap();
        doc.set_text("persisted without mark_dirty", true).unwrap();

        // Persist must detect the version-vector change and write the update.
        cache.persist("agent_1", "nodirty").unwrap();

        let (_, updates) =
            pattern_db::queries::get_checkpoint_and_updates(&dbs.get().unwrap(), "mem_nodirty")
                .unwrap();

        assert!(
            !updates.is_empty(),
            "persist should write an update even when mark_dirty was never called"
        );
    }

    /// Verify that `persist` is a no-op when the doc has not been mutated
    /// since the last persist (version vector unchanged).
    #[test]
    fn test_persist_skips_when_unchanged() {
        let (_dir, dbs) = test_dbs_with_agent();

        let block = MemoryBlock {
            id: "mem_noop".to_string(),
            agent_id: "agent_1".to_string(),
            label: "noop".to_string(),
            description: "Block for no-op persist test".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 5000,
            permission: MemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_block(&dbs.get().unwrap(), &block).unwrap();

        let cache = MemoryCache::new(dbs.clone());

        // Write content and persist once.
        let doc = cache.get("agent_1", "noop").unwrap().unwrap();
        doc.set_text("initial content", true).unwrap();
        cache.mark_dirty("agent_1", "noop");
        cache.persist("agent_1", "noop").unwrap();

        let (_, updates_after_first) =
            pattern_db::queries::get_checkpoint_and_updates(&dbs.get().unwrap(), "mem_noop")
                .unwrap();
        let count_first = updates_after_first.len();
        assert!(count_first > 0, "first persist must store an update");

        // Persist again without any mutations — must be a no-op.
        cache.persist("agent_1", "noop").unwrap();

        let (_, updates_after_second) =
            pattern_db::queries::get_checkpoint_and_updates(&dbs.get().unwrap(), "mem_noop")
                .unwrap();
        assert_eq!(
            updates_after_second.len(),
            count_first,
            "second persist with no mutations must not store additional updates"
        );
    }

    // ========== MemoryStore trait tests ==========

    #[test]
    fn test_create_and_get_block() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block using MemoryStore trait.
        let created_doc = cache
            .create_block(
                "agent_1",
                BlockCreate::new("test_block", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Test block description")
                    .with_char_limit(1000),
            )
            .unwrap();

        assert!(created_doc.id().starts_with("mem_"));

        // Get the block back (should return same doc since it's cached).
        let doc = cache.get_block("agent_1", "test_block").unwrap();
        assert!(doc.is_some());

        // Verify content is initially empty.
        let doc = doc.unwrap();
        assert_eq!(doc.render(), "");

        // Modify and verify.
        doc.set_text("Test content", true).unwrap();
        assert_eq!(doc.render(), "Test content");
    }

    #[test]
    fn test_list_blocks() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create multiple blocks.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("block1", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("First block")
                    .with_char_limit(1000),
            )
            .unwrap();

        cache
            .create_block(
                "agent_1",
                BlockCreate::new("block2", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Second block")
                    .with_char_limit(2000),
            )
            .unwrap();

        cache
            .create_block(
                "agent_1",
                BlockCreate::new("block3", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Third block")
                    .with_char_limit(1500),
            )
            .unwrap();

        // List all blocks.
        let all_blocks = cache.list_blocks(BlockFilter::by_agent("agent_1")).unwrap();
        assert_eq!(all_blocks.len(), 3);

        // List blocks by type.
        let core_blocks = cache
            .list_blocks(BlockFilter::by_type("agent_1", MemoryBlockType::Core))
            .unwrap();
        assert_eq!(core_blocks.len(), 2);

        let working_blocks = cache
            .list_blocks(BlockFilter::by_type("agent_1", MemoryBlockType::Working))
            .unwrap();
        assert_eq!(working_blocks.len(), 1);
        assert_eq!(working_blocks[0].label, "block2");
    }

    #[test]
    fn test_delete_block() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("to_delete", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Will be deleted")
                    .with_char_limit(1000),
            )
            .unwrap();

        // Verify it exists.
        let doc = cache.get_block("agent_1", "to_delete").unwrap();
        assert!(doc.is_some());

        // Delete it.
        cache.delete_block("agent_1", "to_delete").unwrap();

        // Verify it's gone (soft delete, so get_block returns error).
        let doc = cache.get_block("agent_1", "to_delete");
        assert!(doc.is_err());

        // List should not include deleted block.
        let blocks = cache.list_blocks(BlockFilter::by_agent("agent_1")).unwrap();
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn test_get_rendered_content() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new(
                    "content_test",
                    MemoryBlockType::Working,
                    BlockSchema::text(),
                )
                .with_description("Test content rendering")
                .with_char_limit(1000),
            )
            .unwrap();

        // Get and modify.
        let doc = cache.get_block("agent_1", "content_test").unwrap().unwrap();
        doc.set_text("Hello, world!", true).unwrap();

        // Mark dirty and persist.
        cache.mark_dirty("agent_1", "content_test");
        cache.persist_block("agent_1", "content_test").unwrap();

        // Get rendered content.
        let content = cache
            .get_rendered_content("agent_1", "content_test")
            .unwrap();
        assert_eq!(content, Some("Hello, world!".to_string()));
    }

    #[test]
    fn test_archival_operations() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Insert archival entries.
        let id1 = cache
            .insert_archival("agent_1", "First archival entry", None)
            .unwrap();
        assert!(id1.starts_with("arch_"));

        let metadata = serde_json::json!({"source": "test", "importance": "high"});
        let id2 = cache
            .insert_archival(
                "agent_1",
                "Second archival entry with metadata",
                Some(metadata),
            )
            .unwrap();
        assert!(id2.starts_with("arch_"));

        // Search archival (simple substring match).
        let results = cache.search_archival("agent_1", "archival", 10).unwrap();
        assert_eq!(results.len(), 2);

        let results = cache.search_archival("agent_1", "metadata", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].metadata.is_some());

        // Delete archival entry.
        cache.delete_archival(&id1).unwrap();

        // Verify deletion.
        let results = cache.search_archival("agent_1", "First", 10).unwrap();
        assert_eq!(results.len(), 0);

        // Second entry should still be there.
        let results = cache.search_archival("agent_1", "Second", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_get_block_metadata() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("metadata_test", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Test metadata retrieval")
                    .with_char_limit(5000),
            )
            .unwrap();

        // Get metadata without loading full document.
        let metadata = cache
            .get_block_metadata("agent_1", "metadata_test")
            .unwrap();

        assert!(metadata.is_some());
        let metadata = metadata.unwrap();
        assert_eq!(metadata.label, "metadata_test");
        assert_eq!(metadata.description, "Test metadata retrieval");
        assert_eq!(metadata.block_type, MemoryBlockType::Core);
        assert_eq!(metadata.char_limit, 5000);
        assert!(!metadata.pinned);
    }

    // ========== Search functionality tests ==========

    use pattern_core::types::memory_types::{SearchContentType, SearchMode, SearchOptions};

    #[test]
    fn test_search_memory_blocks_fts() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs.clone());

        // Create blocks with searchable content.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Agent personality")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache.get_block("agent_1", "persona").unwrap().unwrap();
        doc.set_text(
            "I am a helpful assistant specializing in Rust programming",
            true,
        )
        .unwrap();
        cache.mark_dirty("agent_1", "persona");
        cache.persist_block("agent_1", "persona").unwrap();

        // Create another block.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("notes", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Working notes")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache.get_block("agent_1", "notes").unwrap().unwrap();
        doc.set_text(
            "Meeting scheduled for tomorrow about Python development",
            true,
        )
        .unwrap();
        cache.mark_dirty("agent_1", "notes");
        cache.persist_block("agent_1", "notes").unwrap();

        // Search for "Rust" - should find persona block.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache
            .search("Rust", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("Rust programming")
        );

        // Search for "Python" - should find notes block.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache
            .search("Python", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("Python development")
        );

        // Search for "development" - should find both.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache
            .search(
                "development",
                opts,
                MemorySearchScope::Agent("agent_1".into()),
            )
            .unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_search_archival_entries_fts() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Insert archival entries.
        cache
            .insert_archival(
                "agent_1",
                "Discussed project requirements for the new authentication system",
                None,
            )
            .unwrap();

        cache
            .insert_archival(
                "agent_1",
                "Reviewed database schema design for user management",
                None,
            )
            .unwrap();

        cache
            .insert_archival(
                "agent_1",
                "Implemented token-based authentication with JWT",
                None,
            )
            .unwrap();

        // Search for "authentication" - should find relevant entries.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search(
                "authentication",
                opts,
                MemorySearchScope::Agent("agent_1".into()),
            )
            .unwrap();
        assert_eq!(results.len(), 2);

        // Verify content.
        assert!(results.iter().any(|r| {
            r.content
                .as_ref()
                .unwrap()
                .contains("authentication system")
        }));
        assert!(results.iter().any(|r| {
            r.content
                .as_ref()
                .unwrap()
                .contains("token-based authentication")
        }));

        // Search for "database".
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("database", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("database schema")
        );
    }

    #[test]
    fn test_search_multiple_content_types() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs.clone());

        // Create a memory block.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Agent personality")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache.get_block("agent_1", "persona").unwrap().unwrap();
        doc.set_text("I specialize in Rust programming and system design", true)
            .unwrap();
        cache.mark_dirty("agent_1", "persona");
        cache.persist_block("agent_1", "persona").unwrap();

        // Create an archival entry.
        cache
            .insert_archival(
                "agent_1",
                "Helped user debug a complex Rust lifetime issue",
                None,
            )
            .unwrap();

        // Search across both types.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks, SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("Rust", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 2);

        // Verify we got results from both types.
        let content_types: Vec<_> = results.iter().map(|r| r.content_type).collect();
        assert!(content_types.contains(&SearchContentType::Blocks));
        assert!(content_types.contains(&SearchContentType::Archival));
    }

    #[test]
    fn test_search_respects_agent_id() {
        let (_dir, dbs) = test_dbs();

        // Create two agents.
        create_test_agent(&dbs, "agent_1");
        create_test_agent(&dbs, "agent_2");

        let cache = MemoryCache::new(dbs);

        // Insert archival for agent_1.
        cache
            .insert_archival("agent_1", "Agent 1 secret information", None)
            .unwrap();

        // Insert archival for agent_2.
        cache
            .insert_archival("agent_2", "Agent 2 secret information", None)
            .unwrap();

        // Search for agent_1 should only return agent_1's data.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search(
                "secret",
                opts.clone(),
                MemorySearchScope::Agent("agent_1".into()),
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.as_ref().unwrap().contains("Agent 1"));

        // Search for agent_2 should only return agent_2's data.
        let results = cache
            .search("secret", opts, MemorySearchScope::Agent("agent_2".into()))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.as_ref().unwrap().contains("Agent 2"));
    }

    #[test]
    fn test_search_limit() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Insert many archival entries with same keyword.
        for i in 0..10 {
            cache
                .insert_archival(
                    "agent_1",
                    &format!("Entry {} about testing functionality", i),
                    None,
                )
                .unwrap();
        }

        // Search with limit of 3.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 3,
        };

        let results = cache
            .search("testing", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_empty_content_types() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs.clone());

        // Create data in both memory blocks and archival.
        cache
            .create_block(
                "agent_1",
                BlockCreate::new("test_block", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Test")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache.get_block("agent_1", "test_block").unwrap().unwrap();
        doc.set_text("Searchable block content", true).unwrap();
        cache.mark_dirty("agent_1", "test_block");
        cache.persist_block("agent_1", "test_block").unwrap();

        cache
            .insert_archival("agent_1", "Searchable archival content", None)
            .unwrap();

        // Search with empty content_types - should search all types.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![],
            limit: 10,
        };

        let results = cache
            .search(
                "Searchable",
                opts,
                MemorySearchScope::Agent("agent_1".into()),
            )
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_hybrid_mode_fallback() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs.clone());

        // Insert archival entry.
        cache
            .insert_archival("agent_1", "Test content for hybrid search", None)
            .unwrap();

        // Search with Hybrid mode (should gracefully fall back to FTS).
        let opts = SearchOptions {
            mode: SearchMode::Hybrid,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("hybrid", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("hybrid search")
        );
    }

    #[test]
    fn test_search_vector_mode_fallback() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs.clone());

        // Insert archival entry.
        cache
            .insert_archival("agent_1", "Test content for vector search", None)
            .unwrap();

        // Search with Vector mode (should gracefully fall back to FTS).
        let opts = SearchOptions {
            mode: SearchMode::Vector,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("vector", opts, MemorySearchScope::Agent("agent_1".into()))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("vector search")
        );
    }

    #[test]
    fn test_search_all_hybrid_mode_fallback() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs.clone());

        // Insert archival entry.
        cache
            .insert_archival("agent_1", "Constellation-wide searchable content", None)
            .unwrap();

        // Search across constellation with Hybrid mode (should gracefully fall back to FTS).
        let opts = SearchOptions {
            mode: SearchMode::Hybrid,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("constellation", opts, MemorySearchScope::Constellation)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("Constellation-wide")
        );
    }

    #[test]
    fn test_replace_text_crdt_aware() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block with some initial content.
        let doc = cache
            .create_block(
                "agent_1",
                BlockCreate::new(
                    "test_replace",
                    MemoryBlockType::Working,
                    BlockSchema::text(),
                )
                .with_description("Test block for replacement")
                .with_char_limit(1000),
            )
            .unwrap();

        // Set initial content.
        doc.set_text("Hello world, this is a test.", true).unwrap();
        cache.mark_dirty("agent_1", "test_replace");
        cache.persist("agent_1", "test_replace").unwrap();

        // Get the version vector before replacement.
        let vv_before = doc.inner().oplog_vv();

        // Perform replacement using CRDT-aware method directly on doc.
        let replaced = doc.replace_text("world", "universe", true).unwrap();

        assert!(replaced, "Replacement should have occurred");

        // Persist the changes.
        cache.mark_dirty("agent_1", "test_replace");
        cache.persist("agent_1", "test_replace").unwrap();

        // Verify the content is correct.
        assert_eq!(doc.text_content(), "Hello universe, this is a test.");

        // Verify version vector advanced (CRDT operation was recorded).
        let vv_after = doc.inner().oplog_vv();
        assert_ne!(
            vv_before.encode().as_slice(),
            vv_after.encode().as_slice(),
            "Version vector should advance after CRDT operation"
        );
    }

    #[test]
    fn test_replace_text_not_found() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block with some content.
        let doc = cache
            .create_block(
                "agent_1",
                BlockCreate::new(
                    "test_replace",
                    MemoryBlockType::Working,
                    BlockSchema::text(),
                )
                .with_description("Test block for replacement")
                .with_char_limit(1000),
            )
            .unwrap();

        // Set initial content.
        doc.set_text("Hello world", true).unwrap();
        cache.mark_dirty("agent_1", "test_replace");
        cache.persist("agent_1", "test_replace").unwrap();

        // Try to replace something that doesn't exist.
        let replaced = doc
            .replace_text("nonexistent", "replacement", true)
            .unwrap();

        assert!(!replaced, "Replacement should not have occurred");

        // Verify content is unchanged.
        assert_eq!(doc.text_content(), "Hello world");
    }

    /// Test that replacement works correctly when content has multi-byte Unicode characters.
    #[test]
    fn test_replace_text_unicode() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block for Unicode replacement testing.
        let doc = cache
            .create_block(
                "agent_1",
                BlockCreate::new(
                    "unicode_test",
                    MemoryBlockType::Working,
                    BlockSchema::text(),
                )
                .with_description("Test block for Unicode replacement")
                .with_char_limit(1000),
            )
            .unwrap();

        // Test case 1: Emoji before target.
        doc.set_text("Hello 🌍 world", true).unwrap();

        let replaced = doc.replace_text("world", "universe", true).unwrap();

        assert!(
            replaced,
            "Replacement should have occurred with emoji before target"
        );
        assert_eq!(
            doc.text_content(),
            "Hello 🌍 universe",
            "Content should correctly replace 'world' with 'universe' after emoji"
        );

        // Test case 2: CJK characters (3 bytes each in UTF-8).
        doc.set_text("日本語 world and more", true).unwrap();

        let replaced = doc.replace_text("world", "世界", true).unwrap();

        assert!(
            replaced,
            "Replacement should have occurred with CJK characters before target"
        );
        assert_eq!(
            doc.text_content(),
            "日本語 世界 and more",
            "Content should correctly replace 'world' with unicode after CJK chars"
        );

        // Test case 3: Multiple emoji and mixed content.
        doc.set_text("🎉🎊 Hello 🌍 beautiful world 🌈", true)
            .unwrap();

        let replaced = doc
            .replace_text("beautiful world", "amazing planet", true)
            .unwrap();

        assert!(
            replaced,
            "Replacement should work with multiple emoji surrounding target"
        );
        assert_eq!(
            doc.text_content(),
            "🎉🎊 Hello 🌍 amazing planet 🌈",
            "Content should correctly handle multiple emoji around replacement"
        );

        // Test case 4: Replace at very start after Unicode prefix.
        doc.set_text("🔥start middle end", true).unwrap();

        let replaced = doc.replace_text("start", "begin", true).unwrap();

        assert!(replaced, "Replacement should work immediately after emoji");
        assert_eq!(
            doc.text_content(),
            "🔥begin middle end",
            "Content should correctly replace right after emoji"
        );

        // Test case 5: Replace emoji itself.
        doc.set_text("Hello 🌍 world", true).unwrap();

        let replaced = doc.replace_text("🌍", "🌎", true).unwrap();

        assert!(
            replaced,
            "Replacement should work when replacing emoji with emoji"
        );
        assert_eq!(
            doc.text_content(),
            "Hello 🌎 world",
            "Content should correctly replace emoji with different emoji"
        );
    }

    /// Test that `spawn_subscriber_for_block` creates a fresh subscriber handle.
    ///
    /// This exercises the supervisor respawn path: the supervisor cancels and
    /// removes a crashed worker, then calls the respawn closure (which calls
    /// `spawn_subscriber_for_block` with the same arguments). The test verifies
    /// that after the initial handle is manually removed, calling the function
    /// again inserts a new handle into the registry.
    #[test]
    fn spawn_subscriber_for_block_creates_and_respawns() {
        use pattern_core::memory::StructuredDocument;
        use pattern_core::types::memory_types::BlockSchema;

        let (_dir, db) = test_dbs();
        let block_id = "respawn_test_block";
        let agent_id = "respawn_test_agent";
        create_test_agent(&db, agent_id);

        // Create a block row so the DB constraint is satisfied.
        {
            let conn = db.get().unwrap();
            let block = pattern_db::models::MemoryBlock {
                id: block_id.to_string(),
                agent_id: agent_id.to_string(),
                label: block_id.to_string(),
                description: "Respawn test block".to_string(),
                block_type: pattern_db::models::MemoryBlockType::Working,
                char_limit: 5000,
                permission: MemoryPermission::ReadWrite,
                pinned: false,
                loro_snapshot: vec![],
                content_preview: None,
                metadata: None,
                embedding_model: None,
                is_active: true,
                frontier: None,
                last_seq: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_block(&conn, &block).unwrap();
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let mount_path = Arc::new(temp_dir.path().to_path_buf());
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let subscribers: Arc<DashMap<String, SubscriberHandle>> = Arc::new(DashMap::new());

        let schema = BlockSchema::text();
        let doc = StructuredDocument::new_text();

        // Step 1: Spawn the initial subscriber.
        spawn_subscriber_for_block(
            block_id,
            schema.clone(),
            &doc,
            reembed_tx.clone(),
            hb_tx.clone(),
            Arc::clone(&mount_path),
            Arc::clone(&db),
            Arc::clone(&subscribers),
        );
        assert!(
            subscribers.contains_key(block_id),
            "initial subscriber should be registered"
        );

        // Step 2: Simulate a crash — cancel the worker, join it, and remove
        // the handle from the registry (exactly what the supervisor does).
        let (_, old_handle) = subscribers.remove(block_id).unwrap();
        old_handle.cancel.cancel();
        // Drop the subscription before joining so the channel sender is gone.
        drop(old_handle._subscription);
        drop(old_handle.event_tx);
        old_handle
            .thread
            .join()
            .expect("worker thread should not panic on cancel");

        assert!(
            !subscribers.contains_key(block_id),
            "subscriber should be absent after simulated crash removal"
        );

        // Step 3: Respawn — mirrors what the respawn closure does.
        spawn_subscriber_for_block(
            block_id,
            schema,
            &doc,
            reembed_tx,
            hb_tx,
            Arc::clone(&mount_path),
            Arc::clone(&db),
            Arc::clone(&subscribers),
        );
        assert!(
            subscribers.contains_key(block_id),
            "respawned subscriber should be registered after crash"
        );

        // Clean up: cancel and join the respawned worker.
        let (_, respawned) = subscribers.remove(block_id).unwrap();
        respawned.cancel.cancel();
        drop(respawned._subscription);
        drop(respawned.event_tx);
        respawned
            .thread
            .join()
            .expect("respawned worker thread should not panic");
    }

    // -------------------------------------------------------------------------
    // TaskList dispatch tests (AC Task 9 — cache.rs)
    // -------------------------------------------------------------------------

    /// `apply_json_to_loro_doc` with a TaskList JSON blob populates the
    /// LoroMovableList. Verifies both that items are inserted and that the
    /// movable list contains LoroValue::Map entries (not serialized JSON strings).
    #[test]
    fn apply_json_to_loro_doc_task_list_populates_movable_list() {
        use loro::LoroDoc;
        use pattern_core::types::memory_types::BlockSchema;

        let schema = BlockSchema::TaskList {
            default_status: None,
            default_owner: None,
            display_limit: None,
        };

        let doc = LoroDoc::new();
        let json = serde_json::json!({
            "items": [
                {
                    "id": "t1",
                    "subject": "Task one",
                    "description": "",
                    "status": "pending",
                    "blocks": [],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "t2",
                    "subject": "Task two",
                    "description": "",
                    "status": "in-progress",
                    "blocks": [],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-02T00:00:00Z",
                    "updated_at": "2026-01-02T00:00:00Z"
                }
            ]
        });

        apply_json_to_loro_doc(&doc, &json, &schema)
            .expect("apply_json_to_loro_doc with TaskList JSON must succeed");
        doc.commit();

        let list = doc.get_movable_list("items");
        assert_eq!(list.len(), 2, "movable list must contain 2 items");

        // Items must be LoroValue::Map (not opaque String).
        let deep = list.get_deep_value();
        let loro::LoroValue::List(items) = &deep else {
            panic!("deep value must be a LoroValue::List, got: {deep:?}");
        };
        for (i, item) in items.iter().enumerate() {
            assert!(
                matches!(item, loro::LoroValue::Map(_)),
                "item {i} must be LoroValue::Map for the render path, got: {item:?}"
            );
        }
    }

    /// `apply_json_to_loro_doc` with an empty items array produces an empty
    /// movable list (no panics, no residual items).
    #[test]
    fn apply_json_to_loro_doc_task_list_empty_items() {
        use loro::LoroDoc;
        use pattern_core::types::memory_types::BlockSchema;

        let schema = BlockSchema::TaskList {
            default_status: None,
            default_owner: None,
            display_limit: None,
        };

        let doc = LoroDoc::new();
        let json = serde_json::json!({ "items": [] });

        apply_json_to_loro_doc(&doc, &json, &schema)
            .expect("apply_json_to_loro_doc with empty TaskList items must succeed");
        doc.commit();

        let list = doc.get_movable_list("items");
        assert_eq!(
            list.len(),
            0,
            "movable list must be empty for empty items array"
        );
    }

    /// `apply_json_to_loro_doc` rejects a TaskList JSON blob that is missing
    /// the required `items` key (Important #2: no silent data loss).
    #[test]
    fn apply_json_to_loro_doc_task_list_rejects_missing_items_key() {
        use loro::LoroDoc;
        use pattern_core::types::memory_types::BlockSchema;

        let schema = BlockSchema::TaskList {
            default_status: None,
            default_owner: None,
            display_limit: None,
        };

        let doc = LoroDoc::new();
        let bad_json = serde_json::json!({ "xyz": "junk" });

        let result = apply_json_to_loro_doc(&doc, &bad_json, &schema);
        assert!(
            result.is_err(),
            "missing 'items' key must produce an error, not silent data loss"
        );
        assert!(
            result.unwrap_err().contains("missing required 'items' key"),
            "error message must mention the missing key"
        );
    }

    /// `apply_json_to_loro_doc` rejects a TaskList JSON blob where `items` is
    /// not an array.
    #[test]
    fn apply_json_to_loro_doc_task_list_rejects_non_array_items() {
        use loro::LoroDoc;
        use pattern_core::types::memory_types::BlockSchema;

        let schema = BlockSchema::TaskList {
            default_status: None,
            default_owner: None,
            display_limit: None,
        };

        let doc = LoroDoc::new();
        let bad_json = serde_json::json!({ "items": "not an array" });

        let result = apply_json_to_loro_doc(&doc, &bad_json, &schema);
        assert!(
            result.is_err(),
            "'items' must be an array — string value must be rejected"
        );
    }

    /// `apply_external_edit` with a TaskList KDL blob applies to disk_doc and
    /// the changes are merged into memory_doc via CRDT update export/import.
    /// Verifies no panics and that the movable list in disk_doc reflects the
    /// edited items.
    #[test]
    fn apply_external_edit_task_list_merges_kdl_into_crdt() {
        use pattern_core::memory::StructuredDocument;
        use pattern_core::types::memory_types::BlockSchema;

        let (_dir, db) = test_dbs();
        let block_id = "tl_ext_block";
        let agent_id = "agent_tl_ext";
        create_test_agent(&db, agent_id);

        let schema = BlockSchema::TaskList {
            default_status: None,
            default_owner: None,
            display_limit: None,
        };

        // Create block in DB.
        {
            let conn = db.get().unwrap();
            let block = pattern_db::models::MemoryBlock {
                id: block_id.to_string(),
                agent_id: agent_id.to_string(),
                label: block_id.to_string(),
                description: "TaskList external edit test".to_string(),
                block_type: pattern_db::models::MemoryBlockType::Working,
                char_limit: 5000,
                permission: MemoryPermission::ReadWrite,
                pinned: false,
                loro_snapshot: vec![],
                content_preview: None,
                metadata: None,
                embedding_model: None,
                is_active: true,
                frontier: None,
                last_seq: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_block(&conn, &block).unwrap();
        }

        // Create the cache, load the doc, and spawn a subscriber.
        let temp_dir = tempfile::tempdir().unwrap();
        let mount_path = Arc::new(temp_dir.path().to_path_buf());
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let subscribers: Arc<DashMap<String, SubscriberHandle>> = Arc::new(DashMap::new());

        let doc = StructuredDocument::new(schema.clone());

        spawn_subscriber_for_block(
            block_id,
            schema.clone(),
            &doc,
            reembed_tx,
            hb_tx,
            Arc::clone(&mount_path),
            Arc::clone(&db),
            Arc::clone(&subscribers),
        );

        // The MemoryCache needs a populated `blocks` map for `apply_external_edit`
        // to find the block. Build a minimal cache directly with the doc + subscriber.
        let cache = MemoryCache::new(Arc::clone(&db));
        // Insert the doc into the cache manually (bypassing DB load).
        {
            cache.blocks.insert(
                block_id.to_string(),
                CachedBlock {
                    doc: doc.clone(),
                    last_seq: 0,
                    last_persisted_frontier: None,
                    dirty: false,
                    last_accessed: chrono::Utc::now(),
                },
            );
        }
        // Move the subscriber handle into the cache's subscriber map.
        {
            let (_, handle) = subscribers.remove(block_id).unwrap();
            cache.subscribers.insert(block_id.to_string(), handle);
        }

        // Build a minimal TaskList KDL blob representing an external edit.
        let kdl_content = r#"task-list {
    item id="ext-1" status="pending" {
        subject "Externally added task"
    }
}"#;

        // Apply the external edit — this is the production path exercised by
        // the file watcher when it detects a human edit.
        cache.apply_external_edit(block_id, kdl_content.as_bytes());

        // Give the subscriber a moment to process (apply_external_edit imports
        // disk_doc updates into memory_doc synchronously, then queues a re-render).
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Verify the disk_doc (accessed via the subscriber) reflects the edit.
        let sub = cache.subscribers.get(block_id).unwrap();
        let disk_doc = Arc::clone(&sub.disk_doc);
        drop(sub);

        let deep = disk_doc.get_movable_list("items").get_deep_value();
        let loro::LoroValue::List(items) = &deep else {
            panic!("disk_doc items must be LoroValue::List after external edit, got: {deep:?}");
        };
        assert_eq!(
            items.len(),
            1,
            "disk_doc must have 1 item after external edit"
        );

        // Clean up.
        let (_, handle) = cache.subscribers.remove(block_id).unwrap();
        handle.cancel.cancel();
        drop(handle._subscription);
        drop(handle.event_tx);
        handle.thread.join().expect("worker should not panic");
    }
}
