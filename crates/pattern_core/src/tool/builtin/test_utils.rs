//! Test utilities for built-in tools

use async_trait::async_trait;
use std::sync::Arc;

use crate::ModelProvider;
use crate::data_source::SourceManager;
use crate::db::ConstellationDatabases;
use crate::memory::{MemoryCache, MemoryResult, MemorySearchResult, MemoryStore, SearchOptions};
use crate::permission::PermissionBroker;
use crate::runtime::{AgentMessageRouter, SearchScope, ToolContext};

/// Helper to create a test agent in the database for foreign key constraints
pub async fn create_test_agent_in_db(dbs: &ConstellationDatabases, id: &str) {
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

/// Create a complete test context with database, memory, and agent created
pub async fn create_test_context_with_agent(
    agent_id: &str,
) -> (
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
    create_test_agent_in_db(&dbs, agent_id).await;

    let memory = Arc::new(MemoryCache::new(Arc::clone(&dbs)));
    let ctx = Arc::new(MockToolContext::new(
        agent_id,
        Arc::clone(&memory) as Arc<dyn MemoryStore>,
        Arc::clone(&dbs),
    ));
    (dbs, memory, ctx)
}

/// Mock ToolContext for testing V2 tools
#[derive(Debug)]
pub struct MockToolContext {
    agent_id: String,
    memory: Arc<dyn MemoryStore>,
    router: AgentMessageRouter,
}

impl MockToolContext {
    /// Create a new MockToolContext for testing
    ///
    /// # Arguments
    /// * `agent_id` - The agent ID to use
    /// * `memory` - The memory store to use
    /// * `dbs` - The combined database connections to use
    pub fn new(
        agent_id: impl Into<String>,
        memory: Arc<dyn MemoryStore>,
        dbs: Arc<ConstellationDatabases>,
    ) -> Self {
        let agent_id = agent_id.into();

        Self {
            router: AgentMessageRouter::new(agent_id.clone(), agent_id.clone(), (*dbs).clone()),
            agent_id,
            memory,
        }
    }
}

#[async_trait]
impl ToolContext for MockToolContext {
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    fn memory(&self) -> &dyn MemoryStore {
        self.memory.as_ref()
    }

    fn router(&self) -> &AgentMessageRouter {
        &self.router
    }

    fn model(&self) -> Option<&dyn ModelProvider> {
        None
    }

    fn permission_broker(&self) -> &'static PermissionBroker {
        crate::permission::broker()
    }

    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        match scope {
            SearchScope::CurrentAgent => self.memory.search(&self.agent_id, query, options).await,
            SearchScope::Agent(ref id) => self.memory.search(id.as_str(), query, options).await,
            SearchScope::Agents(ref ids) => {
                let mut all = Vec::new();
                for id in ids {
                    // TODO: Log or aggregate errors from failed agent searches instead of silently ignoring
                    if let Ok(results) = self
                        .memory
                        .search(id.as_str(), query, options.clone())
                        .await
                    {
                        all.extend(results);
                    }
                }
                Ok(all)
            }
            SearchScope::Constellation => self.memory.search_all(query, options).await,
        }
    }

    fn sources(&self) -> Option<Arc<dyn SourceManager>> {
        // Mock doesn't have source management
        None
    }
}
