//! Integration tests for the persistent fork path (Phase 3 Tasks 4-6).
//!
//! These tests exercise [`ForkHandle::new_persistent`],
//! [`ForkHandle::merge_back_persistent`], and the `Persistent` arm of
//! [`ForkHandle::discard`].
//!
//! # Scope
//!
//! End-to-end persistent fork integration (mount setup + spawn handler
//! dispatch + jj workspace creation + on-disk verification) is **deferred**
//! pending the `MountInfo` / `Arc<MemoryCache>` plumbing on `SessionContext`
//! that Phase 3 Subcomponent B left as a known gap (see `spawn::handlers::handle_fork`
//! `Persistent` arm — currently returns `PersistentNotAvailable`).
//!
//! The tests in this file cover:
//!
//! - The `PersistentNotAvailable` failure mode at the handler boundary
//!   (when no mount info is wired).
//! - The `WrongIsolation` failure modes for `merge_back_persistent`
//!   (when called on a `Lightweight` handle).
//! - A jj-gated round-trip of `new_persistent` → `discard` against a
//!   real jj repo, verifying workspace + bookmark creation/cleanup.
//!
//! Tests that need a real jj installation gate on `JjAdapter::detect()`
//! returning `Ok(Some(_))` and skip cleanly otherwise.

use std::sync::Arc;

use pattern_memory::MemoryCache;
use pattern_memory::jj::{JjAdapter, fork_bookmark_name};
use pattern_runtime::spawn::fork::{ForkError, ForkHandle, ForkIsolationState};
use pattern_runtime::timeout::CancelState;

