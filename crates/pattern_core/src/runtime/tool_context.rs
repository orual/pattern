//! ToolContext: A minimal API surface for tools
//!
//! Provides tools with access to memory, router, model, and permission broker
//! without exposing the full AgentRuntime implementation details.

use async_trait::async_trait;

use crate::ModelProvider;
use crate::id::AgentId;
use crate::memory::{MemoryResult, MemorySearchResult, MemoryStore, SearchOptions};
use crate::permission::PermissionBroker;
use crate::runtime::AgentMessageRouter;

/// Scope for search operations - determines what data is searched
#[derive(Debug, Clone)]
pub enum SearchScope {
    /// Search only the current agent's data (always allowed)
    CurrentAgent,
    /// Search a specific agent's data (requires permission)
    Agent(AgentId),
    /// Search multiple agents' data (requires permission for each)
    Agents(Vec<AgentId>),
    /// Search all data in the constellation (requires broad permission)
    Constellation,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self::CurrentAgent
    }
}

/// What tools can access from the runtime
#[async_trait]
pub trait ToolContext: Send + Sync {
    /// Get the current agent's ID (for default scoping)
    fn agent_id(&self) -> &str;

    /// Get the memory store for blocks, archival, and search
    fn memory(&self) -> &dyn MemoryStore;

    /// Get the message router for send_message
    fn router(&self) -> &AgentMessageRouter;

    /// Get the model provider for tools that need LLM calls
    fn model(&self) -> Option<&dyn ModelProvider>;

    /// Get the permission broker for consent requests
    fn permission_broker(&self) -> &'static PermissionBroker;

    /// Search with explicit scope and permission checks
    async fn search(
        &self,
        query: &str,
        scope: SearchScope,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;
}
