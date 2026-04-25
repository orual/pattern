//! Integration tests for the compaction driver (`pattern_runtime::compaction`).
//!
//! Exercises gate logic, strategy dispatch, DB updates, and TurnHistory
//! rewriting. Uses `MockProviderClient` with configurable `count_tokens`
//! to control whether the gate fires, and scripted `complete` responses
//! for RecursiveSummarization.

use std::sync::Arc;

use jiff::Timestamp;

use pattern_core::ProviderClient;
use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
use pattern_core::types::compression::CompressionStrategy;
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::{ContextPolicy, PersonaSnapshot};
use pattern_core::types::turn::{StopReason, TurnInput, TurnOutput};

use pattern_runtime::compaction::{CompactionOutcome, maybe_compact};
use pattern_runtime::memory::TurnHistory;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, MockProviderClient, test_db};

// ---- helpers ----------------------------------------------------------------

async fn create_test_agent(db: &pattern_db::ConstellationDb, id: &str) {
    use chrono::Utc;
    let agent = pattern_db::models::Agent {
        id: id.to_string(),
        name: "Test Agent".to_string(),
        description: None,
        model_provider: "test".to_string(),
        model_name: "test-model".to_string(),
        system_prompt: "Test prompt".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: pattern_db::models::AgentStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
        .expect("create_test_agent failed");
}

/// Build a SessionContext with a custom PersonaSnapshot and MockProviderClient.
async fn setup_with_persona(
    persona: PersonaSnapshot,
    provider: Arc<MockProviderClient>,
) -> (Arc<SessionContext>, Arc<pattern_db::ConstellationDb>) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider_dyn: Arc<dyn ProviderClient> = provider;
    let db = test_db().await;
    create_test_agent(&db, persona.agent_id.as_str()).await;
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider_dyn, db.clone())
            .with_turn_sink(sink),
    );
    (ctx, db)
}

/// Build a TurnHistory with `n` turns, each with one user + one assistant message.
/// Also persists messages to the DB so archive_messages has rows to mark.
async fn populate_history(
    db: &pattern_db::ConstellationDb,
    agent_id: &str,
    n: usize,
) -> Arc<std::sync::Mutex<TurnHistory>> {
    let mut hist = TurnHistory::empty();

    for i in 0..n {
        let batch_id: BatchId = new_snowflake_id();
        let turn_id = new_snowflake_id();

        let user_msg = Message {
            chat_message: genai::chat::ChatMessage::user(format!("user message {i}")),
            id: MessageId::from(new_id()),
            position: new_snowflake_id(),
            owner_id: AgentId::from(agent_id),
            created_at: Timestamp::now(),
            batch: batch_id.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };

        let assistant_msg = Message {
            chat_message: genai::chat::ChatMessage::assistant(format!("assistant reply {i}")),
            id: MessageId::from(new_id()),
            position: new_snowflake_id(),
            owner_id: AgentId::from(agent_id),
            created_at: Timestamp::now(),
            batch: batch_id.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };

        // Persist to DB.
        let db_user = to_db_message(&user_msg, agent_id);
        let db_asst = to_db_message(&assistant_msg, agent_id);
        pattern_db::queries::create_message(&db.get().unwrap(), &db_user)
            .expect("create_message failed");
        pattern_db::queries::create_message(&db.get().unwrap(), &db_asst)
            .expect("create_message failed");

        let input = TurnInput {
            turn_id: turn_id.clone(),
            batch_id: batch_id.clone(),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![user_msg],
        };

        let output = TurnOutput {
            messages: vec![assistant_msg],
            block_writes: vec![],
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: None,
            cache_metrics: Default::default(),
            completed_at: Timestamp::now(),
        };

        hist.record(turn_id, input, output);
    }

    Arc::new(std::sync::Mutex::new(hist))
}

