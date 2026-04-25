//! Child session registry: tracks live child handles and enforces per-parent
//! concurrency limits via a `tokio::sync::Semaphore`.
//!
//! # Cancel-on-drop contract
//!
//! When a `SpawnRegistry` is dropped (typically because the parent session
//! finishes or errors), `cancel_all` is called synchronously. Every child's
//! `CancelState::cancellation` atomic is flipped to `true`, which handlers
//! observe at their next effect boundary and return the
//! `CANCELLED_SENTINEL`. Semaphore permits held by ephemeral children are
//! released via permit drop, and the cached `Shared<BoxFuture>` results are
//! dropped. The underlying tokio tasks run to completion on their own once
//! they observe the cancel signal — the registry does not abort them.
//!
//! # Semaphore choice
//!
//! `tokio::sync::Semaphore` is used (rather than a counting atomic) because
//! the ephemeral dispatch path (Task 4) will acquire an `OwnedSemaphorePermit`
//! that is held inside `ChildSessionHandle._permit` for the child's lifetime.
//! The permit is released on handle drop (including from `cancel_all`), which
//! returns the slot to the semaphore atomically without any manual bookkeeping.
//!
//! # Mutex choice
//!
//! `parking_lot::Mutex` (sync) is used because `cancel_all` is called from
//! `Drop`, which cannot be async. A `tokio::sync::Mutex` would require an
//! async context; `parking_lot` works on any thread.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use smol_str::SmolStr;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::timeout::CancelState;

/// Discriminator for the kind of spawn a [`ChildSessionHandle`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    /// Short-lived worker that runs a program to completion and returns a
    /// result. Shares the parent's `CancelState`.
    Ephemeral,
    /// Fork of the parent's memory state with isolated compute. Shares the
    /// parent's `CancelState`. Phase 3 adds persistent (jj workspace)
    /// isolation.
    Fork,
    /// Independently-living persona session with its own `CancelState`.
    /// Siblings are NOT tracked in the parent's registry; this variant exists
    /// so `ChildSessionHandle` can distinguish child kinds in tests and future
    /// tooling.
    Sibling,
}

/// Reason an ephemeral child stopped running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationReason {
    /// Model produced final text and stopped (normal completion).
    EndTurn,
    /// Model wanted to keep going but the loop was stopped at a tool boundary.
    ToolUse,
    /// Hit the configured per-ephemeral max-turns ceiling.
    MaxTurns,
    /// Child exceeded its `EphemeralConfig::timeout`.
    Timeout,
    /// Parent cancelled the child via the registry.
    Cancelled,
    /// Child failed with a runtime error (see `SpawnError::Runtime`).
    Error,
}

/// Result returned when a child session completes (successfully or via a
/// non-error termination such as timeout or cancellation surfaced through
/// `terminated`).
///
/// Successful runs include final assistant text in `final_text`. The
/// `progress_log_label` points to the constellation-scoped Log block where
/// the runner appended one entry per wire turn.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SpawnResult {
    /// Session id of the child that produced this result.
    pub child_id: SmolStr,
    /// Final assistant text from the child's last terminal turn, if the
    /// child reached `EndTurn`. `None` for non-terminal stops.
    pub final_text: Option<String>,
    /// Number of wire turns the child drove before stopping.
    pub turns: u32,
    /// Why the child stopped.
    pub terminated: TerminationReason,
    /// Label of the constellation-scoped progress-log block, when the
    /// runner created one. `None` if log-block creation was skipped.
    pub progress_log_label: Option<String>,
}

impl SpawnResult {
    /// Build a `SpawnResult` with the minimum required fields and
    /// every optional slot empty. Provided for downstream consumers
    /// (notably integration tests) that need to construct the
    /// `#[non_exhaustive]` struct without listing every field.
    pub fn new(child_id: impl Into<SmolStr>, terminated: TerminationReason) -> Self {
        Self {
            child_id: child_id.into(),
            final_text: None,
            turns: 0,
            terminated,
            progress_log_label: None,
        }
    }
}

