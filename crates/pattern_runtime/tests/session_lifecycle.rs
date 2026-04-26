//! Session lifecycle integration tests ported from the deleted
//! `SessionMachine`-era tests (Phase 6 Task B retired that path).
//!
//! These exercise the agent-loop path (`open_with_agent_loop` +
//! `step_with_agent_loop`) with `MockProviderClient` and an in-memory DB
//! to cover the high-value scenarios:
//!
//! 1. **Memory round-trip**: open a session with persona memory blocks,
//!    verify blocks are seeded into the store, then run a step and verify
//!    blocks survive the step (persist through session state).
//! 2. **Checkpoint / restore**: checkpoint after a step, restore into a
//!    fresh session, verify the checkpoint log round-trips.
//! 3. **Concurrent session isolation**: two sessions with different
//!    agent_ids against the same DB must not see each other's messages
//!    in their active `TurnHistory`.
//!
//! All tests are gated on `preflight::check()` (tidepool-extract required).

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::{MemoryStore, Session, TurnSink, VecSink};
use pattern_core::types::ids::{BatchId, new_snowflake_id};
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::{StopReason, TurnInput};
use pattern_runtime::SdkLocation;
use pattern_runtime::session::TidepoolSession;
use pattern_runtime::testing::{InMemoryMemoryStore, MockProviderClient};

// ── helpers ──────────────────────────────────────────────────────────────────

fn test_turn_input() -> TurnInput {
    let id = new_snowflake_id();
    TurnInput {
        turn_id: id.clone(),
        batch_id: BatchId::from(id),
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![],
    }
}

/// Create the `agents` DB row so FK constraints on `messages.agent_id` are
/// satisfied when the agent loop persists messages.
async fn create_agent_row(db: &pattern_db::ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: agent_id.to_string(),
        description: None,
        model_provider: "test".to_string(),
        model_name: "test-model".to_string(),
        system_prompt: "test".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: pattern_db::models::AgentStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("create test agent row");
}

// ── 1. memory round-trip through a session ───────────────────────────────────

/// Open a session with persona-declared memory blocks, verify the blocks
/// are created in the store, run one step (text-only), and verify the
/// blocks survive the step (persist through session state).
///
/// This exercises the `seed_persona_memory_blocks` path and validates that
/// memory state is accessible across the session lifecycle — the core
/// scenario from the deleted `SessionMachine`-era memory round-trip test.
///
/// Note: `InMemoryMemoryStore`'s `create_block` returns a `LoroDoc` clone.
/// Content written via `import_from_json` on the returned doc stays on the
/// caller's clone — the stored copy receives only the initial empty state.
/// We therefore verify block *existence* and metadata fidelity rather than
/// text content.  This exercises the meaningful part of the round-trip
/// (creation, persistence across step, metadata propagation).
#[tokio::test]
async fn memory_round_trip_through_session() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::text_turn("I can see your memory blocks."),
    ]));
    let db = pattern_runtime::testing::test_db().await;
    create_agent_row(&db, "agent-mem").await;

    // Persona with a seeded memory block.
    let persona = PersonaSnapshot::new("agent-mem", "MemAgent").with_memory_block(
        smol_str::SmolStr::from("scratch"),
        pattern_core::types::snapshot::MemoryBlockSpec::text("initial content"),
    );
    let sdk = SdkLocation::default();
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());

    let port_registry = std::sync::Arc::new(pattern_runtime::port_registry::PortRegistryImpl::new(
        &tokio::runtime::Handle::current(),
    ));
    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        store.clone(),
        provider,
        db,
        sink,
        None,
        None,
        None,
        port_registry,
    )
    .await
    .expect("open should succeed");

    // After open, the persona-declared block should be seeded into the store.
    let block = store
        .get_block("agent-mem", "scratch")
        .expect("get_block should succeed")
        .expect("scratch block should exist after session open");

    // Verify metadata was propagated from the persona spec.
    let meta = block.metadata();
    assert_eq!(meta.label, "scratch", "block label should match");
    assert_eq!(meta.agent_id, "agent-mem", "agent_id should match");

    // Run one step.
    let reply = session
        .step_with_agent_loop(test_turn_input())
        .await
        .expect("step should succeed");

    assert_eq!(reply.final_stop_reason, StopReason::EndTurn);
    assert_eq!(
        reply.turns.len(),
        1,
        "text-only step produces one wire turn"
    );

    // After the step, the memory block should still be accessible.
    let block_post = store
        .get_block("agent-mem", "scratch")
        .expect("get_block should succeed after step")
        .expect("scratch block should survive the step");
    assert_eq!(
        block_post.metadata().label,
        "scratch",
        "block should retain its label after the step"
    );

    // The turn history should record the step.
    let history = session.turn_history();
    let msg_count = history.lock().unwrap().active_messages().count();
    assert!(
        msg_count > 0,
        "turn history should have at least one message after the step"
    );
}

// ── 2. checkpoint / restore ──────────────────────────────────────────────────