/// Convert a pattern_core::Message to a pattern_db::models::Message for
/// persistence. Simplified version of the agent_loop's persist path.
fn to_db_message(msg: &Message, agent_id: &str) -> pattern_db::models::Message {
    use pattern_db::models::{BatchType, MessageRole};
    let role = match msg.chat_message.role {
        genai::chat::ChatRole::User => MessageRole::User,
        genai::chat::ChatRole::Assistant => MessageRole::Assistant,
        genai::chat::ChatRole::Tool => MessageRole::Tool,
        _ => MessageRole::User,
    };

    let content_json = serde_json::to_value(&msg.chat_message).unwrap_or_default();
    let content_preview = msg.chat_message.content.joined_texts();

    pattern_db::models::Message {
        id: msg.id.to_string(),
        agent_id: agent_id.to_string(),
        position: msg.position.to_string(),
        batch_id: Some(msg.batch.to_string()),
        sequence_in_batch: Some(0),
        role,
        content_json: pattern_db::Json(content_json),
        content_preview,
        batch_type: Some(BatchType::UserRequest),
        source: None,
        source_metadata: None,
        attachments_json: None,
        origin_json: None,
        is_archived: false,
        is_deleted: false,
        // pattern_core::Message.created_at is already jiff::Timestamp; store directly.
        created_at: msg.created_at,
    }
}

/// Build a TurnHistory with `archived_count` real turns (with messages) followed
/// by exactly one empty-messages turn (the "first kept" turn). Used to exercise
/// the `compute_archive_boundary` edge case.
async fn populate_history_with_empty_kept_turn(
    db: &pattern_db::ConstellationDb,
    agent_id: &str,
    archived_count: usize,
) -> Arc<std::sync::Mutex<TurnHistory>> {
    let mut hist = TurnHistory::empty();

    // Add `archived_count` real turns (these will be the ones archivied by
    // the strategy).
    for i in 0..archived_count {
        let batch_id: BatchId = new_snowflake_id();
        let turn_id = new_snowflake_id();

        let user_msg = Message {
            chat_message: genai::chat::ChatMessage::user(format!("user {i}")),
            id: MessageId::from(new_id()),
            position: new_snowflake_id(),
            owner_id: AgentId::from(agent_id),
            created_at: Timestamp::now(),
            batch: batch_id.clone(),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        };

        let db_user = to_db_message(&user_msg, agent_id);
        pattern_db::queries::create_message(&db.get().unwrap(), &db_user).expect("create_message");

        let input = TurnInput {
            turn_id: turn_id.clone(),
            batch_id,
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![user_msg],
        };
        let output = TurnOutput {
            messages: vec![],
            block_writes: vec![],
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: None,
            cache_metrics: Default::default(),
            completed_at: Timestamp::now(),
        };
        hist.record(turn_id, input, output);
    }

    // The "first kept" turn: no messages in either input or output.
    let batch_id: BatchId = new_snowflake_id();
    let turn_id = new_snowflake_id();
    let input_empty = TurnInput {
        turn_id: turn_id.clone(),
        batch_id,
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![], // empty — the edge case
    };
    let output_empty = TurnOutput {
        messages: vec![], // empty — the edge case
        block_writes: vec![],
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
        usage: None,
        cache_metrics: Default::default(),
        completed_at: Timestamp::now(),
    };
    hist.record(turn_id, input_empty, output_empty);

    Arc::new(std::sync::Mutex::new(hist))
}

// ---- tests ------------------------------------------------------------------

#[tokio::test]
async fn gate_skipped_below_message_floor() {
    let provider = Arc::new(MockProviderClient::with_turns(vec![]));
    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::Truncate { keep_recent: 5 }))
            .with_message_floor(100)
            .with_token_threshold(1),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;

    // 3 turns: well below message_floor=100.
    let hist = populate_history(&db, "agent-a", 3).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Skipped {
            reason,
            active_turns,
            ..
        } => {
            assert_eq!(reason, "below message floor");
            assert_eq!(active_turns, 3);
        }
        CompactionOutcome::Fired { .. } => panic!("expected Skipped, got Fired"),
    }
}

#[tokio::test]
async fn gate_skipped_compression_disabled() {
    let provider = Arc::new(MockProviderClient::with_turns(vec![]));
    let persona = PersonaSnapshot::new("agent-a", "Test")
        .with_context_policy(ContextPolicy::default().with_compression(None));
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 5).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Skipped { reason, .. } => {
            assert_eq!(reason, "compression disabled");
        }
        CompactionOutcome::Fired { .. } => panic!("expected Skipped, got Fired"),
    }
}

#[tokio::test]
async fn gate_skipped_below_token_threshold() {
    // 200 turns, but token_count returns 50 (below threshold of 1000).
    let provider = Arc::new(MockProviderClient::with_turns(vec![]).with_token_count(50));
    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::Truncate { keep_recent: 50 }))
            .with_message_floor(0)
            .with_token_threshold(1000),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 200).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Skipped { reason, .. } => {
            assert_eq!(reason, "below token threshold");
        }
        CompactionOutcome::Fired { .. } => panic!("expected Skipped, got Fired"),
    }
}

