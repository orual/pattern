//! Pattern Core - Agent Framework and Memory System
//!
//! This crate provides the core agent framework, memory management,
//! and tool execution system that powers Pattern's multi-agent
//! cognitive support system.

pub mod agent;
pub mod config;
pub mod context;
pub mod coordination;
//pub mod data_source;
pub mod db;
pub mod embeddings;
pub mod error;
#[cfg(feature = "export")]
pub mod export;
pub mod id;
pub mod memory;
pub mod memory_acl;
pub mod messages;
pub mod model;
pub mod oauth;
pub mod permission;
pub mod prompt_template;
pub mod queue;
pub mod realtime;
pub mod runtime;
pub mod tool;
pub mod users;
pub mod utils;

#[cfg(test)]
pub mod test_helpers;

// Macros are automatically available at crate root due to #[macro_export]

pub use crate::utils::SnowflakePosition;
pub use agent::{Agent, AgentState, AgentType};
pub use context::{CompressionStrategy, ContextBuilder, ContextConfig, MessageCompressor};
pub use coordination::{AgentGroup, Constellation, CoordinationPattern};
pub use error::{CoreError, Result};
pub use id::{
    AgentId, ConversationId, Did, IdType, MemoryId, MessageId, ModelId, OAuthTokenId,
    QueuedMessageId, RequestId, SessionId, TaskId, ToolCallId, UserId, WakeupId,
};
pub use messages::queue::{QueuedMessage, ScheduledWakeup};
pub use model::ModelCapability;
pub use model::ModelProvider;
pub use runtime::{AgentRuntime, RuntimeBuilder, RuntimeConfig};
pub use tool::{AiTool, DynamicTool, ToolRegistry, ToolResult};

// Data source types
pub use data_source::{
    // Helper utilities
    BlockBuilder,
    // Manager types
    BlockEdit,
    // Core reference types
    BlockRef,
    // Schema and status types
    BlockSchemaSpec,
    BlockSourceInfo,
    BlockSourceStatus,
    // Block source types
    ConflictResolution,
    // Core traits
    DataBlock,
    DataStream,
    EditFeedback,
    EphemeralBlockCache,
    FileChange,
    FileChangeType,
    Notification,
    NotificationBuilder,
    PermissionRule,
    ReconcileResult,
    SourceManager,
    StreamCursor,
    StreamSourceInfo,
    StreamStatus,
    VersionInfo,
};
/// Re-export commonly used types
pub mod prelude {
    pub use crate::{
        Agent, AgentId, AgentState, AgentType, AiTool, CompressionStrategy, ContextBuilder,
        ContextConfig, CoreError, DynamicTool, IdType, MessageCompressor, ModelCapability,
        ModelProvider, Result, ToolRegistry, ToolResult,
    };
}

#[derive(Debug, Clone)]
pub struct PatternHttpClient {
    pub client: reqwest::Client,
}

impl Default for PatternHttpClient {
    fn default() -> Self {
        Self {
            client: pattern_reqwest_client(),
        }
    }
}

pub fn pattern_reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("pattern/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10)) // 10 second timeout for constellation API calls
        .connect_timeout(std::time::Duration::from_secs(5)) // 5 second connection timeout
        .build()
        .unwrap() // panics for the same reasons Client::new() would: https://docs.rs/reqwest/latest/reqwest/struct.Client.html#panics
}

impl jacquard::http_client::HttpClient for PatternHttpClient {
    type Error = reqwest::Error;

    fn send_http(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> impl Future<Output = core::result::Result<http::Response<Vec<u8>>, Self::Error>> + Send
    {
        async { self.client.send_http(request).await }
    }
}
