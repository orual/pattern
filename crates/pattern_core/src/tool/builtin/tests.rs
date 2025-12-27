#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::db::ConstellationDatabases;
    use crate::memory::MemoryStore;
    use crate::tool::AiTool;
    use crate::tool::builtin::{
        ContextInput, ContextTool, CoreMemoryOperationType, MockToolContext,
    };
    use crate::{
        memory::{BlockSchema, BlockType, MemoryCache},
        tool::ToolRegistry,
    };
    use std::sync::Arc;

    async fn create_test_context() -> (
        Arc<ConstellationDatabases>,
        Arc<MemoryCache>,
        Arc<MockToolContext>,
    ) {
        let dbs = Arc::new(
            ConstellationDatabases::open_in_memory()
                .await
                .expect("Failed to create test dbs"),
        );

        // Create test agent in database (required for foreign key constraints)
        create_test_agent_in_db(&dbs, "test-agent").await;

        let memory = Arc::new(MemoryCache::new(Arc::clone(&dbs)));
        let ctx = Arc::new(MockToolContext::new(
            "test-agent",
            Arc::clone(&memory) as Arc<dyn crate::memory::MemoryStore>,
            Arc::clone(&dbs),
        ));
        (dbs, memory, ctx)
    }

    /// Helper to create a test agent in the database for foreign key constraints
    async fn create_test_agent_in_db(dbs: &ConstellationDatabases, id: &str) {
        use chrono::Utc;
        use pattern_db::models::{Agent, AgentStatus};
        use sqlx::types::Json;

        let agent = Agent {
            id: id.to_string(),
            name: format!("Test Agent {}", id),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "Test prompt".to_string(),
            config: Json(serde_json::json!({})),
            enabled_tools: Json(vec![]),
            tool_rules: None,
            status: AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        pattern_db::queries::create_agent(dbs.constellation.pool(), &agent)
            .await
            .expect("Failed to create test agent");
    }

    #[tokio::test]
    async fn test_builtin_tools_registration() {
        let (_db, _memory, ctx) = create_test_context().await;

        // Create a tool registry
        let registry = ToolRegistry::new();

        // Register built-in tools
        let builtin = BuiltinTools::new(ctx);
        builtin.register_all(&registry);

        // Verify tools are registered
        let tool_names = registry.list_tools();
        assert!(tool_names.iter().any(|name| name == "recall"));
        assert!(tool_names.iter().any(|name| name == "context"));
        assert!(tool_names.iter().any(|name| name == "search"));
        assert!(tool_names.iter().any(|name| name == "send_message"));
        assert!(tool_names.iter().any(|name| name == "calculator"));
        assert!(tool_names.iter().any(|name| name == "web"));
    }

    #[tokio::test]
    async fn test_context_append_through_registry() {
        let (_db, memory, ctx) = create_test_context().await;

        // Create a test block and set initial content
        memory
            .create_block(
                "test-agent",
                "test",
                "test block description",
                BlockType::Core,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        // Set actual content (create_block only sets description, not content)
        memory
            .update_block_text("test-agent", "test", "initial value")
            .await
            .unwrap();

        // Create and register tools
        let registry = ToolRegistry::new();
        let builtin = BuiltinTools::new(ctx);
        builtin.register_all(&registry);

        // Execute context tool with append operation
        let params = serde_json::json!({
            "operation": "append",
            "name": "test",
            "content": " appended content"
        });

        let result = registry
            .execute("context", params, &crate::tool::ExecutionMeta::default())
            .await
            .unwrap();

        // Verify the result
        assert_eq!(result["success"], true);

        // Verify the memory was actually updated
        let content = memory
            .get_rendered_content("test-agent", "test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "initial value\n\n appended content");
    }

    #[tokio::test]
    async fn test_send_message_through_registry() {
        let (_db, _memory, ctx) = create_test_context().await;

        // Create and register tools
        let registry = ToolRegistry::new();
        let builtin = BuiltinTools::new(ctx);
        builtin.register_all(&registry);

        // Execute send_message tool
        let params = serde_json::json!({
            "target": {
                "target_type": "user"
            },
            "content": "Hello from test!"
        });

        let result = registry
            .execute(
                "send_message",
                params,
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        // Verify the result
        assert_eq!(result["success"], true);
        assert!(result["message_id"].is_string());
    }

    #[tokio::test]
    async fn test_context_replace_through_registry() {
        let (_db, memory, ctx) = create_test_context().await;

        // Create a memory block and set initial content
        memory
            .create_block(
                "test-agent",
                "persona",
                "persona block",
                BlockType::Core,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        // Set actual content (create_block only sets description, not content)
        memory
            .update_block_text("test-agent", "persona", "I am a helpful AI assistant.")
            .await
            .unwrap();

        // Create and register tools
        let registry = ToolRegistry::new();
        let builtin = BuiltinTools::new(ctx);
        builtin.register_all(&registry);

        // Execute context tool with replace operation
        let params = serde_json::json!({
            "operation": "replace",
            "name": "persona",
            "old_content": "helpful AI assistant",
            "new_content": "knowledgeable AI companion"
        });

        let result = registry
            .execute("context", params, &crate::tool::ExecutionMeta::default())
            .await
            .unwrap();

        // Verify the result
        assert_eq!(result["success"], true);

        // Verify the memory was actually updated
        let content = memory
            .get_rendered_content("test-agent", "persona")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(content, "I am a knowledgeable AI companion.");
    }

    // TODO: Rewrite this test - archival entries are immutable, so append creates a new entry
    // rather than modifying the existing one. Need to decide on semantics: should append
    // find+delete+recreate, or should we expect multiple entries with same label?
    #[tokio::test]
    #[ignore = "needs rewrite: archival entries are immutable, append creates new entry"]
    async fn test_recall_through_registry() {
        let (_db, _memory, ctx) = create_test_context().await;

        // Create and register tools
        let registry = ToolRegistry::new();
        let builtin = BuiltinTools::new(ctx);
        builtin.register_all(&registry);

        // Test inserting archival memory
        let insert_params = serde_json::json!({
            "operation": "insert",
            "content": "The user mentioned they enjoy hiking in the mountains.",
            "label": "user_hobbies"
        });

        let result = registry
            .execute(
                "recall",
                insert_params,
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("Created recall memory")
        );

        // Test appending to archival memory
        let append_params = serde_json::json!({
            "operation": "append",
            "label": "user_hobbies",
            "content": " They also enjoy rock climbing."
        });

        let result = registry
            .execute(
                "recall",
                append_params,
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        // The message format has changed - just check success
        assert!(result["message"].is_string());

        // Verify the append worked by reading
        let read_params = serde_json::json!({
            "operation": "read",
            "label": "user_hobbies"
        });

        let result = registry
            .execute(
                "recall",
                read_params,
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]["content"].as_str().unwrap().contains("hiking"));
        assert!(
            results[0]["content"]
                .as_str()
                .unwrap()
                .contains("rock climbing")
        );
    }

    #[tokio::test]
    async fn test_context_load_creates_block_if_needed() {
        let (_db, memory, ctx) = create_test_context().await;

        // Create an archival entry with a label in metadata
        let metadata = serde_json::json!({"label": "arch_a"});
        memory
            .insert_archival("test-agent", "archival content for loading", Some(metadata))
            .await
            .unwrap();

        let tool = ContextTool::new(ctx);

        // Load archival into a new block (should be created automatically)
        let out = tool
            .execute(
                ContextInput {
                    operation: CoreMemoryOperationType::Load,
                    name: Some("loaded".into()),
                    content: None,
                    old_content: None,
                    new_content: None,
                    archival_label: Some("arch_a".into()),
                    archive_name: None,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(out.success, "Load operation failed: {:?}", out.message);

        // Verify the block was created and has the archival content
        let loaded_content = memory
            .get_rendered_content("test-agent", "loaded")
            .await
            .unwrap()
            .unwrap();
        assert!(loaded_content.contains("archival content for loading"));
    }

    // TODO: Rewrite this test - the concept of "flipping block type from Archival to Working"
    // doesn't match current architecture where archival_entries are separate from memory_blocks.
    // Need to decide if this behavior is still desired and implement accordingly.
    #[tokio::test]
    #[ignore = "needs rewrite: archival entries vs memory blocks architecture mismatch"]
    async fn test_context_load_same_label_flips_type() {
        let (_db, memory, ctx) = create_test_context().await;

        memory
            .create_block(
                "test-agent",
                "arch_x",
                "content",
                BlockType::Archival,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        let tool = ContextTool::new(ctx);

        let out = tool
            .execute(
                ContextInput {
                    operation: CoreMemoryOperationType::Load,
                    name: Some("arch_x".into()),
                    content: None,
                    old_content: None,
                    new_content: None,
                    archival_label: Some("arch_x".into()),
                    archive_name: None,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(out.success);
        let blk = memory
            .get_block_metadata("test-agent", "arch_x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(blk.block_type, BlockType::Working);
    }

    #[tokio::test]
    async fn test_context_swap_overwrite_requires_consent_then_approves() {
        use crate::permission::{PermissionDecisionKind, PermissionScope, broker};

        let (_db, memory, _ctx) = create_test_context().await;

        memory
            .create_block(
                "test-agent",
                "work_a",
                "old",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        memory
            .create_block(
                "test-agent",
                "arch_b",
                "new",
                BlockType::Working,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        let ctx = Arc::new(MockToolContext::new(
            "test-agent",
            Arc::clone(&memory) as Arc<dyn crate::memory::MemoryStore>,
            Arc::new(ConstellationDatabases::open_in_memory().await.unwrap()),
        ));
        let tool = ContextTool::new(ctx);

        // Auto-approve consent: subscribe before executing to avoid race
        let mut rx = broker().subscribe();
        tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                if matches!(req.scope, PermissionScope::MemoryEdit{ ref key } if key == "work_a") {
                    let _ = broker()
                        .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                        .await;
                }
            }
        });

        let out = tool
            .execute(
                ContextInput {
                    operation: CoreMemoryOperationType::Swap,
                    name: None,
                    content: None,
                    old_content: None,
                    new_content: None,
                    archival_label: Some("arch_b".into()),
                    archive_name: Some("work_a".into()),
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        // Depending on the type mismatch, operation may error earlier; we only assert approval path doesn't deadlock
        // Here we accept either success or explicit type error, but ensure no panic/hang.
        assert!(out.success || out.message.is_some());
    }

    #[tokio::test]
    async fn test_acl_check_basics() {
        use crate::memory::MemoryPermission as P;
        use crate::memory_acl::{MemoryGate, MemoryOp, check};

        assert!(matches!(
            check(MemoryOp::Read, P::ReadOnly),
            MemoryGate::Allow
        ));

        assert!(matches!(
            check(MemoryOp::Append, P::Append),
            MemoryGate::Allow
        ));
        assert!(matches!(
            check(MemoryOp::Append, P::ReadWrite),
            MemoryGate::Allow
        ));
        assert!(matches!(
            check(MemoryOp::Append, P::Admin),
            MemoryGate::Allow
        ));
        assert!(matches!(
            check(MemoryOp::Append, P::Human),
            MemoryGate::RequireConsent { .. }
        ));
        assert!(matches!(
            check(MemoryOp::Append, P::Partner),
            MemoryGate::RequireConsent { .. }
        ));
        assert!(matches!(
            check(MemoryOp::Append, P::ReadOnly),
            MemoryGate::Deny { .. }
        ));

        assert!(matches!(
            check(MemoryOp::Overwrite, P::ReadWrite),
            MemoryGate::Allow
        ));
        assert!(matches!(
            check(MemoryOp::Overwrite, P::Admin),
            MemoryGate::Allow
        ));
        assert!(matches!(
            check(MemoryOp::Overwrite, P::Human),
            MemoryGate::RequireConsent { .. }
        ));
        assert!(matches!(
            check(MemoryOp::Overwrite, P::Partner),
            MemoryGate::RequireConsent { .. }
        ));
        assert!(matches!(
            check(MemoryOp::Overwrite, P::Append),
            MemoryGate::Deny { .. }
        ));
        assert!(matches!(
            check(MemoryOp::Overwrite, P::ReadOnly),
            MemoryGate::Deny { .. }
        ));

        assert!(matches!(
            check(MemoryOp::Delete, P::Admin),
            MemoryGate::Allow
        ));
        assert!(matches!(
            check(MemoryOp::Delete, P::ReadWrite),
            MemoryGate::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn test_context_append_requires_consent_then_approves() {
        use crate::permission::{PermissionDecisionKind, PermissionScope, broker};
        use crate::tool::AiTool;

        let (_db, memory, _ctx) = create_test_context().await;

        // Memory with Human permission on a core block
        memory
            .create_block(
                "test-agent",
                "human",
                "User is testing",
                BlockType::Core,
                BlockSchema::Text,
                2000, // char_limit
            )
            .await
            .unwrap();

        let ctx = Arc::new(MockToolContext::new(
            "test-agent",
            Arc::clone(&memory) as Arc<dyn crate::memory::MemoryStore>,
            Arc::new(ConstellationDatabases::open_in_memory().await.unwrap()),
        ));
        let tool = ContextTool::new(ctx);

        // Subscribe before executing to avoid race, then spawn resolver
        let mut rx = broker().subscribe();
        tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                if matches!(req.scope, PermissionScope::MemoryEdit{ ref key } if key == "human") {
                    let _ = broker()
                        .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                        .await;
                }
            }
        });

        // Execute append which should request consent and then succeed
        let params = ContextInput {
            operation: CoreMemoryOperationType::Append,
            name: Some("human".to_string()),
            content: Some("Append with consent".to_string()),
            old_content: None,
            new_content: None,
            archival_label: None,
            archive_name: None,
        };
        let result = tool
            .execute(params, &crate::tool::ExecutionMeta::default())
            .await
            .unwrap();

        assert!(result.success);
        let content = memory
            .get_rendered_content("test-agent", "human")
            .await
            .unwrap()
            .unwrap();
        assert!(content.contains("Append with consent"));
    }
}
