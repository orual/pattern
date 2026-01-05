//! Integration tests for config merge logic with ConfigPriority.
//!
//! Tests the load_or_create_agent_with_config method which merges
//! TOML config with DB state based on ConfigPriority.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use pattern_core::Result;
use pattern_core::config::{AgentConfig, ConfigPriority, MemoryBlockConfig};
use pattern_core::db::ConstellationDatabases;
use pattern_core::memory::{MemoryPermission, MemoryStore, MemoryType};
use pattern_core::messages::{MessageContent, Request, Response};
use pattern_core::model::{ModelCapability, ModelInfo, ModelProvider, ResponseOptions};
use pattern_core::runtime::RuntimeContext;

/// Mock model provider for testing.
#[derive(Debug, Clone)]
struct TestMockModelProvider {
    response: String,
}

#[async_trait]
impl ModelProvider for TestMockModelProvider {
    fn name(&self) -> &str {
        "test_mock"
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            provider: "test_mock".to_string(),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
            ],
            context_window: 8192,
            max_output_tokens: Some(4096),
            cost_per_1k_prompt_tokens: Some(0.0),
            cost_per_1k_completion_tokens: Some(0.0),
        }])
    }

    async fn complete(&self, _options: &ResponseOptions, _request: Request) -> Result<Response> {
        Ok(Response {
            content: vec![MessageContent::from_text(&self.response)],
            reasoning: None,
            metadata: Default::default(),
        })
    }

    async fn supports_capability(&self, _model: &str, _capability: ModelCapability) -> bool {
        true
    }

    async fn count_tokens(&self, _model: &str, content: &str) -> Result<usize> {
        Ok(content.len() / 4)
    }
}

/// Setup test databases.
async fn setup_test_dbs() -> Arc<ConstellationDatabases> {
    Arc::new(ConstellationDatabases::open_in_memory().await.unwrap())
}

/// Create a mock model provider.
fn mock_model_provider() -> Arc<dyn ModelProvider> {
    Arc::new(TestMockModelProvider {
        response: "test response".to_string(),
    })
}

/// Create a test RuntimeContext.
async fn setup_test_context() -> Arc<RuntimeContext> {
    let dbs = setup_test_dbs().await;
    RuntimeContext::builder()
        .dbs(dbs)
        .model_provider(mock_model_provider())
        .build()
        .await
        .unwrap()
}

/// Create a basic agent config for testing.
fn test_agent_config(name: &str) -> AgentConfig {
    let mut memory = HashMap::new();
    memory.insert(
        "scratchpad".to_string(),
        MemoryBlockConfig {
            content: Some("Initial content".to_string()),
            content_path: None,
            permission: MemoryPermission::ReadWrite,
            memory_type: MemoryType::Working,
            description: Some("Test scratchpad".to_string()),
            id: None,
            shared: false,
            pinned: Some(true),
            char_limit: Some(4096),
            schema: None,
        },
    );

    AgentConfig {
        name: name.to_string(),
        memory,
        ..Default::default()
    }
}

// ============================================================================
// Test: New agent seeds from TOML
// ============================================================================

#[tokio::test]
async fn test_new_agent_seeds_from_toml() {
    let ctx = setup_test_context().await;
    let config = test_agent_config("NewAgent");

    // Load or create - should create since agent doesn't exist.
    let agent = ctx
        .load_or_create_agent_with_config("NewAgent", &config, ConfigPriority::Merge)
        .await
        .unwrap();

    // Verify agent was created.
    assert_eq!(agent.name(), "NewAgent");

    // Verify memory block was created with TOML content.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap();

    assert!(block.is_some(), "Scratchpad block should be created");
    let block = block.unwrap();

    // Verify content from TOML was seeded.
    let content = block.text_content();
    assert_eq!(content, "Initial content");
}

// ============================================================================
// Test: Existing agent with Merge priority preserves content
// ============================================================================

#[tokio::test]
async fn test_existing_agent_merge_preserves_content() {
    let ctx = setup_test_context().await;

    // First, create the agent with initial config.
    let initial_config = test_agent_config("MergeAgent");
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Modify the content in DB (simulating agent activity).
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    block.set_text("Modified by agent", true).unwrap();
    ctx.memory().mark_dirty(&agent_id, "scratchpad");
    ctx.memory().persist(&agent_id, "scratchpad").await.unwrap();

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated TOML config with new metadata but different content.
    let mut updated_config = test_agent_config("MergeAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.content = Some("New TOML content".to_string());
        block_config.pinned = Some(false); // Changed metadata
        block_config.char_limit = Some(8192); // Changed metadata
    }

    // Load with Merge priority - content should be preserved, metadata updated.
    let agent = ctx
        .load_or_create_agent_with_config("MergeAgent", &updated_config, ConfigPriority::Merge)
        .await
        .unwrap();

    // Get the block and verify.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    // Content should be preserved from DB.
    let content = block.text_content();
    assert_eq!(
        content, "Modified by agent",
        "Content should be preserved from DB, not overwritten by TOML"
    );

    // Metadata should be updated from TOML.
    let metadata = block.metadata();
    assert!(!metadata.pinned, "Pinned should be updated from TOML");
    assert_eq!(
        metadata.char_limit, 8192,
        "Char limit should be updated from TOML"
    );
}

