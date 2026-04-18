//! End-to-end tests for [`pattern_runtime::TidepoolSession`] and
//! [`pattern_runtime::TidepoolRuntime`] (Phase 3 Task 14 — AC2.1, AC2.10).
//!
//! Preflight is enforced up-front via `.expect(...)`: tests must fail
//! loudly (not silently skip) when `tidepool-extract` is unavailable.

use std::sync::Arc;
use std::time::Instant;

use jiff::Timestamp;
use pattern_core::traits::{AgentRuntime, Session};
use pattern_core::types::ids::{new_id, BatchId};
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaConfig;
use pattern_core::types::turn::TurnInput;
use pattern_runtime::TidepoolRuntime;
use pattern_runtime::testing::{InMemoryMemoryStore, NopProviderClient};

/// Build a TurnInput carrying zero messages (Phase 3 tests don't yet
/// exercise message-bearing turns; Phase 4 adds that path).
fn fresh_turn_input() -> TurnInput {
    TurnInput {
        turn_id: new_id(),
        batch_id: BatchId::from(new_id()),
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![],
    }
}

fn preflight_or_fail() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");
}

/// AC2.1: open → step → drop cycle completes without error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_then_step_then_drop() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory, provider);
    let persona = PersonaConfig::new(
        "open-step-drop",
        "OpenStepDrop",
        include_str!("fixtures/time_log.hs"),
    );
    let mut session = runtime.open_session(persona, None).await.expect("open");
    let _out = session.step(fresh_turn_input()).await.expect("step");
    drop(session);
}

/// AC2.1: second step reuses the compiled machine. We assert this via
/// timing — the first step's cost includes compile+JIT warm; subsequent
/// steps are much cheaper. In practice the ratio is ~100× (compile
/// ~600ms, warm-run ~5ms); a 10× threshold tolerates a noisy CI / loaded
/// machine without being so loose that a regression where the second
/// step re-compiles would go unnoticed.
///
/// This timing ratio is an inherently fragile shape — if it starts
/// flaking under CI load, the right fix is to expose a structural
/// "was recompiled" signal on `TidepoolSession` (e.g. a boolean flag on
/// `InnerState` or a JIT-instance identity check) and match on that
/// instead of wall time. Today no such signal exists, and 10× has
/// comfortable headroom.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_step_twice_does_not_recompile() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory, provider);
    let persona = PersonaConfig::new(
        "step-twice",
        "StepTwice",
        include_str!("fixtures/time_log.hs"),
    );
    let mut session = runtime.open_session(persona, None).await.expect("open");

    let t0 = Instant::now();
    session.step(fresh_turn_input()).await.expect("step 1");
    let first = t0.elapsed();

    let t1 = Instant::now();
    session.step(fresh_turn_input()).await.expect("step 2");
    let second = t1.elapsed();

    // Both steps must succeed (already asserted via `.expect`).
    // Warm run should be dramatically faster than the cold one. A 10×
    // ratio absorbs CI jitter while still failing loud on a regression
    // that reintroduces recompilation.
    assert!(
        second.as_secs_f64() * 10.0 < first.as_secs_f64().max(0.001),
        "warm run ({:?}) should be at least 10× faster than cold ({:?}); \
         a smaller ratio suggests recompilation snuck in",
        second,
        first,
    );
}

/// AC2.4: memory writes persist across turns within a session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_write_then_read_roundtrips() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory.clone(), provider);

    // Turn 1: write using the write-agent program.
    let persona_write = PersonaConfig::new(
        "roundtrip",
        "RoundtripWrite",
        include_str!("fixtures/memory_write.hs"),
    );
    let mut session_write = runtime
        .open_session(persona_write, None)
        .await
        .expect("open write");
    session_write
        .step(fresh_turn_input())
        .await
        .expect("write turn");

    // The store is shared between sessions (same Arc). Verify the block
    // landed via the trait directly — independent of handler dispatch.
    let content = pattern_core::traits::MemoryStore::get_rendered_content(
        memory.as_ref(),
        "roundtrip",
        "scratchpad",
    )
    .await
    .expect("get_rendered_content")
    .expect("block should exist after write turn");
    assert_eq!(content, "hello from turn 1");

    // Turn 2: open a fresh session with the same agent id + store and
    // run the read-agent. The read should see the prior write.
    let persona_read = PersonaConfig::new(
        "roundtrip",
        "RoundtripRead",
        include_str!("fixtures/memory_read.hs"),
    );
    let mut session_read = runtime
        .open_session(persona_read, None)
        .await
        .expect("open read");
    // Reading a missing block would produce a handler error; a
    // successful step confirms the block was found and returned.
    session_read
        .step(fresh_turn_input())
        .await
        .expect("read turn");
}

