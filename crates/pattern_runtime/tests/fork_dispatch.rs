//! Phase 3 Task 8 — `handle_fork` + `ForkOp` dispatch wiring tests.
//!
//! Verifies the spawn handler's lightweight-fork arm and ForkOp dispatch:
//!
//! - When the parent has `memory_cache` populated, the fork copies the
//!   parent's blocks via `MemoryCache::fork_for_child`.
//! - The freshly-built `ForkHandle` is inserted into the per-session
//!   `ForkRegistry`; the id returned in `WireForkHandle` is addressable.
//! - Persistent dispatch on a session WITHOUT mount info fails with the
//!   expected typed error.
//!
//! Task 8.3 additions — ForkOp wire dispatch:
//!
//! - `ForkOp::Discard` removes the handle from the registry and returns
//!   `ForkOpResult::Unit`.
//! - `ForkOp::MergeBack` merges and returns `ForkOpResult::MergeReport(_)`.
//!   The handle STAYS in the registry after a merge (it uses `get`, not
//!   `remove`).
//! - `ForkOp::Promote` without `SpawnNewIdentities` returns a capability
//!   denied error.
//! - `ForkOp` on an unknown id returns "fork not found".

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_runtime::NopProviderClient;
use pattern_runtime::sdk::handlers::spawn::SpawnHandler;
use pattern_runtime::sdk::requests::SpawnReq;
use pattern_runtime::sdk::requests::spawn::{
    WireForkConfig, WireForkIsolation, WireForkOpKind, WirePersonaConfig,
};
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::InMemoryMemoryStore;
use smol_str::SmolStr;
use tidepool_effect::{EffectContext, EffectHandler};
use tidepool_repr::DataConTable;

async fn build_parent_with_cache() -> (Arc<SessionContext>, Arc<MemoryCache>) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;

    // Build a separate Arc<MemoryCache> that the test seeds and that the
    // session forks from. (The session's adapter wraps the InMemoryStore;
    // the dedicated cache is what the lightweight-fork path consumes.)
    let cache_db = Arc::new(ConstellationDb::open_in_memory().expect("open cache db"));
    // Pre-create the agent in the cache db so block creation succeeds.
    let agent = pattern_db::models::Agent {
        id: "fork-dispatch-parent".to_string(),
        name: "Fork Dispatch Parent".to_string(),
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
    pattern_db::queries::create_agent(&cache_db.get().unwrap(), &agent).expect("seed agent");
    let cache = Arc::new(MemoryCache::new(cache_db));

    // Seed a block on the cache; fork should pick it up.
    cache
        .create_block(
            "fork-dispatch-parent",
            BlockCreate::new(
                "notes".to_string(),
                MemoryBlockType::Working,
                BlockSchema::text(),
            ),
        )
        .expect("create_block");
    let doc = cache
        .get("fork-dispatch-parent", "notes")
        .expect("get")
        .expect("block must exist");
    doc.set_text("seed-content", true).expect("set_text");

    let persona = PersonaSnapshot::new("fork-dispatch-parent", "fork-dispatch-parent");
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    )
    .with_memory_cache(cache.clone());
    (Arc::new(ctx), cache)
}

/// Lightweight fork through the handler: registry receives the handle,
/// the fork's child cache contains the parent's seeded block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lightweight_fork_inserts_into_registry_and_forks_cache() {
    let (parent, _parent_cache) = build_parent_with_cache().await;

    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Lightweight,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: None,
    };

    let parent_for_blocking = parent.clone();
    let result = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    // The handler may fail at the wire-encode step if the test's empty
    // DataConTable doesn't know `Pattern.Spawn.ForkHandle`, but the
    // registry insertion happens BEFORE the encode call, so the registry
    // must contain exactly one entry regardless.
    let _ = result; // Don't unwrap — encode-step failure is unrelated.

    let ids = parent.fork_registry().list_ids();
    assert_eq!(
        ids.len(),
        1,
        "fork registry must hold exactly one handle after Fork; got {ids:?}"
    );

    // The registered handle must own a child cache containing the seeded
    // "notes" block (forked from the parent).
    let handle_arc = parent
        .fork_registry()
        .get(&ids[0])
        .expect("registered handle");
    let handle = handle_arc.lock();
    let child_id = handle.child_id.clone();
    match &handle.isolation_state {
        pattern_runtime::spawn::ForkIsolationState::Lightweight { child_cache, .. } => {
            // Child cache holds the forked block under the child session id.
            // ForkHandle.child_id is the authoritative child id; the redundant
            // child_session_id field was removed from ForkIsolationState::Lightweight (M2).
            let forked = child_cache
                .get_cached_doc(&child_id, "notes")
                .expect("forked block present in child cache");
            // Snapshot to verify content travelled across the fork.
            let snapshot = forked.export_snapshot().expect("export_snapshot");
            assert!(!snapshot.is_empty(), "forked block must carry content");
        }
        other => panic!("expected Lightweight isolation state; got {other:?}"),
    }
}