// ============================================================================
// Test: DbWins ignores TOML entirely
// ============================================================================

#[tokio::test]
async fn test_db_wins_ignores_toml() {
    let ctx = setup_test_context().await;

    // First, create the agent with initial config.
    let initial_config = test_agent_config("DbWinsAgent");
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Modify the content in DB.
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    block.set_text("DB content", true).unwrap();
    ctx.memory().mark_dirty(&agent_id, "scratchpad");
    ctx.memory().persist(&agent_id, "scratchpad").await.unwrap();

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated TOML config with different values.
    let mut updated_config = test_agent_config("DbWinsAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.content = Some("New TOML content".to_string());
        block_config.pinned = Some(false); // Different from original
        block_config.char_limit = Some(8192); // Different from original
    }

    // Load with DbWins priority - everything should come from DB.
    let agent = ctx
        .load_or_create_agent_with_config("DbWinsAgent", &updated_config, ConfigPriority::DbWins)
        .await
        .unwrap();

    // Get the block and verify.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    // Content should be from DB.
    let content = block.text_content();
    assert_eq!(content, "DB content", "Content should be from DB");

    // Metadata should also be from DB (original values).
    let metadata = block.metadata();
    assert!(metadata.pinned, "Pinned should remain true from initial DB");
    assert_eq!(
        metadata.char_limit, 4096,
        "Char limit should remain 4096 from initial DB"
    );
}

// ============================================================================
// Test: TomlWins overwrites config but preserves content
// ============================================================================

#[tokio::test]
async fn test_toml_wins_overwrites_config() {
    let ctx = setup_test_context().await;

    // First, create the agent with initial config.
    let initial_config = test_agent_config("TomlWinsAgent");
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Modify the content in DB.
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    block.set_text("DB content", true).unwrap();
    ctx.memory().mark_dirty(&agent_id, "scratchpad");
    ctx.memory().persist(&agent_id, "scratchpad").await.unwrap();

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated TOML config with different values.
    let mut updated_config = test_agent_config("TomlWinsAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.content = Some("New TOML content".to_string());
        block_config.pinned = Some(false); // Different
        block_config.char_limit = Some(8192); // Different
    }

    // Load with TomlWins priority.
    let agent = ctx
        .load_or_create_agent_with_config(
            "TomlWinsAgent",
            &updated_config,
            ConfigPriority::TomlWins,
        )
        .await
        .unwrap();

    // Get the block and verify.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    // Content should STILL be preserved from DB (never overwrite content).
    let content = block.text_content();
    assert_eq!(
        content, "DB content",
        "Content should be preserved from DB even with TomlWins"
    );

    // But metadata should come from TOML.
    let metadata = block.metadata();
    assert!(
        !metadata.pinned,
        "Pinned should be updated from TOML with TomlWins"
    );
    assert_eq!(
        metadata.char_limit, 8192,
        "Char limit should be updated from TOML with TomlWins"
    );
}

// ============================================================================
// Test: New block in TOML creates it
// ============================================================================

#[tokio::test]
async fn test_merge_creates_new_blocks_from_toml() {
    let ctx = setup_test_context().await;

    // Create agent with just scratchpad.
    let initial_config = test_agent_config("NewBlockAgent");
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Remove agent from registry.
    ctx.remove_agent(&agent_id);

    // Create updated config with an additional block.
    let mut updated_config = test_agent_config("NewBlockAgent");
    updated_config.memory.insert(
        "notes".to_string(),
        MemoryBlockConfig {
            content: Some("New notes block".to_string()),
            content_path: None,
            permission: MemoryPermission::ReadWrite,
            memory_type: MemoryType::Working,
            description: Some("Agent notes".to_string()),
            id: None,
            shared: false,
            pinned: Some(false),
            char_limit: Some(2048),
            schema: None,
        },
    );

    // Load with Merge priority.
    let agent = ctx
        .load_or_create_agent_with_config("NewBlockAgent", &updated_config, ConfigPriority::Merge)
        .await
        .unwrap();

    // Verify the new block was created.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "notes")
        .await
        .unwrap();

    assert!(block.is_some(), "Notes block should be created from TOML");
    let block = block.unwrap();
    let content = block.text_content();
    assert_eq!(content, "New notes block");
}

