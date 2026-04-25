//! Phase 2 Task 4 — ephemeral spawn integration tests.
//!
//! Covers AC3.2 (capability escalation rejection) and AC3.5 (concurrency
//! limit enforcement) — the deterministic, non-LLM-driven slice. AC3.1
//! (success path), AC3.3 (costume), and AC3.4 (timeout) are
//! mock-provider tests that land in a follow-up alongside the
//! progress-log block wiring.

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory, spawn::EphemeralConfig};
use pattern_runtime::NopProviderClient;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::InMemoryMemoryStore;

async fn build_parent(
    parent_caps: Option<CapabilitySet>,
    spawn_limit: Option<usize>,
) -> Arc<SessionContext> {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("ephemeral-parent", "ephemeral-parent");
    if let Some(caps) = parent_caps {
        persona.capabilities = Some(caps);
    }
    let mut ctx = SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    );
    if let Some(limit) = spawn_limit {
        ctx.replace_spawn_registry_for_test(limit);
    }
    Arc::new(ctx)
}

/// AC3.2 — capability escalation surfaces as `SpawnError::CapabilityEscalation`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_escalation_is_rejected_at_fork_time() {
    // Parent has only Memory + Spawn; ephemeral asks for an additional
    // category (Shell). compute_child_caps must reject.
    let parent_caps: CapabilitySet = [EffectCategory::Memory, EffectCategory::Spawn]
        .into_iter()
        .collect();
    let parent = build_parent(Some(parent_caps), None).await;

    let ephemeral_caps: CapabilitySet = [
        EffectCategory::Memory,
        EffectCategory::Spawn,
        EffectCategory::Shell,
    ]
    .into_iter()
    .collect();
    let cfg = EphemeralConfig::new("").with_capabilities(ephemeral_caps);

    let err = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("capability escalation"),
        "expected capability-escalation error, got: {msg}"
    );
}

/// AC3.2 (flag path) — escalation on a flag-only addition is also caught.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_escalation_via_flag_is_rejected() {
    let parent_caps: CapabilitySet =
        std::iter::once(EffectCategory::Spawn).collect::<CapabilitySet>();
    let parent = build_parent(Some(parent_caps), None).await;

    let ephemeral_caps = std::iter::once(EffectCategory::Spawn)
        .collect::<CapabilitySet>()
        .with_flags([CapabilityFlag::SpawnNewIdentities]);
    let cfg = EphemeralConfig::new("").with_capabilities(ephemeral_caps);

    let err = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap_err();
    assert!(err.to_string().contains("capability escalation"));
}

/// AC3.2 (subset path) — a strict subset is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_subset_is_accepted() {
    let parent_caps: CapabilitySet = [
        EffectCategory::Memory,
        EffectCategory::Spawn,
        EffectCategory::Shell,
    ]
    .into_iter()
    .collect();
    let parent = build_parent(Some(parent_caps), None).await;

    let ephemeral_caps: CapabilitySet = std::iter::once(EffectCategory::Memory).collect();
    let cfg = EphemeralConfig::new("").with_capabilities(ephemeral_caps);

    let child_caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
    assert!(child_caps.contains(EffectCategory::Memory));
    assert!(!child_caps.contains(EffectCategory::Shell));
}

/// AC3.5 — registry with limit=2 saturates at the third acquire.
///
/// Verifies the dispatch-time gate: `try_acquire_ephemeral_slot` returns
/// `None` when the ceiling is reached. The handler arm converts that
/// into `SpawnError::ConcurrencyLimitExceeded`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ephemeral_concurrency_limit_saturates() {
    let parent = build_parent(None, Some(2)).await;
    let registry = parent.spawn_registry();

    let permit_a = registry.try_acquire_ephemeral_slot();
    let permit_b = registry.try_acquire_ephemeral_slot();
    let permit_c = registry.try_acquire_ephemeral_slot();

    assert!(permit_a.is_some(), "first slot must be available");
    assert!(permit_b.is_some(), "second slot must be available");
    assert!(permit_c.is_none(), "third slot must be denied with limit=2");

    drop(permit_a);
    drop(permit_b);

    // Slot freed; subsequent acquire succeeds.
    assert!(registry.try_acquire_ephemeral_slot().is_some());
}
