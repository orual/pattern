//! Integration tests for MessageStore and related functionality.
//!
//! These tests verify correct behavior against a real SQLite database,
//! ensuring that message storage, retrieval, batching, and content types
//! all work correctly in practice.

use super::*;
use crate::id::MessageId;
use crate::messages::{
    BatchType, ChatRole, ContentBlock, ContentPart, ImageSource, MessageContent, MessageMetadata,
    MessageOptions, ToolCall, ToolResponse,
};
use crate::utils::get_next_message_position_sync;
use pattern_db::ConstellationDb;
use pattern_db::models::{Agent, AgentStatus};
use sqlx::types::Json as SqlxJson;

/// Helper to create a test database
async fn test_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().await.unwrap()
}

/// Helper to create a test agent in the database
async fn create_test_agent(db: &ConstellationDb, id: &str) {
    let agent = Agent {
        id: id.to_string(),
        name: format!("Test Agent {}", id),
        description: None,
        model_provider: "anthropic".to_string(),
        model_name: "claude".to_string(),
        system_prompt: "test".to_string(),
        config: SqlxJson(serde_json::json!({})),
        enabled_tools: SqlxJson(vec![]),
        tool_rules: None,
        status: AgentStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    pattern_db::queries::create_agent(db.pool(), &agent)
        .await
        .unwrap();
}

// ============================================================================
// Basic MessageStore Operations
// ============================================================================

#[tokio::test]
async fn test_store_and_retrieve_text_message() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    // Create a message with text content
    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::User,
        owner_id: None,
        content: MessageContent::Text("Hello, world!".to_string()),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: false,
        word_count: 2,
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(0),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();

    // Store the message
    store.store(&msg).await.unwrap();

    // Retrieve it back
    let retrieved = store.get_message(&msg_id).await.unwrap();
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.id.0, msg_id);
    assert_eq!(retrieved.role, ChatRole::User);

    // Verify content
    match retrieved.content {
        MessageContent::Text(text) => {
            assert_eq!(text, "Hello, world!");
        }
        _ => panic!("Expected Text content"),
    }

    assert_eq!(retrieved.word_count, 2);
    assert!(!retrieved.has_tool_calls);
}

#[tokio::test]
async fn test_get_recent_messages() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    // Store multiple messages
    for i in 0..5 {
        let msg = Message {
            id: MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content: MessageContent::Text(format!("Message {}", i)),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: 2,
            created_at: chrono::Utc::now(),
            position: Some(get_next_message_position_sync()),
            batch: Some(get_next_message_position_sync()),
            sequence_num: Some(0),
            batch_type: Some(BatchType::UserRequest),
        };
        store.store(&msg).await.unwrap();
        // Small delay to ensure different positions
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    // Get recent messages
    let recent = store.get_recent(3).await.unwrap();
    assert_eq!(recent.len(), 3);

    // Should be ordered newest first
    // The most recent message should be "Message 4"
    if let MessageContent::Text(text) = &recent[0].content {
        assert_eq!(text, "Message 4");
    } else {
        panic!("Expected Text content");
    }
}

#[tokio::test]
async fn test_get_batch_messages() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let batch_id = get_next_message_position_sync();

    // Store messages in the same batch
    for i in 0..3 {
        let msg = Message {
            id: MessageId::generate(),
            role: if i % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            },
            owner_id: None,
            content: MessageContent::Text(format!("Batch message {}", i)),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: 3,
            created_at: chrono::Utc::now(),
            position: Some(get_next_message_position_sync()),
            batch: Some(batch_id),
            sequence_num: Some(i as u32),
            batch_type: Some(BatchType::UserRequest),
        };
        store.store(&msg).await.unwrap();
    }

    // Retrieve batch
    let batch_msgs = store.get_batch(&batch_id.to_string()).await.unwrap();
    assert_eq!(batch_msgs.len(), 3);

    // Should be ordered by sequence_num
    assert_eq!(batch_msgs[0].sequence_num, Some(0));
    assert_eq!(batch_msgs[1].sequence_num, Some(1));
    assert_eq!(batch_msgs[2].sequence_num, Some(2));
}

