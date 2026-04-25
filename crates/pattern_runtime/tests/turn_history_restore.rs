//! Integration tests for `TurnHistory::load` restoration from pattern_db.
//!
//! Exercises the DB → `TurnHistory` reconstruction path to verify that
//! re-spawning a session against the same database restores conversation
//! state correctly.

use std::sync::Arc;

use pattern_core::ProviderClient;
use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::TurnInput;

use pattern_runtime::agent_loop::{NoOpDispatcher, drive_step};
use pattern_runtime::memory::TurnHistory;
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

/// Build a SessionContext wired to a MockProviderClient.
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
    create_test_agent(&db, "agent-a").await;
    let sink: Arc<dyn TurnSink> = Arc::new(VecSink::new());
    let persona = PersonaSnapshot::new("agent-a", "Test Agent");
    let ctx = Arc::new(
        SessionContext::from_persona(
            &persona,
            store,
            provider,
            db.clone(),
            tokio::runtime::Handle::current(),
        )
        .with_turn_sink(sink),
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

// ---- Tests ----------------------------------------------------------------

#[tokio::test]
async fn load_empty_db_returns_empty_history() {
    let db = test_db().await;
    create_test_agent(&db, "agent-a").await;

    let history = TurnHistory::load(&db, "agent-a")
        .await
        .expect("load should succeed on empty DB");

    assert_eq!(history.active_len(), 0, "no active turns");
    assert!(history.summary_head().is_empty(), "no summary head");
    assert_eq!(history.estimated_tokens(), 0, "no tokens");
    assert!(
        history.most_recent_batch_id().is_none(),
        "no batch id on empty"
    );
}

#[tokio::test]
async fn load_single_turn_restores_one_record() {
    let (ctx, db, _provider) = setup(vec![MockProviderClient::text_turn("Hello there!")]).await;

    let batch = BatchId::from(new_snowflake_id());
    let input = user_input("Hi", &batch);
    let history = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));

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

    assert_eq!(reply.turns.len(), 1, "one wire turn");

    // Reload from DB.
    let restored = TurnHistory::load(&db, "agent-a")
        .await
        .expect("load should succeed");

    assert_eq!(
        restored.active_len(),
        1,
        "should restore exactly 1 TurnRecord"
    );

    // Verify the restored messages have correct roles.
    let msgs: Vec<_> = restored.active_messages().collect();
    assert_eq!(msgs.len(), 2, "1 user input + 1 assistant output");

    assert_eq!(
        msgs[0].chat_message.role,
        genai::chat::ChatRole::User,
        "first message is user"
    );
    assert_eq!(
        msgs[1].chat_message.role,
        genai::chat::ChatRole::Assistant,
        "second message is assistant"
    );

    // Verify batch_id is tracked.
    assert!(
        restored.most_recent_batch_id().is_some(),
        "batch_id should be set"
    );

    // Verify estimated tokens is nonzero.
    assert!(
        restored.estimated_tokens() > 0,
        "estimated tokens should be positive"
    );
}

#[tokio::test]
async fn load_tool_use_turn_restores_two_records() {
    use async_trait::async_trait;
    use pattern_core::types::provider::{ToolCall, ToolOutcome};
    use pattern_runtime::agent_loop::EvalDispatcher;

    #[derive(Debug)]
    struct SuccessDispatcher;

    #[async_trait]
    impl EvalDispatcher for SuccessDispatcher {
        async fn dispatch(&self, _tool_call: ToolCall, _preamble: &str) -> ToolOutcome {
            ToolOutcome::Success(serde_json::json!({"ok": true}))
        }
    }

    let (ctx, db, _provider) = setup(vec![
        // Wire turn 1: tool_use.
        MockProviderClient::tool_use_turn(
            "toolu_01",
            "code",
            serde_json::json!({"code": "pure ()"}),
        ),
        // Wire turn 2: final text.
        MockProviderClient::text_turn("Done."),
    ])
    .await;

    let batch = BatchId::from(new_snowflake_id());
    let input = user_input("run something", &batch);
    let history = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));

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

    assert_eq!(reply.turns.len(), 2, "two wire turns");

    // Reload from DB.
    let restored = TurnHistory::load(&db, "agent-a")
        .await
        .expect("load should succeed");

    // Should have 2 TurnRecords:
    // Record 1: input=[user], output=[assistant(tool_use), tool_result]
    // Record 2: input=[], output=[assistant(final)]
    assert_eq!(
        restored.active_len(),
        2,
        "should restore 2 TurnRecords for tool-use batch"
    );

    let records: Vec<_> = restored.iter_active().collect();

    // First record should have user input and tool-use output.
    assert!(
        !records[0].input.messages.is_empty(),
        "first record has user input"
    );
    assert!(
        records[0]
            .output
            .messages
            .iter()
            .any(|m| m.chat_message.role == genai::chat::ChatRole::Tool),
        "first record output contains tool_result"
    );
    assert_eq!(
        records[0].output.stop_reason,
        pattern_core::types::turn::StopReason::ToolUse,
        "first record stop_reason is ToolUse"
    );

    // Second record should be a continuation (empty input).
    assert!(
        records[1].input.messages.is_empty(),
        "second record is continuation (empty input)"
    );
    assert_eq!(
        records[1].output.stop_reason,
        pattern_core::types::turn::StopReason::EndTurn,
        "second record stop_reason is EndTurn"
    );
}

