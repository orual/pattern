// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Fork spawn — Phase 3 Tasks 1-3.
//!
//! A fork is a memory-isolated copy of the parent session with its own
//! compute environment. Phase 3 delivers the lightweight isolation path:
//!
//! - [`ForkIsolationState::Lightweight`]: backed by `LoroDoc::fork()`;
//!   zero disk writes; lives and dies with the spawning runtime. Supports
//!   `merge_back_lightweight` (CRDT import) and `discard` (drops the child
//!   cache without propagation).
//! - [`ForkIsolationState::Persistent`]: stub variant reserved for Tasks
//!   4-6 (jj workspace + namespaced bookmark). Construction and resolution
//!   methods land in the next subcomponent.
//!
//! # Resolution paths
//!
//! - `merge_back_lightweight(&self)` — imports the fork's LoroDoc snapshot
//!   into the parent cache (Tasks 2-3).
//! - `discard(self)` — consumes the handle, signals the child's
//!   `CancelState`, and drops the child cache (Task 3).
//! - `promote()` landed in Task 7; `ForkOp` dispatch in Task 8.
//!
//! # Phase 2 backward compatibility
//!
//! The Phase 2 wire types (`WireForkHandle`) are preserved unchanged.
//! `ForkHandle` gained a richer `isolation_state` field in place of the
//! plain placeholder ids. `check_promote_capability` is preserved.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use pattern_core::spawn::PersonaConfig;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_core::{CapabilityFlag, CapabilitySet, ForkConfig};
use pattern_memory::MemoryCache;
use pattern_memory::jj::JjAdapter;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use tidepool_bridge_derive::ToCore;

use crate::spawn::SpawnError;
use crate::spawn::draft::RuntimeConfigWriter;
use crate::timeout::CancelState;

// ── Seed cache manifest (Phase 3 → Phase 6 bridge) ────────────────────────────

/// Wire version of the seed cache manifest. Bumped when the on-disk shape
/// changes incompatibly so promote can refuse to load older variants.
pub const SEED_CACHE_MANIFEST_VERSION: u32 = 1;

/// One entry per persisted block in a fork-promote seed cache.
///
/// Schema and block_type are captured here because they are NOT recoverable
/// from the raw Loro snapshot bytes — `MemoryCache::insert_from_snapshot`
/// requires both as explicit parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedCacheManifestEntry {
    /// Filename of the snapshot inside the seed cache directory
    /// (e.g. `notes.loro`).
    pub file: String,
    /// Original block label (may differ from `file` when the label contains
    /// `/`, which is sanitised in `file`).
    pub label: String,
    /// Block schema — required by `MemoryCache::insert_from_snapshot`.
    pub schema: BlockSchema,
    /// Block type — required by `MemoryCache::insert_from_snapshot`.
    pub block_type: MemoryBlockType,
}

/// Seed cache manifest. Lives at `<drafts_dir>/<persona_id>.cache/manifest.json`.
///
/// Promotion (Phase 6 T6) reads this to migrate the seed cache into the
/// project mount's `MemoryCache` before opening the new live session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedCacheManifest {
    pub version: u32,
    pub persona_id: String,
    pub entries: Vec<SeedCacheManifestEntry>,
}

// ── ForkError ─────────────────────────────────────────────────────────────────