/// Errors a spawn operation can produce.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum SpawnError {
    /// The concurrency limit for ephemeral children has been reached.
    /// The caller should wait for a child to complete before spawning more.
    #[error("concurrent ephemeral limit reached for parent session: {limit}")]
    ConcurrencyLimitExceeded {
        /// The limit that was reached.
        limit: usize,
    },
    /// The spawn was cancelled by the parent session.
    #[error("spawn cancelled by parent")]
    Cancelled,
    /// The requested capability set asks for capabilities the parent does
    /// not hold. Children may never escalate beyond their parent's set.
    #[error("capability escalation: {reason}")]
    CapabilityEscalation {
        /// Human-readable description of the offending request.
        reason: String,
    },
    /// The ephemeral exceeded its configured timeout.
    #[error("ephemeral timeout exceeded ({timeout:?})")]
    Timeout {
        /// Timeout span that was exceeded.
        timeout: jiff::Span,
    },
    /// The async task driving the ephemeral panicked or was aborted.
    #[error("ephemeral worker panicked: {0}")]
    JoinPanicked(String),
    /// `AwaitSpawn` / `Stop` referenced a child id the registry does not
    /// know about. Cancellation is idempotent and ignores this; await
    /// surfaces it.
    #[error("spawn id not found in registry: {id}")]
    NotFound {
        /// The id that was looked up.
        id: SmolStr,
    },
    /// The synthesized `program` helper module failed to compile via
    /// the host's Haskell probe. Surfaces the GHC error verbatim so
    /// agents can fix the helper before retrying.
    #[error("program helper module failed to compile: {message}")]
    ProgramCompileFailed {
        /// Compiler diagnostic message.
        message: String,
    },
    /// Catch-all for runtime errors propagating from the agent loop or
    /// tidepool eval path. Carries the upstream message verbatim.
    #[error("ephemeral runtime error: {0}")]
    Runtime(String),
}

/// Handle to a running child session.
///
/// Holds the child's cancel state (shared with the child's context),
/// its result future, and any semaphore permit acquired for it. Dropping
/// the handle releases the permit (returning the slot to the parent's
/// semaphore) and drops the cached result future.
pub struct ChildSessionHandle {
    /// Session-scoped identifier for this child.
    pub child_id: SmolStr,
    /// Discriminator — ephemeral, fork, or sibling.
    pub kind: SpawnKind,
    /// Cancel state shared with the child's `SessionContext`.
    ///
    /// Ephemeral and fork children share the parent's `Arc<CancelState>`;
    /// sibling children have an independent one. `cancel_all` flips the
    /// atomic on every handle in the registry regardless.
    pub cancel_state: Arc<CancelState>,
    /// The background future running the child session, wrapped as
    /// `Shared` so multiple awaiters (`AwaitSpawn` + `AwaitAll` containing
    /// the same id) can poll the future without panicking. A raw
    /// `tokio::JoinHandle` is single-consume; `Shared<BoxFuture>` gives
    /// clone-and-multi-await safety at the cost of one heap allocation per
    /// child.
    pub result: Shared<BoxFuture<'static, Result<SpawnResult, SpawnError>>>,
    /// Semaphore permit held for the duration of ephemeral life.
    /// `Some` for `Ephemeral` children; `None` for fork and sibling.
    /// Dropped when the handle is dropped, returning the slot to the
    /// parent's semaphore.
    pub _permit: Option<OwnedSemaphorePermit>,
}

