//! Test utilities for built-in tools.
//!
//! Provides shared test infrastructure for testing built-in tools:
//!
//! - [`MockToolContext`]: A configurable mock implementation of [`ToolContext`]
//! - [`MockSourceManager`]: A mock [`SourceManager`] for testing tools that need data sources
//! - [`create_test_agent_in_db`]: Helper to create test agents for foreign key constraints
//! - [`create_test_context_with_agent`]: Quick setup for basic tool tests
//!
//! # Example
//!
//! ```ignore
//! // Basic test context without source management
//! let (_dbs, _memory, ctx) = create_test_context_with_agent("test_agent").await;
//!
//! // Context with source management for shell/file tools
//! let ctx = MockToolContext::builder()
//!     .agent_id("test_agent")
//!     .with_source_manager(source_manager)
//!     .build(dbs.clone(), memory.clone())
//!     .await;
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::data_source::{
    BlockEdit, BlockRef, BlockSourceInfo, DataBlock, DataStream, EditFeedback, Notification,
    ReconcileResult, SourceManager, StreamCursor, StreamSourceInfo, VersionInfo,
};
use crate::db::ConstellationDatabases;
use crate::id::AgentId;
use crate::memory::{
    MemoryCache, MemoryResult, MemorySearchResult, MemoryStore, SearchOptions, SharedBlockManager,
};
use crate::permission::PermissionBroker;
use crate::runtime::{AgentMessageRouter, SearchScope, ToolContext};
use crate::{ModelProvider, Result};

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

/// Mock ToolContext for testing tools.
///
/// This is a configurable mock that can be used for testing any tool. By default,
/// it returns `None` for `sources()`, but you can configure it with a `SourceManager`
/// using the builder pattern for tools that require data source access (like ShellTool).
///
/// # Example
///
/// ```ignore
/// // Simple context without sources
/// let ctx = MockToolContext::new("agent", memory, dbs);
///
/// // Context with source manager
/// let ctx = MockToolContext::builder()
///     .agent_id("agent")
///     .with_source_manager(source_manager)
///     .build(dbs, memory)
///     .await;
/// ```
#[derive(Debug)]
pub struct MockToolContext {
    agent_id: String,
    memory: Arc<dyn MemoryStore>,
    router: AgentMessageRouter,
    shared_blocks: Arc<SharedBlockManager>,
    sources: Option<Arc<dyn SourceManager>>,
}

impl MockToolContext {
    /// Create a new MockToolContext for testing.
    ///
    /// This creates a basic context without source management. For tools that need
    /// a SourceManager (like ShellTool), use [`MockToolContext::builder()`] instead.
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
        let shared_blocks = Arc::new(SharedBlockManager::new(dbs.clone()));

        Self {
            router: AgentMessageRouter::new(agent_id.clone(), agent_id.clone(), (*dbs).clone()),
            agent_id,
            memory,
            shared_blocks,
            sources: None,
        }
    }

    /// Create a builder for configuring a MockToolContext.
    pub fn builder() -> MockToolContextBuilder {
        MockToolContextBuilder::default()
    }

    /// Create a context with an explicit SourceManager.
    ///
    /// This is a convenience method for when you have a pre-configured SourceManager.
    pub fn with_sources(
        agent_id: impl Into<String>,
        memory: Arc<dyn MemoryStore>,
        dbs: Arc<ConstellationDatabases>,
        sources: Arc<dyn SourceManager>,
    ) -> Self {
        let agent_id = agent_id.into();
        let shared_blocks = Arc::new(SharedBlockManager::new(dbs.clone()));

        Self {
            router: AgentMessageRouter::new(agent_id.clone(), agent_id.clone(), (*dbs).clone()),
            agent_id,
            memory,
            shared_blocks,
            sources: Some(sources),
        }
    }
}

/// Builder for MockToolContext.
///
/// Allows configuring optional components like SourceManager before creating
/// the context.
#[derive(Default)]
pub struct MockToolContextBuilder {
    agent_id: Option<String>,
    sources: Option<Arc<dyn SourceManager>>,
}

impl MockToolContextBuilder {
    /// Set the agent ID.
    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// Set the source manager for tools that need data source access.
    pub fn with_source_manager(mut self, sources: Arc<dyn SourceManager>) -> Self {
        self.sources = Some(sources);
        self
    }

    /// Build the MockToolContext.
    ///
    /// # Panics
    /// Panics if agent_id was not set.
    pub fn build(
        self,
        dbs: Arc<ConstellationDatabases>,
        memory: Arc<dyn MemoryStore>,
    ) -> MockToolContext {
        let agent_id = self.agent_id.expect("agent_id is required");
        let shared_blocks = Arc::new(SharedBlockManager::new(dbs.clone()));

        MockToolContext {
            router: AgentMessageRouter::new(agent_id.clone(), agent_id.clone(), (*dbs).clone()),
            agent_id,
            memory,
            shared_blocks,
            sources: self.sources,
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
        self.sources.clone()
    }

    fn shared_blocks(&self) -> Option<Arc<SharedBlockManager>> {
        Some(self.shared_blocks.clone())
    }
}

// =============================================================================
// MockSourceManager
// =============================================================================

/// Mock SourceManager for testing tools that need data source access.
///
/// This provides a minimal implementation that wraps a single stream source
/// (typically a `ProcessSource` for shell testing). It can be extended with
/// additional sources as needed.
///
/// # Example
///
/// ```ignore
/// use crate::data_source::process::ProcessSource;
///
/// let process_source = Arc::new(ProcessSource::with_local_backend(...));
/// let source_manager = Arc::new(MockSourceManager::with_stream(process_source));
///
/// let ctx = MockToolContext::builder()
///     .agent_id("test")
///     .with_source_manager(source_manager)
///     .build(dbs, memory);
/// ```
pub struct MockSourceManager {
    stream_sources: Vec<Arc<dyn DataStream>>,
}

impl std::fmt::Debug for MockSourceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockSourceManager")
            .field("stream_count", &self.stream_sources.len())
            .finish()
    }
}

