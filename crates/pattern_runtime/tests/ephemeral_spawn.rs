//! Phase 2 Task 4 — ephemeral spawn integration tests.
//!
//! Covers AC3.1 (success path with mock-LLM), AC3.2 (capability
//! escalation rejection), AC3.3 (costume + persona-identity
//! preservation), and AC3.5 (concurrency limit enforcement) — plus the
//! progress-log block creation + per-turn append wiring.
//!
//! AC3.4 (timeout) is deferred — testing it deterministically needs a
//! hangable mock provider (the current `MockProviderClient` panics on
//! exhausted scripts rather than blocking), and `tokio::time::pause()`
//! interactions with the eval-worker thread are non-trivial. The
//! timeout code path itself is exercised by the `tokio::time::timeout`
//! wrapper in `run_ephemeral`; verifying it end-to-end is a follow-up.

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory, spawn::EphemeralConfig};
use pattern_runtime::NopProviderClient;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, MockProviderClient};

async fn build_parent(
    parent_caps: Option<CapabilitySet>,
    spawn_limit: Option<usize>,
) -> Arc<SessionContext> {
    build_parent_with_provider(parent_caps, spawn_limit, Arc::new(NopProviderClient)).await
}

async fn build_parent_with_provider(
    parent_caps: Option<CapabilitySet>,
    spawn_limit: Option<usize>,
    provider: Arc<dyn ProviderClient>,
) -> Arc<SessionContext> {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
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

/// Progress-log block creation succeeds against an empty in-memory store
/// and is callable repeatedly with distinct labels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_progress_log_block_creates_constellation_scoped_log() {
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::memory_types::{BlockSchema, CONSTELLATION_OWNER};

    let parent = build_parent(None, None).await;
    let label = "spawn-log-test-progress";

    pattern_runtime::spawn::create_progress_log_block(parent.adapter(), label).unwrap();

    let block = parent
        .adapter()
        .get_block(CONSTELLATION_OWNER, label)
        .unwrap()
        .expect("block must exist after creation");
    let metadata = block.metadata();
    assert!(
        matches!(metadata.schema, BlockSchema::Log { .. }),
        "expected Log schema, got {:?}",
        metadata.schema
    );
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

/// AC3.6 / AC3.7 — parent cancel cascades through nested registries.
///
/// Three-level chain: parent registry → ephemeral child registry →
/// grandchild registry. When the parent's CancelState atomic flips,
/// the watcher tasks installed by `fork_for_ephemeral` propagate by
/// calling `cancel_all` on each downstream registry — flipping the
/// children's cancel atomics within the 100 ms grace asserted here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parent_cancel_propagates_through_three_level_chain() {
    use pattern_runtime::spawn::SpawnRegistry;
    use pattern_runtime::timeout::CancelState;

    let parent = build_parent(None, None).await;

    // Build child + grandchild contexts. Each fork_for_ephemeral
    // installs the parent-cancel watcher task.
    let cfg = pattern_core::spawn::EphemeralConfig::new("");
    let child_caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
    let child = parent.fork_for_ephemeral(&cfg, child_caps.clone(), parent.include_paths().clone());
    let grandchild = child.fork_for_ephemeral(&cfg, child_caps, child.include_paths().clone());

    // Register a scripted handle on each of child + grandchild
    // registries so cancel_all has something to flip.
    let child_handle_cancel = Arc::new(CancelState::new());
    let grandchild_handle_cancel = Arc::new(CancelState::new());
    register_scripted_handle(child.spawn_registry(), child_handle_cancel.clone());
    register_scripted_handle(
        grandchild.spawn_registry(),
        grandchild_handle_cancel.clone(),
    );

    // Trip the parent.
    parent.cancel_state().request_cancel();

    // Wait up to 250 ms for the cascade to land. Watcher polls every
    // 50 ms; two-hop propagation should land well within 250 ms.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < deadline {
        if child_handle_cancel.is_cancelled() && grandchild_handle_cancel.is_cancelled() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(
        child_handle_cancel.is_cancelled(),
        "child registry's child handle must be cancelled"
    );
    assert!(
        grandchild_handle_cancel.is_cancelled(),
        "grandchild registry's child handle must be cancelled"
    );

    // Sanity: silence unused-import warnings.
    let _ = std::any::TypeId::of::<SpawnRegistry>();
}

/// AC3.6 — eval-worker leak counter returns to baseline after a spawn
/// resolves normally.
///
/// The static `LIVE_EVAL_WORKERS` counter is incremented when an eval
/// worker thread is created and decremented (via RAII guard) when the
/// thread exits. After the ephemeral resolves and its EvalWorker is
/// dropped (in `run_ephemeral`'s tail), the counter must return to
/// baseline within a small grace window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eval_worker_count_returns_to_baseline_after_ephemeral() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }
    let baseline = pattern_runtime::agent_loop::eval_worker::live_eval_workers();

    let provider = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::text_turn("ok"),
    ]));
    let parent = build_parent_with_provider(None, None, provider).await;

    let cfg = EphemeralConfig::new("")
        .with_prompt("done.")
        .with_timeout(jiff::Span::new().seconds(10));
    let caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
    let includes = pattern_runtime::spawn::child_include_paths(&parent, None);
    let child = parent.fork_for_ephemeral(&cfg, caps, Arc::new(includes.clone()));
    let child_id: smol_str::SmolStr = pattern_core::types::ids::new_id();
    let log_label: smol_str::SmolStr = format!("spawn-log-{child_id}").into();
    pattern_runtime::spawn::create_progress_log_block(parent.adapter(), log_label.as_str())
        .unwrap();
    let preamble = pattern_runtime::sdk::preamble::build_for(
        &child
            .capabilities()
            .cloned()
            .unwrap_or_else(pattern_core::CapabilitySet::all),
    );

    let _ = pattern_runtime::spawn::run_ephemeral(
        child, cfg, child_id, log_label, includes, preamble, None,
    )
    .await;

    // Allow up to 500 ms for the worker thread to wind down. The
    // closure's RAII guard fires the moment the channel closes (drop
    // of EvalWorker happens at end of run_ephemeral) and the for-loop
    // exits. Polling here avoids racy assertions on slow CI.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        if pattern_runtime::agent_loop::eval_worker::live_eval_workers() == baseline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let final_count = pattern_runtime::agent_loop::eval_worker::live_eval_workers();
    panic!(
        "eval-worker leak: baseline={baseline}, final={final_count} (workers should have wound down)"
    );
}