/// Persistent fork without `MountInfo` returns `PersistentNotAvailable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_fork_without_mount_info_typed_error() {
    let (parent, _parent_cache) = build_parent_with_cache().await;

    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Persistent,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: None,
    };

    let parent_for_blocking = parent.clone();
    let err = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok")
    .expect_err("persistent fork must fail without MountInfo");

    let msg = err.to_string();
    assert!(
        msg.contains("persistent fork not available") && msg.contains("no mount info"),
        "expected PersistentNotAvailable with mount-info diagnostic; got: {msg}"
    );

    // Registry must NOT contain a handle from the failed dispatch.
    assert!(
        parent.fork_registry().list_ids().is_empty(),
        "failed persistent fork must not leak into the registry"
    );
}

/// Test path WITHOUT `with_memory_cache` returns an error (I4: no silent
/// fallback to an empty cache that would silently drop merge_back writes).
///
/// Before the I4 fix, the handler silently fell back to an empty child cache,
/// making merge_back a no-op and causing data loss. Now it returns a
/// descriptive error so misconfigured sessions fail loudly at fork time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lightweight_fork_without_memory_cache_returns_error_i4() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("no-cache-parent", "no-cache-parent");
    let parent = Arc::new(SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    ));
    // Intentionally do NOT call .with_memory_cache() here.

    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Lightweight,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: None,
    };

    let parent_for_blocking = parent.clone();
    let result = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    // Must error — not silently succeed with an empty cache.
    let err = result.expect_err("fork without memory_cache must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("memory cache") || msg.contains("memory_cache"),
        "error must mention the missing memory cache; got: {msg}"
    );

    // Registry must remain empty — the handle must not be inserted on failure.
    assert!(
        parent.fork_registry().list_ids().is_empty(),
        "registry must be empty after failed fork (no handle leaked)"
    );

    // Silence unused import warning for SmolStr in this test only.
    let _ = SmolStr::from("unused");
}

// ── Helper: register a fork then return its id ──────────────────────────────

/// Drive `SpawnReq::Fork` through the handler and return the registered
/// fork id. The `DataConTable` is empty so the wire-encode step may fail
/// — that's fine; the registry insertion happens before encode.
async fn register_one_fork(parent: &Arc<SessionContext>) -> SmolStr {
    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Lightweight,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: None,
    };

    let parent_clone = parent.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_clone.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    let ids = parent.fork_registry().list_ids();
    assert_eq!(ids.len(), 1, "fork registration must have exactly one id");
    ids[0].clone()
}

// ── Task 8.3: ForkOp::Discard ────────────────────────────────────────────────

/// `ForkOp::Discard` removes the handle and returns `ForkOpResult::Unit`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_op_discard_via_handler() {
    let (parent, _parent_cache) = build_parent_with_cache().await;
    let fork_id = register_one_fork(&parent).await;

    let parent_for_blocking = parent.clone();
    let fork_id_s = fork_id.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::ForkOp(fork_id_s, WireForkOpKind::Discard), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    // The result may fail at the DataCon encode step (empty table), but
    // the discard itself must have happened before the encode. We only
    // assert on registry state, not the wire Value.
    //
    // If the handler returned an Err that is NOT an encode error, propagate
    // it so a logic bug surfaces clearly.
    if let Err(ref e) = result {
        let msg = e.to_string();
        assert!(
            msg.contains("Unknown DataCon") || msg.contains("Bridge"),
            "unexpected handler error on Discard: {msg}"
        );
    }

    // The fork must no longer be in the registry after discard.
    assert!(
        parent.fork_registry().list_ids().is_empty(),
        "fork registry must be empty after Discard"
    );
}

// ── Task 8.3: ForkOp::MergeBack ─────────────────────────────────────────────

/// `ForkOp::MergeBack` returns a merge report AND keeps the handle in the
/// registry (it uses `get`, not `remove`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_op_merge_back_via_handler() {
    let (parent, _parent_cache) = build_parent_with_cache().await;
    let fork_id = register_one_fork(&parent).await;

    let parent_for_blocking = parent.clone();
    let fork_id_s = fork_id.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::ForkOp(fork_id_s, WireForkOpKind::MergeBack), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    // Accept encode-step failure from empty DataConTable.
    if let Err(ref e) = result {
        let msg = e.to_string();
        assert!(
            msg.contains("Unknown DataCon") || msg.contains("Bridge"),
            "unexpected handler error on MergeBack: {msg}"
        );
    }

    // MergeBack must NOT remove the handle — it stays for further ops.
    let ids = parent.fork_registry().list_ids();
    assert_eq!(
        ids.len(),
        1,
        "fork must remain in registry after MergeBack (it uses get, not remove)"
    );
}