#[tokio::test]
async fn load_preserves_active_messages_order() {
    let (ctx, db, _provider) = setup(vec![
        MockProviderClient::text_turn("First response"),
        MockProviderClient::text_turn("Second response"),
    ])
    .await;

    let history = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));

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
        history.clone(),
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("step 2 should succeed");

    // Capture the original message order from the live history.
    let original_positions: Vec<String> = {
        let guard = history.lock().unwrap();
        guard
            .active_messages()
            .map(|m| m.position.to_string())
            .collect()
    };

    // Reload from DB.
    let restored = TurnHistory::load(&db, "agent-a")
        .await
        .expect("load should succeed");

    let restored_positions: Vec<String> = restored
        .active_messages()
        .map(|m| m.position.to_string())
        .collect();

    assert_eq!(
        original_positions.len(),
        restored_positions.len(),
        "same number of messages"
    );

    // Verify positions are in ascending order (lex-sorted).
    let mut sorted = restored_positions.clone();
    sorted.sort();
    assert_eq!(
        restored_positions, sorted,
        "restored messages should be in ascending position order"
    );

    // Verify the positions match the original order.
    assert_eq!(
        original_positions, restored_positions,
        "restored positions should match original order"
    );
}

#[tokio::test]
async fn load_excludes_archived_messages() {
    let (ctx, db, _provider) = setup(vec![
        MockProviderClient::text_turn("First response"),
        MockProviderClient::text_turn("Second response"),
    ])
    .await;

    let history = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));

    // Drive two exchanges.
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

    let batch2 = BatchId::from(new_snowflake_id());
    let input2 = user_input("question two", &batch2);
    let _reply2 = drive_step(
        input2,
        ctx.clone(),
        history.clone(),
        pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        &NoOpDispatcher,
        "",
    )
    .await
    .expect("step 2 should succeed");

    // Count total messages before archiving.
    let all_msgs =
        pattern_db::queries::get_messages_with_archived(&db.get().unwrap(), "agent-a", 1000)
            .expect("query should succeed");
    assert_eq!(all_msgs.len(), 4, "4 total messages before archiving");

    // Archive the first batch's messages. Find the highest position in
    // batch1 and archive everything at or before it.
    let batch1_msgs: Vec<_> = all_msgs
        .iter()
        .filter(|m| m.batch_id.as_deref() == Some(batch1.as_str()))
        .collect();
    assert!(!batch1_msgs.is_empty(), "batch1 messages should exist");
    // Find position just past the last batch1 message to archive them.
    // batch1 messages have lower positions than batch2, so we use
    // a position between batch1's last and batch2's first.
    let mut positions: Vec<&str> = batch1_msgs.iter().map(|m| m.position.as_str()).collect();
    positions.sort();
    let archive_before = {
        // Find the minimum batch2 position.
        let batch2_positions: Vec<&str> = all_msgs
            .iter()
            .filter(|m| m.batch_id.as_deref() == Some(batch2.as_str()))
            .map(|m| m.position.as_str())
            .collect();
        let min_batch2 = batch2_positions.iter().min().unwrap();
        min_batch2.to_string()
    };
    let archived_count =
        pattern_db::queries::archive_messages(&db.get().unwrap(), "agent-a", &archive_before)
            .expect("archive should succeed");
    assert_eq!(archived_count, 2, "should archive 2 messages from batch1");

    // Reload — only batch2 messages should appear.
    let restored = TurnHistory::load(&db, "agent-a")
        .await
        .expect("load should succeed");

    assert_eq!(
        restored.active_len(),
        1,
        "only one TurnRecord should be restored (batch2)"
    );

    let msgs: Vec<_> = restored.active_messages().collect();
    assert_eq!(msgs.len(), 2, "2 messages from batch2 (user + assistant)");

    // Verify the remaining messages belong to batch2.
    for msg in &msgs {
        assert_eq!(
            msg.batch.as_str(),
            batch2.as_str(),
            "remaining messages should be from batch2"
        );
    }
}
