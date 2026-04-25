//! Phase 2 Task 4 — ephemeral spawn integration tests.
//!
//! Covers AC3.1 (success path with mock-LLM), AC3.2 (capability
//! escalation rejection), AC3.3 (costume + persona-identity
//! preservation), AC3.4 (timeout fires cancel + returns Timeout error),
//! AC3.5 (concurrency limit enforcement), and watcher-task leak
//! regression — plus the progress-log block creation + per-turn append
//! wiring.

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

/// C#2 regression — watcher tasks do not leak when child registries are
/// dropped before the parent cancels.
///
/// `fork_for_ephemeral` installs a watcher task on each child's registry.
/// The watcher parks on `CancelState::wait_for_cancel()`. Without the
/// `JoinHandle::abort()` call in `SpawnRegistry::Drop`, each watcher stays
/// alive until the parent's `Arc<CancelState>` reaches refcount 0 —
/// effectively forever in a long-lived non-cancelling parent.
///
/// This test verifies the watcher leak fix is REAL by measuring
/// `Arc::strong_count(&parent.cancel_state)` directly. Each
/// `fork_for_ephemeral` clones the parent's `Arc<CancelState>` into the
/// watcher closure; if the watcher leaks (parked on `notify.notified()`
/// indefinitely), the strong count stays elevated even after the child
/// `Arc<SessionContext>` is dropped.
///
/// The previous version of this test only asserted cascade propagation
/// to a still-live grandchild — that assertion would have passed even
/// with the leak intact. The Arc-strong-count check is the authoritative
/// signal: if the count returns to baseline, watcher tasks were
/// genuinely aborted; if it stays elevated, the leak persists.
///
/// Steps:
/// 1. Record `baseline = Arc::strong_count(&parent.cancel_state)`.
/// 2. Fork N=10 children. Each clones the parent's cancel_state into a
///    watcher closure → expected count = baseline + N during the loop.
/// 3. Drop all N children (their `SpawnRegistry::Drop` aborts watchers).
/// 4. Yield + poll until count returns to baseline (or 500 ms grace
///    expires).
/// 5. Assert count is back to baseline.
/// 6. Separately verify cascade still works for a live grandchild after
///    parent cancel — confirms the abort path doesn't break propagation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watcher_tasks_are_aborted_on_child_registry_drop() {
    let parent = build_parent(None, None).await;
    let cfg = pattern_core::spawn::EphemeralConfig::new("");

    const N: usize = 10;
    let baseline = Arc::strong_count(&parent.cancel_state());

    // Build N child contexts. Each `fork_for_ephemeral` installs a watcher
    // task on the child's registry, with the watcher closure holding a
    // clone of `parent.cancel_state()`.
    let children: Vec<_> = (0..N)
        .map(|_| {
            let caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
            parent.fork_for_ephemeral(&cfg, caps, parent.include_paths().clone())
        })
        .collect();

    // While the children are alive, the watchers hold N additional Arc
    // references on parent.cancel_state. Account for the children themselves
    // also cloning cancel_state into their SessionContext.cancel_state field:
    // each child holds 1 Arc directly + 1 in its watcher = 2 per child.
    let elevated = Arc::strong_count(&parent.cancel_state());
    assert!(
        elevated >= baseline + N,
        "expected at least baseline+N=({} + {}) Arc clones during fan-out, got {}",
        baseline,
        N,
        elevated
    );

    // Drop all N immediate children. Each drop triggers
    // SpawnRegistry::Drop → cancel_all + watcher.abort(). Aborted watchers
    // release their parent_cancel_for_watcher Arc clones.
    drop(children);

    // Poll until the count returns to baseline. The watcher's `abort()`
    // schedules cancellation but doesn't synchronously join — the
    // executor needs a few yields to process the abort + drop the
    // closure's captures.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        if Arc::strong_count(&parent.cancel_state()) == baseline {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let after_drop = Arc::strong_count(&parent.cancel_state());
    assert_eq!(
        after_drop, baseline,
        "Arc<CancelState> strong count must return to baseline ({}) after children dropped; \
         got {} — indicates leaked watcher tasks holding cancel_state clones",
        baseline, after_drop
    );

    // Separately verify cascade still works for a live grandchild after
    // parent cancel — confirms the Weak<SpawnRegistry> upgrade path
    // doesn't break propagation when the registry IS still alive.
    let grandchild_cfg = pattern_core::spawn::EphemeralConfig::new("");
    let grandchild_caps =
        pattern_runtime::spawn::compute_child_caps(&parent, &grandchild_cfg).unwrap();
    let live_child = parent.fork_for_ephemeral(
        &grandchild_cfg,
        grandchild_caps,
        parent.include_paths().clone(),
    );
    let deep_cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    register_scripted_handle(live_child.spawn_registry(), deep_cancel.clone());

    parent.cancel_state().request_cancel();

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < deadline {
        if deep_cancel.is_cancelled() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(
        deep_cancel.is_cancelled(),
        "cancel must propagate to still-live grandchild after parent cancel \
         (Weak::upgrade succeeds when registry is alive)"
    );

    drop(live_child);
}