impl MockSourceManager {
    /// Create an empty MockSourceManager with no sources.
    pub fn new() -> Self {
        Self {
            stream_sources: Vec::new(),
        }
    }

    /// Create a MockSourceManager with a single stream source.
    pub fn with_stream(source: Arc<dyn DataStream>) -> Self {
        Self {
            stream_sources: vec![source],
        }
    }
}

impl Default for MockSourceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceManager for MockSourceManager {
    fn list_streams(&self) -> Vec<String> {
        self.stream_sources
            .iter()
            .map(|s| s.source_id().to_string())
            .collect()
    }

    fn get_stream_info(&self, source_id: &str) -> Option<StreamSourceInfo> {
        self.stream_sources
            .iter()
            .find(|s| s.source_id() == source_id)
            .map(|source| StreamSourceInfo {
                source_id: source_id.to_string(),
                name: source.name().to_string(),
                block_schemas: source.block_schemas(),
                status: source.status(),
                supports_pull: source.supports_pull(),
            })
    }

    async fn pause_stream(&self, _source_id: &str) -> Result<()> {
        Ok(())
    }

    async fn resume_stream(&self, _source_id: &str, _ctx: Arc<dyn ToolContext>) -> Result<()> {
        Ok(())
    }

    async fn subscribe_to_stream(
        &self,
        _agent_id: &AgentId,
        _source_id: &str,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<broadcast::Receiver<Notification>> {
        let (tx, rx) = broadcast::channel(16);
        drop(tx);
        Ok(rx)
    }

    async fn unsubscribe_from_stream(&self, _agent_id: &AgentId, _source_id: &str) -> Result<()> {
        Ok(())
    }

    async fn pull_from_stream(
        &self,
        _source_id: &str,
        _limit: usize,
        _cursor: Option<StreamCursor>,
    ) -> Result<Vec<Notification>> {
        Ok(Vec::new())
    }

    fn list_block_sources(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_block_source_info(&self, _source_id: &str) -> Option<BlockSourceInfo> {
        None
    }

    async fn load_block(
        &self,
        _source_id: &str,
        _path: &std::path::Path,
        _owner: AgentId,
    ) -> Result<BlockRef> {
        Err(crate::CoreError::tool_exec_msg(
            "mock",
            serde_json::json!({}),
            "not implemented",
        ))
    }

    fn get_block_source(&self, _source_id: &str) -> Option<Arc<dyn DataBlock>> {
        None
    }

    fn find_block_source_for_path(&self, _path: &std::path::Path) -> Option<Arc<dyn DataBlock>> {
        None
    }

    fn get_stream_source(&self, source_id: &str) -> Option<Arc<dyn DataStream>> {
        // Check for exact match first.
        if let Some(source) = self
            .stream_sources
            .iter()
            .find(|s| s.source_id() == source_id)
        {
            return Some(source.clone());
        }

        // For shell testing, also match the default process source ID.
        // This enables ShellTool's fallback logic to find ProcessSource.
        const DEFAULT_PROCESS_SOURCE_ID: &str = "process:shell";
        if source_id == DEFAULT_PROCESS_SOURCE_ID {
            // Return the first stream source if it's a ProcessSource.
            // This is a testing convenience - in production, sources are registered explicitly.
            return self.stream_sources.first().cloned();
        }

        None
    }

    async fn create_block(
        &self,
        _source_id: &str,
        _path: &std::path::Path,
        _content: Option<&str>,
        _owner: AgentId,
    ) -> Result<BlockRef> {
        Err(crate::CoreError::tool_exec_msg(
            "mock",
            serde_json::json!({}),
            "not implemented",
        ))
    }

    async fn save_block(&self, _source_id: &str, _block_ref: &BlockRef) -> Result<()> {
        Ok(())
    }

    async fn delete_block(&self, _source_id: &str, _path: &std::path::Path) -> Result<()> {
        Ok(())
    }

    async fn reconcile_blocks(
        &self,
        _source_id: &str,
        _paths: &[PathBuf],
    ) -> Result<Vec<ReconcileResult>> {
        Ok(Vec::new())
    }

    async fn block_history(
        &self,
        _source_id: &str,
        _block_ref: &BlockRef,
    ) -> Result<Vec<VersionInfo>> {
        Ok(Vec::new())
    }

    async fn rollback_block(
        &self,
        _source_id: &str,
        _block_ref: &BlockRef,
        _version: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn diff_block(
        &self,
        _source_id: &str,
        _block_ref: &BlockRef,
        _from: Option<&str>,
        _to: Option<&str>,
    ) -> Result<String> {
        Ok(String::new())
    }

    async fn handle_block_edit(&self, _edit: &BlockEdit) -> Result<EditFeedback> {
        Ok(EditFeedback::Applied { message: None })
    }
}
