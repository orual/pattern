//! Per-session fork tracking — Phase 3 Task 8.
//!
//! Forks created via `Spawn.fork` (`handle_fork`) live in a session-scoped
//! [`ForkRegistry`] so subsequent `ForkOp` dispatches (`MergeBack`,
//! `Discard`, `Promote`, `AwaitResult`) can address them by id.
//!
//! The registry is a trait so production paths can swap in a DB-backed
//! implementation in Phase 6 (mirroring the
//! [`crate::spawn::sibling::SiblingPersonaResolver`] seam from Phase 2).
//! The default in-memory implementation is sufficient for the current
//! single-session use case.
//!
//! # Concurrency model
//!
//! Each registered handle is wrapped in a `parking_lot::Mutex` because
//! [`crate::spawn::fork::ForkHandle`] resolution helpers (`merge_back_*`,
//! `discard`) take `&self` / `self` respectively, but multiple callers
//! may race on the same fork via `MergeBack` (which preserves the handle)
//! and `Discard` / `Promote` (which consume it). Serialisation through
//! the mutex preserves the consume-once invariant: `remove` returns an
//! `Option<Arc<Mutex<ForkHandle>>>` and the caller must `try_unwrap` the
//! Arc to gain ownership for `discard` / `promote`.
//!
//! # Lifetime
//!
//! The registry lives on each spawner's `SessionContext`, not on the
//! daemon. Forks are scoped to the session that created them; when that
//! session closes the registry drops and all outstanding handles are
//! discarded (same Drop semantics as `SpawnRegistry`, on a different
//! collection).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use smol_str::SmolStr;

use crate::spawn::fork::{ForkError, ForkHandle};

/// Trait for tracking outstanding fork handles by id.
///
/// Phase 3 ships [`InMemoryForkRegistry`] as the default; Phase 6 swaps
/// in a DB-backed implementation that survives daemon restart.
pub trait ForkRegistry: Send + Sync + std::fmt::Debug {
    /// Insert a fork handle under `fork_id`.
    ///
    /// Returns [`ForkError::AlreadyExists`] if the id is already
    /// registered. Callers that want to replace a handle must
    /// [`ForkRegistry::remove`] it first.
    fn insert(&self, fork_id: SmolStr, handle: ForkHandle) -> Result<(), ForkError>;

    /// Look up a handle by id without removing it.
    ///
    /// Returns the `Arc<Mutex<ForkHandle>>` so callers can serialise
    /// non-consuming operations (e.g. `merge_back_lightweight`) on the
    /// same handle.
    fn get(&self, fork_id: &SmolStr) -> Option<Arc<Mutex<ForkHandle>>>;

    /// Remove a handle and return ownership of the inner [`ForkHandle`].
    ///
    /// Returns `None` if the id is not registered. Returns `Some(None)`
    /// when the id is registered but another caller still holds an
    /// `Arc` to the wrapping mutex; the caller must retry. The double
    /// `Option` keeps the contract honest about consume-once semantics
    /// without pulling in additional sync primitives.
    fn remove(&self, fork_id: &SmolStr) -> Option<Option<ForkHandle>>;

    /// List the ids of all currently-registered handles, for diagnostics
    /// and tests.
    fn list_ids(&self) -> Vec<SmolStr>;
}

/// Default in-process implementation of [`ForkRegistry`].
#[derive(Debug, Default)]
pub struct InMemoryForkRegistry {
    inner: Mutex<HashMap<SmolStr, Arc<Mutex<ForkHandle>>>>,
}

impl InMemoryForkRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ForkRegistry for InMemoryForkRegistry {
    fn insert(&self, fork_id: SmolStr, handle: ForkHandle) -> Result<(), ForkError> {
        let mut g = self.inner.lock();
        if g.contains_key(&fork_id) {
            return Err(ForkError::AlreadyExists {
                fork_id: fork_id.to_string(),
            });
        }
        g.insert(fork_id, Arc::new(Mutex::new(handle)));
        Ok(())
    }

    fn get(&self, fork_id: &SmolStr) -> Option<Arc<Mutex<ForkHandle>>> {
        self.inner.lock().get(fork_id).cloned()
    }

    fn remove(&self, fork_id: &SmolStr) -> Option<Option<ForkHandle>> {
        let arc = self.inner.lock().remove(fork_id)?;
        // Try to gain ownership. If another caller still holds an Arc
        // (e.g. mid-MergeBack), surface the partial result so the
        // caller can retry rather than silently dropping the handle.
        match Arc::try_unwrap(arc) {
            Ok(mutex) => Some(Some(mutex.into_inner())),
            Err(_arc_still_shared) => Some(None),
        }
    }

    fn list_ids(&self) -> Vec<SmolStr> {
        self.inner.lock().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Weak;

    use pattern_core::CapabilitySet;
    use pattern_db::ConstellationDb;
    use pattern_memory::MemoryCache;

    use crate::timeout::CancelState;

    fn build_fork(id: &str) -> ForkHandle {
        let db = Arc::new(ConstellationDb::open_in_memory().expect("open db"));
        let child_cache = Arc::new(MemoryCache::new(db));
        let cancel = Arc::new(CancelState::new());
        ForkHandle::new_lightweight(
            id.into(),
            format!("child-{id}").into(),
            child_cache,
            "parent".into(),
            Weak::new(),
            cancel,
        )
        .with_spawner_capabilities(CapabilitySet::all())
    }

    #[test]
    fn insert_then_get_returns_handle() {
        let reg = InMemoryForkRegistry::new();
        reg.insert("a".into(), build_fork("a")).expect("insert");
        let fetched = reg.get(&SmolStr::from("a"));
        assert!(fetched.is_some(), "get must return inserted handle");
        assert_eq!(reg.list_ids(), vec![SmolStr::from("a")]);
    }

    #[test]
    fn insert_duplicate_id_returns_already_exists() {
        let reg = InMemoryForkRegistry::new();
        reg.insert("dup".into(), build_fork("dup"))
            .expect("first insert");
        let err = reg
            .insert("dup".into(), build_fork("dup"))
            .expect_err("duplicate id must fail");
        match err {
            ForkError::AlreadyExists { fork_id } => assert_eq!(fork_id, "dup"),
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn remove_returns_owned_handle() {
        let reg = InMemoryForkRegistry::new();
        reg.insert("r".into(), build_fork("r")).expect("insert");
        let owned = reg
            .remove(&SmolStr::from("r"))
            .expect("registered id must be findable")
            .expect("no other Arc held → ownership available");
        assert_eq!(owned.fork_id.as_str(), "r");
        assert!(reg.list_ids().is_empty(), "remove drops the entry");
    }

    #[test]
    fn remove_unknown_id_returns_none() {
        let reg = InMemoryForkRegistry::new();
        assert!(reg.remove(&SmolStr::from("missing")).is_none());
    }

    #[test]
    fn remove_with_outstanding_arc_returns_some_none() {
        let reg = InMemoryForkRegistry::new();
        reg.insert("shared".into(), build_fork("shared"))
            .expect("insert");
        // Hold an Arc concurrently.
        let _outstanding = reg.get(&SmolStr::from("shared")).expect("get");
        let result = reg
            .remove(&SmolStr::from("shared"))
            .expect("entry was registered");
        assert!(
            result.is_none(),
            "remove must surface Some(None) when another Arc is still held"
        );
    }
}