// ============================================================================
// Content Type Tests
// ============================================================================

#[tokio::test]
async fn test_tool_calls_roundtrip() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let tool_calls = vec![
        ToolCall {
            call_id: "call_1".to_string(),
            fn_name: "search".to_string(),
            fn_arguments: serde_json::json!({"query": "test"}),
        },
        ToolCall {
            call_id: "call_2".to_string(),
            fn_name: "recall".to_string(),
            fn_arguments: serde_json::json!({"operation": "read"}),
        },
    ];

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::Assistant,
        owner_id: None,
        content: MessageContent::ToolCalls(tool_calls.clone()),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: true,
        word_count: 1000, // Estimated
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(0),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Retrieve and verify
    let retrieved = store.get_message(&msg_id).await.unwrap().unwrap();
    assert!(retrieved.has_tool_calls);

    match retrieved.content {
        MessageContent::ToolCalls(calls) => {
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].call_id, "call_1");
            assert_eq!(calls[0].fn_name, "search");
            assert_eq!(calls[1].call_id, "call_2");
            assert_eq!(calls[1].fn_name, "recall");
        }
        _ => panic!("Expected ToolCalls content"),
    }
}

#[tokio::test]
async fn test_tool_responses_roundtrip() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let tool_responses = vec![
        ToolResponse {
            call_id: "call_1".to_string(),
            content: "Search results found".to_string(),
            is_error: None,
        },
        ToolResponse {
            call_id: "call_2".to_string(),
            content: "Error: not found".to_string(),
            is_error: Some(true),
        },
    ];

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::Tool,
        owner_id: None,
        content: MessageContent::ToolResponses(tool_responses.clone()),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: false,
        word_count: 6,
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(1),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Retrieve and verify
    let retrieved = store.get_message(&msg_id).await.unwrap().unwrap();
    assert_eq!(retrieved.role, ChatRole::Tool);

    match retrieved.content {
        MessageContent::ToolResponses(responses) => {
            assert_eq!(responses.len(), 2);
            assert_eq!(responses[0].call_id, "call_1");
            assert_eq!(responses[0].content, "Search results found");
            assert_eq!(responses[0].is_error, None);
            assert_eq!(responses[1].is_error, Some(true));
        }
        _ => panic!("Expected ToolResponses content"),
    }
}

#[tokio::test]
async fn test_blocks_content_roundtrip() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let blocks = vec![
        ContentBlock::Text {
            text: "Here's my thinking:".to_string(),
            thought_signature: None,
        },
        ContentBlock::Thinking {
            text: "Let me analyze this carefully...".to_string(),
            signature: Some("sig_123".to_string()),
        },
        ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "search".to_string(),
            input: serde_json::json!({"query": "test"}),
            thought_signature: None,
        },
    ];

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::Assistant,
        owner_id: None,
        content: MessageContent::Blocks(blocks.clone()),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: true, // Contains ToolUse block
        word_count: 100,
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(0),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Retrieve and verify
    let retrieved = store.get_message(&msg_id).await.unwrap().unwrap();
    assert!(retrieved.has_tool_calls);

    match retrieved.content {
        MessageContent::Blocks(blocks) => {
            assert_eq!(blocks.len(), 3);

            // Verify Text block
            match &blocks[0] {
                ContentBlock::Text { text, .. } => {
                    assert_eq!(text, "Here's my thinking:");
                }
                _ => panic!("Expected Text block"),
            }

            // Verify Thinking block
            match &blocks[1] {
                ContentBlock::Thinking { text, signature } => {
                    assert_eq!(text, "Let me analyze this carefully...");
                    assert_eq!(signature.as_deref(), Some("sig_123"));
                }
                _ => panic!("Expected Thinking block"),
            }

            // Verify ToolUse block
            match &blocks[2] {
                ContentBlock::ToolUse { id, name, .. } => {
                    assert_eq!(id, "toolu_1");
                    assert_eq!(name, "search");
                }
                _ => panic!("Expected ToolUse block"),
            }
        }
        _ => panic!("Expected Blocks content"),
    }
}

