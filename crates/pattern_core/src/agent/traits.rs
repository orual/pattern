//! Core AgentV2 trait and extension trait

use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;
use tokio_stream::Stream;

use crate::AgentId;
use crate::agent::{AgentState, ResponseEvent};
use crate::error::CoreError;
use crate::message::{Message, Response};
use crate::runtime::AgentRuntime;

/// Slim agent trait - identity + process loop + state only
///
/// All "doing" (tool execution, message sending) goes through `runtime()`.
/// All "reading" (context building) goes through `runtime().prepare_request()`.
/// Memory access for agents is via tools (context, recall, search), not direct methods.
#[async_trait]
pub trait Agent: Send + Sync + Debug {
    /// Get the agent's unique identifier
    fn id(&self) -> AgentId;

    /// Get the agent's display name
    fn name(&self) -> &str;

    /// Get the agent's runtime for executing actions
    ///
    /// The runtime provides:
    /// - `memory()` - MemoryStore access
    /// - `messages()` - MessageStore access
    /// - `tools()` - ToolRegistry access
    /// - `router()` - Message routing
    /// - `prepare_request()` - Build model requests
    ///
    /// Returns Arc to allow callers to use the runtime as Arc<dyn ToolContext>
    /// for data source operations.
    fn runtime(&self) -> Arc<AgentRuntime>;

    /// Process a message, streaming response events
    ///
    /// This is the main processing loop. Implementation should:
    /// 1. Use `runtime().prepare_request()` to build context
    /// 2. Send request to model provider
    /// 3. Execute any tool calls via `runtime().execute_tool()`
    /// 4. Store responses via `runtime().store_message()`
    /// 5. Stream ResponseEvents as processing proceeds
    async fn process(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError>;

    /// Get the agent's current state and a watch receiver for changes
    async fn state(&self) -> (AgentState, Option<tokio::sync::watch::Receiver<AgentState>>);

    /// Update the agent's state
    async fn set_state(&self, state: AgentState) -> Result<(), CoreError>;
}

/// Extension trait for AgentV2 with convenience methods
///
/// This trait is automatically implemented for all types that implement AgentV2.
/// It provides higher-level operations built on top of the core trait.
#[async_trait]
pub trait AgentExt: Agent {
    /// Process a message and collect the response (non-streaming)
    ///
    /// Convenience wrapper around `process()` for callers who
    /// don't need real-time streaming.
    async fn process_to_response(self: Arc<Self>, message: Message) -> Result<Response, CoreError> {
        let stream = self.process(message).await?;
        super::collect::collect_response(stream).await
    }
}

// Blanket implementation for all AgentV2 types
impl<T: ?Sized + Agent> AgentExt for T {}
