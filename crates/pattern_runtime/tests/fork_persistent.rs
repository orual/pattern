//! Integration tests for the persistent fork path (Phase 3 Tasks 4-6).
//!
//! These tests exercise [`ForkHandle::new_persistent`],
//! [`ForkHandle::merge_back_persistent`], and the `Persistent` arm of
//! [`ForkHandle::discard`].
//!
//! # Scope
//!
//! The tests in this file cover:
//!
//! - The `WrongIsolation` failure modes for `merge_back_persistent` and
//!   `merge_back_lightweight` (isolation-mode mismatch).
//! - A jj-gated round-trip of `new_persistent` → `discard` against a
//!   real jj repo, verifying workspace + bookmark creation/cleanup (AC4.8).
//! - A jj-gated round-trip of `new_persistent` → `merge_back_persistent`
//!   → verify CRDT state in parent, verifying the full merge path (C2).
//! - `fork_bookmark_name` format validation (AC4.10).
//!
//! Tests that need a real jj installation gate on `JjAdapter::detect()`
//! returning `Ok(Some(_))` and skip cleanly otherwise.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_memory::MemoryCache;
use pattern_memory::jj::{JjAdapter, fork_bookmark_name};
use pattern_runtime::spawn::fork::{ForkError, ForkHandle, ForkIsolationState};
use pattern_runtime::timeout::CancelState;