/// Errors produced by [`ForkHandle`] resolution helpers.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum ForkError {
    /// The resolution method is not valid for this fork's isolation mode.
    ///
    /// For example: calling `merge_back_lightweight` on a `Persistent` fork.
    #[error("operation called on the wrong fork isolation mode")]
    WrongIsolation,

    /// The parent memory cache has been dropped.
    ///
    /// `merge_back_lightweight` holds a `Weak<MemoryCache>` for the parent
    /// to avoid reference cycles. This error is returned if the parent's
    /// `Arc<MemoryCache>` was dropped before the merge was attempted.
    #[error("parent memory cache has been dropped; merge_back is no longer possible")]
    ParentDropped,

    /// The fork handle was already resolved (merged, discarded, or promoted).
    ///
    /// `discard(self)` takes the handle by value so a second call is a
    /// compile-time error. This variant exists for the hypothetical future
    /// case where discard takes `&mut self`. It is not raised by the current
    /// implementation but is included per the spec for completeness.
    #[error("fork handle has already been resolved")]
    AlreadyResolved,

    /// An error occurred in a memory document operation.
    #[error("memory document error: {0}")]
    Document(String),

    /// An error occurred in a memory store operation.
    #[error("memory store error: {0}")]
    MemoryStore(String),

    /// Persistent fork was requested but the active mount does not support it.
    ///
    /// Examples: `InRepo` mount with `jj` disabled in `.pattern.kdl`, or no
    /// mount info available on the session at all (e.g. ephemeral test
    /// session built via `from_persona`).
    #[error("persistent fork not available for mount mode: {mode}")]
    PersistentNotAvailable {
        /// Human-readable descriptor of the mount mode that rejected the fork.
        mode: String,
    },

    /// `jj` was required but is not available on the host.
    #[error("jj is not installed or not on PATH; persistent fork requires a working jj")]
    JjUnavailable,

    /// A persistent fork could not be created because the bookmark name is
    /// already in use.
    #[error(
        "bookmark already exists: {name} (use a different task ref or discard the existing fork)"
    )]
    BookmarkConflict {
        /// The conflicting bookmark name.
        name: String,
    },

    /// A `jj merge` operation failed during `merge_back_persistent`.
    #[error("jj merge of {revsets} failed: {message}")]
    JjMerge {
        /// The revsets that were being merged (joined with ", ").
        revsets: String,
        /// Stringified source error from the jj adapter.
        message: String,
    },

    /// One or more best-effort cleanup operations failed during persistent
    /// `discard`.
    ///
    /// Both `workspace_forget` and `bookmark_delete` are attempted; failures
    /// are collected here so callers can diagnose partial-cleanup state.
    #[error("persistent fork discard cleanup had {} failures: {}", errors.len(), errors.join("; "))]
    DiscardCleanup {
        /// Stringified failure messages for each failed cleanup step.
        errors: Vec<String>,
    },

    /// A `jj` operation other than merge failed.
    #[error("jj operation failed: {message}")]
    JjOp {
        /// Stringified source error from the jj adapter.
        message: String,
    },

    /// A fork id collision was detected when inserting into the
    /// [`crate::spawn::fork_registry::ForkRegistry`].
    ///
    /// Only happens if the id-minting layer produces a duplicate (which
    /// it shouldn't — `pattern_core::types::ids::new_id` is UUID-backed)
    /// or if a caller passes a hand-crafted id. Callers that hit this
    /// must remove the existing entry before retrying.
    #[error("fork id already registered: {fork_id}")]
    AlreadyExists {
        /// The duplicate id.
        fork_id: String,
    },

    /// `promote()` was called by a spawner that does not hold
    /// [`pattern_core::CapabilityFlag::SpawnNewIdentities`].
    ///
    /// The check runs against the spawner's capability snapshot captured
    /// at fork-construction time — not against the fork's own (possibly
    /// narrower) capabilities. The authority to mint a new persona
    /// identity belongs to the spawner.
    #[error("promote() requires CapabilityFlag::SpawnNewIdentities; spawner does not hold it")]
    CapabilityDenied,
}

// ── ForkIsolationState ────────────────────────────────────────────────────────

/// Runtime state for a live fork, keyed by isolation mode.
///
/// Phase 3 Tasks 1-3 populate `Lightweight` fully. `Persistent` is a stub
/// that will be fleshed out in Tasks 4-6.
#[derive(Debug)]
pub enum ForkIsolationState {
    /// Sentinel used after `discard()` or `promote()` consume the fork state.
    ///
    /// `ForkHandle::Drop` checks for this variant and skips cleanup work (the
    /// state was already handled by the explicit resolution method). External
    /// callers should never construct or observe this variant directly — it is
    /// only visible because `ForkIsolationState` is `pub` for test construction.
    Resolved,

    /// In-memory fork backed by `LoroDoc::fork()`.
    ///
    /// The child cache owns forked CRDT documents. No disk writes are
    /// produced. Dropping the `ForkHandle` (or calling `discard`) frees
    /// the forked state without touching the parent.
    Lightweight {
        /// The child's in-memory cache of forked LoroDoc instances.
        child_cache: Arc<MemoryCache>,
        /// Weak reference to the parent's cache.
        ///
        /// `Weak` breaks the reference cycle that would otherwise prevent
        /// the parent cache from being freed while an unresolved fork exists.
        /// `merge_back_lightweight` upgrades this to a strong `Arc` at
        /// merge time; if the parent has been dropped, it returns
        /// `ForkError::ParentDropped`.
        parent_cache: Weak<MemoryCache>,
        /// Agent ID of the parent session. Used to locate the correct block
        /// in the parent cache during merge.
        parent_agent_id: SmolStr,
        /// Cancellation handle for the child session.
        ///
        /// `discard` calls `request_cancel()` to signal any in-flight child
        /// work before dropping the child cache.
        cancel_state: Arc<CancelState>,
    },

    /// Persistent fork backed by a jj workspace.
    ///
    /// The fork lives in a dedicated jj workspace rooted at `workspace_path`,
    /// tracked by `bookmark_name`. The child cache mirrors the workspace's
    /// on-disk LoroDoc state. Resolution uses jj-level merge plus loro
    /// snapshot import (`merge_back_persistent`) or workspace + bookmark
    /// teardown (`discard`).
    Persistent {
        /// On-disk path to the new jj workspace (mount-mode dependent).
        workspace_path: PathBuf,
        /// Namespaced bookmark `<agent>/<task>` pinning the fork's working
        /// copy. Constructed via [`pattern_memory::jj::fork_bookmark_name`].
        bookmark_name: String,
        /// Repository root used for jj `workspace_add` / `bookmark_set` /
        /// `bookmark_delete` invocations. For Standalone and Sidecar this is
        /// the mount path itself (the standalone mount IS the jj repo);
        /// InRepo-with-jj would use the project root.
        repo_root: PathBuf,
        /// The child's in-memory cache of forked LoroDoc instances.
        child_cache: Arc<MemoryCache>,
        /// Weak reference to the parent's cache for `merge_back_persistent`.
        parent_cache: Weak<MemoryCache>,
        /// Agent ID of the parent session.
        parent_agent_id: SmolStr,
        /// Cancellation handle for the child session.
        cancel_state: Arc<CancelState>,
    },
}

