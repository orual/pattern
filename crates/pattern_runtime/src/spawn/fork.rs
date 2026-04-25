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
//! - `promote()` and `await_result()` land in Tasks 7-8.
//!
//! # Phase 2 backward compatibility
//!
//! The Phase 2 wire types (`WireForkHandle`) are preserved unchanged.
//! `ForkHandle` gained a richer `isolation_state` field in place of the
//! plain placeholder ids. `check_promote_capability` is preserved.

use std::sync::{Arc, Weak};

use pattern_memory::MemoryCache;
use smol_str::SmolStr;
use tidepool_bridge_derive::ToCore;

use crate::spawn::SpawnError;
use crate::timeout::CancelState;

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
    // Persistent-isolation variants (jj workflow) land in Tasks 4-6.
}

// ── ForkIsolationState ────────────────────────────────────────────────────────

/// Runtime state for a live fork, keyed by isolation mode.
///
/// Phase 3 Tasks 1-3 populate `Lightweight` fully. `Persistent` is a stub
/// that will be fleshed out in Tasks 4-6.
#[derive(Debug)]
pub enum ForkIsolationState {
    /// In-memory fork backed by `LoroDoc::fork()`.
    ///
    /// The child cache owns forked CRDT documents. No disk writes are
    /// produced. Dropping the `ForkHandle` (or calling `discard`) frees
    /// the forked state without touching the parent.
    Lightweight {
        /// The child's in-memory cache of forked LoroDoc instances.
        child_cache: Arc<MemoryCache>,
        /// Stable identifier for the child session (for log correlation).
        child_session_id: SmolStr,
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

    /// Persistent fork backed by a jj workspace (Tasks 4-6 stub).
    ///
    /// No fields yet — the struct is `#[non_exhaustive]` via the enum's
    /// containing type. Construction and resolution land in Tasks 4-6.
    Persistent {
        // Populated in Tasks 4-6.
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
}

impl ForkHandle {
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
            child_id: child_id.clone(),
            isolation_state: ForkIsolationState::Lightweight {
                child_cache,
                child_session_id: child_id,
                parent_cache,
                parent_agent_id,
                cancel_state,
            },
        }
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
                    parent_cache
                        .insert_from_snapshot(parent_agent_id, label, snapshot)
                        .map_err(|e| ForkError::MemoryStore(e.to_string()))?;
                    report.blocks_merged += 1;
                }
            }
        }

        Ok(report)
    }

    /// Discard the fork: signal the child's cancel state and drop the child
    /// cache without propagating any of its writes to the parent.
    ///
    /// Consumes `self` so it cannot be called twice (compile-time guarantee).
    /// `ForkError::AlreadyResolved` is reserved for a hypothetical future
    /// `&mut self` variant but is never returned by the current implementation.
    pub fn discard(self) -> Result<(), ForkError> {
        match &self.isolation_state {
            ForkIsolationState::Lightweight { cancel_state, .. } => {
                cancel_state.request_cancel();
                // Dropping `self` here releases the child_cache Arc and all
                // forked LoroDoc instances. No import to parent occurs.
                Ok(())
            }
            ForkIsolationState::Persistent { .. } => Err(ForkError::WrongIsolation),
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
        ];
        for err in cases {
            let msg = err.to_string();
            assert!(!msg.is_empty(), "ForkError display must not be empty: {err:?}");
        }
    }
}
