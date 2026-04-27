//! Capability-gate, missing-registry, and dispatch behaviour of
//! `Pattern.Constellation` (Phase 6 Task 5).
//!
//! Modeled after `tests/fronting_handler_capability.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_effect::{EffectContext, EffectHandler};
use tidepool_repr::{DataCon, DataConId};

use pattern_core::CapabilitySet;
use pattern_core::ConstellationRegistry;
use pattern_core::EffectCategory;
use pattern_core::constellation::{PersonaRecord, PersonaStatus, RelationshipSpec};
use pattern_core::spawn::RelationshipKind;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_runtime::NopProviderClient;
use pattern_runtime::policy::CAPABILITY_DENIED_PREFIX;
use pattern_runtime::sdk::handlers::ConstellationHandler;
use pattern_runtime::sdk::handlers::constellation::CONSTELLATION_NOT_WIRED_PREFIX;
use pattern_runtime::sdk::requests::ConstellationReq;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{
    InMemoryConstellationRegistry, InMemoryMemoryStore, populated_constellation_test_table,
};

// ── Fixture helpers ───────────────────────────────────────────────────────────

fn datacon_table() -> tidepool_repr::DataConTable {
    let mut table = populated_constellation_test_table();
    // `cx.respond(())` for unit returns needs the `()` constructor; standard
    // table doesn't include it.
    table.insert(DataCon {
        id: DataConId(100),
        name: "()".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: Some("GHC.Tuple.()".to_string()),
    });
    table
}

async fn build_session(
    caps: Option<CapabilitySet>,
    registry: Option<Arc<dyn ConstellationRegistry>>,
) -> Arc<SessionContext> {
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("agent-constellation-test", "agent-constellation-test");
    persona.capabilities = caps;
    let mut ctx = SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    );
    if let Some(reg) = registry {
        ctx = ctx.with_constellation_registry(reg);
    }
    Arc::new(ctx)
}

async fn seeded_registry() -> Arc<InMemoryConstellationRegistry> {
    let reg = Arc::new(InMemoryConstellationRegistry::new());
    // alice (Active, /p1) supervisor_of bob (Draft, /p1); carol (Active, /p2).
    let mut alice = PersonaRecord::new("alice", "Alice", PersonaStatus::Active);
    alice.project_attachments.push(PathBuf::from("/p1"));
    reg.register(alice).await.unwrap();

    let mut bob = PersonaRecord::new("bob", "Bob", PersonaStatus::Draft);
    bob.project_attachments.push(PathBuf::from("/p1"));
    reg.register(bob).await.unwrap();

    let mut carol = PersonaRecord::new("carol", "Carol", PersonaStatus::Active);
    carol.project_attachments.push(PathBuf::from("/p2"));
    reg.register(carol).await.unwrap();

    reg.add_relationship(RelationshipSpec::new(
        "alice",
        "bob",
        RelationshipKind::SupervisorOf,
    ))
    .await
    .unwrap();
    reg
}

// ── Capability gate ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_without_constellation_capability_is_denied() {
    let reg = seeded_registry().await as Arc<dyn ConstellationRegistry>;
    // Caps without Constellation.
    let caps = CapabilitySet::from_iter([EffectCategory::Memory]);
    let ctx = build_session(Some(caps), Some(reg)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let err = h
        .handle(ConstellationReq::List(None), &cx)
        .expect_err("List without Constellation cap must be denied");
    assert!(
        err.to_string().contains(CAPABILITY_DENIED_PREFIX),
        "got: {err}"
    );
    assert!(
        err.to_string().contains("Constellation"),
        "denial should name the missing category; got: {err}"
    );
}

// ── Missing registry ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_registry_returns_not_wired_marker() {
    let ctx = build_session(Some(CapabilitySet::all()), None).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let err = h
        .handle(ConstellationReq::List(None), &cx)
        .expect_err("List without registry must error");
    assert!(
        err.to_string().contains(CONSTELLATION_NOT_WIRED_PREFIX),
        "got: {err}"
    );
}

// ── List dispatch ─────────────────────────────────────────────────────────────

// Production handlers run from the eval-worker thread (no ambient runtime),
// so they `tokio_handle.block_on(...)` registry futures directly. The
// integration test runs inside a tokio runtime, so we use `block_in_place`
// to drop the calling task off the worker before invoking the handler.
fn dispatch<F: FnOnce() -> R, R>(f: F) -> R {
    tokio::task::block_in_place(f)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dispatches_through_registry_with_three_personas() {
    let reg = seeded_registry().await as Arc<dyn ConstellationRegistry>;
    let ctx = build_session(Some(CapabilitySet::all()), Some(reg)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let result = dispatch(|| h.handle(ConstellationReq::List(None), &cx));
    assert!(result.is_ok(), "List(None) returned: {result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_with_project_filter_dispatches() {
    let reg = seeded_registry().await as Arc<dyn ConstellationRegistry>;
    let ctx = build_session(Some(CapabilitySet::all()), Some(reg)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let result = dispatch(|| h.handle(ConstellationReq::List(Some("/p1".to_string())), &cx));
    assert!(result.is_ok(), "List(/p1) returned: {result:?}");
}

// ── Find dispatch ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_with_unknown_kind_returns_clear_error() {
    let reg = seeded_registry().await as Arc<dyn ConstellationRegistry>;
    let ctx = build_session(Some(CapabilitySet::all()), Some(reg)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    // The kind-parse failure is sync — no block_on path. Direct call works.
    let err = h
        .handle(
            ConstellationReq::Find(None, Some("buddy_with".to_string())),
            &cx,
        )
        .expect_err("unknown kind must error");
    assert!(
        err.to_string().contains("unknown relationship kind"),
        "got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_with_supervisor_of_kind_dispatches() {
    let reg = seeded_registry().await as Arc<dyn ConstellationRegistry>;
    let ctx = build_session(Some(CapabilitySet::all()), Some(reg)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let result = dispatch(|| {
        h.handle(
            ConstellationReq::Find(None, Some("supervisor_of".to_string())),
            &cx,
        )
    });
    assert!(
        result.is_ok(),
        "Find(None, supervisor_of) returned: {result:?}"
    );
}

// ── Groups dispatch ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_with_no_filter_returns_ok_for_empty_registry() {
    let reg = seeded_registry().await as Arc<dyn ConstellationRegistry>;
    let ctx = build_session(Some(CapabilitySet::all()), Some(reg)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let result = dispatch(|| h.handle(ConstellationReq::Groups(None), &cx));
    assert!(result.is_ok(), "Groups(None) returned: {result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_with_project_filter_dispatches() {
    let reg = Arc::new(InMemoryConstellationRegistry::new());
    reg.create_group("alpha".to_string(), Some("proj-a".to_string()))
        .await
        .unwrap();
    let reg_dyn = reg as Arc<dyn ConstellationRegistry>;

    let ctx = build_session(Some(CapabilitySet::all()), Some(reg_dyn)).await;
    let table = datacon_table();
    let cx = EffectContext::with_user(&table, &*ctx);

    let mut h = ConstellationHandler;
    let result = dispatch(|| {
        h.handle(
            ConstellationReq::Groups(Some("proj-a".to_string())),
            &cx,
        )
    });
    assert!(result.is_ok(), "Groups(proj-a) returned: {result:?}");
}