/// AC3.4 — timeout fires `SpawnError::Timeout` and marks child cancelled.
///
/// Uses a `MockProviderClient` that never resolves (a hanging provider),
/// driving `run_ephemeral` with a 50 ms timeout. Asserts:
/// - Returns `Err(SpawnError::Timeout { .. })`.
/// - `child.cancel_state().is_cancelled()` is true after.
///
/// Wall-clock 50 ms is reliable enough under `flavor = "multi_thread"`;
/// `tokio::time::pause` is not used because the hanging provider and the
/// eval-worker thread interact with real wall time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac3_4_timeout_fires_cancel_and_returns_timeout_error() {
    if pattern_runtime::preflight::check().is_err() {
        // EvalWorker construction requires tidepool-extract on PATH.
        return;
    }

    // A provider that never produces any events — the run_ephemeral
    // future stays blocked waiting for the stream to complete.
    let provider = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::hanging_turn(),
    ]));
    let parent = build_parent_with_provider(None, None, provider).await;

    let cfg = EphemeralConfig::new("")
        .with_prompt("start.")
        .with_timeout(jiff::Span::new().milliseconds(50));
    let child_caps = pattern_runtime::spawn::compute_child_caps(&parent, &cfg).unwrap();
    let child_includes = pattern_runtime::spawn::child_include_paths(&parent, None);
    let child = parent.fork_for_ephemeral(&cfg, child_caps, Arc::new(child_includes.clone()));
    let child_cancel = child.cancel_state();

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
        cfg.clone(),
        child_id,
        log_label,
        child_includes,
        preamble,
        None,
    )
    .await;

    // AC3.4: must return Timeout error.
    match &result {
        Err(pattern_runtime::spawn::SpawnError::Timeout { timeout }) => {
            let ms = timeout.total(jiff::Unit::Millisecond).unwrap_or(0.0) as i64;
            assert_eq!(ms, 50, "timeout span should match configured 50 ms");
        }
        other => panic!("expected SpawnError::Timeout, got {other:?}"),
    }

    // AC3.4: cancel flag must be set after timeout.
    assert!(
        child_cancel.is_cancelled(),
        "child cancel state must be set after timeout fires"
    );
}

/// `WireForkIsolation::Persistent` on a session without `MountInfo`
/// returns `ForkError::PersistentNotAvailable` with a message naming
/// the missing mount-info wiring.
///
/// In Phase 2 this returned a "Phase 3" placeholder error; Phase 3 Task
/// 8 wired the persistent dispatch. Daemon callers populate `MountInfo`
/// via `SessionContext::with_mount_info`. Sessions constructed via
/// `from_persona` directly (test paths) do not have a mount, so the
/// path should fail closed with a clear diagnostic rather than silently
/// succeeding.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_fork_without_mount_info_returns_persistent_not_available() {
    use pattern_runtime::sdk::handlers::spawn::SpawnHandler;
    use pattern_runtime::sdk::requests::SpawnReq;
    use pattern_runtime::sdk::requests::spawn::{WireForkConfig, WireForkIsolation};
    use tidepool_effect::{EffectContext, EffectHandler};
    use tidepool_repr::DataConTable;

    let parent = build_parent(None, None).await;
    let wire_cfg = WireForkConfig {
        program: String::new(),
        isolation: WireForkIsolation::Persistent,
        capabilities: None,
        timeout_hint_ms: None,
        task_ref: None,
    };

    // Drive the handler from spawn_blocking (simulating eval-worker context).
    let parent_for_blocking = parent.clone();
    let err = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut h = SpawnHandler;
        h.handle(SpawnReq::Fork(wire_cfg), &cx)
    })
    .await
    .expect("spawn_blocking should not panic")
    .expect_err("Persistent fork on mountless session must return an error");

    let msg = err.to_string();
    assert!(
        msg.contains("persistent fork not available") && msg.contains("no mount info"),
        "error must surface PersistentNotAvailable with mount-info diagnostic; got: {msg}"
    );
}

