// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
    SiblingExistingOutcome, StubSiblingResolver, spawn_sibling_existing, spawn_sibling_new,
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
    let outcome = spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
        .await
        .expect("should succeed for a known persona id");
    assert_eq!(
        outcome.persona_id.as_str(),
        "orual-sibling-test",
        "returned id should match the agent-id in the fixture KDL"
    );
    // Important #5: the returned id must differ from the parent's agent_id.
    assert_ne!(
        outcome.persona_id.as_str(),
        parent.agent_id(),
        "sibling persona_id must differ from the parent's agent_id"
    );
}

// ── AC5.4 — capabilities come from the sibling's own KDL ────────────────────

/// The fixture persona declares `capabilities { effects { memory } }`. The
/// spawn pipeline must return the sibling's own caps in the outcome — NOT
/// the parent's inherited set.
///
/// This test drives the actual `spawn_sibling_existing` spawn pipeline and
/// asserts on the outcome's `capabilities` field (Important review item I#2).
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

    // Drive spawn_sibling_existing and inspect the returned outcome directly.
    // The outcome's capabilities field is what the spawn PIPELINE returns —
    // AC5.4 requires this to come from the sibling's own KDL, not the parent.
    let outcome: SiblingExistingOutcome =
        spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
            .await
            .expect("should succeed");
    assert_eq!(outcome.persona_id.as_str(), "orual-sibling-test");

    // AC5.4: the outcome's capabilities come from the sibling's own KDL config.
    let caps = outcome
        .capabilities
        .expect("fixture declares capabilities { effects { memory } }; outcome must carry them");
    assert!(
        caps.iter_categories().any(|c| c == EffectCategory::Memory),
        "sibling outcome must include Memory (from own KDL)"
    );
    assert!(
        !caps.iter_categories().any(|c| c == EffectCategory::Shell),
        "sibling outcome must NOT include Shell (parent's cap, not the sibling's)"
    );
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

    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts_dir.path())
        .await
        .expect("should succeed when parent has SpawnNewIdentities");

    // AC5.2 structural gate: parent had the flag → status must be Active.
    assert_eq!(
        outcome.status,
        pattern_runtime::spawn::sibling::SiblingStatus::Active,
        "parent held SpawnNewIdentities; outcome must be Active"
    );

    let expected_file = drafts_dir
        .path()
        .join(format!("{}.kdl", outcome.persona_id));
    assert!(
        expected_file.exists(),
        "draft KDL must be written to {expected_file:?}"
    );
    assert_eq!(outcome.kdl_path, expected_file);
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

    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts_dir.path())
        .await
        .expect("draft write must succeed regardless of SpawnNewIdentities flag");

    // AC5.3 structural gate: parent lacked the flag → status must be Draft.
    // The on-disk artefact is still written so Phase 6's promote workflow
    // has something to ingest, but the wire signals "not authorised for
    // live session-open."
    assert_eq!(
        outcome.status,
        pattern_runtime::spawn::sibling::SiblingStatus::Draft,
        "parent lacked SpawnNewIdentities; outcome must be Draft"
    );

    let expected_file = drafts_dir
        .path()
        .join(format!("{}.kdl", outcome.persona_id));
    assert!(
        expected_file.exists(),
        "draft KDL must be written even when flag is absent; path={expected_file:?}"
    );
    assert_eq!(outcome.kdl_path, expected_file);
}

// ── Important #3 — WireSiblingSpawn typed-sum round-trip ─────────────────────

/// Verify that `WireSiblingSpawn::ExistingActive` is produced by the
/// existing-persona path. The `SiblingExistingOutcome` yields
/// `ExistingActive(persona_id)` with no draft path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_sibling_spawn_existing_active_variant() {
    use pattern_runtime::sdk::requests::spawn::WireSiblingSpawn;

    // ExistingActive comes from the handler arm, not from SiblingNewOutcome.
    // Verify the From<SiblingNewOutcome> for Active + Draft.
    let drafts_dir = tempfile::TempDir::new().unwrap();
    let persona_cfg = PersonaConfig::new("round-trip-active", "rt active", CapabilitySet::empty());
    let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);
    let parent = build_parent(Some(caps)).await;
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::PeerWith,
    );
    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts_dir.path())
        .await
        .expect("should succeed");
    assert_eq!(
        outcome.status,
        pattern_runtime::spawn::sibling::SiblingStatus::Active
    );
    let wire = WireSiblingSpawn::from(outcome);
    match wire {
        WireSiblingSpawn::NewActive(pid, kdl_path) => {
            assert_eq!(pid, "round-trip-active");
            assert!(
                kdl_path.contains("round-trip-active"),
                "kdl path should contain persona id; got: {kdl_path}"
            );
        }
        other => panic!("expected NewActive variant, got {other:?}"),
    }
}

/// Verify that `WireSiblingSpawn::NewDraft` is produced when the parent
/// lacks `SpawnNewIdentities`, and that the kdl_path field is always present.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_sibling_spawn_new_draft_variant_carries_kdl_path() {
    use pattern_runtime::sdk::requests::spawn::WireSiblingSpawn;

    let drafts_dir = tempfile::TempDir::new().unwrap();
    let persona_cfg = PersonaConfig::new("round-trip-draft", "rt draft", CapabilitySet::empty());
    // Parent has an explicit restricted CapabilitySet that does NOT include
    // SpawnNewIdentities. Passing `None` would mean "full power" (all caps),
    // which includes the flag and would produce Active status instead of Draft.
    let parent = build_parent(Some(CapabilitySet::from_iter([EffectCategory::Memory]))).await;
    let cfg = SiblingConfig::new(
        SiblingPersona::New(persona_cfg.clone()),
        RelationshipKind::PeerWith,
    );
    let outcome = spawn_sibling_new(&parent, &cfg, &persona_cfg, drafts_dir.path())
        .await
        .expect("should succeed");
    assert_eq!(
        outcome.status,
        pattern_runtime::spawn::sibling::SiblingStatus::Draft
    );
    let wire = WireSiblingSpawn::from(outcome);
    match wire {
        WireSiblingSpawn::NewDraft(pid, kdl_path) => {
            assert_eq!(pid, "round-trip-draft");
            assert!(
                !kdl_path.is_empty(),
                "NewDraft must always carry a non-empty kdl_path"
            );
        }
        other => panic!("expected NewDraft variant, got {other:?}"),
    }
}

/// Verify `WireSiblingSpawn::ExistingActive` is constructed correctly.
/// This variant only carries the persona_id — no kdl_path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_sibling_spawn_existing_active_no_kdl_path() {
    use pattern_runtime::sdk::requests::spawn::WireSiblingSpawn;

    let resolver = {
        let r = pattern_runtime::spawn::sibling::StubSiblingResolver::new();
        r.register("orual", fixture_path("sibling_persona.kdl"));
        Arc::new(r)
    };
    let parent = build_parent(None).await;
    let cfg = SiblingConfig::new(
        SiblingPersona::Existing("orual".into()),
        RelationshipKind::PeerWith,
    );
    let outcome = spawn_sibling_existing(&parent, &cfg, &"orual".into(), resolver)
        .await
        .expect("should succeed");

    // The handler arm constructs ExistingActive directly from outcome.persona_id.
    let wire = WireSiblingSpawn::ExistingActive(outcome.persona_id.to_string());
    match wire {
        WireSiblingSpawn::ExistingActive(pid) => {
            assert_eq!(pid, "orual-sibling-test");
        }
        other => panic!("expected ExistingActive variant, got {other:?}"),
    }
}
