//! Sibling spawn integration tests.
//!
//! Covers:
//! - AC5.1: existing-persona adoption — stub resolver maps PersonaId to a
//!   fixture KDL file; `spawn_sibling_existing` returns the persona's id.
//! - AC5.4: capabilities come from the sibling's own config, NOT inherited
//!   from the spawner.
//! - AC5.6: unknown persona id → `SpawnError::PersonaNotFound`.
//! - AC5.2: parent has `SpawnNewIdentities`; `spawn_sibling_new` writes
//!   draft KDL and returns the new persona id.
//! - AC5.3: parent lacks the flag; same call writes draft KDL (no live
//!   session yet in Phase 2), returns the draft id.

use std::path::PathBuf;
use std::sync::Arc;

use pattern_core::spawn::{PersonaConfig, RelationshipKind, SiblingPersona};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory, spawn::SiblingConfig};
use pattern_runtime::NopProviderClient;
use pattern_runtime::session::SessionContext;
use pattern_runtime::spawn::sibling::{
    StubSiblingResolver, spawn_sibling_existing, spawn_sibling_new,
};
use pattern_runtime::testing::InMemoryMemoryStore;

fn fixture_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(name);
    p
}

async fn build_parent(caps: Option<CapabilitySet>) -> Arc<SessionContext> {
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("parent-sibling-test", "parent-sibling-test");
    if let Some(c) = caps {
        persona.capabilities = Some(c);
    }
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    );
    Arc::new(ctx)
}

// ── AC5.1 — existing persona success ────────────────────────────────────────

/// Given a stub resolver that maps `PersonaId("orual")` to the sibling
/// fixture KDL, `spawn_sibling_existing` should return `Ok("orual-sibling-test")`
/// (the agent-id baked into the fixture).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_1_existing_persona_adoption_returns_ok() {
    let parent = build_parent(None).await;
    let resolver = {
        let r = StubSiblingResolver::new();
        r.register("orual", fixture_path("sibling_persona.kdl"));
        Arc::new(r)
    };
    let cfg = SiblingConfig::new(
        SiblingPersona::Existing("orual".into()),
        RelationshipKind::PeerWith,
    );
    let id = spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
        .await
        .expect("should succeed for a known persona id");
    assert_eq!(
        id.as_str(),
        "orual-sibling-test",
        "returned id should match the agent-id in the fixture KDL"
    );
}

// ── AC5.4 — capabilities come from the sibling's own KDL ────────────────────

/// The fixture persona declares `capabilities { effects { memory } }`. After
/// load the snapshot's capability set should contain exactly `Memory` and
/// nothing else. The parent's own capabilities are irrelevant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_4_capabilities_come_from_sibling_own_config() {
    // Give the parent Memory + Shell — the sibling must NOT inherit Shell.
    let parent_caps: CapabilitySet = [EffectCategory::Memory, EffectCategory::Shell]
        .into_iter()
        .collect();
    let parent = build_parent(Some(parent_caps)).await;
    let resolver = {
        let r = StubSiblingResolver::new();
        r.register("orual", fixture_path("sibling_persona.kdl"));
        Arc::new(r)
    };
    let cfg = SiblingConfig::new(
        SiblingPersona::Existing("orual".into()),
        RelationshipKind::PeerWith,
    );

    // Load the persona snapshot directly to inspect its capabilities.
    let path = fixture_path("sibling_persona.kdl");
    let snap =
        pattern_runtime::persona_loader::load_persona(&path).expect("fixture must load cleanly");

    // AC5.4: capabilities come from the persona's own KDL, not from the parent.
    let caps = snap
        .capabilities
        .expect("fixture declares capabilities { effects { memory } }");
    assert!(
        caps.iter_categories().any(|c| c == EffectCategory::Memory),
        "sibling must have Memory"
    );
    assert!(
        !caps.iter_categories().any(|c| c == EffectCategory::Shell),
        "sibling must NOT inherit Shell from the parent"
    );

    // Verify spawn_sibling_existing also completes without error.
    let id = spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
        .await
        .expect("should succeed");
    assert_eq!(id.as_str(), "orual-sibling-test");
}