#[tokio::test]
async fn truncate_strategy_fires_and_drops_old_turns() {
    // 200 turns, token_count above threshold, Truncate(keep_recent=50).
    let provider = Arc::new(MockProviderClient::with_turns(vec![]).with_token_count(5000));
    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::Truncate { keep_recent: 50 }))
            .with_message_floor(0)
            .with_token_threshold(100),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 200).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Fired {
            strategy_name,
            archived_turn_count,
            summary_written,
            active_after,
        } => {
            assert_eq!(strategy_name, "truncate");
            assert_eq!(archived_turn_count, 150);
            assert!(!summary_written);
            assert_eq!(active_after, 50);
        }
        CompactionOutcome::Skipped { reason, .. } => {
            panic!("expected Fired, got Skipped: {reason}");
        }
    }

    // Verify TurnHistory was updated.
    {
        let h = hist.lock().unwrap();
        assert_eq!(h.active_len(), 50);
        assert!(h.post_compaction_pending());
    }

    // Verify no archive_summaries row was created.
    let summaries =
        pattern_db::queries::get_archive_summaries(&db.get().unwrap(), "agent-a").unwrap();
    assert!(summaries.is_empty(), "truncate should not create summaries");
}

#[tokio::test]
async fn recursive_summarization_fires_and_writes_summary() {
    // The mock provider needs to handle:
    // 1. count_tokens (gate check) — returns high count via with_token_count
    // 2. complete (summarization) — scripted text response
    let provider = Arc::new(
        MockProviderClient::with_turns(vec![MockProviderClient::text_turn(
            "This is a compact summary of the conversation.",
        )])
        .with_token_count(5000),
    );

    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::RecursiveSummarization {
                chunk_size: 20,
                summarization_model: "claude-haiku-4-5".to_string(),
                summarization_prompt: None,
            }))
            .with_message_floor(0)
            .with_token_threshold(100),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 200).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Fired {
            strategy_name,
            archived_turn_count,
            summary_written,
            active_after,
        } => {
            assert_eq!(strategy_name, "recursive_summarization");
            assert_eq!(archived_turn_count, 20);
            assert!(summary_written);
            assert_eq!(active_after, 180);
        }
        CompactionOutcome::Skipped { reason, .. } => {
            panic!("expected Fired, got Skipped: {reason}");
        }
    }

    // Verify TurnHistory was updated.
    {
        let h = hist.lock().unwrap();
        assert_eq!(h.active_len(), 180);
        assert!(h.post_compaction_pending());
    }

    // Verify archive_summaries row was created.
    let summaries =
        pattern_db::queries::get_archive_summaries(&db.get().unwrap(), "agent-a").unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].depth, 0);
    assert!(
        summaries[0].summary.contains("compact summary"),
        "summary text should be from the mock provider: {}",
        summaries[0].summary
    );

    // Verify summary_head was reloaded.
    {
        let h = hist.lock().unwrap();
        assert_eq!(h.summary_head().len(), 1);
        assert_eq!(h.summary_head()[0].depth, 0);
    }
}

#[tokio::test]
async fn importance_based_strategy_fires_and_drops_old_turns() {
    // 200 turns, token_count above threshold, ImportanceBased(keep_recent=20,
    // keep_important=10). Expected: 200 - 20 = 180 older turns scored; top-10
    // kept; 170 archived; 30 active (10 important + 20 recent).
    let provider = Arc::new(MockProviderClient::with_turns(vec![]).with_token_count(5000));
    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::ImportanceBased {
                keep_recent: 20,
                keep_important: 10,
            }))
            .with_message_floor(0)
            .with_token_threshold(100),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 200).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Fired {
            strategy_name,
            archived_turn_count,
            summary_written,
            active_after,
        } => {
            assert_eq!(strategy_name, "importance_based");
            // 180 older turns scored; top-10 kept; 170 archived.
            assert_eq!(archived_turn_count, 170);
            // ImportanceBased does not write a summary row.
            assert!(!summary_written);
            // 10 important + 20 recent = 30 active.
            assert_eq!(active_after, 30);
        }
        CompactionOutcome::Skipped { reason, .. } => {
            panic!("expected Fired, got Skipped: {reason}");
        }
    }

    // Verify TurnHistory was updated.
    {
        let h = hist.lock().unwrap();
        assert_eq!(h.active_len(), 30);
        assert!(h.post_compaction_pending());
    }

    // ImportanceBased does not write archive_summaries rows.
    let summaries =
        pattern_db::queries::get_archive_summaries(&db.get().unwrap(), "agent-a").unwrap();
    assert!(
        summaries.is_empty(),
        "importance_based should not create summary rows"
    );
}