#[tokio::test]
async fn test_parts_content_roundtrip() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let parts = vec![
        ContentPart::Text("Check out this image:".to_string()),
        ContentPart::Image {
            content_type: "image/png".to_string(),
            source: ImageSource::Url("https://example.com/image.png".to_string()),
        },
        ContentPart::Text("What do you see?".to_string()),
    ];

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::User,
        owner_id: None,
        content: MessageContent::Parts(parts.clone()),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: false,
        word_count: 300, // Estimated (100 per part)
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(0),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Retrieve and verify
    let retrieved = store.get_message(&msg_id).await.unwrap().unwrap();

    match retrieved.content {
        MessageContent::Parts(parts) => {
            assert_eq!(parts.len(), 3);

            // Verify text parts
            match &parts[0] {
                ContentPart::Text(text) => {
                    assert_eq!(text, "Check out this image:");
                }
                _ => panic!("Expected Text part"),
            }

            // Verify image part
            match &parts[1] {
                ContentPart::Image {
                    content_type,
                    source,
                } => {
                    assert_eq!(content_type, "image/png");
                    match source {
                        ImageSource::Url(url) => {
                            assert_eq!(url, "https://example.com/image.png");
                        }
                        _ => panic!("Expected URL image source"),
                    }
                }
                _ => panic!("Expected Image part"),
            }
        }
        _ => panic!("Expected Parts content"),
    }
}

// ============================================================================
// Content Preview Extraction Tests
// ============================================================================

#[tokio::test]
async fn test_content_preview_text() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::User,
        owner_id: None,
        content: MessageContent::Text("This is searchable text".to_string()),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: false,
        word_count: 4,
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(0),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Query the database directly to check content_preview
    let db_msg = pattern_db::queries::get_message(store.pool(), &msg_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        db_msg.content_preview.as_deref(),
        Some("This is searchable text")
    );
}

#[tokio::test]
async fn test_content_preview_tool_responses() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let tool_responses = vec![
        ToolResponse {
            call_id: "call_1".to_string(),
            content: "First result".to_string(),
            is_error: None,
        },
        ToolResponse {
            call_id: "call_2".to_string(),
            content: "Second result".to_string(),
            is_error: None,
        },
    ];

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::Tool,
        owner_id: None,
        content: MessageContent::ToolResponses(tool_responses),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: false,
        word_count: 4,
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(1),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Query the database directly to check content_preview
    let db_msg = pattern_db::queries::get_message(store.pool(), &msg_id)
        .await
        .unwrap()
        .unwrap();

    // Should combine both responses
    assert!(db_msg.content_preview.is_some());
    let preview = db_msg.content_preview.unwrap();
    assert!(preview.contains("First result"));
    assert!(preview.contains("Second result"));
}

#[tokio::test]
async fn test_content_preview_blocks() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let blocks = vec![
        ContentBlock::Text {
            text: "Text block content".to_string(),
            thought_signature: None,
        },
        ContentBlock::Thinking {
            text: "Thinking block content".to_string(),
            signature: None,
        },
        ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "search".to_string(),
            input: serde_json::json!({}),
            thought_signature: None,
        },
    ];

    let msg = Message {
        id: MessageId::generate(),
        role: ChatRole::Assistant,
        owner_id: None,
        content: MessageContent::Blocks(blocks),
        metadata: MessageMetadata::default(),
        options: MessageOptions::default(),
        has_tool_calls: true,
        word_count: 100,
        created_at: chrono::Utc::now(),
        position: Some(get_next_message_position_sync()),
        batch: Some(get_next_message_position_sync()),
        sequence_num: Some(0),
        batch_type: Some(BatchType::UserRequest),
    };

    let msg_id = msg.id.0.clone();
    store.store(&msg).await.unwrap();

    // Query the database directly to check content_preview
    let db_msg = pattern_db::queries::get_message(store.pool(), &msg_id)
        .await
        .unwrap()
        .unwrap();

    // Should extract text from Text and Thinking blocks, but not ToolUse
    assert!(db_msg.content_preview.is_some());
    let preview = db_msg.content_preview.unwrap();
    assert!(preview.contains("Text block content"));
    assert!(preview.contains("Thinking block content"));
    // ToolUse blocks are not included in preview
    assert!(!preview.contains("search"));
}