// ============================================================================
// Test: Default permission in TOML still updates DB (Task 7 regression test)
// ============================================================================

#[tokio::test]
async fn test_merge_updates_permission_even_when_toml_is_default() {
    let ctx = setup_test_context().await;

    // Create agent with ReadOnly permission (non-default).
    let mut initial_config = test_agent_config("PermTestAgent");
    if let Some(block_config) = initial_config.memory.get_mut("scratchpad") {
        block_config.permission = MemoryPermission::ReadOnly;
    }
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Verify block was created with ReadOnly permission.
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    // Note: block.metadata().permission is pattern_db::models::MemoryPermission.
    assert_eq!(
        block.metadata().permission,
        pattern_core::db::models::MemoryPermission::ReadOnly,
        "Block should start with ReadOnly permission"
    );

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated config with ReadWrite (the default) permission.
    // This is the key scenario: TOML sets permission = "read_write" explicitly
    // or implicitly through the default, and we need to update the DB block
    // that currently has ReadOnly.
    let mut updated_config = test_agent_config("PermTestAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.permission = MemoryPermission::ReadWrite; // Default value
    }

    // Load with Merge priority - permission should be updated from TOML.
    let agent = ctx
        .load_or_create_agent_with_config("PermTestAgent", &updated_config, ConfigPriority::Merge)
        .await
        .unwrap();

    // Get the block and verify permission was updated.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    // Note: block.metadata().permission is pattern_db::models::MemoryPermission.
    assert_eq!(
        block.metadata().permission,
        pattern_core::db::models::MemoryPermission::ReadWrite,
        "Permission should be updated from TOML even when TOML value is the default"
    );
}

// ============================================================================
// Test: Memory type is always applied from TOML
// ============================================================================

#[tokio::test]
async fn test_merge_updates_memory_type_from_toml() {
    let ctx = setup_test_context().await;

    // Create agent with Working memory type.
    let mut initial_config = test_agent_config("TypeTestAgent");
    if let Some(block_config) = initial_config.memory.get_mut("scratchpad") {
        block_config.memory_type = MemoryType::Working;
    }
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Verify block was created with Working type.
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        block.metadata().block_type,
        pattern_core::memory::BlockType::Working,
        "Block should start with Working type"
    );

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated config with Core (the default) memory type.
    let mut updated_config = test_agent_config("TypeTestAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.memory_type = MemoryType::Core; // Default value
    }

    // Load with Merge priority - memory type should be updated from TOML.
    let agent = ctx
        .load_or_create_agent_with_config("TypeTestAgent", &updated_config, ConfigPriority::Merge)
        .await
        .unwrap();

    // Get the block and verify type was updated.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        block.metadata().block_type,
        pattern_core::memory::BlockType::Core,
        "Memory type should be updated from TOML even when TOML value is the default"
    );
}

// ============================================================================
// Test: AgentConfigRef file loading (Task 12, flagged as missing in Task 2)
// ============================================================================

#[tokio::test]
async fn test_agent_config_ref_file_load() {
    use pattern_core::config::AgentConfigRef;

    // Create a temp directory with an agent config file.
    let temp_dir = tempfile::tempdir().unwrap();
    let agent_file = temp_dir.path().join("test_agent.toml");

    // Write agent config to file.
    tokio::fs::write(
        &agent_file,
        r#"
name = "FileLoadedAgent"
system_prompt = "I was loaded from a file"
"#,
    )
    .await
    .unwrap();

    // Create AgentConfigRef pointing to file.
    let config_ref = AgentConfigRef::Path {
        config_path: agent_file.clone(),
    };

    // Resolve should load from file.
    let resolved = config_ref.resolve(temp_dir.path()).await.unwrap();
    assert_eq!(resolved.name, "FileLoadedAgent");
    assert_eq!(
        resolved.system_prompt.as_deref(),
        Some("I was loaded from a file")
    );
}

// ============================================================================
// Test: AgentConfigRef with relative path resolution
// ============================================================================