#[tokio::test]
async fn time_decay_strategy_fires_and_drops_old_turns() {
    // 200 turns, token_count above threshold, TimeDecay with a negative
    // compress_after_hours so the computed cutoff is in the future — all
    // turns are considered "old" and eligible for archival.
    //
    // Why a negative cutoff instead of past-dated test messages?
    // `populate_history` stamps every turn with `Timestamp::now()` at fixture
    // build time; making them look "old" by wall-clock would require either
    // sleeping between turns or manually rewriting per-message timestamps —
    // both more fragile than just pulling the cutoff forward so every
    // now()-stamped turn lands on the "old" side of it. Intentional fixture
    // trick, not a typo.
    //
    // With compress_after_hours = -1.0:
    //   cutoff = Timestamp::now() - (-3_600_000ms) = 1 hour in the future
    //   old_count = 200 (all turns pre-date a future cutoff)
    //   max_archivable = 200 - min_keep_recent(2) = 198
    //   desired_cut = min(200, 198) = 198
    //   safe_cut = 198 (every turn has a unique batch_id → no boundary pull-back)
    // Expected: 198 archived, 2 active.
    let provider = Arc::new(MockProviderClient::with_turns(vec![]).with_token_count(5000));
    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::TimeDecay {
                compress_after_hours: -1.0,
                min_keep_recent: 2,
            }))
            .with_message_floor(0)
            .with_token_threshold(100),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 200).await;

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    match outcome {
        CompactionOutcome::Fired {
            strategy_name,
            archived_turn_count,
            summary_written,
            active_after,
        } => {
            assert_eq!(strategy_name, "time_decay");
            assert_eq!(archived_turn_count, 198);
            // TimeDecay does not write a summary row.
            assert!(!summary_written);
            assert_eq!(active_after, 2);
        }
        CompactionOutcome::Skipped { reason, .. } => {
            panic!("expected Fired, got Skipped: {reason}");
        }
    }

    // Verify TurnHistory was updated.
    {
        let h = hist.lock().unwrap();
        assert_eq!(h.active_len(), 2);
        assert!(h.post_compaction_pending());
    }

    // TimeDecay does not write archive_summaries rows.
    let summaries =
        pattern_db::queries::get_archive_summaries(&db.get().unwrap(), "agent-a").unwrap();
    assert!(
        summaries.is_empty(),
        "time_decay should not create summary rows"
    );
}

#[tokio::test]
async fn archived_messages_marked_is_archived() {
    let provider = Arc::new(MockProviderClient::with_turns(vec![]).with_token_count(5000));
    let persona = PersonaSnapshot::new("agent-a", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::Truncate { keep_recent: 5 }))
            .with_message_floor(0)
            .with_token_threshold(100),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-a", 10).await;

    // Before compaction: all 20 messages (10 turns * 2 msgs) are non-archived.
    let non_archived =
        pattern_db::queries::get_messages(&db.get().unwrap(), "agent-a", i64::MAX).unwrap();
    assert_eq!(non_archived.len(), 20);

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    assert!(matches!(outcome, CompactionOutcome::Fired { .. }));

    // After compaction: only the kept messages should be non-archived.
    let non_archived_after =
        pattern_db::queries::get_messages(&db.get().unwrap(), "agent-a", i64::MAX).unwrap();
    // 5 kept turns * 2 messages = 10 non-archived.
    assert_eq!(
        non_archived_after.len(),
        10,
        "should have 10 non-archived messages (5 kept turns * 2 msgs)"
    );

    // The archived messages should be visible with get_messages_with_archived.
    let all_messages =
        pattern_db::queries::get_messages_with_archived(&db.get().unwrap(), "agent-a", i64::MAX)
            .unwrap();
    assert_eq!(all_messages.len(), 20, "total messages should be unchanged");

    // Count archived ones.
    let archived_count = all_messages.iter().filter(|m| m.is_archived).count();
    assert_eq!(
        archived_count, 10,
        "should have 10 archived messages (5 archived turns * 2 msgs)"
    );
}