// ============================================================================
// BatchType Tests
// ============================================================================

#[tokio::test]
async fn test_batch_type_storage() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    let batch_types = vec![
        BatchType::UserRequest,
        BatchType::AgentToAgent,
        BatchType::SystemTrigger,
        BatchType::Continuation,
    ];

    for (i, batch_type) in batch_types.iter().enumerate() {
        let msg = Message {
            id: MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content: MessageContent::Text(format!("Message {}", i)),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: 2,
            created_at: chrono::Utc::now(),
            position: Some(get_next_message_position_sync()),
            batch: Some(get_next_message_position_sync()),
            sequence_num: Some(0),
            batch_type: Some(*batch_type),
        };

        let msg_id = msg.id.0.clone();
        store.store(&msg).await.unwrap();

        // Retrieve and verify batch type
        let retrieved = store.get_message(&msg_id).await.unwrap().unwrap();
        assert_eq!(retrieved.batch_type, Some(*batch_type));
    }
}

// ============================================================================
// Archive and Delete Tests
// ============================================================================

#[tokio::test]
async fn test_archive_messages() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    // Store several messages with increasing positions
    let positions: Vec<_> = (0..5)
        .map(|_| {
            let pos = get_next_message_position_sync();
            // Small delay to ensure different positions
            std::thread::sleep(std::time::Duration::from_millis(2));
            pos
        })
        .collect();

    for (i, pos) in positions.iter().enumerate() {
        let msg = Message {
            id: MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content: MessageContent::Text(format!("Message {}", i)),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: 2,
            created_at: chrono::Utc::now(),
            position: Some(*pos),
            batch: Some(*pos),
            sequence_num: Some(0),
            batch_type: Some(BatchType::UserRequest),
        };
        store.store(&msg).await.unwrap();
    }

    // Archive messages before position 3
    let archive_before = positions[2].to_string();
    let archived_count = store.archive_before(&archive_before).await.unwrap();
    assert_eq!(archived_count, 2); // Messages 0 and 1

    // get_recent should only return non-archived messages
    let recent = store.get_recent(10).await.unwrap();
    assert_eq!(recent.len(), 3); // Messages 2, 3, 4

    // get_all should include archived messages
    let all = store.get_all(10).await.unwrap();
    assert_eq!(all.len(), 5);
}

#[tokio::test]
async fn test_count_messages() {
    let db = test_db().await;
    create_test_agent(&db, "agent_1").await;

    let store = MessageStore::new(db.pool().clone(), "agent_1");

    // Initially no messages
    assert_eq!(store.count().await.unwrap(), 0);
    assert_eq!(store.count_all().await.unwrap(), 0);

    // Add messages
    for i in 0..5 {
        let msg = Message {
            id: MessageId::generate(),
            role: ChatRole::User,
            owner_id: None,
            content: MessageContent::Text(format!("Message {}", i)),
            metadata: MessageMetadata::default(),
            options: MessageOptions::default(),
            has_tool_calls: false,
            word_count: 2,
            created_at: chrono::Utc::now(),
            position: Some(get_next_message_position_sync()),
            batch: Some(get_next_message_position_sync()),
            sequence_num: Some(0),
            batch_type: Some(BatchType::UserRequest),
        };
        store.store(&msg).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    assert_eq!(store.count().await.unwrap(), 5);
    assert_eq!(store.count_all().await.unwrap(), 5);
}