// ── ForkHandle ────────────────────────────────────────────────────────────────

/// Handle referencing an in-progress fork.
///
/// Wraps a [`ForkIsolationState`] that describes how the fork is backed
/// (in-memory LoroDoc or persistent jj workspace) and provides resolution
/// helpers:
///
/// - [`merge_back_lightweight`](Self::merge_back_lightweight) — import fork's
///   state into the parent.
/// - [`discard`](Self::discard) — drop the fork without propagating to parent.
///
/// The `fork_id` and `child_id` fields from Phase 2 are carried as
/// `SmolStr` members of the struct for backward compatibility with
/// `WireForkHandle` serialization.
#[derive(Debug)]
pub struct ForkHandle {
    /// Stable identifier for this fork operation.
    pub fork_id: SmolStr,
    /// Identifier for the child session executing the fork's program.
    pub child_id: SmolStr,
    /// Runtime isolation state: in-memory or persistent.
    pub isolation_state: ForkIsolationState,
    /// Snapshot of the spawner's capability set at fork-construction
    /// time.
    ///
    /// Consulted by [`ForkHandle::promote`] — the authority to mint a
    /// new persona identity belongs to the spawner, not to the fork
    /// (which may have been restricted to a narrower capability set).
    /// Defaults to [`CapabilitySet::all`] when constructed via the
    /// `new_lightweight` / `new_persistent` constructors; the spawn
    /// handler overrides it via [`ForkHandle::with_spawner_capabilities`]
    /// using the parent's live caps.
    pub spawner_capabilities: CapabilitySet,
    /// Cancel-propagation watcher task for parent→child cancel cascading.
    ///
    /// The watcher parks on the parent's `wait_for_cancel()` and flips the
    /// child's cancel state when the parent fires. This handle is stored here
    /// so that if the fork resolves cleanly (via `discard`, `merge_back`, or
    /// `promote`) before the parent cancels, we can abort the watcher and
    /// avoid a perpetual parked task.
    ///
    /// `None` when the fork was constructed without a tokio runtime context
    /// (e.g. in unit tests that build `ForkHandle` directly).
    pub cancel_watcher: Option<tokio::task::JoinHandle<()>>,
    pub cfg: Option<ForkConfig>,
}

impl ForkHandle {
    /// Construct a persistent `ForkHandle`.
    ///
    /// Called from the spawn handler after `JjAdapter::workspace_add` and
    /// `JjAdapter::bookmark_set` have succeeded. The handler is responsible
    /// for cleaning up the workspace and bookmark on any failure between
    /// those steps and this constructor — once the handle exists, cleanup
    /// flows through [`ForkHandle::discard`] or [`ForkHandle::merge_back_persistent`].
    #[allow(clippy::too_many_arguments)]
    pub fn new_persistent(
        fork_id: SmolStr,
        child_id: SmolStr,
        workspace_path: PathBuf,
        bookmark_name: String,
        repo_root: PathBuf,
        child_cache: Arc<MemoryCache>,
        parent_agent_id: SmolStr,
        parent_cache: Weak<MemoryCache>,
        cancel_state: Arc<CancelState>,
    ) -> Self {
        Self {
            fork_id,
            child_id,
            isolation_state: ForkIsolationState::Persistent {
                workspace_path,
                bookmark_name,
                repo_root,
                child_cache,
                parent_cache,
                parent_agent_id,
                cancel_state,
            },
            spawner_capabilities: CapabilitySet::all(),
            cancel_watcher: None,
            cfg: None,
        }
    }

    /// Construct a lightweight `ForkHandle`.
    ///
    /// Called from the spawn handler once `MemoryCache::fork_for_child` has
    /// produced the child cache.
    pub fn new_lightweight(
        fork_id: SmolStr,
        child_id: SmolStr,
        child_cache: Arc<MemoryCache>,
        parent_agent_id: SmolStr,
        parent_cache: Weak<MemoryCache>,
        cancel_state: Arc<CancelState>,
    ) -> Self {
        Self {
            fork_id,
            child_id,
            isolation_state: ForkIsolationState::Lightweight {
                child_cache,
                parent_cache,
                parent_agent_id,
                cancel_state,
            },
            spawner_capabilities: CapabilitySet::all(),
            cancel_watcher: None,
            cfg: None,
        }
    }

    /// Override the spawner-capability snapshot.
    ///
    /// The spawn handler calls this immediately after constructing the
    /// handle to attach the live parent's capability set; tests that
    /// care about capability gating may also call it explicitly. Test
    /// fixtures that don't exercise [`ForkHandle::promote`] can omit
    /// this and inherit the [`CapabilitySet::all`] default.
    #[must_use]
    pub fn with_spawner_capabilities(mut self, caps: CapabilitySet) -> Self {
        self.spawner_capabilities = caps;
        self
    }

