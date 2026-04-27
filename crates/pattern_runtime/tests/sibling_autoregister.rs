//! Phase 6 T6 — sibling auto-registration in the constellation registry.
//!
//! Verifies AC5.5 / AC5.7:
//! - `spawn_sibling_existing` adds a parent → sibling relationship edge.
//! - `spawn_sibling_new` registers the new persona with `Active` status when
//!   the parent holds `SpawnNewIdentities`, or `Draft` status otherwise; in
//!   both cases the parent → sibling relationship edge is added.
//! - Without a registry wired on the parent, spawn proceeds without a
//!   registry call (back-compat for test/non-daemon paths).

use std::path::PathBuf;
use std::sync::Arc;

use pattern_core::ConstellationRegistry;
use pattern_core::constellation::{EdgeDirection, PersonaRecord, PersonaStatus, RegistryScope};
use pattern_core::spawn::{PersonaConfig, RelationshipKind, SiblingPersona};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory, spawn::SiblingConfig};
use pattern_runtime::NopProviderClient;
use pattern_runtime::session::SessionContext;
use pattern_runtime::spawn::sibling::{
    StubSiblingResolver, spawn_sibling_existing, spawn_sibling_new,
};
use pattern_runtime::testing::{InMemoryConstellationRegistry, InMemoryMemoryStore};

fn fixture_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(name);
    p
}

async fn build_parent_with_registry(
    registry: Arc<dyn ConstellationRegistry>,
    caps: Option<CapabilitySet>,
) -> Arc<SessionContext> {
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("parent-autoreg", "parent-autoreg");
    persona.capabilities = caps;
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    );
    Arc::new(ctx.with_constellation_registry(registry))
}

// ── AC5.5 — existing persona adoption registers the relationship edge ───────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_5_existing_sibling_adds_relationship_edge() {
    let registry = Arc::new(InMemoryConstellationRegistry::new());
    // Pre-register the parent so add_relationship's both-endpoints check passes.
    registry
        .register(PersonaRecord::new(
            "parent-autoreg",
            "parent-autoreg",
            PersonaStatus::Active,
        ))
        .await
        .unwrap();
    let registry_dyn: Arc<dyn ConstellationRegistry> = registry.clone();
    let parent = build_parent_with_registry(registry_dyn, Some(CapabilitySet::all())).await;

    let resolver = {
        let r = StubSiblingResolver::new();
        r.register("orual", fixture_path("sibling_persona.kdl"));
        Arc::new(r)
    };
    let cfg = SiblingConfig::new(
        SiblingPersona::Existing("orual".into()),
        RelationshipKind::SupervisorOf,
    );
    let outcome = spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
        .await
        .expect("spawn_sibling_existing must succeed");

    // The sibling should now be registered, and there should be an outgoing
    // SupervisorOf edge from parent → sibling.
    let sibling_id = outcome.persona_id.clone();
    let sibling = registry
        .get(&sibling_id)
        .await
        .unwrap()
        .expect("sibling must be registered");
    assert_eq!(sibling.status, PersonaStatus::Active);

    let parent_record = registry
        .get(&"parent-autoreg".into())
        .await
        .unwrap()
        .unwrap();
    let outgoing = parent_record
        .relationships
        .iter()
        .find(|e| e.other == sibling_id && e.direction == EdgeDirection::Outgoing)
        .expect("parent must have an outgoing edge to the sibling");
    assert_eq!(outgoing.kind, RelationshipKind::SupervisorOf);
}

// ── AC5.5 — new-identity Active path registers + relationships ──────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_5_new_identity_active_registers_with_relationship() {
    let registry = Arc::new(InMemoryConstellationRegistry::new());
    registry
        .register(PersonaRecord::new(
            "parent-autoreg",
            "parent-autoreg",
            PersonaStatus::Active,
        ))
        .await
        .unwrap();
    let registry_dyn: Arc<dyn ConstellationRegistry> = registry.clone();
    let caps = CapabilitySet::from_iter([EffectCategory::Memory])
        .with_flags([CapabilityFlag::SpawnNewIdentities]);
    let parent = build_parent_with_registry(registry_dyn, Some(caps)).await;

    let drafts = tempfile::TempDir::new().unwrap();
    let persona_cfg = PersonaConfig::new(
        "new-active-sibling",
        "system prompt",
        CapabilitySet::from_iter([EffectCategory::Memory]),
    );
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::PeerWith,
    );

    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts.path())
        .await
        .expect("spawn_sibling_new must succeed");
    let sibling_id = outcome.persona_id.clone();

    let sibling = registry
        .get(&sibling_id)
        .await
        .unwrap()
        .expect("new active sibling must be registered");
    assert_eq!(sibling.status, PersonaStatus::Active);
    assert!(
        sibling.config_path.is_some(),
        "registry record must carry the draft KDL path"
    );

    let parent_record = registry
        .get(&"parent-autoreg".into())
        .await
        .unwrap()
        .unwrap();
    assert!(
        parent_record
            .relationships
            .iter()
            .any(|e| e.other == sibling_id
                && e.kind == RelationshipKind::PeerWith
                && e.direction == EdgeDirection::Outgoing),
        "parent must have outgoing PeerWith edge to new active sibling"
    );
}

// ── AC5.7 — new-identity Draft path registers as Draft ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_7_new_identity_without_flag_registers_as_draft() {
    let registry = Arc::new(InMemoryConstellationRegistry::new());
    registry
        .register(PersonaRecord::new(
            "parent-autoreg",
            "parent-autoreg",
            PersonaStatus::Active,
        ))
        .await
        .unwrap();
    let registry_dyn: Arc<dyn ConstellationRegistry> = registry.clone();
    // Caps WITHOUT `SpawnNewIdentities`.
    let caps = CapabilitySet::from_iter([EffectCategory::Memory]);
    let parent = build_parent_with_registry(registry_dyn, Some(caps)).await;

    let drafts = tempfile::TempDir::new().unwrap();
    let persona_cfg =
        PersonaConfig::new("new-draft-sibling", "system prompt", CapabilitySet::empty());
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::SpecialistFor,
    );

    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts.path())
        .await
        .expect("spawn_sibling_new must succeed in draft path");

    let sibling = registry
        .get(&outcome.persona_id)
        .await
        .unwrap()
        .expect("draft sibling must be registered");
    assert_eq!(
        sibling.status,
        PersonaStatus::Draft,
        "without SpawnNewIdentities flag, registration must be Draft"
    );

    // ctx.constellation.list() (via registry) sees the draft.
    let all = registry.list(RegistryScope::All).await.unwrap();
    assert!(
        all.iter().any(|r| r.id == outcome.persona_id),
        "draft must show up in registry list"
    );
}

// ── No registry wired → spawn still proceeds (back-compat) ──────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_registry_wired_spawn_still_succeeds() {
    // Build a parent WITHOUT a constellation registry.
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("parent-noreg", "parent-noreg");
    persona.capabilities = Some(CapabilitySet::all());
    let parent = Arc::new(SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    ));

    let drafts = tempfile::TempDir::new().unwrap();
    let persona_cfg = PersonaConfig::new("no-reg-sibling", "system prompt", CapabilitySet::empty());
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::PeerWith,
    );

    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts.path())
        .await
        .expect("spawn_sibling_new must succeed without registry wired");
    assert_eq!(outcome.persona_id.as_str(), "no-reg-sibling");
}
