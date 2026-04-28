//! C-1: production sibling resolver wired against `ConstellationRegistry`.
//!
//! Verifies that `ConstellationSiblingResolver` (the production resolver
//! used by the daemon) actually resolves persona ids to their KDL paths
//! by querying the registry, and that the end-to-end `spawn_sibling_existing`
//! path succeeds when the persona is registered with a populated
//! `config_path`.
//!
//! Before this fix, the daemon wired no resolver at all, so
//! `ctx.spawn.sibling(SiblingPersona::Existing(id))` always failed with
//! `RegistryError::PersonaNotFound` regardless of registry state.
//! Once the wiring landed, a second issue surfaced: the resolver trait
//! was sync but the registry is async — bridging via `Handle::block_on`
//! panicked when called from inside an async future driven by the
//! tokio runtime. The fix made the trait async, eliminating the bridge.

use std::path::PathBuf;
use std::sync::Arc;

use pattern_core::ConstellationRegistry;
use pattern_core::constellation::{EdgeDirection, PersonaRecord, PersonaStatus};
use pattern_core::spawn::{RelationshipKind, SiblingConfig, SiblingPersona};
use pattern_core::types::ids::PersonaId;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilitySet, EffectCategory};
use pattern_runtime::NopProviderClient;
use pattern_runtime::session::SessionContext;
use pattern_runtime::spawn::sibling::{
    ConstellationSiblingResolver, RegistryError, SiblingPersonaResolver, spawn_sibling_existing,
};
use pattern_runtime::testing::{InMemoryConstellationRegistry, InMemoryMemoryStore};

fn fixture_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(name);
    p
}

// ── Resolver-level tests ─────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn constellation_resolver_returns_config_path_for_registered_persona() {
    let registry: Arc<dyn ConstellationRegistry> = Arc::new(InMemoryConstellationRegistry::new());
    let kdl = fixture_path("sibling_persona.kdl");

    let mut record = PersonaRecord::new("orual", "orual", PersonaStatus::Active);
    record.config_path = Some(kdl.clone());
    registry.register(record).await.unwrap();

    let resolver = ConstellationSiblingResolver::new(Arc::clone(&registry));

    let resolved = resolver
        .resolve_path(&"orual".into())
        .await
        .expect("registered persona with config_path must resolve");

    assert_eq!(
        resolved, kdl,
        "resolver must return the registry's config_path verbatim"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn constellation_resolver_reports_unknown_id_as_persona_not_found() {
    let registry: Arc<dyn ConstellationRegistry> = Arc::new(InMemoryConstellationRegistry::new());
    let resolver = ConstellationSiblingResolver::new(Arc::clone(&registry));

    let err = resolver
        .resolve_path(&"missing".into())
        .await
        .expect_err("unknown persona must be PersonaNotFound");

    let id = match err {
        RegistryError::PersonaNotFound(id) => id,
        other => panic!("expected PersonaNotFound, got: {other:?}"),
    };
    assert_eq!(id, PersonaId::from("missing"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn constellation_resolver_treats_record_without_config_path_as_unresolvable() {
    let registry: Arc<dyn ConstellationRegistry> = Arc::new(InMemoryConstellationRegistry::new());
    // Register a Draft persona that has no on-disk KDL file yet.
    registry
        .register(PersonaRecord::new(
            "draft",
            "draft",
            PersonaStatus::Draft,
        ))
        .await
        .unwrap();

    let resolver = ConstellationSiblingResolver::new(Arc::clone(&registry));

    let err = resolver
        .resolve_path(&"draft".into())
        .await
        .expect_err("record without config_path must be PersonaNotFound");

    let id = match err {
        RegistryError::PersonaNotFound(id) => id,
        other => panic!("expected PersonaNotFound, got: {other:?}"),
    };
    assert_eq!(id, PersonaId::from("draft"));
}

// ── End-to-end test: spawn_sibling_existing through the production resolver ─

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_sibling_existing_works_with_constellation_resolver() {
    // Build a registry pre-populated with both the parent and the sibling
    // (matching the daemon's startup state: personas are auto-registered
    // when their sessions open).
    let registry = Arc::new(InMemoryConstellationRegistry::new());
    registry
        .register(PersonaRecord::new(
            "parent-resolver-test",
            "parent-resolver-test",
            PersonaStatus::Active,
        ))
        .await
        .unwrap();

    let mut sibling_record = PersonaRecord::new("orual", "orual", PersonaStatus::Active);
    sibling_record.config_path = Some(fixture_path("sibling_persona.kdl"));
    registry.register(sibling_record).await.unwrap();

    let registry_dyn: Arc<dyn ConstellationRegistry> = registry.clone();

    // Build a parent SessionContext with the registry wired and full caps so
    // SpawnNewIdentities + Spawn category checks pass.
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("parent-resolver-test", "parent-resolver-test");
    persona.capabilities = Some(CapabilitySet::all());
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    );
    let parent = Arc::new(ctx.with_constellation_registry(Arc::clone(&registry_dyn)));

    // The production resolver — same construction the daemon uses.
    let resolver: Arc<dyn SiblingPersonaResolver> =
        Arc::new(ConstellationSiblingResolver::new(registry_dyn));

    let cfg = SiblingConfig::new(
        SiblingPersona::Existing("orual".into()),
        RelationshipKind::SupervisorOf,
    );

    let outcome = spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
        .await
        .expect("spawn_sibling_existing through ConstellationSiblingResolver must succeed");

    // Sibling persona id resolved. spawn_sibling_existing returns the
    // persona id loaded from the resolved KDL file (which the fixture
    // declares as "orual-sibling-test"), not the lookup id we passed in.
    assert_eq!(outcome.persona_id, PersonaId::from("orual-sibling-test"));

    // Sibling capability set non-empty (loaded from the sibling's own KDL,
    // not inherited from parent — contract of spawn_sibling_existing).
    let sib_caps = outcome
        .capabilities
        .expect("sibling KDL declares a capabilities block, so caps must be Some");
    assert!(
        sib_caps.contains(EffectCategory::Memory),
        "sibling caps from KDL must include Memory"
    );

    // Parent now has an Outgoing SupervisorOf edge to the sibling — proves
    // the registry round-trip + relationship registration both fired.
    let parent_record = registry
        .get(&"parent-resolver-test".into())
        .await
        .unwrap()
        .unwrap();
    assert!(
        parent_record.relationships.iter().any(|e| e.other
            == "orual-sibling-test"
            && e.direction == EdgeDirection::Outgoing
            && matches!(e.kind, RelationshipKind::SupervisorOf)),
        "parent must have an Outgoing SupervisorOf edge to the sibling \
         after spawn_sibling_existing; got: {:?}",
        parent_record.relationships
    );
}