    /// Attach the parent→child cancel-propagation watcher task.
    ///
    /// The spawn handler spawns a task that parks on
    /// `parent.wait_for_cancel()` and, when the parent fires, upgrades the
    /// child's `Weak<CancelState>` and calls `request_cancel()`. The
    /// resulting `JoinHandle` is stored here so that if the fork resolves
    /// cleanly (via `discard`, `merge_back`, or `promote`) before the
    /// parent cancels, the watcher can be aborted and will not remain
    /// parked indefinitely.
    ///
    /// This method is intentionally separate from the constructors so
    /// that unit tests that build `ForkHandle` directly (without a tokio
    /// runtime) can omit it and inherit `None`.
    #[must_use]
    pub fn with_cancel_watcher(mut self, handle: tokio::task::JoinHandle<()>) -> Self {
        self.cancel_watcher = Some(handle);
        self
    }

    pub fn with_cfg(mut self, cfg: ForkConfig) -> Self {
        self.cfg = Some(cfg);
        self
    }

    /// Import the fork's CRDT state back into the parent cache.
    ///
    /// Iterates every block in the child cache, exports its snapshot via
    /// `LoroDoc::export_snapshot()`, and applies it to the matching block in
    /// the parent cache via `apply_updates`. Loro's vector-clock CRDT
    /// semantics guarantee that concurrent edits on both sides converge
    /// deterministically — all ops from both timelines are preserved.
    ///
    /// If the parent cache no longer holds the block (e.g. it was evicted),
    /// the snapshot is still applied when the block is next loaded.
    ///
    /// Returns `ForkError::WrongIsolation` if called on a `Persistent` fork.
    /// Returns `ForkError::ParentDropped` if the parent `Arc` was dropped.
    pub fn merge_back_lightweight(&self) -> Result<crate::spawn::merge::MergeReport, ForkError> {
        let (child_cache, parent_weak, parent_agent_id) = match &self.isolation_state {
            ForkIsolationState::Lightweight {
                child_cache,
                parent_cache,
                parent_agent_id,
                ..
            } => (child_cache, parent_cache, parent_agent_id),
            ForkIsolationState::Persistent { .. } => return Err(ForkError::WrongIsolation),
            ForkIsolationState::Resolved => return Err(ForkError::AlreadyResolved),
        };

        let parent_cache = parent_weak.upgrade().ok_or(ForkError::ParentDropped)?;

        let mut report = crate::spawn::merge::MergeReport::default();

        for child_doc in child_cache.snapshot_cached_docs() {
            let snapshot = child_doc
                .export_snapshot()
                .map_err(|e| ForkError::Document(e.to_string()))?;
            let label = child_doc.label().to_string();

            // Try to locate the matching block in the parent cache.
            match parent_cache.get_cached_doc(parent_agent_id, &label) {
                Some(parent_doc) => {
                    parent_doc
                        .apply_updates(&snapshot)
                        .map_err(|e| ForkError::Document(e.to_string()))?;
                    report.blocks_merged += 1;
                }
                None => {
                    // Block was created inside the fork (no parent equivalent).
                    // Insert the snapshot directly into the parent cache.
                    // Preserve the originating document's schema and block_type
                    // so the subscriber worker renders the correct file format.
                    parent_cache
                        .insert_from_snapshot(
                            parent_agent_id,
                            label,
                            snapshot,
                            child_doc.schema().clone(),
                            child_doc.block_type(),
                        )
                        .map_err(|e| ForkError::MemoryStore(e.to_string()))?;
                    report.blocks_merged += 1;
                }
            }
        }

        Ok(report)
    }