/// Important #4 / AC3.5 — handler-side concurrency limit enforcement.
///
/// Drives `SpawnHandler::handle(SpawnReq::Ephemeral(_))` three times in
/// sequence on a parent with `replace_spawn_registry_for_test(2)`.
/// Asserts the third call returns `EffectError::Handler` whose message
/// contains "concurrent ephemeral limit reached" — verifying the
/// handler arm correctly translates a `None` permit into the wire-level
/// error, NOT just that the underlying semaphore saturates.
///
/// Pattern matches `persistent_fork_stub_returns_phase_3_error` for the
/// handler-via-`spawn_blocking` invocation shape.
///
/// Preflight-gated: requires `tidepool-extract` because the first two
/// successful Ephemeral calls construct an `EvalWorker` per spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ac3_5_handler_side_concurrency_limit_returns_handler_error() {
    use pattern_runtime::sdk::handlers::spawn::SpawnHandler;
    use pattern_runtime::sdk::requests::SpawnReq;
    use pattern_runtime::sdk::requests::spawn::WireEphemeralConfig;
    use tidepool_effect::{EffectContext, EffectHandler};
    use tidepool_repr::DataConTable;

    if pattern_runtime::preflight::check().is_err() {
        // The first two successful calls actually fork an EvalWorker, which
        // requires the harness binary on PATH. Skip cleanly without it.
        return;
    }

    // Build a parent with a limit-2 spawn registry. Use a hanging mock
    // provider so the children stay alive (don't complete and free their
    // permits) while we test saturation.
    // Two hanging scripts — one per successful Ephemeral. The third call
    // rejects at the registry (try_acquire returns None) before reaching
    // the provider, so 2 scripts is exactly right; a third would be unused.
    let provider = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::hanging_turn(),
        MockProviderClient::hanging_turn(),
    ]));
    let parent = build_parent_with_provider(None, Some(2), provider).await;

    // Drive the handler 3 times in sequence from `spawn_blocking` —
    // mirroring the eval-worker thread context that the handler runs
    // under in production.
    let parent_for_blocking = parent.clone();
    let outcomes = tokio::task::spawn_blocking(move || {
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, parent_for_blocking.as_ref());
        let mut handler = SpawnHandler;

        let mk = || WireEphemeralConfig {
            program: String::new(),
            costume: None,
            capabilities: None,
            timeout_ms: None,
            prompt: None,
        };

        let r1 = handler.handle(SpawnReq::Ephemeral(mk()), &cx);
        let r2 = handler.handle(SpawnReq::Ephemeral(mk()), &cx);
        let r3 = handler.handle(SpawnReq::Ephemeral(mk()), &cx);
        (
            r1.map(|_| ()).map_err(|e| e.to_string()),
            r2.map(|_| ()).map_err(|e| e.to_string()),
            r3.map(|_| ()).map_err(|e| e.to_string()),
        )
    })
    .await
    .expect("spawn_blocking should not panic");

    // Calls 1 and 2 may fail at the wire-encode step (the test's empty
    // DataConTable doesn't know `Pattern.Spawn.EphemeralSpawn`), but the
    // permit is acquired BEFORE encode and stored on the registered
    // ChildSessionHandle, so the registry is saturated regardless. The
    // load-bearing assertion is on call 3's error mode.
    let third_err = outcomes
        .2
        .expect_err("third Ephemeral must return Err — registry saturated at limit=2");
    assert!(
        third_err.contains("concurrent ephemeral limit"),
        "third call must fail with the concurrency-limit message, NOT with an \
         encode/bridge error. Got: {third_err}"
    );
    assert!(
        third_err.contains("2"),
        "error must surface the configured limit (2); got: {third_err}"
    );
    // Defensive: if calls 1 and 2 had ALSO returned the concurrency-limit
    // error, the test would be a false positive (limit would never have
    // been hit because permits weren't acquired). Verify the first two
    // didn't return a concurrency-limit error.
    if let Err(e) = &outcomes.0 {
        assert!(
            !e.contains("concurrent ephemeral limit"),
            "first call must not report concurrency-limit (it should have acquired \
             the first permit); got: {e}"
        );
    }
    if let Err(e) = &outcomes.1 {
        assert!(
            !e.contains("concurrent ephemeral limit"),
            "second call must not report concurrency-limit (it should have acquired \
             the second permit); got: {e}"
        );
    }

    // Tear down the parent so its watcher tasks abort cleanly.
    drop(parent);
}

