//! In-memory cache of StructuredDocument instances.
//!
//! The v3 refactor replaced the previous `ConstellationDatabases` wrapper
//! (which bundled `pattern_db` + `pattern_auth`) with direct `pattern_db::ConstellationDb`
//! access. Memory operations don't need the auth DB; consumers that require
//! both wire them separately.

use crate::db_bridge::{DbResultExt, core_search_type_to_db, db_search_result_to_core};
use crate::skill::{SkillProvenance, assign_trust_tier, resolve_source_for_path};
use crate::subscriber::SubscriberHandle;
use crate::subscriber::event::{Heartbeat, ReembedRequest};
use crate::subscriber::supervisor::{SupervisorState, run_supervisor};
use crate::types_internal::CachedBlock;
use chrono::Utc;
use jiff::Timestamp;

/// Convert chrono::DateTime<Utc> → jiff::Timestamp at the pattern_db boundary.
/// pattern_db rows use chrono; pattern_core BlockMetadata + ArchivalEntry use jiff.
fn chrono_to_jiff(dt: chrono::DateTime<chrono::Utc>) -> Timestamp {
    Timestamp::from_nanosecond(dt.timestamp_nanos_opt().unwrap_or(0) as i128).unwrap_or_default()
}
use dashmap::DashMap;
use pattern_core::memory::StructuredDocument;
use pattern_core::traits::EmbeddingProvider;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, BlockSchema, MemoryError,
    MemoryPermission, MemoryResult, MemorySearchResult, MemorySearchScope, Scope, SearchMode,
    SearchOptions, SharedBlockInfo, UndoRedoDepth, UndoRedoOp,
};
use pattern_db::Json;
use pattern_db::{ConstellationDb, MemoryBlockType};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
    pub(crate) embedding_provider: Option<Arc<dyn EmbeddingProvider>>,

    /// Stored tokio runtime handle for sync-context query embedding.
    /// Set by callers that construct the cache from a tokio context.
    /// `search_archival` (and other sync paths that need to drive async
    /// work) prefer this over `Handle::try_current()`, which fails when
    /// invoked from the eval worker's runtime-less OS thread.
    pub(crate) tokio_handle: Option<tokio::runtime::Handle>,

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

    /// Optional base path for `Scope::Global` blocks (persona-state).
    /// When set, persona-scoped blocks render to
    /// `<persona_state_dir>/@<persona_id>/blocks/<type>/<label>.<ext>`
    /// — typically `$XDG_STATE_HOME/pattern/personas/`. When `None`,
    /// persona blocks fall back to the in-mount path
    /// `<mount>/blocks/@<persona_id>/<type>/<label>.<ext>` (back-compat
    /// for unmounted dev sessions).
    persona_state_dir: Option<Arc<PathBuf>>,

    /// Optional path to the first-party skill directory (e.g.
    /// `pattern_runtime/resources/skills`). When set, skills loaded from
    /// files under this directory are classified as `SkillSource::SdkResourceDir`
    /// and receive `SkillTrustTier::FirstParty` regardless of their declared tier.
    ///
    /// This must be injected from outside `pattern_memory` because the
    /// canonical first-party path lives in `pattern_runtime`, which depends on
    /// `pattern_memory` (not the other way around). The correct injection
    /// path is via the `first_party_skills_dir` parameter of
    /// [`crate::mount::attach`] / [`crate::mount::attach_with_paths`], which
    /// in turn call `with_first_party_skills_dir` internally.
    first_party_skills_dir: Option<PathBuf>,

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

    /// Fan-out registry for block-change notifications. Subscriber
    /// workers fire callbacks here after a successful render; the
    /// `pattern_runtime::wake` module's `BlockChanged` and
    /// `TaskDependencyResolved` evaluators subscribe via
    /// [`Self::block_change_notifier`] and push wake activations onto
    /// the agent's mailbox in response.
    block_change_notifier: crate::subscriber::BlockChangeNotifier,

    /// Cross-block memory event observer. Concrete-cache impls publish on
    /// this; MemorySync handlers (and other future cross-block observers)
    /// subscribe to get raw loro update bytes + origin metadata as edits
    /// happen. See `pattern_core::observer::MemoryObserver`.
    observer: pattern_core::observer::MemoryObserver,

    /// Reverse mapping from canonical file path to block_id. Populated
    /// when subscribers are spawned; used by `BlockFanoutRouter` to
    /// resolve file-change events back to their block_id.
    path_to_block_id: Arc<DashMap<PathBuf, String>>,
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
            tokio_handle: None,
            blocks: Arc::new(DashMap::new()),
            subscribers: Arc::new(DashMap::new()),
            default_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
            mount_path: None,
            persona_state_dir: None,
            first_party_skills_dir: None,
            reembed_tx: None,
            heartbeat_tx: None,
            supervisor_cancel: CancellationToken::new(),
            supervisor_state: Arc::new(SupervisorState::new()),
            supervisor_task: None,
            block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            observer: pattern_core::observer::MemoryObserver::new(),
            path_to_block_id: Arc::new(DashMap::new()),
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
            tokio_handle: None,
            blocks: Arc::new(DashMap::new()),
            subscribers: Arc::new(DashMap::new()),
            default_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
            mount_path: None,
            persona_state_dir: None,
            first_party_skills_dir: None,
            reembed_tx: None,
            heartbeat_tx: None,
            supervisor_cancel: CancellationToken::new(),
            supervisor_state: Arc::new(SupervisorState::new()),
            supervisor_task: None,
            block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            observer: pattern_core::observer::MemoryObserver::new(),
            path_to_block_id: Arc::new(DashMap::new()),
        }
    }

    /// The cache's block-change notifier. Subscriber workers fire
    /// callbacks here after each successful render; consumers
    /// (typically `pattern_runtime::wake` evaluators) register
    /// callbacks via [`crate::subscriber::BlockChangeNotifier::subscribe`]
    /// and receive a [`crate::subscriber::Subscription`] guard whose
    /// `Drop` unsubscribes.
    /// Access the cross-block memory observer for this cache.
    pub fn memory_observer(&self) -> &pattern_core::observer::MemoryObserver {
        &self.observer
    }

    pub fn block_change_notifier(&self) -> &crate::subscriber::BlockChangeNotifier {
        &self.block_change_notifier
    }

    /// Set a custom default character limit for new memory blocks
    pub fn with_default_char_limit(mut self, limit: usize) -> Self {
        self.default_char_limit = limit;
        self
    }

    /// Store a tokio runtime handle on the cache so sync code paths
    /// (notably `search_archival`'s query-embedding step) can drive
    /// async embedding-provider calls without needing an ambient
    /// runtime via `Handle::try_current()`.
    pub fn with_tokio_handle(mut self, handle: tokio::runtime::Handle) -> Self {
        self.tokio_handle = Some(handle);
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
    /// Enable cross-mount persona-state path layout for `Scope::Global`
    /// blocks. When set, persona-scoped blocks render under
    /// `<dir>/@<persona_id>/blocks/...` rather than the in-mount fallback
    /// path. Production wiring sets this to `$XDG_STATE_HOME/pattern/personas/`.
    #[must_use]
    /// Get a clone of the reembed-queue sender, if the cache was
    /// configured with a mount path (which spawns the embedding queue).
    /// Used by the session opener to plumb message-embedding dispatch
    /// into `SessionContext::reembed_tx`.
    pub fn reembed_tx(&self) -> Option<&tokio::sync::mpsc::UnboundedSender<crate::subscriber::event::ReembedRequest>> {
        self.reembed_tx.as_ref()
    }

    pub fn with_persona_state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.persona_state_dir = Some(Arc::new(dir.into()));
        self
    }

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
        // Prefer the stored handle (set by attach() via with_tokio_handle),
        // fall back to ambient runtime detection. Same pattern as the search
        // paths — eval-worker thread has no ambient runtime, so callers that
        // construct cache from sync contexts need to plumb a handle in.
        let supervisor_handle = self
            .tokio_handle
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok());
        match supervisor_handle {
            Some(handle) => {
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
                let respawn_persona_state_dir = self.persona_state_dir.clone();
                let respawn_reembed_tx = reembed_tx;
                let respawn_heartbeat_tx = heartbeat_tx;
                let respawn_block_change_notifier = self.block_change_notifier.clone();
                let respawn_observer = self.observer.clone();
                let respawn_path_to_block_id = Arc::clone(&self.path_to_block_id);

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
                            respawn_persona_state_dir.clone(),
                            Arc::clone(&respawn_db),
                            Arc::clone(&respawn_subscribers),
                            respawn_block_change_notifier.clone(),
                            respawn_observer.clone(),
                            Arc::clone(&respawn_path_to_block_id),
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
            None => {
                tracing::warn!(
                    "no tokio handle (stored or ambient) when configuring mount path; \
                     supervisor will not run — subscriber heartbeat timeouts will not be detected"
                );
            }
        }

        self
    }

    /// Configure the first-party skill directory for trust-tier enforcement.
    ///
    /// When set, skills loaded from files under `dir` are classified as
    /// [`SkillSource::SdkResourceDir`] and assigned `SkillTrustTier::FirstParty`
    /// regardless of the `trust_tier` value in their YAML frontmatter.
    ///
    /// This is called internally by [`crate::mount::attach`] / [`crate::mount::attach_with_paths`],
    /// which receive the first-party path from `pattern_runtime::sdk::FIRST_PARTY_SKILL_DIR`
    /// via their `first_party_skills_dir` parameter. It cannot be baked into
    /// `pattern_memory` itself because the first-party path is relative to
    /// `pattern_runtime`'s `CARGO_MANIFEST_DIR`, which is only known at
    /// `pattern_runtime`'s build time.
    ///
    /// Not `pub` — callers must go through the attach API, which is the
    /// correct-by-construction path. Tests that need to exercise trust-tier
    /// override pass a test-specific path via `attach_with_paths`.
    ///
    /// [`SkillSource::SdkResourceDir`]: crate::skill::SkillSource::SdkResourceDir
    pub(crate) fn with_first_party_skills_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.first_party_skills_dir = Some(dir.into());
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
            // Block doesn't exist or caller has no access. The trait
            // contract for `MemoryStore::get_block` is `Ok(None)` for
            // missing — see `pattern_core::error::memory` module doc
            // for the read/write missing-block split.
            None => return Ok(None),
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
    /// Returns true if two block schemas are compatible enough to share a
    /// loro doc — i.e. their top-level container kinds match. Two `Text`
    /// schemas are compatible; `Text` and `Map` are not.
    ///
    /// Used by the soft-delete reactivation path: reusing the same loro
    /// doc state with a different container layout would produce a block
    /// whose stored shape disagrees with its declared schema.
    fn schema_compatible_static(a: &BlockSchema, b: &BlockSchema) -> bool {
        std::mem::discriminant(a) == std::mem::discriminant(b)
    }

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
            // Missing or soft-deleted: read-path → Ok(None). Mirrors
            // the contract of `get_block`/`get_block_metadata` — see
            // `pattern_core::error::memory` module doc.
            _ => return Ok(None),
        };

        Ok(Some(
            self.hydrate_doc_from_db(&block, effective_permission)?,
        ))
    }

    /// Rebuild a `CachedBlock` from a DB `MemoryBlock` row by replaying its
    /// snapshot + outstanding updates. Does NOT consult `is_active` — the
    /// caller is responsible for deciding whether reading a soft-deleted
    /// block makes sense in their context.
    ///
    /// Used by:
    /// - `load_from_db` (read path, after filtering is_active = true)
    /// - `create_block`'s soft-delete reactivation branch (rebuilds the
    ///   prior incarnation's doc so that the new BlockCreate's content is
    ///   applied as a CRDT edit on top, preserving update history and
    ///   continuing the seq sequence — rather than starting from seq 0
    ///   and colliding with the prior incarnation's update rows on the
    ///   `(block_id, seq)` UNIQUE constraint).
    fn hydrate_doc_from_db(
        &self,
        block: &pattern_db::models::MemoryBlock,
        effective_permission: MemoryPermission,
    ) -> MemoryResult<CachedBlock> {
        // Build BlockMetadata from DB block.
        let mut metadata = db_block_to_metadata(block);
        // Override with effective permission (may differ for shared blocks).
        metadata.permission = effective_permission;

        let agent_id = block.agent_id.clone();

        // Get and apply any updates since the snapshot.
        let (_checkpoint, updates) =
            pattern_db::queries::get_checkpoint_and_updates(&*self.db.get().mem()?, &block.id)
                .mem()?;

        // Create StructuredDocument from snapshot with metadata.
        let doc = if block.loro_snapshot.is_empty() {
            StructuredDocument::new_with_metadata(metadata.clone(), Some(agent_id.clone()))
        } else {
            StructuredDocument::from_snapshot_with_metadata(
                &block.loro_snapshot,
                metadata.clone(),
                Some(agent_id.clone()),
            )?
        };

        for update in &updates {
            doc.apply_updates(&update.update_blob)?;
        }

        let mut last_seq = updates.last().map(|u| u.seq).unwrap_or(block.last_seq);

        // Disk-precedence at startup: if the block's canonical file exists
        // and its content differs from the freshly-hydrated doc render,
        // the human likely edited the file while the daemon was off.
        // Merge the disk content into the doc via the schema bridge, then
        // persist the resulting ops as a new DB update so the merge is
        // durable. Only runs when mount_path is configured (production).
        if let Some(mount_path) = &self.mount_path {
            let scope = Scope::from_db_key(&block.agent_id)
                .unwrap_or_else(|| Scope::Global(block.agent_id.clone().into()));
            let ext = block_schema_extension(&doc.schema());
            let file_path = block_file_path(
                mount_path.as_path(),
                self.persona_state_dir.as_deref().map(|p| p.as_path()),
                &scope,
                doc.block_type(),
                doc.label(),
                ext,
            );
            if let Ok(disk_bytes) = std::fs::read(&file_path) {
                let rendered = doc.render();
                if rendered.as_bytes() != disk_bytes.as_slice() {
                    // Disk diverged — apply via bridge to merge the human's edit.
                    let vv_before = doc.inner().oplog_vv();
                    if let Err(e) = crate::subscriber::bridge::apply_block_external_edit(
                        doc.inner(),
                        &doc.schema().clone(),
                        &disk_bytes,
                        &file_path,
                    ) {
                        tracing::warn!(
                            block_id = %block.id,
                            path = ?file_path,
                            error = %e,
                            "hydrate disk-merge: bridge.apply_external failed; using DB state only"
                        );
                    } else {
                        doc.inner().commit();
                        // Persist the merge as a new DB update so it's
                        // durable and visible to subsequent loads.
                        if let Ok(blob) = doc.inner().export(loro::ExportMode::updates(&vv_before))
                            && !blob.is_empty()
                        {
                            let new_frontier = doc.current_version();
                            let frontier_bytes = new_frontier.encode();
                            match pattern_db::queries::store_update(
                                &mut *self.db.get().mem()?,
                                &block.id,
                                &blob,
                                Some(&frontier_bytes),
                                Some("disk-merge-on-hydrate"),
                            ) {
                                Ok(seq) => {
                                    last_seq = seq;
                                    tracing::info!(
                                        block_id = %block.id,
                                        path = ?file_path,
                                        "hydrate: merged disk edit into doc + persisted to DB"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        block_id = %block.id,
                                        error = %e,
                                        "hydrate disk-merge: store_update failed; merge in-memory only"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        let frontier = doc.current_version();

        Ok(CachedBlock {
            doc,
            last_seq,
            last_persisted_frontier: Some(frontier),
            dirty: false,
            last_accessed: Utc::now(),
        })
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
                return Err(MemoryError::WriteToMissingBlock {
                    scope: Scope::from_db_key(agent_id)
                        .unwrap_or_else(|| Scope::Global(agent_id.into())),
                    label: label.to_string(),
                    op: "persist_block".to_string(),
                });
            }
        };

        let entry = self
            .blocks
            .get(&block_id)
            .ok_or_else(|| MemoryError::WriteToMissingBlock {
                scope: Scope::from_db_key(agent_id)
                    .unwrap_or_else(|| Scope::Global(agent_id.into())),
                label: label.to_string(),
                op: "persist_block".to_string(),
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
        let mut entry =
            self.blocks
                .get_mut(&block_id)
                .ok_or_else(|| MemoryError::WriteToMissingBlock {
                    scope: Scope::from_db_key(agent_id)
                        .unwrap_or_else(|| Scope::Global(agent_id.into())),
                    label: label.to_string(),
                    op: "persist_block".to_string(),
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

        // Single-doc world: synchronously flush the doc to disk via
        // the subscriber's SyncedDoc. The doc the subscriber holds IS the
        // same loro doc this cache entry just persisted to DB; write_local
        // renders that doc and atomic-writes the canonical bytes.
        if let Some(sub) = self.subscribers.get(&block_id) {
            if let Err(e) = sub.synced_doc.write_local() {
                tracing::warn!(
                    block_id = %block_id,
                    error = %e,
                    "synced_doc.write_local failed during persist; disk file may be stale"
                );
            }
        }

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
    ///
    /// Pre-Phase-1 this method silently no-opped on misses. The new
    /// [`MemoryStore::mark_dirty`] trait method returns `Result`; the
    /// inner helper here is now [`Self::mark_dirty_checked`]. This
    /// legacy entry point remains for backward compatibility within
    /// the cache module's internal callers — it logs at debug on miss
    /// rather than erroring.
    pub fn mark_dirty(&self, agent_id: &str, label: &str) {
        let _ = self.mark_dirty_lookup(agent_id, label);
    }

    /// Inner lookup used by both the legacy [`Self::mark_dirty`] and the
    /// `Result`-returning [`Self::mark_dirty_checked`]. Returns `Some(())`
    /// when the dirty flag was set, `None` when no cached entry matched.
    fn mark_dirty_lookup(&self, agent_id: &str, label: &str) -> Option<()> {
        let block_id = self
            .blocks
            .iter()
            .find(|entry| entry.doc.agent_id() == agent_id && entry.doc.label() == label)
            .map(|entry| entry.doc.id().to_string())?;
        let mut cached = self.blocks.get_mut(&block_id)?;
        cached.dirty = true;
        Some(())
    }

    /// `Result`-returning variant of [`Self::mark_dirty`]: returns
    /// [`MemoryError::WriteToMissingBlock`] when the
    /// `(agent_id, label)` pair does not match any cached entry.
    pub fn mark_dirty_checked(
        &self,
        agent_id: &str,
        label: &str,
        scope: &Scope,
    ) -> MemoryResult<()> {
        match self.mark_dirty_lookup(agent_id, label) {
            Some(()) => Ok(()),
            None => Err(MemoryError::WriteToMissingBlock {
                scope: scope.clone(),
                label: label.to_string(),
                op: "mark_dirty".to_string(),
            }),
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
            self.persona_state_dir.clone(),
            Arc::clone(&self.db),
            Arc::clone(&self.subscribers),
            self.block_change_notifier.clone(),
            self.observer.clone(),
            Arc::clone(&self.path_to_block_id),
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

        // Get the subscriber's synced_doc. Without a subscriber there's no
        // SyncedDoc pipeline to route the external edit through.
        let Some(subscriber) = self.subscribers.get(block_id) else {
            tracing::debug!(
                block_id = %block_id,
                "external edit for block without subscriber; skipping merge"
            );
            return;
        };

        // Hold an Arc to synced_doc so we can call apply_external_bytes after
        // releasing the DashMap lock.
        let synced_doc = Arc::clone(&subscriber.synced_doc);
        drop(subscriber); // Release the DashMap lock.

        let schema = doc.schema().clone();

        // For Skill blocks, enforce trust-tier from provenance BEFORE routing
        // through synced_doc.apply_external_bytes. The bridge's apply_external
        // cannot enforce trust because it lacks access to mount_path and
        // first_party_skills_dir. We parse, adjust the tier, and re-emit to
        // bytes so the standard SyncedDoc pipeline processes the corrected
        // content (bridge call → disk_doc update → memory_doc CRDT import).
        //
        // For all other schemas, content is passed through unchanged.
        let content_to_apply: std::borrow::Cow<[u8]> =
            if matches!(schema, BlockSchema::Skill { .. }) {
                match (|| -> Result<Vec<u8>, String> {
                    let mut skill_file = crate::fs::markdown_skill::parse(content)
                        .map_err(|e| format!("Skill parse failed: {e}"))?;

                    let file_path = self
                        .mount_path
                        .as_deref()
                        .map(|mp| mp.join(format!("{block_id}.md")));
                    let fp_ref = self.first_party_skills_dir.as_deref();
                    let mount_paths: Vec<PathBuf> = self
                        .mount_path
                        .as_deref()
                        .map(|mp| vec![mp.to_path_buf()])
                        .unwrap_or_default();
                    let mount_refs: Vec<&std::path::Path> =
                        mount_paths.iter().map(|p| p.as_path()).collect();
                    if let Some(ref fp) = file_path {
                        let source = resolve_source_for_path(fp, fp_ref, &mount_refs);
                        let provenance = SkillProvenance {
                            source,
                            declared_tier: Some(skill_file.metadata.trust_tier),
                        };
                        skill_file.metadata.trust_tier = assign_trust_tier(&provenance);
                    }

                    // Re-emit with the corrected trust tier so synced_doc's bridge
                    // processes trust-safe bytes — write_skill_to_loro_doc inside
                    // the bridge will then record the correct tier in disk_doc.
                    let corrected = crate::fs::markdown_skill::emit(
                        &skill_file.metadata,
                        &skill_file.extras,
                        &skill_file.body,
                    )
                    .map_err(|e| format!("Skill emit failed after trust-tier correction: {e}"))?;
                    Ok(corrected.into_bytes())
                })() {
                    Ok(bytes) => std::borrow::Cow::Owned(bytes),
                    Err(e) => {
                        tracing::error!(
                            block_id = %block_id,
                            error = %e,
                            "Skill trust-tier enforcement failed; skipping external edit"
                        );
                        metrics::counter!("memory.external_edit.import_failed").increment(1);
                        return;
                    }
                }
            } else {
                std::borrow::Cow::Borrowed(content)
            };

        // Route through synced_doc.apply_external_bytes. This is the single
        // source of truth for the external-edit pipeline: bridge call →
        // disk_doc update → memory_doc CRDT import → last_saved_frontier
        // advance → external_subscribers fanout. (Echo-suppression state —
        // last_written_mtime/hash — is intentionally NOT touched here; those
        // track our own writes and updating them on external apply would
        // suppress legitimate subsequent external edits.) The cache must not
        // duplicate any of this logic (D1 fix: previously the cache reached
        // directly into disk_doc and replicated the export/import steps here).
        if let Err(e) = synced_doc.apply_external_bytes(&content_to_apply) {
            tracing::error!(
                block_id = %block_id,
                error = %e,
                "external edit import failed"
            );
            metrics::counter!("memory.external_edit.import_failed").increment(1);
            return;
        }

        // Post-apply: update FTS5 and mark dirty. These are cache-level
        // concerns that synced_doc does not own.
        //
        // For TaskList blocks, also run `reconcile_task_list` inside the same
        // transaction so that the `tasks` and `task_edges` sqlite indexes
        // reflect the external edit immediately — without waiting for the
        // subscriber worker to receive a CommitEvent (which does not fire for
        // CRDT updates imported via `subscribe_local_update`).
        let preview = doc.render();
        // Single-doc world: synced_doc.doc() IS the doc the agent mutated and
        // that the bridge reconciles external edits into.
        let reconcile_doc = synced_doc.doc();

        match self.db.get() {
            Ok(mut conn) => {
                let preview_str = if preview.is_empty() {
                    None
                } else {
                    Some(preview.as_str())
                };

                if matches!(
                    schema,
                    pattern_core::types::memory_types::BlockSchema::TaskList { .. }
                ) {
                    // TaskList: FTS + task reconcile in a single transaction
                    // (mirrors render_cycle atomicity in the subscriber worker).
                    match conn.transaction() {
                        Ok(tx) => {
                            if let Err(e) = pattern_db::queries::update_block_preview(
                                &tx,
                                block_id,
                                preview_str,
                            ) {
                                metrics::counter!("memory.external_edit.fts_update_failed")
                                    .increment(1);
                                tracing::error!(
                                    block_id = %block_id, error = %e,
                                    "FTS5 update failed in TaskList external-edit transaction; rolling back"
                                );
                                // tx drops without commit → implicit rollback.
                            } else if let Err(e) = crate::subscriber::task::reconcile_task_list(
                                &tx,
                                block_id,
                                reconcile_doc,
                            ) {
                                metrics::counter!("memory.external_edit.reconcile_failed")
                                    .increment(1);
                                tracing::error!(
                                    block_id = %block_id, error = %e,
                                    "TaskList reconcile failed during external edit; transaction rolled back"
                                );
                                // tx drops without commit → both FTS and reconcile roll back.
                            } else if let Err(e) = tx.commit() {
                                metrics::counter!("memory.external_edit.reconcile_failed")
                                    .increment(1);
                                tracing::error!(
                                    block_id = %block_id, error = %e,
                                    "TaskList external-edit transaction commit failed"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                block_id = %block_id, error = %e,
                                "failed to open transaction for TaskList external-edit reconcile"
                            );
                        }
                    }
                } else {
                    // Non-TaskList: standalone FTS update.
                    if let Err(e) =
                        pattern_db::queries::update_block_preview(&conn, block_id, preview_str)
                    {
                        metrics::counter!("memory.external_edit.fts_update_failed").increment(1);
                        tracing::error!(
                            block_id = %block_id,
                            error = %e,
                            "FTS5 update failed after external edit merge"
                        );
                    }
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

    /// Get a reference to a subscriber handle by block_id.
    ///
    /// Used by the watcher for self-echo suppression (mtime comparison).
    pub(crate) fn subscriber_handle(
        &self,
        block_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, SubscriberHandle>> {
        self.subscribers.get(block_id)
    }

    /// Resolve a filesystem path back to its block_id.
    ///
    /// Used by `BlockFanoutRouter` to map file-change events from the
    /// filesystem watcher to their corresponding block_id in the cache.
    pub(crate) fn resolve_block_id_from_path(&self, path: &std::path::Path) -> Option<String> {
        self.path_to_block_id.get(path).map(|e| e.value().clone())
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
                // Prefer the stored handle (set by attach() at construction).
                // Fall back to Handle::try_current() for callers that happen
                // to run inside an ambient runtime; warn if neither.
                let handle = self
                    .tokio_handle
                    .clone()
                    .or_else(|| tokio::runtime::Handle::try_current().ok());
                match handle {
                    Some(handle) => match std::thread::scope(|s| {
                        let provider = provider.clone();
                        let q = query.to_string();
                        s.spawn(move || handle.block_on(provider.embed_query(&q))).join()
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
                    },
                    None => {
                        tracing::warn!(
                            "No tokio handle (stored or ambient) for query embedding, falling back to FTS"
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

            let resolve = |block_id: &str| {
                self.blocks.get(block_id).map(|cb| {
                    let agent_key = cb.doc.agent_id().to_string();
                    let scope = pattern_core::types::memory_types::Scope::from_db_key(&agent_key)
                        .unwrap_or_else(|| pattern_core::types::memory_types::Scope::global(&agent_key));
                    (scope, smol_str::SmolStr::from(cb.doc.label()))
                })
            };
            return Ok(all_results
                .into_iter()
                .map(|r| db_search_result_to_core(r, &resolve))
                .collect());
        }

        // Execute search.
        let results = builder.execute().mem()?;

        tracing::debug!(
            "search_impl: agent_id_filter={:?} content_types={:?} mode={:?} embedding_present={} returned={}",
            agent_id_filter,
            options.content_types,
            effective_mode,
            query_embedding.is_some(),
            results.len()
        );
        for r in &results {
            tracing::debug!("search_impl result: {:?}", r);
        }

        let resolve = |block_id: &str| {
            self.blocks.get(block_id).map(|cb| {
                let agent_key = cb.doc.agent_id().to_string();
                let scope = pattern_core::types::memory_types::Scope::from_db_key(&agent_key)
                    .unwrap_or_else(|| pattern_core::types::memory_types::Scope::global(&agent_key));
                (scope, smol_str::SmolStr::from(cb.doc.label()))
            })
        };
        Ok(results.into_iter().map(|r| db_search_result_to_core(r, &resolve)).collect())
    }
}

impl MemoryCache {
    // ========== Fork / isolation helpers ==========

    /// Insert a pre-built `CachedBlock` directly into the cache map.
    ///
    /// `pub(crate)` — only used by [`fork_for_child`](Self::fork_for_child).
    /// This bypasses the DB-backed load path intentionally: the forked doc
    /// is not a DB row yet; it lives in memory until an explicit persist.
    pub(crate) fn insert_cached_block(&self, block_id: String, block: CachedBlock) {
        self.blocks.insert(block_id, block);
    }

    /// Return the number of blocks currently held in the in-memory cache.
    ///
    /// Useful for tests and diagnostics. Does not trigger DB access.
    pub fn cached_block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Look up a cached block by the owning agent's ID and the block label,
    /// returning a cloned `StructuredDocument` if the block is in memory.
    ///
    /// Unlike [`get`](Self::get), this does NOT consult the database — it
    /// only scans the in-memory map. Returns `None` when:
    /// - the block has not yet been loaded (cache miss), or
    /// - no in-memory block matches both `agent_id` and `label`.
    ///
    /// Primarily used by the fork/merge path where a forked child cache holds
    /// docs that have no corresponding DB row yet.
    pub fn get_cached_doc(&self, agent_id: &str, label: &str) -> Option<StructuredDocument> {
        for entry in self.blocks.iter() {
            let cached = entry.value();
            if cached.doc.agent_id() == agent_id && cached.doc.label() == label {
                return Some(cached.doc.clone());
            }
        }
        None
    }

    /// Return all in-memory cached documents as a snapshot.
    ///
    /// Returns a `Vec` of cloned `StructuredDocument` instances for every
    /// block currently held in the in-memory map. Used by
    /// `merge_back_lightweight` to walk the child's blocks without requiring a
    /// DB round-trip.
    ///
    /// Cloning a `StructuredDocument` is cheap because `LoroDoc` is
    /// internally reference-counted.
    pub fn snapshot_cached_docs(&self) -> Vec<StructuredDocument> {
        self.blocks
            .iter()
            .map(|entry| entry.value().doc.clone())
            .collect()
    }

    /// Insert a block into the cache from a raw Loro snapshot byte slice.
    ///
    /// Used by `merge_back_lightweight` when the fork created a block that
    /// does not yet exist on the parent side. The block is registered in the
    /// in-memory map only — it becomes a DB row on the next `persist()` call.
    ///
    /// `agent_id` and `label` are used to reconstruct the block metadata.
    /// `schema` and `block_type` must match the originating document — passing
    /// the wrong schema causes the subscriber worker to misrender the block on
    /// the next persist cycle.
    pub fn insert_from_snapshot(
        &self,
        agent_id: &str,
        label: String,
        snapshot: Vec<u8>,
        schema: pattern_core::types::memory_types::BlockSchema,
        block_type: MemoryBlockType,
    ) -> Result<(), MemoryError> {
        use pattern_core::memory::StructuredDocument;
        use pattern_core::types::memory_types::BlockMetadata;
        use uuid::Uuid;

        let mut metadata = BlockMetadata::standalone(schema);
        metadata.id = Uuid::new_v4().to_string();
        metadata.agent_id = agent_id.to_string();
        metadata.label = label;
        metadata.block_type = block_type;

        let doc = StructuredDocument::from_snapshot_with_metadata(&snapshot, metadata, None)
            .map_err(|e| MemoryError::Other(e.to_string()))?;

        let block_id = doc.id().to_string();
        self.insert_cached_block(
            block_id,
            CachedBlock {
                doc,
                last_seq: 0,
                last_persisted_frontier: None,
                dirty: true,
                last_accessed: Utc::now(),
            },
        );
        Ok(())
    }

    /// Fork every block whose embedded `agent_id` matches `parent_agent`,
    /// producing a new `MemoryCache` over the forked `LoroDoc` instances.
    ///
    /// Shared infrastructure (DB handle) is Arc-cloned cheaply. Foreign-owned
    /// blocks (owned by agents other than `parent_agent`) are skipped — the
    /// child cache contains only blocks the parent itself owns, retagged with
    /// `child_agent` as the new owner.
    ///
    /// The child cache starts with `dirty = false` on all blocks because the
    /// parent's pending in-memory writes have NOT been transferred — only the
    /// committed CRDT state is forked. This is intentional: a fork is a
    /// snapshot of the committed state, not a capture of in-flight edits.
    pub fn fork_for_child(
        &self,
        parent_agent: &str,
        child_agent: &str,
    ) -> Result<MemoryCache, MemoryError> {
        let child = MemoryCache::new(Arc::clone(&self.db));
        for entry in self.blocks.iter() {
            let (block_id, cached) = (entry.key().clone(), entry.value());
            if cached.doc.agent_id() != parent_agent {
                continue;
            }
            let mut forked_doc = cached.doc.fork();
            forked_doc.retag_owner(child_agent);
            child.insert_cached_block(
                block_id,
                CachedBlock {
                    doc: forked_doc,
                    last_seq: cached.last_seq,
                    last_persisted_frontier: cached.last_persisted_frontier.clone(),
                    // Fork starts clean — parent's pending writes do not transfer.
                    dirty: false,
                    last_accessed: Utc::now(),
                },
            );
        }
        Ok(child)
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
// ---------------------------------------------------------------------------
// Block file path helpers
// ---------------------------------------------------------------------------

/// Compute the canonical filesystem path for a block file.
///
/// Path dispatch by [`Scope`]:
///
/// - `Scope::Local(_)`: `<mount>/blocks/<type_dir>/<label>.<ext>`. Project
///   blocks live directly under the mount's `blocks/` dir without a per-
///   agent subdir — they're shared workspace state across the constellation.
/// - `Scope::Global(persona_id)`: when `persona_state_dir` is provided,
///   `<persona_state_dir>/@<persona_id>/blocks/<type_dir>/<label>.<ext>`.
///   When not provided (no XDG state available), falls back to the
///   in-mount path `<mount>/blocks/@<persona_id>/<type_dir>/<label>.<ext>`
///   for back-compat with unmounted dev sessions.
///
/// Agent subdirectories are created lazily by the caller; this function
/// only computes the path.
fn block_file_path(
    mount_path: &std::path::Path,
    persona_state_dir: Option<&std::path::Path>,
    scope: &Scope,
    block_type: MemoryBlockType,
    label: &str,
    ext: &str,
) -> PathBuf {
    let type_dir = match block_type {
        MemoryBlockType::Core => "core",
        MemoryBlockType::Working => "working",
        _ => "working", // Future block types default to working directory
    };
    let safe_label = sanitize_block_label(label);
    match scope {
        Scope::Local(_) => mount_path
            .join("blocks")
            .join(type_dir)
            .join(format!("{safe_label}.{ext}")),
        Scope::Global(persona_id) => {
            let base = persona_state_dir
                .map(|p| p.join(format!("@{persona_id}")))
                .unwrap_or_else(|| mount_path.join("blocks").join(format!("@{persona_id}")));
            // When using persona_state_dir, layout is
            // `<base>/blocks/<type>/<label>.<ext>`. When falling back to
            // the mount, the path is already `<mount>/blocks/@<id>/`, so
            // we skip the extra `blocks/` segment for back-compat.
            if persona_state_dir.is_some() {
                base.join("blocks")
                    .join(type_dir)
                    .join(format!("{safe_label}.{ext}"))
            } else {
                base.join(type_dir).join(format!("{safe_label}.{ext}"))
            }
        }
    }
}

/// Sanitize a block label for use as a filename.
///
/// Allows alphanumeric, hyphen, underscore, and dot. Everything else
/// becomes a hyphen. Consecutive hyphens are collapsed.
fn sanitize_block_label(label: &str) -> String {
    let raw: String = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse consecutive hyphens.
    let mut result = String::with_capacity(raw.len());
    let mut prev_hyphen = false;
    for c in raw.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result
}

pub(crate) fn spawn_subscriber_for_block(
    block_id: &str,
    schema: BlockSchema,
    doc: &StructuredDocument,
    reembed_tx: tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
    heartbeat_tx: crossbeam_channel::Sender<Heartbeat>,
    mount_path: Arc<PathBuf>,
    persona_state_dir: Option<Arc<PathBuf>>,
    db: Arc<ConstellationDb>,
    subscribers: Arc<DashMap<String, SubscriberHandle>>,
    block_change_notifier: crate::subscriber::BlockChangeNotifier,
    observer: pattern_core::observer::MemoryObserver,
    path_to_block_id: Arc<DashMap<PathBuf, String>>,
) {
    // Don't double-spawn.
    if subscribers.contains_key(block_id) {
        return;
    }

    let (event_tx, event_rx) = crossbeam_channel::bounded(64);
    let cancel = CancellationToken::new();

    // Shared pause state for flush-pause-resume quiesce.
    let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pause_complete = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let resume_signal = Arc::new((Mutex::new(false), std::sync::Condvar::new()));

    // Recover the doc's typed Scope from the encoded `agent_id` it carries.
    // Pre-Phase-1 docs that haven't been migrated have a bare agent_id; we
    // treat those as `Scope::Global(agent_id)` so they keep working.
    let doc_scope =
        Scope::from_db_key(doc.agent_id()).unwrap_or_else(|| Scope::Global(doc.agent_id().into()));

    // Determine the canonical file extension for this schema so we can compute
    // the block file path for the SyncedDoc. The extension must match what
    // render_canonical_from_disk_doc would return for this schema.
    let ext = block_schema_extension(&schema);
    let file_path = block_file_path(
        &mount_path,
        persona_state_dir.as_deref().map(|p| p.as_path()),
        &doc_scope,
        doc.block_type(),
        doc.label(),
        &ext,
    );
    // Ensure the agent/type directory exists (lazy creation).
    if let Some(parent) = file_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                block_id = %block_id,
                path = ?parent,
                error = %e,
                "failed to create block directory; file sync disabled for this block"
            );
            return;
        }
    }
    // Register the reverse mapping (path -> block_id) for the filesystem watcher.
    path_to_block_id.insert(file_path.clone(), block_id.to_string());

    // Build the SyncedDoc for this block. RouterOwned mode: no internal
    // filesystem watcher (the mount-wide DirWatcher<BlockFanoutRouter> handles
    // external edit routing) and no internal local-update subscription (the
    // worker's CommitEvent channel handles that). The SyncedDoc owns disk_doc,
    // echo-suppression state, atomic_write, and last_saved_frontier.
    //
    // LoroDoc::clone is a reference clone — it shares the same underlying
    // state as doc.inner(). This means SyncedDoc's memory_doc IS the same
    // Loro state as the StructuredDocument's doc, so apply_external_bytes
    // correctly propagates external edits into the live memory_doc.
    // Single-doc world: SyncedDoc takes the LoroDoc directly (Loro is
    // internally Arc'd). The block subscriber holds an `Arc<SyncedDoc>` and
    // can call write_local for synchronous disk persistence.
    let synced_doc_loro = doc.inner().clone();
    let bridge = Arc::new(crate::subscriber::bridge::BlockSchemaBridge::new(
        schema.clone(),
    ));
    let synced_doc =
        match crate::loro_sync::SyncedDoc::open_router_owned(crate::loro_sync::SyncedDocConfig {
            path: file_path,
            doc: synced_doc_loro,
            bridge,
            event_channel_bound: 64,
            // Block-subscriber path: external edits arrive via
            // `apply_external_bytes` (which bypasses the watcher-based
            // conflict check entirely), not through the watcher. AutoMerge
            // here is explicit rather than implicit — the policy field is
            // checked only for watcher-delivered events.
            conflict_policy: crate::loro_sync::ConflictPolicy::AutoMerge,
        }) {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::error!(
                    block_id = %block_id,
                    error = %e,
                    "failed to open SyncedDoc for block; file sync disabled"
                );
                metrics::counter!("memory.sync_worker.spawn_failed").increment(1);
                return;
            }
        };

    // Wire subscribe_local_update on memory_doc: when the agent writes
    // to memory_doc, capture the raw Loro update bytes and forward them
    // to the worker thread for import into disk_doc and file rendering.
    // When paused, skip try_send — writes accumulate in memory_doc and
    // are reconciled via version-vector diff on resume.
    //
    // We subscribe on the StructuredDocument's inner LoroDoc directly
    // (not synced_doc.memory_doc(), which is the same shared state).
    // RouterOwned mode does not set up a local-update subscription inside
    // SyncedDoc, so this is the only subscription on the memory_doc.
    let block_id_owned = block_id.to_string();
    let tx_clone = event_tx.clone();
    let paused_flag = Arc::clone(&paused);
    // Observer-side: build the BlockAddr from the doc's scope + label so
    // cross-block observers (MemorySync handlers etc) can filter and route
    // by stable wire-side addressing. Cloning the observer is cheap (Arc'd
    // internally); the closure owns its own handle to publish on.
    let observer_for_closure = observer.clone();
    let block_addr_for_closure = pattern_core::types::memory_types::BlockAddr {
        scope: doc_scope.clone(),
        label: doc.label().into(),
    };
    let subscription = doc
        .inner()
        .subscribe_local_update(Box::new(move |update_bytes| {
            if !paused_flag.load(std::sync::atomic::Ordering::Acquire) {
                // Persistence path (per-block crossbeam, bounded-blocking, no drops).
                let _ = tx_clone.try_send(crate::subscriber::event::CommitEvent {
                    block_id: block_id_owned.clone(),
                    update_bytes: update_bytes.clone(),
                });
                // Observer path (tokio broadcast, drop-on-lag, origin=None for
                // local agent edits). Imported plugin deltas don't fire this
                // callback (loro's subscribe_local_update is local-only) so
                // origin=None is the right default here.
                observer_for_closure.publish(
                    pattern_core::observer::MemoryEvent::Delta {
                        addr: block_addr_for_closure.clone(),
                        update_bytes: update_bytes.clone(),
                        origin: None,
                    },
                );
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
        doc: doc.clone(),
        paused: Arc::clone(&paused),
        pause_complete: Arc::clone(&pause_complete),
        resume_signal: Arc::clone(&resume_signal),
        block_change_notifier: block_change_notifier.clone(),
        synced_doc: Arc::clone(&synced_doc),
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
            paused,
            pause_complete,
            resume_signal,
            synced_doc,
        },
    );
}

/// Return the canonical file extension for a block schema.
///
/// Mirrors the extension returned by
/// [`render_canonical_from_disk_doc`](crate::subscriber::worker::render_canonical_from_disk_doc).
fn block_schema_extension(schema: &BlockSchema) -> &'static str {
    match schema {
        BlockSchema::Text { .. } | BlockSchema::Skill { .. } => "md",
        BlockSchema::Map { .. }
        | BlockSchema::Composite { .. }
        | BlockSchema::List { .. }
        | BlockSchema::TaskList { .. } => "kdl",
        BlockSchema::Log { .. } => "jsonl",
        _ => "dat",
    }
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
pub(crate) fn apply_json_to_loro_doc(
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
        // Skill blocks are NOT imported via the JSON path — the external-edit
        // inbound path calls `write_skill_to_loro_doc` directly after parsing
        // via `markdown_skill::parse`. This arm is structurally unreachable
        // through normal code paths. If it is ever reached, that indicates a
        // logic error in the caller (e.g., a new code site that constructs a
        // JSON payload and calls this function for a Skill schema without going
        // through the YAML-frontmatter pipeline). Return a clear error.
        (_, pattern_core::types::memory_types::BlockSchema::Skill { .. }) => Err(
            "apply_json_to_loro_doc must not be called for Skill blocks: use \
             write_skill_to_loro_doc (markdown_skill::loro_bridge) instead"
                .to_string(),
        ),
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
        created_at: chrono_to_jiff(block.created_at),
        updated_at: chrono_to_jiff(block.updated_at),
    }
}

/// Helper function to convert DB ArchivalEntry to our ArchivalEntry.
fn db_archival_to_archival(entry: &pattern_db::models::ArchivalEntry) -> ArchivalEntry {
    ArchivalEntry {
        id: entry.id.clone(),
        agent_id: entry.agent_id.clone(),
        content: entry.content.clone(),
        metadata: entry.metadata.as_ref().map(|j| j.0.clone()),
        created_at: chrono_to_jiff(entry.created_at),
    }
}

impl MemoryStore for MemoryCache {
    fn observer(&self) -> Option<&pattern_core::observer::MemoryObserver> {
        Some(&self.observer)
    }

    fn push_external_commit(
        &self,
        scope: &pattern_core::types::memory_types::Scope,
        label: &str,
        update_bytes: Vec<u8>,
    ) -> pattern_core::error::MemoryResult<()> {
        // Resolve (scope, label) → block_id via the cached block. If the block
        // isn't loaded, we have nowhere to send the commit; that's a bug at the
        // caller (they should have loaded it before importing), so warn + skip.
        let cached_id = self
            .blocks
            .iter()
            .find(|entry| {
                let cb = entry.value();
                let doc_scope = pattern_core::types::memory_types::Scope::from_db_key(cb.doc.agent_id())
                    .unwrap_or_else(|| pattern_core::types::memory_types::Scope::Global(cb.doc.agent_id().into()));
                doc_scope == *scope && cb.doc.label() == label
            })
            .map(|entry| entry.key().clone());
        let Some(block_id) = cached_id else {
            tracing::warn!(scope = ?scope, label = %label, "push_external_commit: block not loaded; skip");
            return Ok(());
        };

        // Lazy-spawn the per-block subscriber if needed (no-op if already up,
        // or if mount_path-less so subscriber machinery is disabled).
        self.maybe_spawn_subscriber_for_block(&block_id);

        // Push the CommitEvent on the subscriber's crossbeam channel. Worker
        // picks it up + runs disk render + FTS5 + embed exactly like a
        // local-edit-driven event.
        if let Some(handle) = self.subscribers.get(&block_id) {
            handle
                .event_tx
                .try_send(crate::subscriber::event::CommitEvent {
                    block_id: block_id.clone(),
                    update_bytes,
                })
                .map_err(|e| pattern_core::error::MemoryError::Other(format!(
                    "push_external_commit: try_send: {e}"
                )))?;
        } else {
            // No subscriber even after maybe_spawn — likely no mount_path
            // configured, so persistence is disabled for this store. Quiet skip.
            tracing::debug!(block_id = %block_id, "push_external_commit: no subscriber (persistence disabled)");
        }
        Ok(())
    }

    fn create_block(&self, scope: &Scope, create: BlockCreate) -> MemoryResult<StructuredDocument> {
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
        let now_jiff = chrono_to_jiff(now);

        // Encode scope as prefixed string for DB storage. The cache's
        // in-memory lookups also compare against this encoded form via
        // `doc.agent_id()` so Local("x") and Global("x") never collide.
        let agent_id = scope.to_db_key();

        // Build BlockMetadata.
        let block_metadata = BlockMetadata {
            id: block_id.clone(),
            agent_id: agent_id.clone(),
            label: label.clone(),
            description: description.clone(),
            block_type,
            schema: schema.clone(),
            char_limit: effective_char_limit,
            permission,
            pinned: false,
            created_at: now_jiff,
            updated_at: now_jiff,
        };

        // Create new StructuredDocument with metadata. Mutable because the
        // soft-delete-undelete path may need to swap the id later (after we
        // discover an existing inactive row to reactivate).
        let doc =
            StructuredDocument::new_with_metadata(block_metadata.clone(), Some(agent_id.clone()));

        // For Skill blocks, initialize the "metadata" and "extras" LoroMap
        // containers with sensible defaults so the subscriber worker can
        // render the block immediately without encountering a missing-metadata
        // error. Without this step, `project_metadata_from_loro` would fail
        // on the first render cycle and increment `fts_update_failed`.
        //
        // We use `label` as the skill name because:
        //   - It's the canonical human-readable identifier for the block.
        //   - It's always non-empty (required by BlockCreate validation).
        //   - It survives without the user having to call write_skill_to_loro_doc.
        if let pattern_core::types::memory_types::BlockSchema::Skill { .. } = &schema {
            let loro_doc = doc.inner();
            let metadata_map = loro_doc.get_map("metadata");
            metadata_map
                .insert(
                    "name",
                    loro::LoroValue::String(block_metadata.label.clone().into()),
                )
                .map_err(|e| {
                    MemoryError::Other(format!(
                        "Skill create_block: metadata insert('name') failed: {e}"
                    ))
                })?;
            metadata_map
                .insert("trust_tier", loro::LoroValue::String("ad-hoc".into()))
                .map_err(|e| {
                    MemoryError::Other(format!(
                        "Skill create_block: metadata insert('trust_tier') failed: {e}"
                    ))
                })?;
            // Initialize description, keywords_json, and hooks_json to their
            // empty/null defaults so the projection helpers always find them.
            metadata_map
                .insert("description", loro::LoroValue::Null)
                .map_err(|e| {
                    MemoryError::Other(format!(
                        "Skill create_block: metadata insert('description') failed: {e}"
                    ))
                })?;
            metadata_map
                .insert("keywords_json", loro::LoroValue::String("[]".into()))
                .map_err(|e| {
                    MemoryError::Other(format!(
                        "Skill create_block: metadata insert('keywords_json') failed: {e}"
                    ))
                })?;
            metadata_map
                .insert("hooks_json", loro::LoroValue::Null)
                .map_err(|e| {
                    MemoryError::Other(format!(
                        "Skill create_block: metadata insert('hooks_json') failed: {e}"
                    ))
                })?;
            // Touch the "extras" map so it exists (empty) in the snapshot.
            let _extras_map = loro_doc.get_map("extras");
            loro_doc.commit();
        }

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
        let mut db_block = pattern_db::models::MemoryBlock {
            id: block_id.clone(),
            agent_id: agent_id.clone(),
            label,
            description: description.clone(),
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

        // Soft-delete + Memory.create idempotency: if a block with the same
        // (agent_id, label) exists but is_active = false, reuse its row
        // rather than failing on the UNIQUE(agent_id, label) constraint.
        //
        // CRDT semantic: the reactivation is treated as another edit on the
        // existing loro doc, NOT a fresh start. We hydrate the soft-deleted
        // block's doc from snapshot + outstanding updates, then apply any
        // metadata changes from the new BlockCreate (description, char_limit,
        // permission). Subsequent content writes from the caller (typically
        // `write_text_into` + `persist_block` in the SDK Memory.Create handler)
        // become CRDT operations that advance the doc's version vector,
        // generating new `memory_block_updates` rows at `last_seq + 1`,
        // `last_seq + 2`, ... — no collision on the `(block_id, seq)` UNIQUE
        // constraint with the prior incarnation's update history.
        //
        // Schema mismatch errors. The loro doc structure is schema-bound
        // (Text vs Map vs List vs Log); reusing a Text doc with a Map schema
        // would produce a doc whose container layout disagrees with its
        // metadata. The loro library itself can technically handle mixed
        // containers, but the resulting block would be silently broken from
        // the agent's perspective. Surface the mismatch as a typed error.
        //
        // If the existing row IS active, fall through to `create_block` which
        // surfaces the UNIQUE(agent_id, label) conflict as a typed error.
        let existing = pattern_db::queries::get_block_by_label(
            &*self.db.get().mem()?,
            &agent_id,
            &db_block.label,
        )
        .mem()?;

        let (mut final_doc, cached) = if let Some(prev) = existing {
            if !prev.is_active {
                // ---- Reactivation path: hydrate prev doc, apply metadata diffs ----

                // Schema must match. db_block_to_metadata extracts the schema
                // from the stored metadata JSON; compare against the new
                // BlockCreate's schema (carried on `block_metadata`).
                let prev_metadata = db_block_to_metadata(&prev);
                if !Self::schema_compatible_static(&prev_metadata.schema, &block_metadata.schema) {
                    return Err(MemoryError::Other(format!(
                        "create_block: cannot reactivate soft-deleted block {label:?} \
                         with a different schema (was {prev_schema:?}, requested {new_schema:?}). \
                         Use a different label, or restore the prior schema.",
                        label = db_block.label,
                        prev_schema = prev_metadata.schema,
                        new_schema = block_metadata.schema,
                    )));
                }

                // Rebuild the prior incarnation's doc + cached state.
                let mut hydrated = self.hydrate_doc_from_db(&prev, permission)?;

                // Apply metadata diffs from the new BlockCreate. The doc's
                // BlockMetadata is mutated in place; loro state (snapshot,
                // frontier, last_seq) is preserved.
                {
                    let meta = hydrated.doc.metadata_mut();
                    meta.description = description.clone();
                    meta.char_limit = effective_char_limit;
                    meta.permission = permission;
                    meta.block_type = block_type;
                    meta.updated_at = now_jiff;
                }
                hydrated.dirty = true;
                hydrated.last_accessed = now;

                // Build a MemoryBlock for reactivate_block that carries the
                // new metadata fields BUT preserves prev's loro state
                // (snapshot, frontier, last_seq). This way the row's metadata
                // is overwritten with caller-supplied values while the CRDT
                // history continues from where it was.
                db_block.id = prev.id.clone();
                db_block.loro_snapshot = prev.loro_snapshot.clone();
                db_block.frontier = prev.frontier.clone();
                db_block.last_seq = prev.last_seq;
                db_block.created_at = prev.created_at;

                let updated = pattern_db::queries::reactivate_block(
                    &*self.db.get().mem()?,
                    &prev.id,
                    &db_block,
                )
                .mem()?;
                if updated == 0 {
                    return Err(MemoryError::Other(format!(
                        "reactivate_block: row vanished between get and update for id {}",
                        prev.id
                    )));
                }

                let returned_doc = hydrated.doc.clone();
                (returned_doc, hydrated)
            } else {
                // Active row exists — surface the UNIQUE conflict.
                pattern_db::queries::create_block(&*self.db.get().mem()?, &db_block).mem()?;
                let cached = CachedBlock {
                    doc: doc.clone(),
                    last_seq: 0,
                    last_persisted_frontier: None,
                    dirty: false,
                    last_accessed: now,
                };
                (doc, cached)
            }
        } else {
            // ---- Fresh-create path: no prior row ----
            pattern_db::queries::create_block(&*self.db.get().mem()?, &db_block).mem()?;
            let cached = CachedBlock {
                doc: doc.clone(),
                last_seq: 0,
                last_persisted_frontier: None,
                dirty: false,
                last_accessed: now,
            };
            (doc, cached)
        };

        let block_id = db_block.id.clone();
        // Ensure the doc's metadata.id matches the canonical id (matters on
        // the reactivation path where we adopt prev.id).
        if final_doc.metadata().id != block_id {
            final_doc.metadata_mut().id = block_id.clone();
        }

        self.blocks.insert(block_id, cached);

        Ok(final_doc)
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        // Delegate to existing get method using the encoded db key.
        self.get(&scope.to_db_key(), label)
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        // Query DB for block metadata without loading full document.
        let key = scope.to_db_key();
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, &key, label).mem()?;

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

    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: pattern_core::types::block::BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let key = scope.to_db_key();
        // Hard-delete from DB (not soft-delete) so create_block succeeds.
        let conn = self.db.get().mem()?;
        conn.execute(
            "DELETE FROM memory_blocks WHERE agent_id = ?1 AND label = ?2",
            rusqlite::params![key, create.label],
        )
        .map_err(|e| MemoryError::Other(format!("hard delete for replace: {e}")))?;
        // Also remove from in-memory cache if present.
        if let Ok(Some(block)) =
            pattern_db::queries::get_block_by_label(&*conn, &key, &create.label)
        {
            self.blocks.remove(&block.id);
        }
        drop(conn);
        self.create_block(scope, create)
    }

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        // Get block ID first.
        let key = scope.to_db_key();
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, &key, label).mem()?;

        if let Some(block) = block {
            // Drop from cache first (will persist if dirty and cancel subscriber).
            if self.blocks.contains_key(&block.id) {
                self.drop_doc(&key, label)?;
            }

            // Soft-delete in DB.
            pattern_db::queries::deactivate_block(&*self.db.get().mem()?, &block.id).mem()?;
        }

        Ok(())
    }

    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        // Get doc, call doc.render().
        let doc = self.get(&scope.to_db_key(), label)?;
        Ok(doc.map(|d| d.render()))
    }

    fn persist_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        // Delegate to existing persist method.
        self.persist(&scope.to_db_key(), label)
    }

    fn commit_write(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        let key = scope.to_db_key();
        MemoryCache::mark_dirty_checked(self, &key, label, scope)?;
        self.persist(&key, label)?;
        if let Ok(Some(block)) =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, &key, label)
        {
            self.maybe_spawn_subscriber_for_block(&block.id);
        }
        Ok(())
    }

    fn mark_dirty(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        // Delegate to existing method, but propagate failure as a typed
        // error rather than silently no-opping. Phase-1 redesign: callers
        // routing the wrong scope no longer get a silent miss.
        MemoryCache::mark_dirty_checked(self, &scope.to_db_key(), label, scope)
    }

    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        // Generate archival entry ID.
        let entry_id = format!("arch_{}", Uuid::new_v4().simple());

        // Create archival entry.
        let entry = pattern_db::models::ArchivalEntry {
            id: entry_id.clone(),
            agent_id: scope.to_db_key(),
            content: content.to_string(),
            metadata: metadata.map(pattern_db::Json),
            chunk_index: 0,
            parent_entry_id: None,
            created_at: Utc::now(),
        };

        // Store in DB.
        pattern_db::queries::create_archival_entry(&*self.db.get().mem()?, &entry).mem()?;

        // Push a re-embed request so the vector arm of hybrid retrieval can
        // match this entry. Without this, archival inserts go to FTS only —
        // search hits are limited to literal-word overlap. Drop the send if
        // the queue isn't configured (no embedding provider in test setups).
        if let Some(tx) = &self.reembed_tx {
            let bytes = content.as_bytes().to_vec();
            let hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
            let _ = tx.send(crate::subscriber::event::ReembedRequest {
                block_id: entry_id.clone(),
                content_type: pattern_db::vector::ContentType::ArchivalEntry,
                canonical_bytes: bytes,
                content_hash: hash,
            });
        }

        Ok(entry_id)
    }

    fn search_archival(
        &self,
        scope: &Scope,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        // Hybrid retrieval: compute query embedding (if provider available)
        // so the vector arm of execute_hybrid actually fires. Without this,
        // the search builder receives only a text query and falls into the
        // FTS-only branch even when SearchMode::Hybrid is requested.
        let query_embedding = if let Some(provider) = &self.embedding_provider {
            // Prefer the stored handle (set by callers via with_tokio_handle).
            // Fall back to Handle::try_current() for callers that happen to
            // run inside an ambient runtime; warn if neither is available.
            let handle = self
                .tokio_handle
                .clone()
                .or_else(|| tokio::runtime::Handle::try_current().ok());
            match handle {
                Some(handle) => match std::thread::scope(|s| {
                    let provider = provider.clone();
                    let q = query.to_string();
                    s.spawn(move || handle.block_on(provider.embed_query(&q))).join()
                }) {
                    Ok(Ok(emb)) => Some(emb),
                    Ok(Err(e)) => {
                        tracing::warn!(
                            "archival query embedding failed, falling back to FTS-only: {}",
                            e
                        );
                        None
                    }
                    Err(_) => {
                        tracing::warn!(
                            "archival query embedding thread panicked, falling back to FTS-only"
                        );
                        None
                    }
                },
                None => {
                    tracing::warn!(
                        "no tokio handle (stored or ambient) for archival query embedding, falling back to FTS-only"
                    );
                    None
                }
            }
        } else {
            None
        };

        let search_conn = self.db.get().mem()?;
        let key = scope.to_db_key();
        tracing::debug!("search_archival agent id used: {key}");
        let mut builder = pattern_db::search::search(&search_conn)
            .text(query)
            .mode(pattern_db::search::SearchMode::Hybrid)
            .limit(limit as i64)
            .filter(pattern_db::search::ContentFilter::archival(Some(&key)));
        if let Some(ref emb) = query_embedding {
            builder = builder.embedding(emb);
        }
        let results = builder.execute().mem()?;

        // Convert search results to ArchivalEntry.
        let mut entries = Vec::new();
        for result in results {
            tracing::debug!("search_archival result: {:?}", result);
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
            MemorySearchScope::Scope(ref s) => {
                let key = s.to_db_key();
                self.search_impl(Some(&key), query, options)
            }
            MemorySearchScope::Constellation => self.search_impl(None, query, options),
            _ => Err(MemoryError::Other(
                "unsupported search scope variant".into(),
            )),
        }
    }

    fn list_shared_blocks(&self, scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        let key = scope.to_db_key();
        let shared = pattern_db::queries::get_shared_blocks(&*self.db.get().mem()?, &key).mem()?;

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
        requester: &Scope,
        owner: &Scope,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // 1. Check access FIRST - DB is source of truth.
        let requester_key = requester.to_db_key();
        let owner_key = owner.to_db_key();
        let access_result = pattern_db::queries::check_block_access(
            &*self.db.get().mem()?,
            &requester_key,
            &owner_key,
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
        let block = self.load_from_db(&owner_key, label, shared_permission)?;

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
        scope: &Scope,
        label: &str,
        patch: BlockMetadataPatch,
    ) -> MemoryResult<()> {
        if patch.is_empty() {
            return Ok(());
        }

        // Get block from DB.
        let key = scope.to_db_key();
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, &key, label).mem()?;

        let block = block.ok_or_else(|| MemoryError::WriteToMissingBlock {
            scope: scope.clone(),
            label: label.to_string(),
            op: "update_block_metadata".to_string(),
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

    fn undo_redo(&self, scope: &Scope, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        // Get block ID from DB.
        let key = scope.to_db_key();
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, &key, label).mem()?;

        let block = block.ok_or_else(|| MemoryError::WriteToMissingBlock {
            scope: scope.clone(),
            label: label.to_string(),
            op: "undo_redo".to_string(),
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

    fn history_depth(&self, scope: &Scope, label: &str) -> MemoryResult<UndoRedoDepth> {
        let key = scope.to_db_key();
        let block =
            pattern_db::queries::get_block_by_label(&*self.db.get().mem()?, &key, label).mem()?;

        let block = block.ok_or_else(|| MemoryError::WriteToMissingBlock {
            scope: scope.clone(),
            label: label.to_string(),
            op: "history_depth".to_string(),
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

        // Missing block: read path returns Ok(None) per the trait contract.
        let doc = cache.get("agent_1", "nonexistent").unwrap();
        assert!(doc.is_none());
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
                &Scope::global("agent_1"),
                BlockCreate::new("test_block", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Test block description")
                    .with_char_limit(1000),
            )
            .unwrap();

        assert!(created_doc.id().starts_with("mem_"));

        // Get the block back (should return same doc since it's cached).
        let doc = cache
            .get_block(&Scope::global("agent_1"), "test_block")
            .unwrap();
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
                &Scope::global("agent_1"),
                BlockCreate::new("block1", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("First block")
                    .with_char_limit(1000),
            )
            .unwrap();

        cache
            .create_block(
                &Scope::global("agent_1"),
                BlockCreate::new("block2", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Second block")
                    .with_char_limit(2000),
            )
            .unwrap();

        cache
            .create_block(
                &Scope::global("agent_1"),
                BlockCreate::new("block3", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Third block")
                    .with_char_limit(1500),
            )
            .unwrap();

        // List all blocks.
        let all_blocks = cache
            .list_blocks(BlockFilter::by_agent(Scope::global("agent_1").to_db_key()))
            .unwrap();
        assert_eq!(all_blocks.len(), 3);

        // List blocks by type.
        let core_blocks = cache
            .list_blocks(BlockFilter::by_type(
                Scope::global("agent_1").to_db_key(),
                MemoryBlockType::Core,
            ))
            .unwrap();
        assert_eq!(core_blocks.len(), 2);

        let working_blocks = cache
            .list_blocks(BlockFilter::by_type(
                Scope::global("agent_1").to_db_key(),
                MemoryBlockType::Working,
            ))
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
                &Scope::global("agent_1"),
                BlockCreate::new("to_delete", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Will be deleted")
                    .with_char_limit(1000),
            )
            .unwrap();

        // Verify it exists.
        let doc = cache
            .get_block(&Scope::global("agent_1"), "to_delete")
            .unwrap();
        assert!(doc.is_some());

        // Delete it.
        cache
            .delete_block(&Scope::global("agent_1"), "to_delete")
            .unwrap();

        // Verify it's gone (soft delete → get_block returns Ok(None)).
        let doc = cache
            .get_block(&Scope::global("agent_1"), "to_delete")
            .unwrap();
        assert!(doc.is_none());

        // List should not include deleted block.
        let blocks = cache
            .list_blocks(BlockFilter::by_agent(Scope::global("agent_1").to_db_key()))
            .unwrap();
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn test_create_block_undeletes_soft_deleted() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create, delete, then recreate with the same label. Without the
        // undelete-on-create logic this would fail with a UNIQUE conflict
        // (the soft-deleted row keeps the label reserved).
        let scope = Scope::global("agent_1");
        cache
            .create_block(
                &scope,
                BlockCreate::new("reusable", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("first incarnation")
                    .with_char_limit(1000),
            )
            .unwrap();

        // Capture the id so we can verify reactivation reuses it.
        let first = cache.get_block(&scope, "reusable").unwrap().unwrap();
        let first_id = first.metadata().id.clone();

        cache.delete_block(&scope, "reusable").unwrap();
        // Confirm read path treats it as gone.
        assert!(cache.get_block(&scope, "reusable").unwrap().is_none());

        // Recreate — must succeed, must reuse the same id, must apply
        // the new description rather than the old one.
        cache
            .create_block(
                &scope,
                BlockCreate::new("reusable", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("second incarnation")
                    .with_char_limit(1000),
            )
            .unwrap();

        let second = cache.get_block(&scope, "reusable").unwrap().unwrap();
        let second_id = second.metadata().id.clone();
        assert_eq!(
            first_id, second_id,
            "reactivation must reuse the soft-deleted block's id"
        );
        assert_eq!(
            second.metadata().description,
            "second incarnation",
            "new BlockCreate's description must overwrite the old one"
        );

        // List blocks: exactly one (the reactivated row), not two.
        let blocks = cache
            .list_blocks(BlockFilter::by_agent(scope.to_db_key()))
            .unwrap();
        assert_eq!(
            blocks.len(),
            1,
            "reactivation must not produce a duplicate row"
        );
    }

    #[test]
    fn test_create_block_active_duplicate_still_errors() {
        // The undelete logic only fires when the existing row is
        // is_active = false. If the row is active, the original UNIQUE
        // conflict behaviour must still surface — agents that genuinely
        // collide on a label deserve to learn about it.
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);
        let scope = Scope::global("agent_1");

        cache
            .create_block(
                &scope,
                BlockCreate::new("taken", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("first")
                    .with_char_limit(1000),
            )
            .unwrap();

        // Second create with the same label while the first is still
        // active must error.
        let result = cache.create_block(
            &scope,
            BlockCreate::new("taken", MemoryBlockType::Working, BlockSchema::text())
                .with_description("second")
                .with_char_limit(1000),
        );
        assert!(
            result.is_err(),
            "create_block must fail when the label is already in use by an active block"
        );
    }

    #[test]
    fn test_reactivation_continues_seq_after_content_writes() {
        // Regression test for the soft-delete + recreate flow when the
        // prior incarnation had persisted content (memory_block_updates
        // rows at seq >= 1). Without seq-continuation, the second
        // create's persist would collide on the (block_id, seq) UNIQUE
        // constraint.
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);
        let scope = Scope::global("agent_1");

        // First incarnation: create + write + persist (advances last_seq).
        let doc1 = cache
            .create_block(
                &scope,
                BlockCreate::new("reusable", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("first incarnation")
                    .with_char_limit(1000),
            )
            .unwrap();
        doc1.set_text("first body", true).unwrap();
        cache.mark_dirty(&scope.to_db_key(), "reusable");
        cache.persist_block(&scope, "reusable").unwrap();

        let id1 = doc1.metadata().id.clone();

        // Soft-delete.
        cache.delete_block(&scope, "reusable").unwrap();

        // Second incarnation: create with new description, then write
        // new content. This is what the SDK Memory.Create handler does.
        let doc2 = cache
            .create_block(
                &scope,
                BlockCreate::new("reusable", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("second incarnation")
                    .with_char_limit(2000),
            )
            .unwrap();

        // Reactivation must reuse the prior id.
        let id2 = doc2.metadata().id.clone();
        assert_eq!(
            id1, id2,
            "reactivation must reuse the soft-deleted block's id"
        );

        // The hydrated doc carries the prior content as the starting
        // state — the new BlockCreate's content (none yet) hasn't been
        // applied. The metadata diffs from the new BlockCreate ARE
        // applied (description, char_limit).
        assert_eq!(doc2.metadata().description, "second incarnation");
        assert_eq!(doc2.metadata().char_limit, 2000);

        // Write new content as a CRDT edit on top. This is the moment
        // that previously collided with the prior incarnation's seq=1
        // row. With seq-continuation it persists at seq=2+.
        doc2.set_text("second body", true).unwrap();
        cache.mark_dirty(&scope.to_db_key(), "reusable");
        cache.persist_block(&scope, "reusable").unwrap();

        // Verify visible content is the new body.
        let rendered = cache.get_rendered_content(&scope, "reusable").unwrap();
        assert_eq!(rendered, Some("second body".to_string()));

        // List blocks: exactly one row (the reactivated one).
        let blocks = cache
            .list_blocks(BlockFilter::by_agent(scope.to_db_key()))
            .unwrap();
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn test_reactivation_rejects_schema_mismatch() {
        // Soft-delete a Text block, then try to recreate at the same
        // label with a Map schema. Should error rather than silently
        // produce a doc whose loro layout disagrees with its declared
        // schema.
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);
        let scope = Scope::global("agent_1");

        cache
            .create_block(
                &scope,
                BlockCreate::new(
                    "shapeshifter",
                    MemoryBlockType::Working,
                    BlockSchema::text(),
                )
                .with_description("text")
                .with_char_limit(1000),
            )
            .unwrap();
        cache.delete_block(&scope, "shapeshifter").unwrap();

        let result = cache.create_block(
            &scope,
            BlockCreate::new(
                "shapeshifter",
                MemoryBlockType::Working,
                BlockSchema::Map { fields: Vec::new() },
            )
            .with_description("map")
            .with_char_limit(1000),
        );
        assert!(
            result.is_err(),
            "reactivation with a different schema must error"
        );
    }

    #[test]
    fn test_get_rendered_content() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block.
        cache
            .create_block(
                &Scope::global("agent_1"),
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
        let doc = cache
            .get_block(&Scope::global("agent_1"), "content_test")
            .unwrap()
            .unwrap();
        doc.set_text("Hello, world!", true).unwrap();

        // Mark dirty and persist.
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "content_test");
        cache
            .persist_block(&Scope::global("agent_1"), "content_test")
            .unwrap();

        // Get rendered content.
        let content = cache
            .get_rendered_content(&Scope::global("agent_1"), "content_test")
            .unwrap();
        assert_eq!(content, Some("Hello, world!".to_string()));
    }

    #[test]
    fn test_archival_operations() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Insert archival entries.
        let id1 = cache
            .insert_archival(&Scope::global("agent_1"), "First archival entry", None)
            .unwrap();
        assert!(id1.starts_with("arch_"));

        let metadata = serde_json::json!({"source": "test", "importance": "high"});
        let id2 = cache
            .insert_archival(
                &Scope::global("agent_1"),
                "Second archival entry with metadata",
                Some(metadata),
            )
            .unwrap();
        assert!(id2.starts_with("arch_"));

        // Search archival (simple substring match).
        let results = cache
            .search_archival(&Scope::global("agent_1"), "archival", 10)
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = cache
            .search_archival(&Scope::global("agent_1"), "metadata", 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].metadata.is_some());

        // Delete archival entry.
        cache.delete_archival(&id1).unwrap();

        // Verify deletion.
        let results = cache
            .search_archival(&Scope::global("agent_1"), "First", 10)
            .unwrap();
        assert_eq!(results.len(), 0);

        // Second entry should still be there.
        let results = cache
            .search_archival(&Scope::global("agent_1"), "Second", 10)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_get_block_metadata() {
        let (_dir, dbs) = test_dbs_with_agent();
        let cache = MemoryCache::new(dbs);

        // Create a block.
        cache
            .create_block(
                &Scope::global("agent_1"),
                BlockCreate::new("metadata_test", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Test metadata retrieval")
                    .with_char_limit(5000),
            )
            .unwrap();

        // Get metadata without loading full document.
        let metadata = cache
            .get_block_metadata(&Scope::global("agent_1"), "metadata_test")
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
                &Scope::global("agent_1"),
                BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Agent personality")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache
            .get_block(&Scope::global("agent_1"), "persona")
            .unwrap()
            .unwrap();
        doc.set_text(
            "I am a helpful assistant specializing in Rust programming",
            true,
        )
        .unwrap();
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "persona");
        cache
            .persist_block(&Scope::global("agent_1"), "persona")
            .unwrap();

        // Create another block.
        cache
            .create_block(
                &Scope::global("agent_1"),
                BlockCreate::new("notes", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Working notes")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache
            .get_block(&Scope::global("agent_1"), "notes")
            .unwrap()
            .unwrap();
        doc.set_text(
            "Meeting scheduled for tomorrow about Python development",
            true,
        )
        .unwrap();
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "notes");
        cache
            .persist_block(&Scope::global("agent_1"), "notes")
            .unwrap();

        // Search for "Rust" - should find persona block.
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache
            .search(
                "Rust",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
            .search(
                "Python",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
                MemorySearchScope::Scope(Scope::global("agent_1")),
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
                &Scope::global("agent_1"),
                "Discussed project requirements for the new authentication system",
                None,
            )
            .unwrap();

        cache
            .insert_archival(
                &Scope::global("agent_1"),
                "Reviewed database schema design for user management",
                None,
            )
            .unwrap();

        cache
            .insert_archival(
                &Scope::global("agent_1"),
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
                MemorySearchScope::Scope(Scope::global("agent_1")),
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
            .search(
                "database",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
                &Scope::global("agent_1"),
                BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text())
                    .with_description("Agent personality")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache
            .get_block(&Scope::global("agent_1"), "persona")
            .unwrap()
            .unwrap();
        doc.set_text("I specialize in Rust programming and system design", true)
            .unwrap();
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "persona");
        cache
            .persist_block(&Scope::global("agent_1"), "persona")
            .unwrap();

        // Create an archival entry.
        cache
            .insert_archival(
                &Scope::global("agent_1"),
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
            .search(
                "Rust",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
            .insert_archival(
                &Scope::global("agent_1"),
                "Agent 1 secret information",
                None,
            )
            .unwrap();

        // Insert archival for agent_2.
        cache
            .insert_archival(
                &Scope::global("agent_2"),
                "Agent 2 secret information",
                None,
            )
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
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.as_ref().unwrap().contains("Agent 1"));

        // Search for agent_2 should only return agent_2's data.
        let results = cache
            .search(
                "secret",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_2")),
            )
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
                    &Scope::global("agent_1"),
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
            .search(
                "testing",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
                &Scope::global("agent_1"),
                BlockCreate::new("test_block", MemoryBlockType::Working, BlockSchema::text())
                    .with_description("Test")
                    .with_char_limit(1000),
            )
            .unwrap();

        let doc = cache
            .get_block(&Scope::global("agent_1"), "test_block")
            .unwrap()
            .unwrap();
        doc.set_text("Searchable block content", true).unwrap();
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "test_block");
        cache
            .persist_block(&Scope::global("agent_1"), "test_block")
            .unwrap();

        cache
            .insert_archival(
                &Scope::global("agent_1"),
                "Searchable archival content",
                None,
            )
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
                MemorySearchScope::Scope(Scope::global("agent_1")),
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
            .insert_archival(
                &Scope::global("agent_1"),
                "Test content for hybrid search",
                None,
            )
            .unwrap();

        // Search with Hybrid mode (should gracefully fall back to FTS).
        let opts = SearchOptions {
            mode: SearchMode::Hybrid,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search(
                "hybrid",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
            .insert_archival(
                &Scope::global("agent_1"),
                "Test content for vector search",
                None,
            )
            .unwrap();

        // Search with Vector mode (should gracefully fall back to FTS).
        let opts = SearchOptions {
            mode: SearchMode::Vector,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search(
                "vector",
                opts,
                MemorySearchScope::Scope(Scope::global("agent_1")),
            )
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
            .insert_archival(
                &Scope::global("agent_1"),
                "Constellation-wide searchable content",
                None,
            )
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
                &Scope::global("agent_1"),
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
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "test_replace");
        cache
            .persist(&Scope::global("agent_1").to_db_key(), "test_replace")
            .unwrap();

        // Get the version vector before replacement.
        let vv_before = doc.inner().oplog_vv();

        // Perform replacement using CRDT-aware method directly on doc.
        let replaced = doc.replace_text("world", "universe", true).unwrap();

        assert!(replaced, "Replacement should have occurred");

        // Persist the changes.
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "test_replace");
        cache
            .persist(&Scope::global("agent_1").to_db_key(), "test_replace")
            .unwrap();

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
                &Scope::global("agent_1"),
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
        cache.mark_dirty(&Scope::global("agent_1").to_db_key(), "test_replace");
        cache
            .persist(&Scope::global("agent_1").to_db_key(), "test_replace")
            .unwrap();

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
                &Scope::global("agent_1"),
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

        let notifier = crate::subscriber::BlockChangeNotifier::new();

        // Step 1: Spawn the initial subscriber.
        spawn_subscriber_for_block(
            block_id,
            schema.clone(),
            &doc,
            reembed_tx.clone(),
            hb_tx.clone(),
            Arc::clone(&mount_path),
            None,
            Arc::clone(&db),
            Arc::clone(&subscribers),
            notifier.clone(),
            pattern_core::observer::MemoryObserver::new(),
            Arc::new(DashMap::new()),
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
            None,
            Arc::clone(&db),
            Arc::clone(&subscribers),
            notifier,
            pattern_core::observer::MemoryObserver::new(),
            Arc::new(DashMap::new()),
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
            None,
            Arc::clone(&db),
            Arc::clone(&subscribers),
            crate::subscriber::BlockChangeNotifier::new(),
            pattern_core::observer::MemoryObserver::new(),
            Arc::new(DashMap::new()),
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

        // Verify the disk_doc (accessed via the subscriber's synced_doc) reflects
        // the edit.
        let sub = cache.subscribers.get(block_id).unwrap();
        let disk_doc = sub.synced_doc.doc().clone();
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

    // region: trust-tier override tests (C5-test)

    /// Helper: create a DB block entry with a Skill schema and return the
    /// created block's ID. `block_id` is used as both ID and label.
    fn create_skill_block_in_db(db: &ConstellationDb, block_id: &str, agent_id: &str) {
        use pattern_db::models::{MemoryBlock, MemoryBlockType};
        let conn = db.get().unwrap();
        let block = MemoryBlock {
            id: block_id.to_string(),
            agent_id: agent_id.to_string(),
            label: block_id.to_string(),
            description: "Skill trust-tier test block".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 10_000,
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

    /// Helper: build a minimal MemoryCache with mount_path + first_party_skills_dir
    /// wired, and populate it with a Skill StructuredDocument + subscriber.
    ///
    /// Both `mount_path_dir` and `fp_dir_path` are caller-supplied so that
    /// tests can control whether `<mount_path>/<block_id>.md` is under `fp_dir`
    /// (by passing the same tempdir for both) or not (separate tempdirs).
    ///
    /// Returns `(cache, doc)` — the caller must keep any TempDirs alive.
    fn setup_skill_cache_with_fp_dir(
        db: Arc<pattern_db::ConstellationDb>,
        block_id: &str,
        mount_path_dir: &std::path::Path,
        fp_dir_path: &std::path::Path,
    ) -> (MemoryCache, pattern_core::memory::StructuredDocument) {
        use pattern_core::memory::StructuredDocument;
        use pattern_core::types::memory_types::BlockSchema;

        let mount_path = Arc::new(mount_path_dir.to_path_buf());
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (reembed_tx2, _reembed_rx2) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded::<crate::subscriber::event::Heartbeat>(64);
        let (hb_tx2, hb_rx2) =
            crossbeam_channel::bounded::<crate::subscriber::event::Heartbeat>(64);
        let subscribers: Arc<DashMap<String, SubscriberHandle>> = Arc::new(DashMap::new());

        let schema = BlockSchema::Skill {
            expected_keys: vec![],
        };
        let doc = StructuredDocument::new(schema.clone());

        spawn_subscriber_for_block(
            block_id,
            schema,
            &doc,
            reembed_tx,
            hb_tx,
            Arc::clone(&mount_path),
            None,
            Arc::clone(&db),
            Arc::clone(&subscribers),
            crate::subscriber::BlockChangeNotifier::new(),
            pattern_core::observer::MemoryObserver::new(),
            Arc::new(DashMap::new()),
        );

        // Build the cache with both mount_path (so apply_external_edit reconstructs
        // `mount_path/<block_id>.md`) and first_party_skills_dir (so the trust-tier
        // enforcement logic in apply_external_edit fires correctly).
        let cache = MemoryCache::new(Arc::clone(&db))
            .with_mount_path(mount_path_dir.to_path_buf(), reembed_tx2, hb_tx2, hb_rx2)
            .with_first_party_skills_dir(fp_dir_path.to_path_buf());

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
        {
            let (_, handle) = subscribers.remove(block_id).unwrap();
            cache.subscribers.insert(block_id.to_string(), handle);
        }

        (cache, doc)
    }

    /// Read the `trust_tier` from the disk_doc stored in a subscriber handle,
    /// using `project_metadata_from_loro`.
    fn read_trust_tier_from_disk_doc(
        cache: &MemoryCache,
        block_id: &str,
    ) -> pattern_core::types::memory_types::SkillTrustTier {
        use crate::fs::markdown_skill::loro_bridge::project_metadata_from_loro;

        let sub = cache.subscribers.get(block_id).unwrap();
        let disk_doc = sub.synced_doc.doc().clone();
        drop(sub);

        let deep = disk_doc.get_deep_value();
        let loro::LoroValue::Map(root) = &deep else {
            panic!("disk_doc root must be a Map; got: {deep:?}");
        };
        project_metadata_from_loro(root)
            .expect("project_metadata_from_loro must succeed after a valid apply_external_edit")
            .trust_tier
    }

    /// `apply_external_edit` with a Skill block whose frontmatter declares
    /// `trust_tier: first-party` BUT the file path is NOT under
    /// `first_party_skills_dir` — the enforced tier must NOT be `FirstParty`.
    /// Authors cannot self-promote a skill to FirstParty by writing it in the
    /// YAML frontmatter.
    ///
    /// Note: this test documents the user-facing semantic (self-promotion is
    /// blocked). It is INVARIANT to whether `first_party_skills_dir` is
    /// threaded through `attach` — even with plumbing disabled, a file outside
    /// any first-party/mount-skills directory falls through to `Runtime →
    /// AdHoc`, satisfying the `!= FirstParty` assertion. The genuine plumbing
    /// verification is `apply_external_edit_skill_preserves_first_party_for_paths_inside_fp_dir`
    /// below (positive case: stored tier == FirstParty only when plumbing is
    /// wired).
    #[test]
    fn apply_external_edit_skill_overrides_declared_first_party_tier_outside_fp_dir() {
        let (_dir, db) = test_dbs();
        let block_id = "skill_tier_override_block";
        let agent_id = "agent_skill_tier_override";
        create_test_agent(&db, agent_id);
        create_skill_block_in_db(&db, block_id, agent_id);

        // mount_tmp and fp_tmp are separate — block_id.md (in mount_tmp)
        // is NOT under fp_tmp, so the tier must not be FirstParty.
        let mount_tmp = tempfile::tempdir().unwrap();
        let fp_tmp = tempfile::tempdir().unwrap();
        let (cache, _doc) = setup_skill_cache_with_fp_dir(
            Arc::clone(&db),
            block_id,
            mount_tmp.path(),
            fp_tmp.path(),
        );

        // Skill frontmatter declares first-party, but the file is in mount_tmp,
        // NOT under fp_tmp → enforced tier must NOT be FirstParty.
        let md = "---\nname: my-skill\ntrust_tier: first-party\ndescription: test\n---\nbody\n";
        cache.apply_external_edit(block_id, md.as_bytes());
        std::thread::sleep(std::time::Duration::from_millis(100));

        let tier = read_trust_tier_from_disk_doc(&cache, block_id);
        assert_ne!(
            tier,
            pattern_core::types::memory_types::SkillTrustTier::FirstParty,
            "a skill file outside fp_dir must not be granted FirstParty, \
             even if the frontmatter declares it; got tier={tier:?}"
        );

        // Clean up subscriber.
        let (_, handle) = cache.subscribers.remove(block_id).unwrap();
        handle.cancel.cancel();
        drop(handle._subscription);
        drop(handle.event_tx);
        handle.thread.join().expect("worker should not panic");
    }

    /// `apply_external_edit` with a Skill block whose file path IS under
    /// `first_party_skills_dir` must receive `SkillTrustTier::FirstParty`
    /// regardless of the declared tier in the frontmatter.
    ///
    /// The path is under fp_dir because mount_path == fp_dir, so the
    /// reconstructed path `mount_path/<block_id>.md` starts_with fp_dir.
    #[test]
    fn apply_external_edit_skill_preserves_first_party_for_paths_inside_fp_dir() {
        let (_dir, db) = test_dbs();
        let block_id = "skill_fp_inside_block";
        let agent_id = "agent_skill_fp_inside";
        create_test_agent(&db, agent_id);
        create_skill_block_in_db(&db, block_id, agent_id);

        // Use the same tempdir for mount_path AND fp_dir so that
        // `mount_path/<block_id>.md` starts_with fp_dir → SdkResourceDir source.
        let combined_tmp = tempfile::tempdir().unwrap();
        let (cache, _doc) = setup_skill_cache_with_fp_dir(
            Arc::clone(&db),
            block_id,
            combined_tmp.path(),
            combined_tmp.path(),
        );

        // Frontmatter declares ad-hoc, but path IS under fp_dir → FirstParty wins.
        let md = "---\nname: sdk-skill\ntrust_tier: ad-hoc\ndescription: test\n---\nbody\n";
        cache.apply_external_edit(block_id, md.as_bytes());
        std::thread::sleep(std::time::Duration::from_millis(100));

        let tier = read_trust_tier_from_disk_doc(&cache, block_id);
        assert_eq!(
            tier,
            pattern_core::types::memory_types::SkillTrustTier::FirstParty,
            "a skill file inside fp_dir must receive FirstParty, \
             even if frontmatter declares ad-hoc; got tier={tier:?}"
        );

        // Clean up.
        let (_, handle) = cache.subscribers.remove(block_id).unwrap();
        handle.cancel.cancel();
        drop(handle._subscription);
        drop(handle.event_tx);
        handle.thread.join().expect("worker should not panic");
    }

    /// `apply_external_edit` with a Skill file outside fp_dir that declares
    /// `plugin-installed` — the stored tier must be `PluginInstalled` AND the
    /// `skill.plugin_installed_tier_without_plugin_system` counter must fire.
    ///
    /// Note: `assign_trust_tier` short-circuits on `declared_tier ==
    /// PluginInstalled` before consulting source classification, so this test
    /// is invariant to whether `first_party_skills_dir` plumbing is wired. It
    /// verifies a different property than the other two tests in this group —
    /// specifically, that the plugin-installed declaration is preserved and
    /// emits the expected observability signal, not that path-based source
    /// classification works. Path-plumbing regression coverage is in
    /// `apply_external_edit_skill_preserves_first_party_for_paths_inside_fp_dir`.
    #[test]
    fn apply_external_edit_skill_preserves_plugin_installed_declaration() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let (_dir, db) = test_dbs();
        let block_id = "skill_plugin_tier_block";
        let agent_id = "agent_skill_plugin_tier";
        create_test_agent(&db, agent_id);
        create_skill_block_in_db(&db, block_id, agent_id);

        // mount_tmp and fp_tmp are separate — file is outside fp_dir.
        let mount_tmp = tempfile::tempdir().unwrap();
        let fp_tmp = tempfile::tempdir().unwrap();
        let (cache, _doc) = setup_skill_cache_with_fp_dir(
            Arc::clone(&db),
            block_id,
            mount_tmp.path(),
            fp_tmp.path(),
        );

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        // File is outside fp_dir; declares plugin-installed → PluginInstalled preserved + metric.
        let md =
            "---\nname: plugin-skill\ntrust_tier: plugin-installed\ndescription: test\n---\nbody\n";

        metrics::with_local_recorder(&recorder, || {
            cache.apply_external_edit(block_id, md.as_bytes());
        });
        std::thread::sleep(std::time::Duration::from_millis(100));

        let tier = read_trust_tier_from_disk_doc(&cache, block_id);
        assert_eq!(
            tier,
            pattern_core::types::memory_types::SkillTrustTier::PluginInstalled,
            "plugin-installed declaration must be preserved by assign_trust_tier; \
             got tier={tier:?}"
        );

        // The observability counter must have fired inside with_local_recorder.
        let snapshot = snapshotter.snapshot().into_vec();
        let entry = snapshot.iter().find(|(ck, _, _, _)| {
            ck.key().name() == "skill.plugin_installed_tier_without_plugin_system"
        });
        assert!(
            entry.is_some(),
            "expected 'skill.plugin_installed_tier_without_plugin_system' counter; \
             snapshot: {snapshot:?}"
        );
        let (_, _, _, value) = entry.unwrap();
        assert_eq!(
            *value,
            DebugValue::Counter(1),
            "plugin-installed counter must be 1 after one skill edit"
        );

        // Clean up.
        let (_, handle) = cache.subscribers.remove(block_id).unwrap();
        handle.cancel.cancel();
        drop(handle._subscription);
        drop(handle.event_tx);
        handle.thread.join().expect("worker should not panic");
    }

    // endregion: trust-tier override tests (C5-test)

    // region: fork_for_child

    /// `fork_for_child` forks only blocks owned by the parent agent, skipping
    /// foreign-owned blocks, and retags ownership on the forked copies.
    #[test]
    fn fork_for_child_only_forks_parent_owned_blocks() {
        let (_dir, db) = test_dbs();
        let parent_id = "parent-agent";
        let other_id = "other-agent";
        let child_id = "child-agent";
        // fork_for_child is an internal method that takes raw agent_id strings,
        // so we must use the db_key form to match what create_block stores.
        let parent_key = Scope::global(parent_id).to_db_key();
        let other_key = Scope::global(other_id).to_db_key();
        let child_key = Scope::global(child_id).to_db_key();

        create_test_agent(&db, parent_id);
        create_test_agent(&db, other_id);
        create_test_agent(&db, child_id);

        let cache = MemoryCache::new(db);

        // Create a block owned by the parent.
        let parent_bc = pattern_core::types::block::BlockCreate::new(
            "notes".to_string(),
            MemoryBlockType::Working,
            pattern_core::types::memory_types::BlockSchema::text(),
        );
        cache
            .create_block(&Scope::global(parent_id), parent_bc)
            .unwrap();

        // Create a block owned by another agent — should NOT appear in fork.
        let other_bc = pattern_core::types::block::BlockCreate::new(
            "other-notes".to_string(),
            MemoryBlockType::Working,
            pattern_core::types::memory_types::BlockSchema::text(),
        );
        cache
            .create_block(&Scope::global(other_id), other_bc)
            .unwrap();

        let child_cache = cache
            .fork_for_child(&parent_key, &child_key)
            .expect("fork_for_child must succeed");

        // The child cache has the parent's block retagged to child ownership.
        assert_eq!(
            child_cache.blocks.len(),
            1,
            "child cache should contain exactly one block (the parent's)"
        );
        let child_block = child_cache.blocks.iter().next().unwrap();
        assert_eq!(
            child_block.value().doc.agent_id(),
            child_key,
            "forked block should be retagged with child agent id"
        );
        assert_eq!(
            child_block.value().doc.label(),
            "notes",
            "forked block label should match parent's block"
        );
        // suppress unused variable warning
        let _ = other_key;
    }

    /// Writes to a forked child cache do not affect the parent cache.
    #[test]
    fn fork_for_child_writes_do_not_propagate_to_parent() {
        let (_dir, db) = test_dbs();
        let parent_id = "isolate-parent";
        let child_id = "isolate-child";
        // fork_for_child is an internal method that takes raw agent_id strings,
        // so we must use the db_key form to match what create_block stores.
        let parent_key = Scope::global(parent_id).to_db_key();
        let child_key = Scope::global(child_id).to_db_key();

        create_test_agent(&db, parent_id);
        create_test_agent(&db, child_id);

        let cache = MemoryCache::new(db);

        let bc = pattern_core::types::block::BlockCreate::new(
            "notes".to_string(),
            MemoryBlockType::Working,
            pattern_core::types::memory_types::BlockSchema::text(),
        );
        cache.create_block(&Scope::global(parent_id), bc).unwrap();

        // Write initial content to the parent.
        {
            // The internal get() uses the raw agent_id string stored in doc.
            let doc = cache.get(&parent_key, "notes").unwrap().unwrap();
            doc.set_text("initial", true).unwrap();
        }

        let child_cache = cache
            .fork_for_child(&parent_key, &child_key)
            .expect("fork_for_child must succeed");

        // Write different content in the child.
        {
            let child_doc = child_cache.blocks.iter().next().unwrap();
            child_doc
                .value()
                .doc
                .set_text("child-change", true)
                .unwrap();
        }

        // Parent should still read the initial value.
        {
            let parent_doc = cache.get(&parent_key, "notes").unwrap().unwrap();
            assert_eq!(
                parent_doc.text_content(),
                "initial",
                "parent should not observe child's write"
            );
        }
    }

    // endregion: fork_for_child

    // region: hydrate disk-merge regression tests

    /// Regression for the Memory.append-eats-first-write bug.
    ///
    /// Scenario: human edits the canonical block .md file while the daemon
    /// is stopped. On startup, cache.load_from_db must merge the disk diff
    /// into the doc and persist that merge as a new DB update.
    #[test]
    fn hydrate_disk_merge_picks_up_offline_edit() {
        use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};

        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();
        let dbs = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        create_test_agent(&dbs, "agent_1");

        // 1. Create+persist via a setup cache so DB has the block + a snapshot.
        let cache_setup = MemoryCache::new(Arc::clone(&dbs));
        let create = pattern_core::types::block::BlockCreate::new(
            "merge-test".to_string(),
            MemoryBlockType::Working,
            BlockSchema::text(),
        )
        .with_description("hydrate-merge regression")
        .with_char_limit(5000);
        let scope = Scope::Global("agent_1".into());
        let doc0 = MemoryStore::create_block(&cache_setup, &scope, create).unwrap();
        doc0.set_text("db side\n", true).unwrap();
        MemoryStore::mark_dirty(&cache_setup, &scope, "merge-test").unwrap();
        MemoryStore::persist_block(&cache_setup, &scope, "merge-test").unwrap();
        drop(doc0);
        drop(cache_setup);

        // 2. Write a divergent disk file (simulates human editing while daemon was off).
        let block_dir = mount.join("blocks").join("@agent_1").join("working");
        std::fs::create_dir_all(&block_dir).unwrap();
        std::fs::write(block_dir.join("merge-test.md"), "human edited content\n").unwrap();

        // 3. Fresh cache with mount_path (simulates daemon restart).
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, hb_rx) = crossbeam_channel::bounded::<crate::subscriber::event::Heartbeat>(64);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let cache = MemoryCache::new(Arc::clone(&dbs)).with_mount_path(
            mount.clone(),
            reembed_tx,
            hb_tx,
            hb_rx,
        );

        // 4. Hydrate — should run disk-merge.
        let _doc = MemoryStore::get_block(&cache, &scope, "merge-test")
            .unwrap()
            .expect("block hydrated");

        // 5. Doc reflects the merged state (human's edit adopted).
        let rendered = MemoryStore::get_rendered_content(&cache, &scope, "merge-test")
            .unwrap()
            .expect("rendered content");
        assert!(
            rendered.contains("human edited content"),
            "hydrate-disk-merge must adopt offline disk edit; got: {rendered:?}"
        );

        // 6. New DB update with author="disk-merge-on-hydrate" was persisted.
        let block_db = pattern_db::queries::get_block_by_label(
            &dbs.get().unwrap(),
            &scope.to_db_key(),
            "merge-test",
        )
        .unwrap()
        .expect("block exists");
        let (_chk, all_updates) =
            pattern_db::queries::get_checkpoint_and_updates(&dbs.get().unwrap(), &block_db.id)
                .unwrap();
        let merge_update = all_updates
            .iter()
            .find(|u| u.source.as_deref() == Some("disk-merge-on-hydrate"));
        assert!(
            merge_update.is_some(),
            "a 'disk-merge-on-hydrate' update must be persisted; updates: {:?}",
            all_updates
                .iter()
                .map(|u| (u.seq, u.source.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// Regression for multi-append-loses-first-write.
    ///
    /// Two sequential appends should both survive on a hydrated block where
    /// the disk file matches the DB rendering. Pre-fix, the lazy SyncedDoc
    /// spawn's seed step Myers-diffed stale disk over cache-hydrated doc,
    /// reverting the first append's ops.
    #[test]
    fn multi_append_after_hydrate_preserves_all_writes() {
        use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};

        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();
        let dbs = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        create_test_agent(&dbs, "agent_1");

        let cache_setup = MemoryCache::new(Arc::clone(&dbs));
        let create = pattern_core::types::block::BlockCreate::new(
            "multi-append".to_string(),
            MemoryBlockType::Working,
            BlockSchema::text(),
        )
        .with_description("multi-append regression")
        .with_char_limit(5000);
        let scope = Scope::Global("agent_1".into());
        let doc0 = MemoryStore::create_block(&cache_setup, &scope, create).unwrap();
        doc0.set_text("baseline\n", true).unwrap();
        MemoryStore::mark_dirty(&cache_setup, &scope, "multi-append").unwrap();
        MemoryStore::persist_block(&cache_setup, &scope, "multi-append").unwrap();
        drop(doc0);
        drop(cache_setup);

        // Disk matches DB so the disk-merge step does NOT fire.
        let block_dir = mount.join("blocks").join("@agent_1").join("working");
        std::fs::create_dir_all(&block_dir).unwrap();
        std::fs::write(block_dir.join("multi-append.md"), "baseline\n").unwrap();

        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, hb_rx) = crossbeam_channel::bounded::<crate::subscriber::event::Heartbeat>(64);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let cache = MemoryCache::new(Arc::clone(&dbs)).with_mount_path(
            mount.clone(),
            reembed_tx,
            hb_tx,
            hb_rx,
        );

        // Two sequential appends mimicking Memory.append handler flow.
        let doc1 = MemoryStore::get_block(&cache, &scope, "multi-append")
            .unwrap()
            .unwrap();
        doc1.append("first append\n", false).unwrap();
        MemoryStore::mark_dirty(&cache, &scope, "multi-append").unwrap();
        MemoryStore::persist_block(&cache, &scope, "multi-append").unwrap();

        let doc2 = MemoryStore::get_block(&cache, &scope, "multi-append")
            .unwrap()
            .unwrap();
        doc2.append("second append\n", false).unwrap();
        MemoryStore::mark_dirty(&cache, &scope, "multi-append").unwrap();
        MemoryStore::persist_block(&cache, &scope, "multi-append").unwrap();

        let rendered = MemoryStore::get_rendered_content(&cache, &scope, "multi-append")
            .unwrap()
            .expect("rendered");
        assert!(
            rendered.contains("first append"),
            "first append must survive lazy SyncedDoc spawn; rendered: {rendered:?}"
        );
        assert!(
            rendered.contains("second append"),
            "second append must survive; rendered: {rendered:?}"
        );
    }

    // endregion: hydrate disk-merge regression tests
}