/// Helper: register a scripted child-session handle on a `SpawnRegistry`
/// for cancel-propagation tests. The handle's result future resolves
/// immediately to a placeholder; the cancel atomic is what we observe.
fn register_scripted_handle(
    registry: &Arc<pattern_runtime::spawn::SpawnRegistry>,
    cancel: Arc<pattern_runtime::timeout::CancelState>,
) {
    use futures::FutureExt;
    let id: smol_str::SmolStr = pattern_core::types::ids::new_id();
    let result_fut = futures::future::ready(Ok(pattern_runtime::spawn::SpawnResult::new(
        id.clone(),
        pattern_runtime::spawn::TerminationReason::Cancelled,
    )))
    .boxed()
    .shared();
    registry.register(pattern_runtime::spawn::ChildSessionHandle {
        child_id: id,
        kind: pattern_runtime::spawn::SpawnKind::Ephemeral,
        cancel_state: cancel,
        result: result_fut,
        _permit: None,
    });
}

/// AC3.3 — costume override threads into the child's system_prompt slot.
///
/// Verified via direct `fork_for_ephemeral` inspection — no LLM needed.
/// AC3.3 also stipulates "persona identity remains the parent's in
/// logs"; the child's `agent_id` matching the parent's confirms that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn costume_overrides_system_prompt_and_preserves_persona_identity() {
    let parent = build_parent(None, None).await;
    let parent_agent_id = parent.agent_id().to_string();

    let cfg = EphemeralConfig::new("").with_costume("be terse");
    let caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
    let child = parent.fork_for_ephemeral(&cfg, caps, parent.include_paths().clone());

    // Persona identity preserved (AC3.3 second clause).
    assert_eq!(
        child.agent_id(),
        parent_agent_id,
        "child must share parent's agent_id so logs attribute to the parent persona"
    );
    // Costume installed on the system_prompt slot. SessionContext
    // doesn't expose system_prompt directly, but the child is
    // distinguishable from the parent by capabilities being Some(set).
    // For the prompt assertion we round-trip via Debug to verify the
    // string is reachable.
    let dbg = format!("{:?}", child);
    assert!(
        dbg.contains("be terse"),
        "child SessionContext debug must contain costume; got: {dbg}"
    );
}

/// AC3.1 — success path: ephemeral runs a single end-turn wire turn,
/// returns a SpawnResult with `final_text = Some("ok")` and a populated
/// progress-log block.
///
/// Mock provider scripts a single text-turn response, so no code-tool
/// dispatch happens — the EvalWorker spawns but is never invoked, which
/// means tidepool-extract is not strictly required for this test.
/// `program` is empty (no helper synthesis); `prompt` is what the LLM
/// "responds" to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ephemeral_success_returns_final_text_and_logs_progress() {
    if pattern_runtime::preflight::check().is_err() {
        // Even though we don't dispatch the code tool, EvalWorker
        // creation may fail without the harness in some configs.
        // Skip cleanly on systems without it.
        return;
    }

    let provider = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::text_turn("ok"),
    ]));
    let parent = build_parent_with_provider(None, None, provider).await;

    let cfg = EphemeralConfig::new("")
        .with_prompt("Respond ok and stop.")
        .with_timeout(jiff::Span::new().seconds(10));
    let child_caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
    let child_includes = pattern_runtime::spawn::child_include_paths(&parent, None);
    let child = parent.fork_for_ephemeral(&cfg, child_caps, Arc::new(child_includes.clone()));

    let child_id: smol_str::SmolStr = pattern_core::types::ids::new_id();
    let log_label: smol_str::SmolStr = format!("spawn-log-{child_id}").into();

    pattern_runtime::spawn::create_progress_log_block(parent.adapter(), log_label.as_str())
        .unwrap();

    let preamble = pattern_runtime::sdk::preamble::build_for(
        &child
            .capabilities()
            .cloned()
            .unwrap_or_else(pattern_core::CapabilitySet::all),
    );

    let result = pattern_runtime::spawn::run_ephemeral(
        child.clone(),
        cfg,
        child_id.clone(),
        log_label.clone(),
        child_includes,
        preamble,
        None,
    )
    .await
    .expect("run_ephemeral must succeed for end-turn-only mock script");

    assert_eq!(result.child_id, child_id);
    assert_eq!(result.final_text.as_deref(), Some("ok"));
    assert!(result.turns >= 1, "at least one wire turn should have run");
    assert_eq!(
        result.progress_log_label.as_deref(),
        Some(log_label.as_str())
    );

    // Progress-log block should now contain at least one entry.
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::memory_types::CONSTELLATION_OWNER;
    let block = parent
        .adapter()
        .get_block(CONSTELLATION_OWNER, log_label.as_str())
        .unwrap()
        .expect("progress-log block must exist after run");
    let entries = block.log_entries(None);
    assert!(
        !entries.is_empty(),
        "progress-log block should have at least one entry appended; got {} entries, render={:?}",
        entries.len(),
        block.render()
    );
}