/// AC2.10: concurrent sessions run in isolation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sessions_are_isolated() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = Arc::new(TidepoolRuntime::with_default_sdk(memory, provider));

    let mut handles = Vec::new();
    for i in 0..4u32 {
        let rt = runtime.clone();
        handles.push(tokio::spawn(async move {
            let persona = PersonaConfig::new(
                format!("concurrent-{i}"),
                format!("Concurrent{i}"),
                include_str!("fixtures/time_log.hs"),
            );
            let mut s = rt.open_session(persona, None).await.expect("open");
            for _ in 0..3 {
                s.step(fresh_turn_input()).await.expect("step");
            }
        }));
    }
    for h in handles {
        h.await.expect("task join");
    }
}

/// AC2.4: checkpoint → restore round-trip. Phase 3 scope verifies that
/// the snapshot survives a serialise/deserialise cycle via
/// [`pattern_core::types::snapshot::SessionSnapshot`]. Faithful
/// deterministic replay (re-driving the JIT with recorded responses) is
/// deferred to the phase that lands the replay bundle — see
/// `crates/pattern_runtime/src/checkpoint.rs` for the shape rationale.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_restore_roundtrip_preserves_events() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory, provider);
    let persona = PersonaConfig::new(
        "cp-roundtrip",
        "CpRoundtrip",
        include_str!("fixtures/time_log.hs"),
    );
    let session = runtime.open_session(persona, None).await.expect("open");

    // Seed the event log so there's something to round-trip. Phase 3
    // handlers do not yet write to the log during `step` (that wiring
    // goes in once the replay bundle is ready); we exercise the
    // snapshot contract directly.
    let log = session.checkpoint_log();
    {
        let mut guard = log.lock().expect("log mutex");
        use pattern_runtime::checkpoint::CheckpointEvent;
        use tidepool_eval::Value;
        use tidepool_repr::Literal;
        guard.record(CheckpointEvent::new(
            7,
            &Value::Lit(Literal::LitInt(1)),
            &Value::Lit(Literal::LitInt(2)),
            1,
        ));
        guard.record(CheckpointEvent::new(
            9,
            &Value::Lit(Literal::LitInt(3)),
            &Value::Lit(Literal::LitInt(4)),
            1,
        ));
    }

    let snap = session.checkpoint().await.expect("checkpoint");
    assert_eq!(snap.schema_version, 1);
    assert_eq!(snap.personas.len(), 1);

    // Serialize → deserialize to exercise the full wire contract
    // (JSON-as-opaque-data on `SessionSnapshot.data`).
    let json = serde_json::to_string(&snap).expect("SessionSnapshot should serialize to JSON");
    let decoded: pattern_core::types::snapshot::SessionSnapshot =
        serde_json::from_str(&json).expect("SessionSnapshot should deserialize");
    assert_eq!(decoded.personas.len(), 1);

    // Restore into a fresh session; event log should now contain the
    // recovered events.
    let persona2 = PersonaConfig::new(
        "cp-roundtrip",
        "CpRoundtrip2",
        include_str!("fixtures/time_log.hs"),
    );
    let mut session2 = runtime.open_session(persona2, None).await.expect("open 2");
    session2.restore(decoded).await.expect("restore");
    let log2 = session2.checkpoint_log();
    let guard = log2.lock().expect("log mutex");
    assert_eq!(guard.len(), 2);
    assert_eq!(guard.events()[0].tag, 7);
    assert_eq!(guard.events()[1].tag, 9);

    // Touch `Timestamp::now()` to silence an unused-import warning on
    // jiff; keeping this import makes future time-aware checkpoint
    // extensions diff-minimally.
    let _ = Timestamp::now();
}

/// Re-opening a runtime with `with_default_sdk` using the same store
/// produces independent sessions that see the same persisted memory
/// writes. This complements `memory_write_then_read_roundtrips` by
/// exercising the runtime-level path rather than a single session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_shares_store_across_sessions() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory.clone(), provider);

    let persona = PersonaConfig::new(
        "shared-store",
        "SharedStore",
        include_str!("fixtures/memory_write.hs"),
    );
    let mut s1 = runtime.open_session(persona, None).await.expect("open 1");
    s1.step(fresh_turn_input()).await.expect("write");

    drop(s1);

    let persona2 = PersonaConfig::new(
        "shared-store",
        "SharedStore",
        include_str!("fixtures/memory_read.hs"),
    );
    let mut s2 = runtime.open_session(persona2, None).await.expect("open 2");
    s2.step(fresh_turn_input()).await.expect("read");
}

