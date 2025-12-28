#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::db::ConstellationDatabases;
    use crate::tool::builtin::MockToolContext;
    use crate::{memory::MemoryCache, tool::ToolRegistry};
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
        assert!(tool_names.iter().any(|name| name == "search"));
        assert!(tool_names.iter().any(|name| name == "send_message"));
        assert!(tool_names.iter().any(|name| name == "calculator"));
        assert!(tool_names.iter().any(|name| name == "web"));
    }

    #[tokio::test]
    async fn test_new_v2_builtin_tools_registration() {
        let (_db, _memory, ctx) = create_test_context().await;

        let registry = ToolRegistry::new();
        let builtin = BuiltinTools::new(ctx);
        builtin.register_all(&registry);

        let tool_names = registry.list_tools();

        // New v2 tools
        assert!(
            tool_names.iter().any(|n| n == "block"),
            "block tool should be registered, found: {:?}",
            tool_names
        );
        assert!(
            tool_names.iter().any(|n| n == "block_edit"),
            "block_edit tool should be registered, found: {:?}",
            tool_names
        );
        assert!(
            tool_names.iter().any(|n| n == "source"),
            "source tool should be registered, found: {:?}",
            tool_names
        );

        // Existing tools still present
        assert!(
            tool_names.iter().any(|n| n == "recall"),
            "recall tool should still be registered"
        );
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

    // ============================================================================
    // SourceTool Tests
    // ============================================================================

    #[tokio::test]
    async fn test_source_tool_list() {
        use super::super::source::SourceTool;
        use super::super::types::{SourceInput, SourceOp};
        use crate::tool::AiTool;

        let (_db, _memory, ctx) = create_test_context().await;

        let tool = SourceTool::new(ctx);
        let result = tool
            .execute(
                SourceInput {
                    op: SourceOp::List,
                    source_id: None,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        // Should succeed even with no sources (MockToolContext returns None for sources())
        assert!(result.success);
        assert!(result.message.contains("sources"));
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
}