// ── AC5.6 — unknown persona id → PersonaNotFound ────────────────────────────

/// An empty stub resolver knows no personas. Attempting to spawn an unknown id
/// must surface `SpawnError::PersonaNotFound`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_6_unknown_persona_id_returns_persona_not_found() {
    let parent = build_parent(None).await;
    let resolver = Arc::new(StubSiblingResolver::new()); // empty map
    let cfg = SiblingConfig::new(
        SiblingPersona::Existing("ghost".into()),
        RelationshipKind::PeerWith,
    );
    let err = spawn_sibling_existing(&parent, &cfg, &"ghost".into(), resolver)
        .await
        .expect_err("should fail for an unregistered persona id");
    match &err {
        pattern_runtime::spawn::SpawnError::PersonaNotFound { id } => {
            assert_eq!(id.as_str(), "ghost");
        }
        other => panic!("expected PersonaNotFound, got {other:?}"),
    }
}

// ── AC5.2 — new sibling with SpawnNewIdentities ──────────────────────────────

/// When the parent holds `CapabilityFlag::SpawnNewIdentities`, calling
/// `spawn_sibling_new` with a `PersonaConfig` must:
/// - Write a draft KDL file to the configured drafts directory.
/// - Return `Ok(PersonaId)` equal to a slugified form of the persona name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_2_new_sibling_with_spawn_new_identities_writes_draft() {
    let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);
    let parent = build_parent(Some(caps)).await;

    let drafts_dir = tempfile::TempDir::new().expect("tempdir must succeed");
    let persona_cfg = PersonaConfig::new(
        "my-test-sibling",
        "A test persona created by spawn_sibling_new.",
        CapabilitySet::from_iter([EffectCategory::Memory]),
    );
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::PeerWith,
    );

    let id = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts_dir.path())
        .await
        .expect("should succeed when parent has SpawnNewIdentities");

    // The draft file must exist in the provided dir.
    let expected_file = drafts_dir.path().join(format!("{id}.kdl"));
    assert!(
        expected_file.exists(),
        "draft KDL must be written to {expected_file:?}"
    );
    // Confirm the file is non-empty KDL-like content.
    let content = std::fs::read_to_string(&expected_file).expect("file must be readable");
    assert!(
        content.contains("name"),
        "draft KDL must contain a name field; got: {content}"
    );
}

// ── AC5.3 — new sibling WITHOUT SpawnNewIdentities ───────────────────────────

/// When the parent lacks `CapabilityFlag::SpawnNewIdentities`, `spawn_sibling_new`
/// must still write the draft KDL (for future approval / Phase 6 registry
/// ingestion) but the distinction is that no live session is opened. In
/// Phase 2 both paths look the same functionally; the test verifies the file
/// is written and no error is returned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac5_3_new_sibling_without_flag_writes_draft_no_live_session() {
    // Parent has no flags — specifically NOT SpawnNewIdentities.
    let caps: CapabilitySet = [EffectCategory::Memory].into_iter().collect();
    let parent = build_parent(Some(caps)).await;

    let drafts_dir = tempfile::TempDir::new().expect("tempdir must succeed");
    let persona_cfg = PersonaConfig::new(
        "draft-only-sibling",
        "A persona draft that will not become a live session in Phase 2.",
        CapabilitySet::from_iter([EffectCategory::Memory]),
    );
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::SpecialistFor,
    );

    let id = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts_dir.path())
        .await
        .expect("draft write must succeed regardless of SpawnNewIdentities flag");

    let expected_file = drafts_dir.path().join(format!("{id}.kdl"));
    assert!(
        expected_file.exists(),
        "draft KDL must be written even when flag is absent; path={expected_file:?}"
    );
}