/// Open a session, run one step, checkpoint, then restore into a fresh
/// session and verify the checkpoint log round-trips.
#[tokio::test]
async fn checkpoint_and_restore_round_trips() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::text_turn("Hello from checkpoint test."),
    ]));
    let db = pattern_runtime::testing::test_db().await;
    create_agent_row(&db, "agent-ckpt").await;

    let persona = PersonaSnapshot::new("agent-ckpt", "CkptAgent");
    let sdk = SdkLocation::default();
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());

    let port_registry = std::sync::Arc::new(pattern_runtime::port_registry::PortRegistryImpl::new(
        &tokio::runtime::Handle::current(),
    ));
    let session = TidepoolSession::open_with_agent_loop(
        persona.clone(),
        &sdk,
        store.clone(),
        provider,
        db.clone(),
        sink,
        None,
        None,
        None,
        port_registry,
    )
    .await
    .expect("open should succeed");

    // Run one step.
    let _reply = session
        .step_with_agent_loop(test_turn_input())
        .await
        .expect("step should succeed");

    // Checkpoint.
    let snapshot = session
        .checkpoint()
        .await
        .expect("checkpoint should succeed");

    // The snapshot should contain at least one persona entry.
    assert!(
        !snapshot.personas.is_empty(),
        "snapshot should have at least one persona"
    );

    // Restore into a fresh session (same persona + same DB, fresh store).
    let store2: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider2: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
    let sink2: Arc<dyn TurnSink> = Arc::new(VecSink::new());

    let port_registry2 = std::sync::Arc::new(
        pattern_runtime::port_registry::PortRegistryImpl::new(&tokio::runtime::Handle::current()),
    );
    let mut session2 = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        store2,
        provider2,
        db,
        sink2,
        None,
        None,
        None,
        port_registry2,
    )
    .await
    .expect("second open should succeed");

    // Restore should succeed.
    session2
        .restore(snapshot.clone())
        .await
        .expect("restore should succeed");

    // Validate the round-trip by re-checkpointing and comparing persona
    // entries. The restored log may be empty for a text-only step (no
    // effect exchanges recorded), so we compare the persona-level
    // snapshot structure rather than event counts.
    let snapshot2 = session2
        .checkpoint()
        .await
        .expect("re-checkpoint should succeed");
    assert_eq!(
        snapshot.personas.len(),
        snapshot2.personas.len(),
        "re-checkpoint should preserve persona count"
    );
    // Verify the extra (event-log JSON) matches after round-trip.
    assert_eq!(
        snapshot.personas[0].extra, snapshot2.personas[0].extra,
        "extra (event log) should be identical after restore + re-checkpoint"
    );
}

// ── 3. concurrent session isolation ──────────────────────────────────────────

/// Spawn two sessions with different agent_ids against the same DB. Run a
/// step on each. Assert neither session sees the other's messages in its
/// active TurnHistory.
#[tokio::test]
async fn concurrent_session_isolation() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let db = pattern_runtime::testing::test_db().await;
    create_agent_row(&db, "agent-alpha").await;
    create_agent_row(&db, "agent-beta").await;

    let sdk = SdkLocation::default();

    // Session A.
    let store_a: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider_a: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::text_turn("I am alpha."),
    ]));
    let sink_a: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let persona_a = PersonaSnapshot::new("agent-alpha", "Alpha");

    let port_registry_a = std::sync::Arc::new(
        pattern_runtime::port_registry::PortRegistryImpl::new(&tokio::runtime::Handle::current()),
    );
    let session_a = TidepoolSession::open_with_agent_loop(
        persona_a,
        &sdk,
        store_a,
        provider_a,
        db.clone(),
        sink_a,
        None,
        None,
        None,
        port_registry_a,
    )
    .await
    .expect("open A");

    // Session B.
    let store_b: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider_b: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![
        MockProviderClient::text_turn("I am beta."),
    ]));
    let sink_b: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let persona_b = PersonaSnapshot::new("agent-beta", "Beta");

    let port_registry_b = std::sync::Arc::new(
        pattern_runtime::port_registry::PortRegistryImpl::new(&tokio::runtime::Handle::current()),
    );
    let session_b = TidepoolSession::open_with_agent_loop(
        persona_b,
        &sdk,
        store_b,
        provider_b,
        db.clone(),
        sink_b,
        None,
        None,
        None,
        port_registry_b,
    )
    .await
    .expect("open B");

    // Run both steps concurrently.
    let (reply_a, reply_b) = tokio::join!(
        session_a.step_with_agent_loop(test_turn_input()),
        session_b.step_with_agent_loop(test_turn_input()),
    );
    let reply_a = reply_a.expect("step A");
    let reply_b = reply_b.expect("step B");

    // Both should have completed successfully.
    assert_eq!(reply_a.final_stop_reason, StopReason::EndTurn);
    assert_eq!(reply_b.final_stop_reason, StopReason::EndTurn);

    // Check that each session's TurnHistory only contains its own messages.
    let history_a = session_a.turn_history();
    let history_b = session_b.turn_history();

    let msgs_a: Vec<_> = history_a
        .lock()
        .unwrap()
        .active_messages()
        .map(|m| m.owner_id.clone())
        .collect();
    let msgs_b: Vec<_> = history_b
        .lock()
        .unwrap()
        .active_messages()
        .map(|m| m.owner_id.clone())
        .collect();

    // All messages in session A's history should be for agent-alpha.
    for agent in &msgs_a {
        assert_eq!(
            agent.as_str(),
            "agent-alpha",
            "session A's history should only contain agent-alpha messages, found: {agent}"
        );
    }
    // All messages in session B's history should be for agent-beta.
    for agent in &msgs_b {
        assert_eq!(
            agent.as_str(),
            "agent-beta",
            "session B's history should only contain agent-beta messages, found: {agent}"
        );
    }

    // Neither should be empty (each ran a step).
    assert!(
        !msgs_a.is_empty(),
        "session A should have recorded at least one message"
    );
    assert!(
        !msgs_b.is_empty(),
        "session B should have recorded at least one message"
    );
}