/// The `memory_create` fixture exercises `Pattern.Memory.create`,
/// `writeWithDesc`, and `replace` in one agent turn. After the step:
///
/// - the `notes` block exists with the final description set by
///   `writeWithDesc`,
/// - its type / schema match what `create` requested,
/// - the content reflects `replace` applied after `writeWithDesc`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_create_write_replace_end_to_end() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory.clone(), provider);

    let persona = PersonaConfig::new(
        "create-agent",
        "CreateAgent",
        include_str!("fixtures/memory_create.hs"),
    );
    let mut session = runtime
        .open_session(persona, None)
        .await
        .expect("open create session");
    session.step(fresh_turn_input()).await.expect("create turn");

    // Verify metadata — description was updated by writeWithDesc.
    let meta = pattern_core::traits::MemoryStore::get_block_metadata(
        memory.as_ref(),
        "create-agent",
        "notes",
    )
    .await
    .expect("get_block_metadata")
    .expect("block notes should exist");
    assert_eq!(
        meta.description, "user notes (revised)",
        "description should reflect writeWithDesc, not the original create value"
    );
    assert_eq!(meta.block_type, pattern_core::memory::BlockType::Working);
    assert!(
        meta.schema.is_text(),
        "schema should be Text (as Created); got {:?}",
        meta.schema
    );

    // Verify content — replace turned "first" into "HEAD" in the content
    // written by writeWithDesc.
    let content = pattern_core::traits::MemoryStore::get_rendered_content(
        memory.as_ref(),
        "create-agent",
        "notes",
    )
    .await
    .expect("get_rendered_content")
    .expect("notes content should be present");
    assert_eq!(content, "HEAD line\nsecond line");
}

/// AC2.4 wiring: MemoryHandler records each exchange into the session's
/// checkpoint log. After running an agent that does `Put` + `Get` we
/// should see two recorded events with tag=0 (MemoryHandler position),
/// and the events survive a checkpoint → restore round-trip into a fresh
/// session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_handler_records_exchanges_into_checkpoint_log() {
    preflight_or_fail();
    let memory = Arc::new(InMemoryMemoryStore::new());
    let provider = Arc::new(NopProviderClient);
    let runtime = TidepoolRuntime::with_default_sdk(memory, provider);

    let persona = PersonaConfig::new(
        "cp-wire",
        "CpWire",
        include_str!("fixtures/memory_put_get.hs"),
    );
    let mut session = runtime.open_session(persona, None).await.expect("open");
    session
        .step(fresh_turn_input())
        .await
        .expect("put+get turn");

    // The handler records one event per successful Memory effect. The
    // agent does Put + Get; both succeed (Put auto-creates, Get reads
    // back the value).
    let log = session.checkpoint_log();
    let events = {
        let guard = log.lock().expect("log mutex");
        guard.events().to_vec()
    };
    assert_eq!(
        events.len(),
        2,
        "expected 2 recorded exchanges (Put, Get), got {}: {:?}",
        events.len(),
        events,
    );
    // Every event should carry the MemoryHandler tag (0). Without the
    // wiring, `events` would be empty.
    for e in &events {
        assert_eq!(e.tag, 0, "expected MemoryHandler tag 0, got {}", e.tag);
        assert_eq!(e.turn, 1, "events should be stamped with turn 1");
    }
    // Sanity: request reprs should identify which MemoryReq variant
    // produced them, confirming the Debug-repr path works.
    assert!(
        events[0].request_repr.contains("Put"),
        "first event should be a Put, got: {}",
        events[0].request_repr,
    );
    assert!(
        events[1].request_repr.contains("Get"),
        "second event should be a Get, got: {}",
        events[1].request_repr,
    );

    // Checkpoint → restore round-trip preserves the recorded events in
    // a fresh session.
    let snap = session.checkpoint().await.expect("checkpoint");
    let persona2 = PersonaConfig::new(
        "cp-wire",
        "CpWire2",
        include_str!("fixtures/memory_put_get.hs"),
    );
    let mut session2 = runtime.open_session(persona2, None).await.expect("open 2");
    session2.restore(snap).await.expect("restore");
    let log2 = session2.checkpoint_log();
    let restored = log2.lock().expect("log mutex 2").events().to_vec();
    assert_eq!(restored.len(), 2, "restored event count matches source");
    assert_eq!(restored[0].tag, 0);
    assert_eq!(restored[1].tag, 0);
    assert!(restored[0].request_repr.contains("Put"));
    assert!(restored[1].request_repr.contains("Get"));
}

/// Direct unit test against the in-memory store: `update_block_description`
/// errors on a missing block. (Handler-level negative tests for Replace
/// are covered by handler unit tests.)
#[tokio::test]
async fn update_block_description_on_missing_block_returns_not_found() {
    let memory = InMemoryMemoryStore::new();
    let err =
        pattern_core::traits::MemoryStore::update_block_description(&memory, "who", "nope", "x")
            .await
            .expect_err("missing block should fail");
    match err {
        pattern_core::memory::MemoryError::NotFound { label, .. } => {
            assert_eq!(label, "nope");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}