/// C#3 — `block_on` in the spawn handler arm executes correctly from the
/// eval-worker thread (simulated via `tokio::task::spawn_blocking`).
///
/// This test does NOT go through the full Haskell eval path. Instead:
/// 1. Constructs a real `SessionContext` with `Handle::current()`.
/// 2. Registers a scripted child handle whose result future resolves
///    immediately to a known `SpawnResult`.
/// 3. Calls `tokio_handle().block_on(registry.wait_for(id))` from a
///    `spawn_blocking` task, exactly mirroring the pattern in
///    `handle_await_spawn`, `handle_await_all`, and `handle_sibling`.
/// 4. Asserts the result matches the registered handle.
///
/// If `block_on` deadlocks under a single-worker runtime, this test will
/// hang (and be caught by the test timeout). The `worker_threads = 2`
/// annotation ensures at least one thread is available for the tokio
/// future while `spawn_blocking` occupies the other.
///
/// Note: we test the `block_on` invocation directly rather than going
/// through `handle_await_spawn` itself, because `cx.respond()` requires
/// the datacon table to have `Pattern.Spawn.SpawnResult` constructors
/// registered — which is not available outside the GHC eval path.
/// The root bug (deadlock risk) is in the `block_on` call, not the
/// downstream `cx.respond` encoding, so this test exercises exactly the
/// right surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c3_block_on_await_spawn_executes_from_blocking_thread() {
    use futures::FutureExt;
    use pattern_runtime::spawn::{ChildSessionHandle, SpawnKind, SpawnResult, TerminationReason};
    use pattern_runtime::timeout::CancelState;

    let parent = build_parent(None, None).await;

    // Register a scripted child handle with a known result.
    let child_id = smol_str::SmolStr::from("c3-test-child");
    let expected_text = "hello from child".to_string();
    // Use SpawnResult::new() because SpawnResult is #[non_exhaustive].
    let mut expected_result = SpawnResult::new(child_id.clone(), TerminationReason::EndTurn);
    expected_result.final_text = Some(expected_text.clone());
    expected_result.turns = 1;
    let result_fut = futures::future::ready(Ok(expected_result)).boxed().shared();
    parent.spawn_registry().register(ChildSessionHandle {
        child_id: child_id.clone(),
        kind: SpawnKind::Ephemeral,
        cancel_state: Arc::new(CancelState::new()),
        result: result_fut,
        _permit: None,
    });

    // Call `tokio_handle().block_on(registry.wait_for(id))` from
    // spawn_blocking, mirroring the exact pattern used in the handler arm.
    // If block_on deadlocks with a single-worker runtime, this test hangs.
    let parent_for_blocking = parent.clone();
    let child_id_for_blocking = child_id.clone();
    let spawn_result = tokio::task::spawn_blocking(move || {
        let registry = parent_for_blocking.spawn_registry().clone();
        let handle = parent_for_blocking.tokio_handle().clone();
        handle.block_on(registry.wait_for(&child_id_for_blocking))
    })
    .await
    .expect("spawn_blocking should not panic")
    .expect("block_on(wait_for) must succeed for a registered child");

    assert_eq!(
        spawn_result.child_id.as_str(),
        "c3-test-child",
        "child_id must match the registered handle"
    );
    assert_eq!(
        spawn_result.final_text.as_deref(),
        Some("hello from child"),
        "final_text must round-trip through the Shared<BoxFuture>"
    );
    assert_eq!(spawn_result.turns, 1, "turns must match");
    assert_eq!(
        spawn_result.terminated,
        TerminationReason::EndTurn,
        "termination reason must match"
    );
}