    /// Import the persistent fork's CRDT state back into the parent cache.
    ///
    /// Composes a jj-level merge with a loro-level snapshot import:
    /// 1. Commit any outstanding child writes in the workspace so the merge
    ///    sees a clean working copy.
    /// 2. Run `jj new <bookmark> @` (with a synthesized describe) to create
    ///    a merge commit in the parent's workspace.
    /// 3. For each block in the child cache, apply its LoroDoc snapshot to
    ///    the matching parent block (or insert it if new).
    ///
    /// jj-level conflicts (concurrent edits to the same path on both sides)
    /// surface in the working-copy state, not as adapter errors — Loro CRDT
    /// converges deterministically on the in-memory side regardless. The
    /// `JjMerge` variant exists for actual command failures (unknown revset,
    /// IO error, etc.).
    ///
    /// Returns `ForkError::WrongIsolation` if called on a `Lightweight` fork.
    /// Returns `ForkError::ParentDropped` if the parent `Arc` was dropped.
    pub fn merge_back_persistent(&self) -> Result<crate::spawn::merge::MergeReport, ForkError> {
        let (workspace_path, bookmark_name, repo_root, child_cache, parent_weak, parent_agent_id) =
            match &self.isolation_state {
                ForkIsolationState::Persistent {
                    workspace_path,
                    bookmark_name,
                    repo_root,
                    child_cache,
                    parent_cache,
                    parent_agent_id,
                    ..
                } => (
                    workspace_path,
                    bookmark_name,
                    repo_root,
                    child_cache,
                    parent_cache,
                    parent_agent_id,
                ),
                ForkIsolationState::Lightweight { .. } => return Err(ForkError::WrongIsolation),
                ForkIsolationState::Resolved => return Err(ForkError::AlreadyResolved),
            };

        let parent_cache = parent_weak.upgrade().ok_or(ForkError::ParentDropped)?;
        let adapter = JjAdapter::detect()
            .map_err(|e| ForkError::JjOp {
                message: e.to_string(),
            })?
            .ok_or(ForkError::JjUnavailable)?;

        // 1. Commit outstanding child writes. `jj commit` succeeds even on an
        //    empty working copy, so this is safe to run unconditionally.
        adapter
            .commit(
                workspace_path,
                &format!("fork merge_back from {}", bookmark_name),
            )
            .map_err(|e| ForkError::JjOp {
                message: e.to_string(),
            })?;

        // 2. Run the jj-level merge in the repo root's workspace.
        //
        // We use `<workspace-name>@` to refer to the fork workspace's HEAD
        // AFTER the commit above. The bookmark is set at fork creation time
        // and does NOT auto-advance when `jj commit` runs in the child
        // workspace, so using the bookmark revset as a parent would merge
        // the fork's *pre-commit* state (the same as the root's `@`), which
        // collapses the merge to a single-parent fast-forward.
        //
        // `<name>@` is jj's revset syntax for "the working-copy commit of
        // workspace <name>"; after `jj commit`, the workspace `@` moves to
        // the new empty commit that follows the committed snapshot.
        let workspace_name = workspace_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| ForkError::JjOp {
                message: format!(
                    "workspace path has no file name: {}",
                    workspace_path.display()
                ),
            })?;
        // After `jj commit` the workspace `@` is the new empty continuation.
        // The committed snapshot is the parent of `@` — i.e. `<workspace>@-`.
        let fork_rev = format!("{workspace_name}@-");
        let parents: [&str; 2] = [fork_rev.as_str(), "@"];
        adapter
            .merge(
                repo_root,
                &parents,
                Some(&format!("merge fork {}", bookmark_name)),
            )
            .map_err(|e| ForkError::JjMerge {
                revsets: format!("{workspace_name}@-, @"),
                message: e.to_string(),
            })?;