impl std::fmt::Debug for ChildSessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildSessionHandle")
            .field("child_id", &self.child_id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Tracks live child session handles for a parent session.
///
/// Enforces a per-parent concurrency limit on ephemeral children via a
/// `tokio::sync::Semaphore`. When the registry is dropped (parent session
/// ends), all registered children have their cancel state flipped and their
/// handles dropped — releasing semaphore permits and dropping result futures.
#[derive(Debug)]
pub struct SpawnRegistry {
    /// Session id of the parent that owns this registry.
    parent_id: SmolStr,
    /// Live child handles. `parking_lot::Mutex` (not `tokio::sync::Mutex`)
    /// because `cancel_all` is called from `Drop`, which is sync.
    children: Mutex<Vec<ChildSessionHandle>>,
    /// Semaphore governing the maximum number of concurrently live ephemeral
    /// children. Stored as `Arc` so `try_acquire_ephemeral_slot` can hand
    /// out `OwnedSemaphorePermit`s that are independent of the registry's
    /// lifetime.
    concurrent_ephemeral_limit: Arc<Semaphore>,
    /// The configured limit value. Stored separately so error messages and
    /// the `concurrent_ephemeral_limit` accessor can report the original
    /// ceiling without re-deriving it from `Semaphore::available_permits`
    /// (which fluctuates as permits are acquired and released).
    limit: usize,
}

impl SpawnRegistry {
    /// Create a new registry for `parent_id` with the given ephemeral
    /// concurrency ceiling.
    ///
    /// A limit of 0 means no ephemeral children are allowed; all
    /// `try_acquire_ephemeral_slot` calls will return `None`.
    pub fn new(parent_id: impl Into<SmolStr>, limit: usize) -> Self {
        Self {
            parent_id: parent_id.into(),
            children: Mutex::new(Vec::new()),
            concurrent_ephemeral_limit: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    /// Session id of the parent that owns this registry.
    pub fn parent_id(&self) -> &SmolStr {
        &self.parent_id
    }

    /// The configured concurrent-ephemeral ceiling. Handlers surface this
    /// in `SpawnError::ConcurrencyLimitExceeded` messages so operators can
    /// tune the limit.
    pub fn concurrent_ephemeral_limit(&self) -> usize {
        self.limit
    }

    /// Try to acquire a semaphore permit for a new ephemeral child.
    ///
    /// Returns `Some(permit)` if a slot is available, `None` if the limit
    /// has been reached. The permit must be stored in the
    /// `ChildSessionHandle._permit` field so the slot is returned when the
    /// handle is dropped.
    pub fn try_acquire_ephemeral_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.concurrent_ephemeral_limit
            .clone()
            .try_acquire_owned()
            .ok()
    }

    /// Register a child handle. The handle will be cancelled and dropped
    /// when `cancel_all` fires (including from `Drop`).
    pub fn register(&self, handle: ChildSessionHandle) {
        self.children.lock().push(handle);
    }

    /// Look up a registered child by id and await its result.
    ///
    /// Returns the cached `Shared<BoxFuture>` value cloned out of the
    /// registry — multiple awaiters can call this for the same id without
    /// stepping on each other (Shared::clone is cheap). Returns
    /// `SpawnError::NotFound` synchronously when no child with that id
    /// exists.
    pub async fn wait_for(&self, id: &SmolStr) -> Result<SpawnResult, SpawnError> {
        // Lock briefly to clone the Shared future, then drop the lock
        // before awaiting so nothing else blocks on registry mutation
        // while we wait.
        let fut = {
            let children = self.children.lock();
            children
                .iter()
                .find(|h| &h.child_id == id)
                .map(|h| h.result.clone())
                .ok_or_else(|| SpawnError::NotFound { id: id.clone() })?
        };
        fut.await
    }

    /// Set the cancel flag on a single registered child by id.
    ///
    /// Idempotent: a missing id is a no-op (matches the `Stop` effect's
    /// idempotence contract). Returns whether a child was found and
    /// flagged.
    pub fn cancel_one(&self, id: &SmolStr) -> bool {
        let children = self.children.lock();
        if let Some(handle) = children.iter().find(|h| &h.child_id == id) {
            handle
                .cancel_state
                .cancellation
                .store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Cancel all registered children and release their resources.
    ///
    /// For each child, the `cancellation` atomic on its `CancelState` is set
    /// to `true` (the child's handlers observe this at the next effect
    /// boundary). The children vec is then drained, which:
    /// - drops `OwnedSemaphorePermit`s → returns slots to the semaphore.
    /// - drops `Shared<BoxFuture>` → frees the cached result handle.
    ///
    /// This method is idempotent: calling it a second time after the vec
    /// has been drained is a no-op.
    pub fn cancel_all(&self) {
        let mut children = self.children.lock();
        // Signal every child's cancel state before dropping handles so the
        // child's next handler boundary observes the flag before any permit
        // is released back into the semaphore (ordering is not load-bearing
        // here, but it communicates intent clearly).
        for child in children.iter() {
            child
                .cancel_state
                .cancellation
                .store(true, Ordering::SeqCst);
        }
        // Clear the vec: drops permits (releases semaphore slots) and drops
        // Shared<BoxFuture> result caches. The underlying tokio tasks
        // continue to run until they observe the cancel flag — we just
        // forget about tracking them.
        children.clear();
    }
}

impl Drop for SpawnRegistry {
    fn drop(&mut self) {
        // Enforce AC3.6: parent session completing (or erroring) cancels all
        // children automatically. No async context required — cancel_all is
        // sync.
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;

    use super::*;

    /// Build a scripted `ChildSessionHandle` for use in unit tests.
    ///
    /// The result future resolves immediately to `Ok(SpawnResult { child_id })`;
    /// no real EvalWorker is involved. The permit is `None` unless the caller
    /// acquires one from a registry.
    fn scripted_handle(
        child_id: impl Into<SmolStr>,
        cancel_state: Arc<CancelState>,
        permit: Option<OwnedSemaphorePermit>,
    ) -> ChildSessionHandle {
        let id: SmolStr = child_id.into();
        let result = futures::future::ready(Ok(SpawnResult {
            child_id: id.clone(),
            final_text: None,
            turns: 0,
            terminated: TerminationReason::EndTurn,
            progress_log_label: None,
        }))
        .boxed()
        .shared();
        ChildSessionHandle {
            child_id: id,
            kind: SpawnKind::Ephemeral,
            cancel_state,
            result,
            _permit: permit,
        }
    }

    // ----- AC3.5: concurrency limit -----

    /// A registry with limit=2 allows two ephemeral slots and denies a
    /// third. Verifies the semaphore enforces the ceiling faithfully.
    #[test]
    fn try_acquire_ephemeral_slot_saturates_at_limit() {
        let reg = SpawnRegistry::new("parent-1", 2);

        let permit1 = reg.try_acquire_ephemeral_slot();
        let permit2 = reg.try_acquire_ephemeral_slot();
        let permit3 = reg.try_acquire_ephemeral_slot();

        assert!(permit1.is_some(), "first slot should be available");
        assert!(permit2.is_some(), "second slot should be available");
        assert!(permit3.is_none(), "third slot should be denied: limit is 2");

        // Explicitly hold permits alive until here so the compiler does not
        // drop them before the third acquire.
        drop(permit1);
        drop(permit2);
    }

    // ----- AC3.6: cancel propagation -----

    /// `cancel_all` flips every child's `CancelState::cancellation` to true.
    #[test]
    fn cancel_all_flips_child_cancel_state() {
        let reg = SpawnRegistry::new("parent-2", 4);
        let cs1 = Arc::new(CancelState::new());
        let cs2 = Arc::new(CancelState::new());

        reg.register(scripted_handle("child-a", cs1.clone(), None));
        reg.register(scripted_handle("child-b", cs2.clone(), None));

        assert!(!cs1.is_cancelled(), "precondition: child-a not cancelled");
        assert!(!cs2.is_cancelled(), "precondition: child-b not cancelled");

        reg.cancel_all();

        assert!(cs1.is_cancelled(), "child-a should be cancelled");
        assert!(cs2.is_cancelled(), "child-b should be cancelled");
    }

    /// Dropping the registry cancels all children (AC3.6 drop path).
    #[test]
    fn drop_cancels_all_children() {
        let cs = Arc::new(CancelState::new());
        {
            let reg = SpawnRegistry::new("parent-3", 4);
            reg.register(scripted_handle("child-c", cs.clone(), None));
            assert!(
                !cs.is_cancelled(),
                "precondition: not cancelled before drop"
            );
            // reg drops here
        }
        assert!(
            cs.is_cancelled(),
            "cancellation should be set after registry drop"
        );
    }

    /// `cancel_all` is idempotent: calling it a second time after the children
    /// vec has been drained must not panic or corrupt state.
    #[test]
    fn cancel_all_is_idempotent() {
        let reg = SpawnRegistry::new("parent-4", 4);
        let cs = Arc::new(CancelState::new());
        reg.register(scripted_handle("child-d", cs.clone(), None));

        reg.cancel_all();
        assert!(cs.is_cancelled(), "cancelled after first call");

        // Second call: no children remain; must be a no-op.
        reg.cancel_all();
        assert!(cs.is_cancelled(), "still cancelled after second call");
    }

    /// Two independent registries have independent semaphore limits.
    /// Acquiring slots from one does not affect the other.
    #[test]
    fn nested_registry_independent_limits() {
        let parent_reg = SpawnRegistry::new("parent-5", 2);
        let child_reg = SpawnRegistry::new("child-5", 1);

        let p1 = parent_reg.try_acquire_ephemeral_slot();
        let p2 = parent_reg.try_acquire_ephemeral_slot();
        let p3 = parent_reg.try_acquire_ephemeral_slot(); // should be None

        let c1 = child_reg.try_acquire_ephemeral_slot();
        let c2 = child_reg.try_acquire_ephemeral_slot(); // should be None

        assert!(p1.is_some(), "parent slot 1 available");
        assert!(p2.is_some(), "parent slot 2 available");
        assert!(p3.is_none(), "parent slot 3 denied (limit=2)");

        assert!(c1.is_some(), "child slot 1 available");
        assert!(c2.is_none(), "child slot 2 denied (limit=1)");

        drop(p1);
        drop(p2);
        drop(c1);
    }

    /// After `cancel_all`, the semaphore permits held by children are
    /// released (handles dropped), so a subsequent `try_acquire_ephemeral_slot`
    /// succeeds again.
    #[test]
    fn register_then_cancel_releases_permit_via_drop() {
        let reg = SpawnRegistry::new("parent-6", 1);

        // Acquire the only slot and hand it to a child handle.
        let permit = reg
            .try_acquire_ephemeral_slot()
            .expect("slot should be available at start");

        // After handing the permit to the handle, the slot is "consumed".
        let cs = Arc::new(CancelState::new());
        reg.register(scripted_handle("child-e", cs.clone(), Some(permit)));

        // Slot is held by the child handle; another acquire should fail.
        assert!(
            reg.try_acquire_ephemeral_slot().is_none(),
            "slot should be occupied by the registered child"
        );

        // cancel_all drops the handle, which drops the permit, returning the
        // slot to the semaphore.
        reg.cancel_all();

        // Now the slot is free again.
        let permit_after = reg.try_acquire_ephemeral_slot();
        assert!(
            permit_after.is_some(),
            "slot should be available after cancel_all released the permit"
        );
        drop(permit_after);
    }

    // ----- AC3.* — wait_for -----

    /// `wait_for(id)` resolves to the registered child's cached result.
    #[tokio::test]
    async fn wait_for_resolves_registered_child_result() {
        let reg = SpawnRegistry::new("parent-w1", 4);
        let cs = Arc::new(CancelState::new());
        reg.register(scripted_handle("child-w-a", cs, None));

        let res = reg
            .wait_for(&SmolStr::from("child-w-a"))
            .await
            .expect("wait_for must succeed for a registered child");
        assert_eq!(res.child_id.as_str(), "child-w-a");
        assert_eq!(res.terminated, TerminationReason::EndTurn);
    }

    /// `wait_for(id)` returns `NotFound` for an unknown id without
    /// touching any other state.
    #[tokio::test]
    async fn wait_for_unknown_id_returns_not_found() {
        let reg = SpawnRegistry::new("parent-w2", 4);
        let err = reg
            .wait_for(&SmolStr::from("does-not-exist"))
            .await
            .expect_err("wait_for must error on unknown id");
        match err {
            SpawnError::NotFound { id } => {
                assert_eq!(id.as_str(), "does-not-exist")
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // ----- cancel_one -----

    /// `cancel_one(id)` flips just the named child's cancel flag and
    /// leaves siblings untouched.
    #[test]
    fn cancel_one_flips_only_the_named_child() {
        let reg = SpawnRegistry::new("parent-c1", 4);
        let cs1 = Arc::new(CancelState::new());
        let cs2 = Arc::new(CancelState::new());
        reg.register(scripted_handle("child-c-a", cs1.clone(), None));
        reg.register(scripted_handle("child-c-b", cs2.clone(), None));

        let found = reg.cancel_one(&SmolStr::from("child-c-a"));
        assert!(found, "cancel_one should report the child was found");

        assert!(cs1.is_cancelled(), "child-c-a should be cancelled");
        assert!(
            !cs2.is_cancelled(),
            "child-c-b should NOT be cancelled by sibling cancel"
        );
    }

    /// `cancel_one(id)` is idempotent for unknown ids — returns false
    /// rather than panicking.
    #[test]
    fn cancel_one_unknown_id_is_noop() {
        let reg = SpawnRegistry::new("parent-c2", 4);
        let cs = Arc::new(CancelState::new());
        reg.register(scripted_handle("child-c-c", cs.clone(), None));

        let found = reg.cancel_one(&SmolStr::from("not-registered"));
        assert!(!found, "cancel_one returns false for unknown id");
        assert!(
            !cs.is_cancelled(),
            "registered child must not be affected by unrelated cancel_one"
        );
    }
}