#[tokio::test]
async fn test_agent_config_ref_relative_path() {
    use pattern_core::config::AgentConfigRef;

    // Create a temp directory with a subdirectory for the agent config.
    let temp_dir = tempfile::tempdir().unwrap();
    let agents_dir = temp_dir.path().join("agents");
    tokio::fs::create_dir(&agents_dir).await.unwrap();

    let agent_file = agents_dir.join("relative_agent.toml");

    // Write agent config to file.
    tokio::fs::write(
        &agent_file,
        r#"
name = "RelativePathAgent"
system_prompt = "Loaded via relative path"
"#,
    )
    .await
    .unwrap();

    // Create AgentConfigRef with relative path.
    let config_ref = AgentConfigRef::Path {
        config_path: std::path::PathBuf::from("agents/relative_agent.toml"),
    };

    // Resolve should load from file relative to temp_dir.
    let resolved = config_ref.resolve(temp_dir.path()).await.unwrap();
    assert_eq!(resolved.name, "RelativePathAgent");
    assert_eq!(
        resolved.system_prompt.as_deref(),
        Some("Loaded via relative path")
    );
}

// ============================================================================
// Test: Pinned field update on reload
// ============================================================================

#[tokio::test]
async fn test_merge_updates_pinned_from_toml() {
    let ctx = setup_test_context().await;

    // Create agent with pinned=true initially.
    let mut initial_config = test_agent_config("PinnedTestAgent");
    if let Some(block_config) = initial_config.memory.get_mut("scratchpad") {
        block_config.pinned = Some(true);
    }
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Verify block was created with pinned=true.
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    assert!(
        block.metadata().pinned,
        "Block should start with pinned=true"
    );

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated config with pinned=false.
    let mut updated_config = test_agent_config("PinnedTestAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.pinned = Some(false);
    }

    // Load with Merge priority - pinned should be updated from TOML.
    let agent = ctx
        .load_or_create_agent_with_config("PinnedTestAgent", &updated_config, ConfigPriority::Merge)
        .await
        .unwrap();

    // Get the block and verify pinned was updated.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    assert!(
        !block.metadata().pinned,
        "Pinned should be updated to false from TOML"
    );
}

// ============================================================================
// Test: char_limit update on reload
// ============================================================================

#[tokio::test]
async fn test_merge_updates_char_limit_from_toml() {
    let ctx = setup_test_context().await;

    // Create agent with char_limit=4096 initially.
    let mut initial_config = test_agent_config("CharLimitTestAgent");
    if let Some(block_config) = initial_config.memory.get_mut("scratchpad") {
        block_config.char_limit = Some(4096);
    }
    let agent = ctx.create_agent(&initial_config).await.unwrap();
    let agent_id = agent.id().to_string();

    // Verify block was created with char_limit=4096.
    let block = ctx
        .memory()
        .get_block(&agent_id, "scratchpad")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        block.metadata().char_limit,
        4096,
        "Block should start with char_limit=4096"
    );

    // Remove agent from registry to simulate restart.
    ctx.remove_agent(&agent_id);

    // Create updated config with char_limit=8192.
    let mut updated_config = test_agent_config("CharLimitTestAgent");
    if let Some(block_config) = updated_config.memory.get_mut("scratchpad") {
        block_config.char_limit = Some(8192);
    }

    // Load with Merge priority - char_limit should be updated from TOML.
    let agent = ctx
        .load_or_create_agent_with_config(
            "CharLimitTestAgent",
            &updated_config,
            ConfigPriority::Merge,
        )
        .await
        .unwrap();

    // Get the block and verify char_limit was updated.
    let block = ctx
        .memory()
        .get_block(agent.id().as_str(), "scratchpad")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        block.metadata().char_limit,
        8192,
        "Char limit should be updated to 8192 from TOML"
    );
}

// ============================================================================
// Test: Deprecation check errors on singular [agent]
// ============================================================================

#[tokio::test]
async fn test_deprecation_check_errors_on_singular_agent() {
    use pattern_core::config::PatternConfig;
    use pattern_core::error::ConfigError;

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("deprecated.toml");

    tokio::fs::write(
        &config_path,
        r#"
[agent]
name = "OldStyle"
"#,
    )
    .await
    .unwrap();

    let result = PatternConfig::load_with_deprecation_check(&config_path).await;
    assert!(
        matches!(&result, Err(ConfigError::Deprecated { field, .. }) if field == "agent"),
        "Expected Deprecated error for singular [agent], got: {:?}",
        result
    );
}

// ============================================================================
// Test: Deprecation check passes for correct [[agents]] format
// ============================================================================

#[tokio::test]
async fn test_deprecation_check_passes_for_plural_agents() {
    use pattern_core::config::PatternConfig;

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("correct.toml");

    tokio::fs::write(
        &config_path,
        r#"
[[agents]]
name = "NewStyle"

[[agents]]
name = "AnotherAgent"
"#,
    )
    .await
    .unwrap();

    let result = PatternConfig::load_with_deprecation_check(&config_path).await;
    assert!(
        result.is_ok(),
        "Expected Ok for plural [[agents]], got: {:?}",
        result
    );

    let config = result.unwrap();
    assert_eq!(config.agents.len(), 2);
}