        // 3. Reconcile loro state by importing every child block snapshot
        //    into the parent cache. Loro CRDT convergence handles concurrent
        //    edits deterministically.
        let mut report = crate::spawn::merge::MergeReport::default();
        for child_doc in child_cache.snapshot_cached_docs() {
            let snapshot = child_doc
                .export_snapshot()
                .map_err(|e| ForkError::Document(e.to_string()))?;
            let label = child_doc.label().to_string();

            match parent_cache.get_cached_doc(parent_agent_id, &label) {
                Some(parent_doc) => {
                    parent_doc
                        .apply_updates(&snapshot)
                        .map_err(|e| ForkError::Document(e.to_string()))?;
                }
                None => {
                    parent_cache
                        .insert_from_snapshot(
                            parent_agent_id,
                            label,
                            snapshot,
                            child_doc.schema().clone(),
                            child_doc.block_type(),
                        )
                        .map_err(|e| ForkError::MemoryStore(e.to_string()))?;
                }
            }
            report.blocks_merged += 1;
        }
        Ok(report)
    }

    /// Discard the fork: signal the child's cancel state and drop the child
    /// state without propagating any of its writes to the parent.
    ///
    /// For a `Lightweight` fork this is a pure in-memory teardown — the
    /// child cache Arc is dropped at end of scope.
    ///
    /// For a `Persistent` fork this is the path that releases jj's tracking:
    /// `bookmark_delete` first, then `workspace_forget`. Cleanup runs in that
    /// order so the cheap-to-orphan side fails first; both steps are attempted
    /// regardless of individual failures and any errors are collected into
    /// [`ForkError::DiscardCleanup`] so the caller can diagnose partial cleanup.
    ///
    /// **The workspace directory on disk is NOT deleted** — `jj workspace
    /// forget` removes the workspace from jj's metadata but leaves the
    /// underlying files in place. This is deliberate: if an agent discards a
    /// fork in error, the work is recoverable by re-importing the directory.
    /// If a partner wants the disk space back, they can `rm -rf` the path
    /// manually, or the agent can request shell/file permission and do it
    /// itself.
    ///
    /// Bare-drop / panic / session shutdown also preserve persistent state —
    /// see [`Drop`] for the durability contract.
    ///
    /// Consumes `self` so it cannot be called twice (compile-time guarantee).
    /// `ForkError::AlreadyResolved` is reserved for a hypothetical future
    /// `&mut self` variant but is never returned by the current implementation.
    pub fn discard(mut self) -> Result<(), ForkError> {
        // Abort the parent→child watcher first. If the fork is being
        // discarded cleanly (not as a result of parent cancellation), we do
        // not want the watcher task to wake up and fire `request_cancel` on
        // the child after the child state has already been dropped.
        //
        // We use `take()` so that when `Drop` runs on `self` at the end of
        // this method, the watcher field is already `None` and the Drop impl
        // does not attempt a redundant abort.
        if let Some(watcher) = self.cancel_watcher.take() {
            watcher.abort();
        }

        // Replace isolation_state with the sentinel before matching, so that
        // Drop (which runs when `self` goes out of scope at the end of this
        // method) observes `Resolved` and skips redundant cleanup.
        let state = std::mem::replace(&mut self.isolation_state, ForkIsolationState::Resolved);
        match state {
            ForkIsolationState::Lightweight { cancel_state, .. } => {
                cancel_state.request_cancel();
                // Dropping `self` here releases the child_cache Arc and all
                // forked LoroDoc instances. No import to parent occurs.
                Ok(())
            }
            ForkIsolationState::Persistent {
                workspace_path,
                bookmark_name,
                repo_root,
                cancel_state,
                ..
            } => {
                // Cancel first so no in-flight child writes race the delete.
                cancel_state.request_cancel();

                let adapter = JjAdapter::detect()
                    .map_err(|e| ForkError::JjOp {
                        message: e.to_string(),
                    })?
                    .ok_or(ForkError::JjUnavailable)?;

                // jj `workspace forget` takes the workspace name (not path).
                // The workspace_add path uses the directory-name-as-name
                // convention; pull it from the path's final component.
                let workspace_name = workspace_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| ForkError::JjOp {
                        message: format!(
                            "workspace path has no file name: {}",
                            workspace_path.display()
                        ),
                    })?;

                // Order: delete the bookmark first, then forget the
                // workspace. The bookmark is the cheap-to-orphan side
                // (a name in jj's bookmark list); the workspace is the
                // heavy state (on-disk files + a working-copy commit).
                // If bookmark_delete fails, we still want to attempt
                // workspace_forget so the heavier on-disk artifact is
                // reclaimed; both errors are collected.
                let mut errs: Vec<String> = Vec::new();
                if let Err(e) = adapter.bookmark_delete(&repo_root, &bookmark_name) {
                    errs.push(format!("bookmark_delete({}): {}", bookmark_name, e));
                }
                if let Err(e) = adapter.workspace_forget(&repo_root, workspace_name) {
                    errs.push(format!("workspace_forget({}): {}", workspace_name, e));
                }

                if errs.is_empty() {
                    Ok(())
                } else {
                    Err(ForkError::DiscardCleanup { errors: errs })
                }
            }
            ForkIsolationState::Resolved => {
                // Idempotent: `discard` consumes `self` so the linear-flow
                // double-call is impossible, but a future refactor that
                // adopts `&mut self` would land here. Treat as a no-op.
                Ok(())
            }
        }
    }

    /// Promote the fork into a new draft persona identity.
    ///
    /// Capability is checked against the spawner's snapshot — only a
    /// spawner with [`CapabilityFlag::SpawnNewIdentities`] may mint a
    /// new persona. The fork's current memory cache is extracted before
    /// the handle is consumed; for `Persistent` forks the workspace's
    /// outstanding writes are committed as a final revset prior to
    /// extraction so the promoted persona starts from a clean state.
    ///
    /// The seed memory cache is persisted to
    /// `<drafts_dir>/<persona_id>.cache/<label>.loro` (one file per cached
    /// LoroDoc) so the promoted persona's memory state survives the call.
    /// Phase 6's persona registry will re-load these snapshots when the
    /// draft is promoted to a live session.
    ///
    /// Returns the [`PersonaId`] minted from `cfg.name`. The draft KDL
    /// is written to `<drafts_dir>/<persona_id>.kdl` via
    /// [`RuntimeConfigWriter`]; the writer creates the directory lazily.
    ///
    /// # Errors
    ///
    /// - [`ForkError::CapabilityDenied`] if the spawner snapshot lacks
    ///   `SpawnNewIdentities`.
    /// - [`ForkError::JjUnavailable`] / [`ForkError::JjOp`] if a
    ///   persistent fork's final commit fails.
    /// - [`ForkError::Document`] if the draft KDL or seed-cache write
    ///   fails (I/O error is wrapped here for uniform reporting).
    pub fn promote(
        mut self,
        cfg: PersonaConfig,
        drafts_dir: &Path,
    ) -> Result<PersonaId, ForkError> {
        if !self
            .spawner_capabilities
            .has_flag(CapabilityFlag::SpawnNewIdentities)
        {
            return Err(ForkError::CapabilityDenied);
        }

        // Abort the watcher before any fallible work so it never fires
        // redundantly after the fork is consumed.
        if let Some(watcher) = self.cancel_watcher.take() {
            watcher.abort();
        }

        let persona_id: PersonaId = SmolStr::from(cfg.name.clone());

        // Extract the fork's memory state. For Persistent, commit any
        // outstanding workspace writes first so the promoted persona
        // can later inherit a clean revset. We do NOT delete the
        // bookmark — the promoted persona is expected to inherit it
        // (Phase 6 wires the inheritance).
        //
        // Replace isolation_state with the sentinel so Drop sees `Resolved`
        // rather than attempting cleanup after the state is already consumed.
        let state = std::mem::replace(&mut self.isolation_state, ForkIsolationState::Resolved);
        let seed_cache: Arc<MemoryCache> = match state {
            ForkIsolationState::Lightweight { child_cache, .. } => child_cache,
            ForkIsolationState::Persistent {
                child_cache,
                workspace_path,
                bookmark_name,
                ..
            } => {
                let adapter = JjAdapter::detect()
                    .map_err(|e| ForkError::JjOp {
                        message: e.to_string(),
                    })?
                    .ok_or(ForkError::JjUnavailable)?;
                adapter
                    .commit(
                        &workspace_path,
                        &format!("fork promote: {persona_id} (bookmark {bookmark_name})"),
                    )
                    .map_err(|e| ForkError::JjOp {
                        message: e.to_string(),
                    })?;
                child_cache
            }
            ForkIsolationState::Resolved => {
                unreachable!("promote called on an already-resolved ForkHandle")
            }
        };

        // Mint the draft KDL. Delegates to `sibling::mint_draft_kdl` —
        // the same helper used by `spawn_sibling_new` — so the file
        // format is identical and the persona loader ingests both shapes.
        let writer = RuntimeConfigWriter::new(drafts_dir.to_owned());
        let kdl = crate::spawn::sibling::mint_draft_kdl(&cfg);
        writer
            .write_draft(persona_id.as_str(), &kdl)
            .map_err(|e| ForkError::Document(format!("draft write: {e}")))?;

        // Persist the seed cache to disk so the promoted persona's memory
        // state survives this call. Each cached LoroDoc is exported as a
        // raw snapshot and written to
        //   <drafts_dir>/<persona_id>.cache/<label>.loro
        // alongside a `manifest.json` that lists every block's
        // (label, schema, block_type) tuple. The manifest is what the
        // promotion path (Phase 6 T6) reads to call
        // `MemoryCache::insert_from_snapshot`, which needs the schema and
        // block_type explicitly (they are NOT recoverable from the raw
        // snapshot bytes).
        let cache_dir = drafts_dir.join(format!("{persona_id}.cache"));
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            ForkError::Document(format!("create seed cache dir {cache_dir:?}: {e}"))
        })?;

        let mut manifest_entries: Vec<SeedCacheManifestEntry> = Vec::new();
        let mut docs_persisted: u32 = 0;
        for doc in seed_cache.snapshot_cached_docs() {
            let snapshot = doc
                .export_snapshot()
                .map_err(|e| ForkError::Document(format!("export_snapshot: {e}")))?;
            // Use the block label as the filename. Labels are validated by
            // `BlockCreate` so they are safe for use as path components; we
            // still sanitise `/` in case of composite labels.
            let safe_label = doc.label().replace('/', "__");
            let file_name = format!("{safe_label}.loro");
            let snap_path = cache_dir.join(&file_name);
            std::fs::write(&snap_path, &snapshot)
                .map_err(|e| ForkError::Document(format!("write seed cache {snap_path:?}: {e}")))?;

            manifest_entries.push(SeedCacheManifestEntry {
                file: file_name,
                label: doc.label().to_string(),
                schema: doc.schema().clone(),
                block_type: doc.block_type(),
            });
            docs_persisted += 1;
        }

        let manifest = SeedCacheManifest {
            version: SEED_CACHE_MANIFEST_VERSION,
            persona_id: persona_id.to_string(),
            entries: manifest_entries,
        };
        let manifest_path = cache_dir.join("manifest.json");
        let manifest_json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| ForkError::Document(format!("serialize seed cache manifest: {e}")))?;
        std::fs::write(&manifest_path, manifest_json).map_err(|e| {
            ForkError::Document(format!("write seed cache manifest {manifest_path:?}: {e}"))
        })?;

        tracing::info!(
            persona_id = %persona_id,
            docs_persisted,
            source = "runtime.spawn.fork.promote",
            "fork promoted to draft persona; seed cache persisted"
        );

        Ok(persona_id)
    }
}