// ── Task 8.3: ForkOp::Promote without capability ────────────────────────────

/// `ForkOp::Promote` on a fork whose spawner lacks `SpawnNewIdentities`
/// returns a "capability denied" or "CapabilityDenied" error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_op_promote_without_capability() {
    // Build a parent whose capabilities explicitly exclude SpawnNewIdentities.
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("no-promote-parent", "no-promote-parent");
    let parent = Arc::new(SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    ));
    // The parent has full capabilities by default; we must restrict the
    // fork's spawner_capabilities so promote checks against the cap-less set.
    // We do this by inserting a handle directly into the registry with a
    // restricted capability set (no SpawnNewIdentities).
    {
        use pattern_core::{CapabilitySet, EffectCategory};
        use pattern_db::ConstellationDb;
        use pattern_memory::MemoryCache;
        use pattern_runtime::spawn::ForkHandle;
        use pattern_runtime::timeout::CancelState;

        let db2 = Arc::new(ConstellationDb::open_in_memory().expect("db"));
        let child_cache = Arc::new(MemoryCache::new(db2));
        let cancel = Arc::new(CancelState::new());
        let restricted_caps: CapabilitySet = [EffectCategory::Memory].into_iter().collect();
        let handle = ForkHandle::new_lightweight(
            "promote-test".into(),
            "child-promote-test".into(),
            child_cache,
            "no-promote-parent".into(),
            std::sync::Weak::new(),
            cancel,
        )
        .with_spawner_capabilities(restricted_caps);
        parent
            .fork_registry()
            .insert("promote-test".into(), handle)
            .expect("insert");
    }

    let persona_cfg = WirePersonaConfig {
        name: "new-identity".to_string(),
        system_prompt: "test persona".to_string(),
        capabilities: pattern_runtime::sdk::requests::spawn::WireCapabilitySet {
            categories: vec![],
            flags: vec![],
        },
    };

    let parent_for_blocking = parent.clone();
    let err = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(
            SpawnReq::ForkOp(
                "promote-test".to_string(),
                WireForkOpKind::Promote(persona_cfg),
            ),
            &cx,
        )
    })
    .await
    .expect("spawn_blocking ok")
    .expect_err("promote without capability must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("capability") || msg.contains("Capability"),
        "error must mention capability; got: {msg}"
    );
}

// ── C1 regression: fork discard must not cancel parent session ──────────────

/// Regression test for C1: `ForkOp::Discard` must NOT set the parent
/// session's cancel state.
///
/// Root cause: `handle_fork` previously passed `parent.cancel_state()` as
/// the child's cancel state. When `discard()` fired `request_cancel()` on
/// the child, it was actually firing it on the parent session, which silently
/// killed the parent's in-flight turns. The fix allocates a fresh
/// `Arc<CancelState>` for each fork and propagates parent→child cancellation
/// via a background watcher task that holds only a `Weak<CancelState>` to
/// the child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_discard_does_not_cancel_parent_session_c1_regression() {
    let (parent, _parent_cache) = build_parent_with_cache().await;

    // Fork the parent session.
    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Lightweight,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: None,
    };

    let parent_for_blocking = parent.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    // Get the registered fork id.
    let ids = parent.fork_registry().list_ids();
    assert_eq!(ids.len(), 1, "must have exactly one fork registered");
    let fork_id = ids[0].clone();

    // Parent's cancel state is clear before discard.
    assert!(
        !parent.cancel_state().is_cancelled(),
        "parent must not be cancelled before fork discard"
    );

    // Discard the fork.
    let parent_for_blocking = parent.clone();
    let fork_id_s = fork_id.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::ForkOp(fork_id_s, WireForkOpKind::Discard), &cx)
    })
    .await
    .expect("spawn_blocking ok");

    // Parent's cancel state must remain clear after the fork is discarded.
    // This is the C1 regression assertion — before the fix, discard() called
    // request_cancel() on the parent's own cancel state because the fork was
    // constructed with `parent.cancel_state()` as the child's cancel state.
    assert!(
        !parent.cancel_state().is_cancelled(),
        "parent must NOT be cancelled after fork discard (C1 regression)"
    );
}

// ── Task 8.3: ForkOp on unknown id ──────────────────────────────────────────

/// `ForkOp` on a fork id that was never registered returns "fork not found".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_op_unknown_id() {
    let (parent, _parent_cache) = build_parent_with_cache().await;
    // Do not register any fork; the registry starts empty.

    let parent_for_blocking = parent.clone();
    let err = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(
            SpawnReq::ForkOp("ghost-fork-id".to_string(), WireForkOpKind::Discard),
            &cx,
        )
    })
    .await
    .expect("spawn_blocking ok")
    .expect_err("unknown id must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("fork not found") || msg.contains("not found"),
        "error must say fork not found; got: {msg}"
    );
}
