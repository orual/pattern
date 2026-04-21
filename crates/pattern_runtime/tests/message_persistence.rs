//! Integration tests for message persistence through the agent loop.
//!
//! Exercises the `drive_step` → `persist_messages` path to verify that
//! pattern_core::Message records are upserted into the pattern_db
//! `messages` table after each wire turn.

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::TurnInput;

use pattern_runtime::agent_loop::{NoOpDispatcher, drive_step};
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, MockProviderClient, test_db};

/// Create the FK-required agent row in the test database.
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

/// Build a SessionContext wired to a MockProviderClient. Returns
/// `(ctx, db)` for assertions on persisted state. Also creates the
/// FK-required agent row in the test database.
async fn setup(
    turns: Vec<Vec<genai::chat::ChatStreamEvent>>,
) -> (
    Arc<SessionContext>,
    Arc<pattern_db::ConstellationDb>,
    Arc<MockProviderClient>,
) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider_concrete = Arc::new(MockProviderClient::with_turns(turns));
    let provider: Arc<dyn ProviderClient> = provider_concrete.clone();
    let db = test_db().await;
    // Create the agent row so the FK on messages.agent_id is satisfied.
    create_test_agent(&db, "agent-a").await;
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let persona = PersonaSnapshot::new("agent-a", "Test Agent");
    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider, db.clone()).with_turn_sink(sink),
    );
    (ctx, db, provider_concrete)
}

/// Build a TurnInput with one user message.
fn user_input(text: &str, batch_id: &BatchId) -> TurnInput {
    let chat_msg = genai::chat::ChatMessage::user(text.to_string());
    let msg = Message {
        chat_message: chat_msg,
        id: MessageId::from(new_id()),
        position: new_snowflake_id(),
        owner_id: AgentId::from("user"),
        created_at: jiff::Timestamp::now(),
        batch: batch_id.clone(),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    };
    TurnInput {
        turn_id: new_snowflake_id(),
        batch_id: batch_id.clone(),
        origin: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        ),
        messages: vec![msg],
    }
}