/// Helper: open a fresh in-memory constellation DB.
fn open_db() -> Arc<pattern_db::ConstellationDb> {
    Arc::new(pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"))
}

/// Helper: open a DB pre-seeded with the given agent ids.
fn open_db_with_agents(agent_ids: &[&str]) -> Arc<pattern_db::ConstellationDb> {
    let db = open_db();
    for &id in agent_ids {
        let agent = pattern_db::models::Agent {
            id: id.to_string(),
            name: format!("Persistent Test Agent {id}"),
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
        pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
            .expect("create_agent FK seed");
    }
    db
}

/// Helper: create a text block in the cache and set its initial content.
fn seed_text_block(cache: &MemoryCache, agent_id: &str, label: &str, content: &str) {
    let bc = BlockCreate::new(
        label.to_string(),
        MemoryBlockType::Working,
        BlockSchema::text(),
    );
    cache.create_block(agent_id, bc).expect("create_block");
    let doc = cache
        .get(agent_id, label)
        .expect("get after create")
        .expect("block must exist");
    doc.set_text(content, true).expect("set_text");
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
// C2: jj-gated merge_back_persistent — diamond concurrent edit + jj merge
// ---------------------------------------------------------------------------

/// Diamond concurrent-edit test for `merge_back_persistent`:
///
/// 1. Parent writes "parent-initial" (write A).
/// 2. Fork for child.
/// 3. Fork (child cache) writes "child-write-b" (write B).
/// 4. Parent (parent cache) writes "parent-write-c" CONCURRENTLY (write C).
/// 5. `merge_back_persistent` — jj-level merge + Loro CRDT convergence.
/// 6. Both B and C must survive in the merged parent cache (all appends
///    preserved — the CRDT invariant).
/// 7. The jj repo must contain a merge commit (2 parents) for `merge_back`'s
///    `jj new <bookmark> @` step.
///
/// This is the full C2 regression coverage. The previous test only verified
/// write B; this also verifies concurrent write C and the jj-level merge
/// commit, matching the lightweight diamond test in `fork_merge_lightweight.rs`.
///
/// Skipped cleanly when `jj` is not on PATH.
#[test]
fn merge_back_persistent_reconciles_crdt_state_jj_gated() {
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

    const PARENT_ID: &str = "merge-back-parent";
    const CHILD_ID: &str = "merge-back-child";
    const LABEL: &str = "notes";

    // Build shared DB + caches.
    let db = open_db_with_agents(&[PARENT_ID, CHILD_ID]);
    let parent_cache = Arc::new(MemoryCache::new(Arc::clone(&db)));

    // Write A: seed a block on the parent before forking.
    seed_text_block(&parent_cache, PARENT_ID, LABEL, "parent-initial");
    // Ensure it's in the cache before fork.
    let _ = parent_cache.get(PARENT_ID, LABEL).unwrap().unwrap();

    // Fork the parent's cache for the child.
    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(PARENT_ID, CHILD_ID)
            .expect("fork_for_child"),
    );

    // Init a real jj repo and create the fork workspace + bookmark.
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().to_path_buf();
    adapter.init_repo(&repo_root).expect("jj git init");
    adapter.commit(&repo_root, "init").expect("initial commit");

    let bookmark_name = fork_bookmark_name(PARENT_ID, None);
    let workspace_path = repo_root
        .join("workspaces")
        .join(bookmark_name.replace('/', "__"));
    if let Some(parent_dir) = workspace_path.parent() {
        std::fs::create_dir_all(parent_dir).expect("create workspaces parent");
    }
    adapter
        .workspace_add(&repo_root, &workspace_path)
        .expect("workspace_add");
    adapter
        .bookmark_set(&repo_root, &bookmark_name, "@")
        .expect("bookmark_set");

    // Write B: fork writes divergent content into the child cache.
    {
        let child_doc = child_cache
            .get_cached_doc(CHILD_ID, LABEL)
            .expect("child notes block must exist in child cache");
        child_doc
            .append_text(" child-write-b", true)
            .expect("append_text on child");
    }

    // Write C: CONCURRENT edit on the parent's cache (after the fork but
    // before merge_back). This simulates the parent session continuing to
    // work while the fork runs in parallel. Both B and C must survive via
    // Loro CRDT convergence.
    {
        let parent_doc = parent_cache
            .get(PARENT_ID, LABEL)
            .expect("get parent doc")
            .expect("notes block must exist");
        parent_doc
            .append_text(" parent-write-c", true)
            .expect("append_text on parent (concurrent write C)");
    }

    // Build the persistent ForkHandle and call merge_back.
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle::new_persistent(
        "fork-mbp".into(),
        CHILD_ID.into(),
        workspace_path.clone(),
        bookmark_name.clone(),
        repo_root.clone(),
        Arc::clone(&child_cache),
        PARENT_ID.into(),
        Arc::downgrade(&parent_cache),
        cancel,
    );

    handle
        .merge_back_persistent()
        .expect("merge_back_persistent must succeed with a real jj repo");

    // Assertion 1: CRDT convergence — both B and C must appear in the
    // merged parent cache. Neither write must be silently discarded.
    let parent_doc = parent_cache
        .get(PARENT_ID, LABEL)
        .expect("get")
        .expect("notes block must be in parent cache");
    let text = parent_doc.text_content();
    assert!(
        text.contains("child-write-b"),
        "parent cache must contain fork's write B after merge_back_persistent; got: {text:?}"
    );
    assert!(
        text.contains("parent-write-c"),
        "parent cache must contain parent's concurrent write C after merge_back_persistent; got: {text:?}"
    );

    // Assertion 2: jj-level merge commit. `merge_back_persistent` runs
    // `jj new <bookmark> @` which creates a commit with exactly two parents.
    // We inspect the jj log and check that at least one commit has ≥ 2 parents.
    let log_entries = adapter
        .log(&repo_root, "all()")
        .expect("jj log must succeed after merge_back_persistent");
    let has_merge_commit = log_entries.iter().any(|entry| entry.parents.len() >= 2);
    assert!(
        has_merge_commit,
        "jj repo must contain a merge commit (≥ 2 parents) after merge_back_persistent; \
         log entries: {log_entries:?}"
    );
}

// ---------------------------------------------------------------------------
// I5: jj-gated bookmark collision detection (AC4.10)
// ---------------------------------------------------------------------------

/// `handle_fork` must return `BookmarkConflict` when the target bookmark
/// already exists in the repo, rather than silently moving it.
///
/// The pre-check in `handle_fork_persistent` calls `bookmark_list` before
/// any workspace mutation so the repo stays clean on conflict detection.
///
/// Skipped cleanly when `jj` is not on PATH.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_fork_returns_bookmark_conflict_when_bookmark_exists_i5() {
    use pattern_core::ProviderClient;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_memory::modes::StorageMode;
    use pattern_runtime::NopProviderClient;
    use pattern_runtime::sdk::handlers::spawn::SpawnHandler;
    use pattern_runtime::sdk::requests::SpawnReq;
    use pattern_runtime::sdk::requests::spawn::{WireForkConfig, WireForkIsolation};
    use pattern_runtime::session::{MountInfo, SessionContext};
    use pattern_runtime::testing::{InMemoryMemoryStore, populated_spawn_test_table};
    use tidepool_effect::{EffectContext, EffectHandler};

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

    use pattern_core::types::block_ref::BlockRef;
    use pattern_runtime::sdk::requests::spawn::WireBlockRef;

    const AGENT_ID: &str = "bm-collision-agent";
    // A stable task label makes `fork_bookmark_name` deterministic so the
    // pre-created bookmark name matches what `handle_fork_persistent` computes
    // from the same `task_ref`. Without a task_ref, `fork_bookmark_name` falls
    // back to `anon-<random>` which is different each call.
    const TASK_LABEL: &str = "collision-task";

    // Init a real jj repo.
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().to_path_buf();
    adapter.init_repo(&repo_root).expect("jj git init");
    adapter.commit(&repo_root, "init").expect("initial commit");

    // Pre-create the bookmark that `fork_bookmark_name` would generate.
    // This simulates a second fork attempt for the same agent/task.
    let task_ref = BlockRef::new(TASK_LABEL, "test-block-id");
    let conflicting_name = fork_bookmark_name(AGENT_ID, Some(&task_ref));
    adapter
        .bookmark_set(&repo_root, &conflicting_name, "@")
        .expect("pre-create conflicting bookmark");

    // Build a parent session with MountInfo pointing at the real repo.
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"));
    let db_with_agents = open_db_with_agents(&[AGENT_ID]);
    let parent_cache = Arc::new(MemoryCache::new(db_with_agents));
    let persona = PersonaSnapshot::new(AGENT_ID, AGENT_ID);
    let parent = Arc::new(
        SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        )
        .with_memory_cache(parent_cache)
        .with_mount_info(MountInfo {
            repo_root: repo_root.clone(),
            workspace_root: repo_root.join("workspaces"),
            mode: StorageMode::Standalone {
                mount_path: repo_root.clone(),
                project_id: "test-project".to_string(),
            },
            jj_enabled: true,
        }),
    );

    // Supply the same task label so `handle_fork_persistent` generates the
    // same bookmark name and hits the pre-existing conflict.
    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Persistent,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: Some(WireBlockRef {
            label: TASK_LABEL.to_string(),
            block_id: "test-block-id".to_string(),
            agent_id: "_constellation_".to_string(),
        }),
    };

    let parent_clone = parent.clone();
    let err = tokio::task::spawn_blocking(move || {
        let table = populated_spawn_test_table();
        let cx = EffectContext::with_user(&table, parent_clone.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok")
    .expect_err("fork with conflicting bookmark must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("bookmark already exists") || msg.contains(&conflicting_name),
        "error must describe the bookmark conflict; got: {msg}"
    );

    // Registry must not have a handle — the conflict is detected before any
    // workspace mutation.
    assert!(
        parent.fork_registry().list_ids().is_empty(),
        "no handle must be registered when bookmark collision is detected"
    );
}

// ---------------------------------------------------------------------------
// I6: cleanup via handler path — workspace_forget + bookmark_delete both run
// ---------------------------------------------------------------------------

/// Verify that when a persistent fork is discarded through the handler, BOTH
/// `workspace_forget` AND `bookmark_delete` are called, leaving the jj repo in
/// a clean state with no leaked workspace or bookmark.
///
/// This test drives the full handler path:
///   `SpawnHandler::handle(Fork)` → `SpawnHandler::handle(ForkOp::Discard)`
///
/// Cleanup validation:
/// - The workspace must be absent from `jj workspace list` after discard.
/// - The bookmark must be absent from `jj bookmark list` after discard.
///
/// `ForkHandle::discard()` for a Persistent handle calls both
/// `workspace_forget` and `bookmark_delete` unconditionally, which is the
/// same pair of cleanup calls that `handle_fork_persistent` makes when
/// `fork_for_child` fails mid-setup. Driving them through the handler path
/// (rather than via `JjAdapter` directly) tests that the handler correctly
/// wires the cleanup without leaking resources.
///
/// The partial-failure scenario (workspace_add succeeds but bookmark_set
/// fails, calling workspace_forget only) cannot be exercised reliably without
/// a `JjAdapter` test-double seam — `bookmark_set` with revset `@` is always
/// valid in a properly initialised repo. That path is covered by reading the
/// handler source directly; the observable invariant here is the end-state
/// after a full discard.
///
/// Skipped cleanly when `jj` is not on PATH.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_fork_handler_cleanup_both_workspace_and_bookmark_i6() {
    use pattern_core::ProviderClient;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::block_ref::BlockRef;
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_memory::modes::StorageMode;
    use pattern_runtime::NopProviderClient;
    use pattern_runtime::sdk::handlers::spawn::SpawnHandler;
    use pattern_runtime::sdk::requests::SpawnReq;
    use pattern_runtime::sdk::requests::spawn::{
        WireBlockRef, WireForkConfig, WireForkIsolation, WireForkOpKind,
    };
    use pattern_runtime::session::{MountInfo, SessionContext};
    use pattern_runtime::testing::{InMemoryMemoryStore, populated_spawn_test_table};
    use tidepool_effect::{EffectContext, EffectHandler};

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

    const AGENT_ID: &str = "i6-cleanup-agent";
    const TASK_LABEL: &str = "i6-cleanup-task";

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().to_path_buf();
    adapter.init_repo(&repo_root).expect("jj git init");
    adapter.commit(&repo_root, "init").expect("initial commit");

    // Build a parent session with MountInfo pointing at the real repo.
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().expect("open db"));
    let db_with_agents = open_db_with_agents(&[AGENT_ID]);
    let parent_cache = Arc::new(MemoryCache::new(db_with_agents));
    let persona = PersonaSnapshot::new(AGENT_ID, AGENT_ID);
    let workspace_root = repo_root.join("workspaces");
    std::fs::create_dir_all(&workspace_root).expect("create workspaces dir");
    let parent = Arc::new(
        SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        )
        .with_memory_cache(parent_cache)
        .with_mount_info(MountInfo {
            repo_root: repo_root.clone(),
            workspace_root: workspace_root.clone(),
            mode: StorageMode::Standalone {
                mount_path: repo_root.clone(),
                project_id: "test-project".to_string(),
            },
            jj_enabled: true,
        }),
    );

    // Step 1: Fork via the handler (creates workspace + bookmark in jj).
    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Persistent,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: Some(WireBlockRef {
            label: TASK_LABEL.to_string(),
            block_id: "test-block-id".to_string(),
            agent_id: "_constellation_".to_string(),
        }),
    };
    let parent_clone = parent.clone();
    tokio::task::spawn_blocking(move || {
        let table = populated_spawn_test_table();
        let cx = EffectContext::with_user(&table, parent_clone.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok")
    .expect("persistent Fork via handler must succeed");

    // Confirm jj repo has the workspace and bookmark after the fork.
    let task_ref = BlockRef::new(TASK_LABEL, "test-block-id");
    let expected_bookmark = fork_bookmark_name(AGENT_ID, Some(&task_ref));

    let ws_list = adapter
        .workspace_list(&repo_root)
        .expect("workspace_list after fork");
    let safe_dir = expected_bookmark.replace('/', "__");
    assert!(
        ws_list.iter().any(|w| w.name.contains(&safe_dir)),
        "workspace must exist in jj after Fork via handler; got: {ws_list:?}"
    );
    let bm_list = adapter
        .bookmark_list(&repo_root)
        .expect("bookmark_list after fork");
    assert!(
        bm_list.iter().any(|b| b.name == expected_bookmark),
        "bookmark must exist in jj after Fork via handler; got: {bm_list:?}"
    );

    // Step 2: Discard via the handler (cleans up workspace + bookmark).
    let fork_ids = parent.fork_registry().list_ids();
    assert_eq!(fork_ids.len(), 1, "must have exactly one fork registered");
    let fork_id = fork_ids[0].to_string();

    let parent_clone = parent.clone();
    let fork_id_s = fork_id.clone();
    tokio::task::spawn_blocking(move || {
        let table = populated_spawn_test_table();
        let cx = EffectContext::with_user(&table, parent_clone.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::ForkOp(fork_id_s, WireForkOpKind::Discard), &cx)
    })
    .await
    .expect("spawn_blocking ok")
    .expect("ForkOp::Discard via handler must succeed");

    // Confirm both workspace AND bookmark are gone after discard — the
    // cleanup must have called workspace_forget AND bookmark_delete.
    let ws_list = adapter
        .workspace_list(&repo_root)
        .expect("workspace_list after discard");
    assert!(
        !ws_list.iter().any(|w| w.name.contains(&safe_dir)),
        "workspace must be gone after discard (workspace_forget ran); got: {ws_list:?}"
    );
    let bm_list = adapter
        .bookmark_list(&repo_root)
        .expect("bookmark_list after discard");
    assert!(
        !bm_list.iter().any(|b| b.name == expected_bookmark),
        "bookmark must be gone after discard (bookmark_delete ran); got: {bm_list:?}"
    );

    // Registry must also be empty after discard.
    assert!(
        parent.fork_registry().list_ids().is_empty(),
        "fork registry must be empty after discard"
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
