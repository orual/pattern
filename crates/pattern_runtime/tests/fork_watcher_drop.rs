//! Regression tests for Critical #1: cancel_watcher abort on all ForkHandle
//! resolution paths.
//!
//! Prior to the fix, `cancel_watcher` was only aborted in `discard()`. On
//! `merge_back_lightweight`, `promote`, or bare-drop, the watcher task
//! continued running indefinitely, holding a strong `Arc<CancelState>` to
//! the parent and leaking a tokio task.
//!
//! These tests verify that after each resolution path the leaked-Arc count
//! returns to baseline by observing `Arc::strong_count` after waiting for
//! the aborted task to be cleaned up by the runtime.
//!
//! # Design
//!
//! We use an abort-handle-based approach: before attaching the watcher to the
//! ForkHandle, we retain the task's `AbortHandle` so we can independently
//! join/check the task after the ForkHandle is dropped. We then wait (with a
//! bounded spin) for the strong count to converge.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use pattern_core::CapabilitySet;
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_runtime::spawn::fork::{ForkHandle, ForkIsolationState};
use pattern_runtime::timeout::CancelState;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn open_db() -> Arc<ConstellationDb> {
    Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"))
}

/// Build a lightweight ForkHandle (no watcher attached yet).
fn build_lightweight(
    parent_id: &str,
    child_id: &str,
) -> (Arc<MemoryCache>, Arc<MemoryCache>, ForkHandle) {
    let db = open_db();
    let agent = pattern_db::models::Agent {
        id: parent_id.to_string(),
        name: format!("Watcher Test Agent {parent_id}"),
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("seed parent agent");
    pattern_db::queries::create_agent(
        &db.get().unwrap(),
        &pattern_db::models::Agent {
            id: child_id.to_string(),
            name: format!("Watcher Test Child {child_id}"),
            ..agent
        },
    )
    .expect("seed child agent");

    let parent_cache = Arc::new(MemoryCache::new(Arc::clone(&db)));
    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(parent_id, child_id)
            .expect("fork_for_child"),
    );
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-watcher-test".into(),
        child_id.into(),
        Arc::clone(&child_cache),
        parent_id.into(),
        Arc::downgrade(&parent_cache),
        cancel,
    )
    .with_spawner_capabilities(CapabilitySet::all());
    (parent_cache, child_cache, handle)
}

/// Spawn a watcher task that parks on parent cancel and holds a counter Arc.
///
/// Returns the JoinHandle (to be given to ForkHandle) and a reference to the
/// counter so we can check after abort that the counter's refcount has dropped.
fn spawn_watcher(counter: Arc<AtomicUsize>) -> tokio::task::JoinHandle<()> {
    // The watcher holds a strong Arc to `counter`. When the task is aborted
    // and its future dropped, that strong ref is released.
    let counter_for_task = Arc::clone(&counter);
    // We use an infinite loop to simulate a task that parks indefinitely
    // (like `wait_for_cancel()`). The `std::hint::black_box` call prevents
    // the compiler from optimising the loop away.
    tokio::spawn(async move {
        let _hold = counter_for_task; // keep the Arc alive
        // Park until externally aborted.
        loop {
            tokio::task::yield_now().await;
        }
    })
}

/// Wait up to `timeout_ms` for `Arc::strong_count(arc)` to equal `expected`.
/// Yields to the tokio executor between checks.
async fn wait_for_count<T>(arc: &Arc<T>, expected: usize, timeout_ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        tokio::task::yield_now().await;
        if Arc::strong_count(arc) == expected {
            return;
        }
        if std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

// ---------------------------------------------------------------------------
// Critical #1 — path A: merge_back_lightweight + drop
// ---------------------------------------------------------------------------

/// After `merge_back_lightweight` is called and the handle is dropped, the
/// watcher task must be aborted.
///
/// We verify this by checking that an Arc held by the watcher's closure is
/// released back to baseline strong_count after the handle drops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_aborted_after_merge_back_lightweight_and_drop() {
    let counter = Arc::new(AtomicUsize::new(0));
    // Baseline: one Arc — the `counter` binding in this test.
    assert_eq!(Arc::strong_count(&counter), 1);

    let (parent_cache, _child_cache, handle) =
        build_lightweight("watcher-merge-parent", "watcher-merge-child");

    let watcher = spawn_watcher(Arc::clone(&counter));
    // counter strong_count is now 2 (test binding + task closure).
    let handle = handle.with_cancel_watcher(watcher);

    // merge_back_lightweight does NOT consume the handle.
    let _ = handle.merge_back_lightweight();

    // Drop the handle — this is the path the fix targets.
    drop(handle);
    drop(parent_cache);

    // Wait for the runtime to process the abort and release the task's Arc.
    wait_for_count(&counter, 1, 500).await;

    assert_eq!(
        Arc::strong_count(&counter),
        1,
        "watcher task's Arc must be released after merge_back_lightweight + drop"
    );
}

// ---------------------------------------------------------------------------
// Critical #1 — path B: promote
// ---------------------------------------------------------------------------

/// After `promote` consumes the handle, the watcher task must be aborted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_aborted_after_promote() {
    use pattern_core::CapabilityFlag;
    use pattern_core::spawn::PersonaConfig;

    let counter = Arc::new(AtomicUsize::new(0));
    assert_eq!(Arc::strong_count(&counter), 1);

    let (_parent_cache, _child_cache, handle) =
        build_lightweight("watcher-promote-parent", "watcher-promote-child");

    let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);
    let handle = handle.with_spawner_capabilities(caps);
    let watcher = spawn_watcher(Arc::clone(&counter));
    let handle = handle.with_cancel_watcher(watcher);

    let drafts = tempfile::TempDir::new().expect("tempdir");
    let cfg = PersonaConfig::new(
        "watcher-draft",
        "you are a watcher-drop test draft",
        CapabilitySet::empty(),
    );
    // `promote` consumes the handle and must abort the watcher before returning.
    let _pid = handle
        .promote(cfg, drafts.path())
        .expect("promote must succeed with SpawnNewIdentities flag");

    wait_for_count(&counter, 1, 500).await;

    assert_eq!(
        Arc::strong_count(&counter),
        1,
        "watcher task's Arc must be released after promote"
    );
}

// ---------------------------------------------------------------------------
// Critical #1 — path C: bare drop (no resolution method)
// ---------------------------------------------------------------------------

/// Dropping a `ForkHandle` without calling any resolution method must abort
/// the watcher (e.g. the caller panicked or returned early from an error path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_aborted_on_bare_drop() {
    let counter = Arc::new(AtomicUsize::new(0));
    assert_eq!(Arc::strong_count(&counter), 1);

    let db = open_db();
    let child_cache = Arc::new(MemoryCache::new(Arc::clone(&db)));
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle {
        fork_id: "bare-drop-fork".into(),
        child_id: "bare-drop-child".into(),
        isolation_state: ForkIsolationState::Lightweight {
            child_cache,
            parent_cache: std::sync::Weak::new(),
            parent_agent_id: "bare-drop-parent".into(),
            cancel_state: cancel,
        },
        spawner_capabilities: CapabilitySet::all(),
        cancel_watcher: None,
    };

    let watcher = spawn_watcher(Arc::clone(&counter));
    let handle = handle.with_cancel_watcher(watcher);

    // Drop without calling any resolution method.
    drop(handle);

    wait_for_count(&counter, 1, 500).await;

    assert_eq!(
        Arc::strong_count(&counter),
        1,
        "watcher task's Arc must be released after bare drop"
    );
}