// ── Drop ──────────────────────────────────────────────────────────────────────

impl Drop for ForkHandle {
    /// Abort the cancel-propagation watcher task on every resolution path.
    ///
    /// The watcher parks on `parent.wait_for_cancel()` and holds a strong
    /// `Arc<CancelState>` for the parent. Without this abort, an unresolved
    /// fork (dropped without calling `discard`, `merge_back`, or `promote`)
    /// keeps the watcher task alive indefinitely, extending the parent's
    /// `CancelState` lifetime and leaking a tokio task.
    ///
    /// Explicit resolution methods (`discard`, `promote`) already call
    /// `self.cancel_watcher.take().map(|h| h.abort())` before returning, so
    /// when `Drop` runs after them the field is already `None` and this abort
    /// call is a cheap no-op.
    ///
    /// # Durability of `Persistent` forks
    ///
    /// Bare-drop deliberately does NOT run `workspace_forget` /
    /// `bookmark_delete`. `Persistent` forks are durable on-disk state
    /// owned by the user — the workspace + bookmark must survive
    /// session restart, panic, scope exit, or any other implicit drop
    /// path. They are released from jj's tracking ONLY when the user
    /// explicitly calls [`ForkHandle::discard`] (drop jj tracking;
    /// on-disk files stay) or
    /// [`ForkHandle::merge_back_persistent`] (fold work into parent).
    /// Outstanding persistent forks at next startup are re-discoverable
    /// via the jj workspace list. Reclaiming the workspace directory's
    /// disk space is a manual `rm -rf` step (or an agent shell op
    /// behind permission), never automatic.
    ///
    /// Lightweight forks have no on-disk footprint, so bare-drop just
    /// releases the in-memory `Arc<MemoryCache>` and the cancel
    /// state — no cleanup needed.
    fn drop(&mut self) {
        if let Some(handle) = self.cancel_watcher.take() {
            handle.abort();
        }
    }
}