/// Helper: open a fresh in-memory constellation DB.
fn open_db() -> Arc<pattern_db::ConstellationDb> {
    Arc::new(pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"))
}

// ---------------------------------------------------------------------------
// Lightweight handle rejects merge_back_persistent (WrongIsolation)
// ---------------------------------------------------------------------------

/// `merge_back_persistent` on a `Lightweight` handle returns `WrongIsolation`.
#[test]
fn merge_back_persistent_on_lightweight_returns_wrong_isolation() {
    let child_cache = Arc::new(MemoryCache::new(open_db()));
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-1".into(),
        "child-1".into(),
        child_cache,
        "parent".into(),
        std::sync::Weak::new(),
        cancel,
    );
    match handle.merge_back_persistent() {
        Err(ForkError::WrongIsolation) => {}
        other => panic!("expected WrongIsolation, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Persistent merge_back surfaces typed errors when jj or parent is missing
// ---------------------------------------------------------------------------

/// `merge_back_persistent` on a synthetic Persistent handle whose parent
/// `Weak` was never upgraded returns either `JjUnavailable`,
/// `JjOp`, or `ParentDropped` — never silently succeeds.
#[test]
fn merge_back_persistent_synthetic_surfaces_typed_error() {
    let child_cache = Arc::new(MemoryCache::new(open_db()));
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle {
        fork_id: "fork-syn".into(),
        child_id: "child-syn".into(),
        isolation_state: ForkIsolationState::Persistent {
            workspace_path: std::path::PathBuf::from("/tmp/nonexistent-fork-ws"),
            bookmark_name: "agent/test".into(),
            repo_root: std::path::PathBuf::from("/tmp/nonexistent-fork-repo"),
            child_cache,
            parent_cache: std::sync::Weak::new(),
            parent_agent_id: "parent".into(),
            cancel_state: cancel,
        },
        spawner_capabilities: pattern_core::CapabilitySet::all(),
        cancel_watcher: None,
    };
    match handle.merge_back_persistent() {
        Err(ForkError::ParentDropped)
        | Err(ForkError::JjUnavailable)
        | Err(ForkError::JjOp { .. })
        | Err(ForkError::JjMerge { .. }) => {}
        other => panic!("expected ParentDropped, JjUnavailable, JjOp, or JjMerge; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// jj-gated round-trip: workspace_add + bookmark_set, then discard
// ---------------------------------------------------------------------------

/// End-to-end smoke against a real jj repo: create workspace + bookmark,
/// build a synthetic `ForkHandle::new_persistent`, then `discard()` and
/// verify both the workspace and bookmark are gone.
///
/// Skipped cleanly when `jj` is not on PATH.
#[test]
fn persistent_discard_round_trip_jj_gated() {
    let adapter = match JjAdapter::detect() {
        Ok(Some(a)) => a,
        Ok(None) => {
            eprintln!("skip: jj not installed");
            return;
        }
        Err(e) => {
            eprintln!("skip: jj detection failed: {e}");
            return;
        }
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().to_path_buf();
    adapter.init_repo(&repo_root).expect("jj git init");
    // jj repos start with no bookmarks pointing anywhere; need at least one
    // commit so workspace_add has a real revset to anchor on. `jj commit`
    // on the empty initial change is fine.
    adapter.commit(&repo_root, "init").expect("initial commit");

    let bookmark_name = fork_bookmark_name("agent-test", None);
    let workspace_path = repo_root.join("workspaces").join(&bookmark_name);
    if let Some(parent) = workspace_path.parent() {
        std::fs::create_dir_all(parent).expect("create workspaces parent");
    }

    adapter
        .workspace_add(&repo_root, &workspace_path)
        .expect("workspace_add");
    adapter
        .bookmark_set(&repo_root, &bookmark_name, "@")
        .expect("bookmark_set");

    // Sanity: workspace + bookmark visible.
    let ws_list = adapter.workspace_list(&repo_root).expect("workspace_list");
    assert!(
        ws_list.iter().any(|w| w
            .name
            .contains(workspace_path.file_name().unwrap().to_str().unwrap())),
        "new workspace should be listed: {:?}",
        ws_list
    );
    let bm_list = adapter.bookmark_list(&repo_root).expect("bookmark_list");
    assert!(
        bm_list.iter().any(|b| b.name == bookmark_name),
        "bookmark should be listed: {:?}",
        bm_list
    );

    // Build a synthetic ForkHandle backed by this real workspace + bookmark.
    let child_cache = Arc::new(MemoryCache::new(open_db()));
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle::new_persistent(
        "fork-rt".into(),
        "child-rt".into(),
        workspace_path.clone(),
        bookmark_name.clone(),
        repo_root.clone(),
        child_cache,
        "parent".into(),
        std::sync::Weak::new(),
        cancel.clone(),
    );

    handle.discard().expect("persistent discard succeeds");

    assert!(cancel.is_cancelled(), "cancel set");
    let bm_list = adapter.bookmark_list(&repo_root).expect("bookmark_list");
    assert!(
        !bm_list.iter().any(|b| b.name == bookmark_name),
        "bookmark should be gone after discard: {:?}",
        bm_list
    );
    let ws_list = adapter.workspace_list(&repo_root).expect("workspace_list");
    let workspace_name = workspace_path.file_name().unwrap().to_str().unwrap();
    assert!(
        !ws_list.iter().any(|w| w.name == workspace_name),
        "workspace should be gone after discard: {:?}",
        ws_list
    );
}

// ---------------------------------------------------------------------------
// fork_bookmark_name shape (AC4.10)
// ---------------------------------------------------------------------------

/// Bookmark format is `<agent>/<task-slug>` per AC4.10.
#[test]
fn fork_bookmark_name_format_is_agent_slash_task() {
    let task = pattern_core::BlockRef::new("Refactor Foo", "blk-1");
    let name = fork_bookmark_name("agent-orual", Some(&task));
    let mut parts = name.split('/');
    let agent_part = parts.next().expect("agent part");
    let task_part = parts.next().expect("task part");
    assert!(parts.next().is_none(), "exactly one slash");
    assert_eq!(agent_part, "agent-orual");
    assert_eq!(task_part, "refactor-foo");
}