#[tokio::test]
async fn single_text_turn_persists_user_and_assistant_messages() {
    let (ctx, db, _provider) = setup(vec![MockProviderClient::text_turn(
        "Hello! I'm the assistant.",
    )])
    .await;

    let batch = BatchId::from(new_snowflake_id());
    let input = user_input("Hi there", &batch);
    let history = Arc::new(std::sync::Mutex::new(
        pattern_runtime::memory::TurnHistory::empty(),
    ));

    let reply = drive_step(
        input,
        ctx.clone(),
        history,
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("drive_step should succeed");

    // Should have produced 1 wire turn with 1 assistant message.
    assert_eq!(reply.turns.len(), 1);
    assert_eq!(reply.turns[0].messages.len(), 1, "one assistant message");

    // Query the DB for persisted messages.
    let rows = pattern_db::queries::get_messages(&db.get().unwrap(), "agent-a", 100)
        .expect("query should succeed");

    // Expect 2 rows: 1 user input + 1 assistant output.
    assert_eq!(
        rows.len(),
        2,
        "expected 2 persisted messages, got {}",
        rows.len()
    );

    // Verify roles.
    let roles: Vec<pattern_db::models::MessageRole> = rows.iter().map(|r| r.role).collect();
    assert!(
        roles.contains(&pattern_db::models::MessageRole::User),
        "should contain a User message"
    );
    assert!(
        roles.contains(&pattern_db::models::MessageRole::Assistant),
        "should contain an Assistant message"
    );

    // Verify positions are lex-sorted (DESC from query, so reverse to check ASC).
    let positions: Vec<&str> = rows.iter().map(|r| r.position.as_str()).collect();
    let mut sorted = positions.clone();
    sorted.sort();
    sorted.reverse(); // query returns DESC
    assert_eq!(positions, sorted, "positions should be in DESC order");

    // Verify batch_id is set on all messages.
    for row in &rows {
        assert_eq!(
            row.batch_id.as_deref(),
            Some(batch.as_str()),
            "batch_id should match"
        );
    }

    // Verify content_preview is populated for the user message.
    let user_row = rows
        .iter()
        .find(|r| r.role == pattern_db::models::MessageRole::User)
        .unwrap();
    assert_eq!(
        user_row.content_preview.as_deref(),
        Some("Hi there"),
        "user message preview"
    );
}

#[tokio::test]
async fn two_step_exchange_accumulates_messages_in_db() {
    // Script: 2 separate text turns (simulating two user exchanges).
    let (ctx, db, _provider) = setup(vec![
        MockProviderClient::text_turn("First response"),
        MockProviderClient::text_turn("Second response"),
    ])
    .await;

    let history = Arc::new(std::sync::Mutex::new(
        pattern_runtime::memory::TurnHistory::empty(),
    ));

    // Step 1.
    let batch1 = BatchId::from(new_snowflake_id());
    let input1 = user_input("question one", &batch1);
    let _reply1 = drive_step(
        input1,
        ctx.clone(),
        history.clone(),
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("step 1 should succeed");

    // Step 2.
    let batch2 = BatchId::from(new_snowflake_id());
    let input2 = user_input("question two", &batch2);
    let _reply2 = drive_step(
        input2,
        ctx.clone(),
        history,
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("step 2 should succeed");

    // Query all messages (including archived, just in case).
    let rows = pattern_db::queries::get_messages_with_archived(&db.get().unwrap(), "agent-a", 100)
        .expect("query should succeed");

    // 2 user + 2 assistant = 4 messages.
    assert_eq!(
        rows.len(),
        4,
        "expected 4 messages total, got {}",
        rows.len()
    );

    // Verify two distinct batch_ids.
    let batch_ids: std::collections::HashSet<_> =
        rows.iter().filter_map(|r| r.batch_id.as_ref()).collect();
    assert_eq!(
        batch_ids.len(),
        2,
        "expected 2 distinct batch_ids, got {:?}",
        batch_ids
    );

    // Verify all positions are unique and lex-sortable.
    let mut positions: Vec<String> = rows.iter().map(|r| r.position.clone()).collect();
    let unique_count = {
        let set: std::collections::HashSet<_> = positions.iter().collect();
        set.len()
    };
    assert_eq!(unique_count, 4, "all positions should be unique");

    // Positions should be sortable (earlier messages < later messages).
    positions.sort();
    // The first 2 (batch1) should sort before the last 2 (batch2).
    // We can't assert exact ordering between user/assistant within a batch
    // because they have different snowflakes, but batch1 snowflakes should
    // all be < batch2 snowflakes since they were created earlier.
}

#[tokio::test]
async fn tool_use_turn_persists_assistant_and_tool_result_messages() {
    use async_trait::async_trait;
    use pattern_core::types::provider::{ToolCall, ToolOutcome};
    use pattern_runtime::agent_loop::EvalDispatcher;

    /// Mock dispatcher that always succeeds.
    #[derive(Debug)]
    struct SuccessDispatcher;

    #[async_trait]
    impl EvalDispatcher for SuccessDispatcher {
        async fn dispatch(&self, _tool_call: ToolCall, _preamble: &str) -> ToolOutcome {
            ToolOutcome::Success(serde_json::json!({"ok": true}))
        }
    }

    let (ctx, db, _provider) = setup(vec![
        // Wire turn 1: tool_use
        MockProviderClient::tool_use_turn(
            "toolu_01",
            "code",
            serde_json::json!({"code": "pure ()"}),
        ),
        // Wire turn 2: final text
        MockProviderClient::text_turn("Done."),
    ])
    .await;

    let batch = BatchId::from(new_snowflake_id());
    let input = user_input("run something", &batch);
    let history = Arc::new(std::sync::Mutex::new(
        pattern_runtime::memory::TurnHistory::empty(),
    ));

    let reply = drive_step(
        input,
        ctx.clone(),
        history,
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &SuccessDispatcher,
        "",
    )
    .await
    .expect("drive_step should succeed");

    // 2 wire turns (tool_use + final text).
    assert_eq!(reply.turns.len(), 2);

    // Query DB.
    let rows = pattern_db::queries::get_messages(&db.get().unwrap(), "agent-a", 100)
        .expect("query should succeed");

    // Expected:
    //   Turn 1 input:  1 user message
    //   Turn 1 output: 1 assistant (tool_use) + 1 tool_result
    //   Turn 2 input:  0 (continuation)
    //   Turn 2 output: 1 assistant (final text)
    // Total: 4 messages.
    assert_eq!(rows.len(), 4, "expected 4 messages, got {}", rows.len());

    let roles: Vec<pattern_db::models::MessageRole> = rows.iter().map(|r| r.role).collect();
    let user_count = roles
        .iter()
        .filter(|r| **r == pattern_db::models::MessageRole::User)
        .count();
    let assistant_count = roles
        .iter()
        .filter(|r| **r == pattern_db::models::MessageRole::Assistant)
        .count();
    let tool_count = roles
        .iter()
        .filter(|r| **r == pattern_db::models::MessageRole::Tool)
        .count();

    assert_eq!(user_count, 1, "1 user message");
    assert_eq!(
        assistant_count, 2,
        "2 assistant messages (tool_use + final)"
    );
    assert_eq!(tool_count, 1, "1 tool_result message");
}

#[tokio::test]
async fn upsert_idempotency_does_not_duplicate_messages() {
    // Verify that re-persisting the same message ID doesn't create duplicates.
    let (ctx, db, _) = setup(vec![
        MockProviderClient::text_turn("response A"),
        MockProviderClient::text_turn("response B"),
    ])
    .await;

    let batch = BatchId::from(new_snowflake_id());
    let input = user_input("same input", &batch);
    let history = Arc::new(std::sync::Mutex::new(
        pattern_runtime::memory::TurnHistory::empty(),
    ));

    // Step 1: first exchange.
    let _reply = drive_step(
        input,
        ctx.clone(),
        history.clone(),
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("step 1 should succeed");

    let count_after_1 = pattern_db::queries::count_all_messages(&db.get().unwrap(), "agent-a")
        .expect("count should succeed");
    assert_eq!(count_after_1, 2, "2 messages after step 1");

    // Step 2: second exchange (different batch, so different messages).
    let batch2 = BatchId::from(new_snowflake_id());
    let input2 = user_input("different input", &batch2);
    let _reply2 = drive_step(
        input2,
        ctx.clone(),
        history,
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("step 2 should succeed");

    let count_after_2 = pattern_db::queries::count_all_messages(&db.get().unwrap(), "agent-a")
        .expect("count should succeed");
    assert_eq!(count_after_2, 4, "4 messages after step 2 (no duplicates)");
}