// ── WireForkHandle ────────────────────────────────────────────────────────────

/// Wire mirror of `ForkHandle` for the Haskell return direction.
///
/// Maps to the Haskell type:
/// ```haskell
/// data ForkHandle = ForkHandle
///   { forkHandleId      :: SpawnId
///   , forkHandleChildId :: SpawnId
///   }
/// ```
///
/// The `ToCore` encoding is positional — `fork_id` encodes at position 0,
/// `child_id` at position 1 — so the Haskell record selectors (`forkHandleId`,
/// `forkHandleChildId`) are documentation-only from the wire perspective.
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Spawn", name = "ForkHandle")]
pub struct WireForkHandle {
    /// Stable fork operation identifier.
    pub fork_id: String,
    /// Child session identifier.
    pub child_id: String,
}

impl From<&ForkHandle> for WireForkHandle {
    fn from(h: &ForkHandle) -> Self {
        Self {
            fork_id: h.fork_id.to_string(),
            child_id: h.child_id.to_string(),
        }
    }
}

// ── check_promote_capability ──────────────────────────────────────────────────

/// Gate the `promote()` resolution helper (Task 7) on the parent's capability
/// set.
///
/// Returns `Ok(())` when the parent holds
/// [`pattern_core::CapabilityFlag::SpawnNewIdentities`]. Returns
/// [`SpawnError::CapabilityEscalation`] otherwise — `promote()` creates a
/// new identity from a fork, which is the same capability class as
/// `SiblingPersona::New`.
pub fn check_promote_capability(
    parent_caps: &pattern_core::CapabilitySet,
) -> Result<(), SpawnError> {
    if parent_caps.has_flag(pattern_core::CapabilityFlag::SpawnNewIdentities) {
        Ok(())
    } else {
        Err(SpawnError::CapabilityEscalation {
            reason: "promote() requires CapabilityFlag::SpawnNewIdentities; \
                     parent does not hold this flag"
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory};

    use super::*;

    // ── ForkHandle round-trip ───────────────────────────────────────────────

    /// A `ForkHandle` converts into a `WireForkHandle` with the same ids.
    #[test]
    fn fork_handle_into_wire_preserves_ids() {
        let db = std::sync::Arc::new(
            pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"),
        );
        let child_cache = std::sync::Arc::new(pattern_memory::MemoryCache::new(db));
        let parent_cancel = std::sync::Arc::new(CancelState::new());
        let h = ForkHandle::new_lightweight(
            smol_str::SmolStr::from("fork-abc"),
            smol_str::SmolStr::from("child-xyz"),
            child_cache,
            smol_str::SmolStr::from("parent-agent"),
            std::sync::Weak::new(),
            parent_cancel,
        );
        let w = WireForkHandle::from(&h);
        assert_eq!(w.fork_id, "fork-abc");
        assert_eq!(w.child_id, "child-xyz");
    }

    // ── check_promote_capability ─────────────────────────────────────────────

    /// Parent with `SpawnNewIdentities` flag: `check_promote_capability` → Ok.
    #[test]
    fn promote_capability_ok_when_flag_present() {
        let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);
        assert!(
            check_promote_capability(&caps).is_ok(),
            "should be Ok when flag is held"
        );
    }

    /// Parent WITHOUT the flag: `check_promote_capability` → CapabilityEscalation.
    #[test]
    fn promote_capability_err_when_flag_absent() {
        let caps: CapabilitySet = [EffectCategory::Memory].into_iter().collect();
        let err = check_promote_capability(&caps).expect_err("should fail without the flag");
        match err {
            SpawnError::CapabilityEscalation { reason } => {
                assert!(
                    reason.contains("SpawnNewIdentities"),
                    "error should name the missing flag; got: {reason}"
                );
            }
            other => panic!("expected CapabilityEscalation, got {other:?}"),
        }
    }

    // ── ForkError display ────────────────────────────────────────────────────

    /// Each `ForkError` variant has a distinct, non-empty display message.
    #[test]
    fn fork_error_display_is_informative() {
        let cases: &[ForkError] = &[
            ForkError::WrongIsolation,
            ForkError::ParentDropped,
            ForkError::AlreadyResolved,
            ForkError::Document("doc-err".into()),
            ForkError::MemoryStore("store-err".into()),
            ForkError::PersistentNotAvailable {
                mode: "in-repo".into(),
            },
            ForkError::JjUnavailable,
            ForkError::BookmarkConflict {
                name: "agent/foo".into(),
            },
            ForkError::JjMerge {
                revsets: "agent/foo, @".into(),
                message: "boom".into(),
            },
            ForkError::DiscardCleanup {
                errors: vec!["a".into(), "b".into()],
            },
            ForkError::JjOp {
                message: "boom".into(),
            },
            ForkError::CapabilityDenied,
        ];
        for err in cases {
            let msg = err.to_string();
            assert!(
                !msg.is_empty(),
                "ForkError display must not be empty: {err:?}"
            );
        }
    }
}