/// Regression test for `compute_archive_boundary` edge case: the first kept
/// turn (at `archived_count`) has empty `input.messages` and `output.messages`.
///
/// Before the fix, `min_pos = None` caused a silent fall-through to
/// `Ok(String::new())`, meaning `archive_messages` matched no rows (position
/// < "" is never true), compaction reported success but left the context
/// window unaffected, and the token estimate never decreased.
///
/// After the fix, the function either falls through to the "archive-all"
/// fallback branch (returning a boundary beyond the last archived turn's
/// messages) or returns `CompactionInternalError` when all turns are empty.
/// In this test the archived turns DO have messages, so the fallback branch
/// fires and we get a non-empty boundary.
#[tokio::test]
async fn compute_archive_boundary_empty_kept_turn_uses_fallback() {
    let db = test_db().await;
    let agent_id = "agent-boundary-edge";
    create_test_agent(&db, agent_id).await;

    // Build a history: 5 real turns (with messages) + 1 empty-messages turn.
    // With Truncate { keep_recent: 1 }, archived_count = 5.
    let archived_count = 5;
    let hist = populate_history_with_empty_kept_turn(&db, agent_id, archived_count).await;

    let provider = Arc::new(
        MockProviderClient::with_turns(vec![]).with_token_count(200_000), // force gate to fire
    );
    let persona = PersonaSnapshot::new(agent_id, "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::Truncate { keep_recent: 1 }))
            .with_message_floor(1)
            .with_token_threshold(1),
    );

    let (ctx, _db) = setup_with_persona(persona, provider).await;
    // Provide the already-constructed history so we control the exact shape.
    let result = maybe_compact(&ctx, &hist, ctx.context_policy()).await;

    // The outcome must not be the silent-skip case: either it fired (fallback
    // boundary was non-empty so archive_messages ran) or it returned an
    // explicit CompactionInternalError (all fallback branches exhausted).
    // Either is correct — what must NOT happen is a silent `Ok(Skipped)` with
    // reason "strategy archived zero turns" due to a `""` boundary.
    match result {
        Ok(CompactionOutcome::Fired {
            archived_turn_count,
            ..
        }) => {
            // Fallback branch fired. The 5 real turns got archived.
            assert_eq!(
                archived_turn_count, archived_count,
                "fallback boundary should archive all {archived_count} real turns"
            );
        }
        Err(pattern_core::error::RuntimeError::CompactionInternalError { .. }) => {
            // Also acceptable: explicit error (all fallback branches exhausted).
        }
        Ok(CompactionOutcome::Skipped { reason, .. }) => {
            panic!(
                "expected Fired or CompactionInternalError, got Skipped: {reason}; \
                 the empty-kept-turn bug would produce 'strategy archived zero turns'"
            );
        }
        Err(e) => {
            panic!("unexpected error: {e:?}");
        }
    }
}

/// When compaction fires, `maybe_compact` must call `rotate_session_uuid` on the
/// provider client. This signals the compaction-cycle boundary to the provider
/// so it sees a fresh session UUID rather than the pre-compaction UUID.
///
/// Regression test for fix #1 (code-review finding: SessionUuidRotator.rotate
/// not wired into compaction trigger).
#[tokio::test]
async fn compaction_fired_rotates_session_uuid() {
    // Use a token count high enough to trigger the gate, with Truncate strategy
    // so we exercise the full Fired path without requiring a provider
    // complete() call.
    let provider = Arc::new(MockProviderClient::with_turns(vec![]).with_token_count(5000));
    // Keep a clone before setup_with_persona consumes the Arc.
    let provider_ref = provider.clone();

    let persona = PersonaSnapshot::new("agent-uuid-rotate", "Test").with_context_policy(
        ContextPolicy::default()
            .with_compression(Some(CompressionStrategy::Truncate { keep_recent: 50 }))
            .with_message_floor(0)
            .with_token_threshold(100),
    );
    let (ctx, db) = setup_with_persona(persona, provider).await;
    let hist = populate_history(&db, "agent-uuid-rotate", 200).await;

    assert_eq!(
        provider_ref.rotate_count(),
        0,
        "rotate_count must be 0 before compaction"
    );

    let outcome = maybe_compact(&ctx, &hist, ctx.context_policy())
        .await
        .expect("maybe_compact failed");

    // Confirm compaction fired (not skipped).
    assert!(
        matches!(outcome, CompactionOutcome::Fired { .. }),
        "expected Fired outcome to exercise rotation path"
    );

    assert_eq!(
        provider_ref.rotate_count(),
        1,
        "rotate_session_uuid should have been called exactly once after Fired"
    );
}
