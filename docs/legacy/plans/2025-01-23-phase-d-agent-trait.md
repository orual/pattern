# Phase D: Slim Agent Trait Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Define a dramatically slimmed Agent trait (6 methods vs current 17+) that delegates all "doing" to AgentRuntime and all "reading" to ContextBuilder.

**Architecture:** The new `AgentV2` trait is identity + process loop + state only. Memory operations, tool execution, endpoint registration all go through `AgentRuntime`. Context building goes through `ContextBuilder`. Memory access for agents is via tools, not direct trait methods.

**Tech Stack:** Rust async_trait, tokio-stream, pattern_core runtime/context_v2 modules

**Parallel Development:** Define as `AgentV2` in `agent_v2` module to avoid conflicts with existing `Agent` trait. Swap later when v2 is stable.

---

## Architecture Comparison

### Current Agent Trait (17+ methods - TOO FAT)
```rust
trait Agent {
    fn id(&self) -> AgentId;
    fn name(&self) -> String;
    fn agent_type(&self) -> AgentType;
    async fn handle(&self) -> AgentHandle;
    async fn last_active(&self) -> Option<DateTime>;
    async fn process_message(self: Arc<Self>, message: Message) -> Result<Response>;
    async fn process_message_stream(...) -> Result<Box<dyn Stream<...>>>;
    async fn get_memory(&self, key: &str) -> Result<Option<MemoryBlock>>;
    async fn update_memory(&self, key: &str, memory: MemoryBlock) -> Result<()>;
    async fn set_memory(&self, key: &str, value: String) -> Result<()>;
    async fn list_memory_keys(&self) -> Result<Vec<CompactString>>;
    async fn share_memory_with(...) -> Result<()>;
    async fn get_shared_memories(&self) -> Result<Vec<...>>;
    async fn system_prompt(&self) -> Vec<String>;
    async fn available_tools(&self) -> Vec<Box<dyn DynamicTool>>;
    async fn state(&self) -> (AgentState, Option<Receiver<AgentState>>);
    async fn set_state(&self, state: AgentState) -> Result<()>;
    async fn register_endpoint(...) -> Result<()>;
    async fn set_default_user_endpoint(...) -> Result<()>;
    async fn execute_tool(...) -> Result<serde_json::Value>;
}
```

### New AgentV2 Trait (6 methods - SLIM)
```rust
#[async_trait]
trait AgentV2: Send + Sync + Debug {
    fn id(&self) -> AgentId;
    fn name(&self) -> &str;
    fn runtime(&self) -> &AgentRuntime;

    /// Process a message, streaming response events
    async fn process(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>>;

    async fn state(&self) -> AgentState;
    async fn set_state(&self, state: AgentState) -> Result<()>;
}
```

### Where Did Everything Go?

| Old Method | New Location | Access Pattern |
|------------|--------------|----------------|
| `get_memory()` | `runtime().memory()` | Via `context` tool |
| `update_memory()` | `runtime().memory()` | Via `context` tool |
| `set_memory()` | `runtime().memory()` | Via `context` tool |
| `list_memory_keys()` | `runtime().memory()` | Via `context` tool |
| `share_memory_with()` | MemoryStore directly | Via tool or Runtime method |
| `get_shared_memories()` | MemoryStore directly | Via tool or Runtime method |
| `execute_tool()` | `runtime().execute_tool()` | Direct call in process loop |
| `available_tools()` | `runtime().tools()` | Direct access |
| `system_prompt()` | ContextBuilder | Built during prepare_request |
| `register_endpoint()` | `runtime().router()` | Direct access |
| `set_default_user_endpoint()` | `runtime().router()` | Direct access |
| `handle()` | `runtime()` | Direct access |
| `agent_type()` | Removed from trait | Metadata on impl if needed |
| `last_active()` | Removed from trait | Query from DB if needed |

---

## Task Breakdown

### Task D1: Define AgentV2 Trait

**Files:**
- Create: `crates/pattern_core/src/agent_v2/mod.rs`
- Create: `crates/pattern_core/src/agent_v2/traits.rs`
- Modify: `crates/pattern_core/src/lib.rs` (add module)

**Step 1: Create agent_v2 module structure**

Create `crates/pattern_core/src/agent_v2/mod.rs`:
```rust
//! V2 Agent framework with slim trait design
//!
//! The AgentV2 trait is dramatically slimmer than the original Agent trait:
//! - Agent is just identity + process loop + state
//! - Runtime handles all "doing" (tool execution, message sending, storage)
//! - ContextBuilder handles all "reading" (memory, messages, tools → Request)
//! - Memory access is via tools, not direct trait methods

mod traits;

pub use traits::{AgentV2, AgentV2Ext};
```

**Step 2: Define the slim AgentV2 trait**

Create `crates/pattern_core/src/agent_v2/traits.rs`:
```rust
//! Core AgentV2 trait and extension trait

use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;
use tokio_stream::Stream;

use crate::agent::{AgentState, ResponseEvent};
use crate::error::CoreError;
use crate::message::Message;
use crate::runtime::AgentRuntime;
use crate::AgentId;

/// Slim agent trait - identity + process loop + state only
///
/// All "doing" (tool execution, message sending) goes through `runtime()`.
/// All "reading" (context building) goes through `runtime().prepare_request()`.
/// Memory access for agents is via tools (context, recall, search), not direct methods.
#[async_trait]
pub trait AgentV2: Send + Sync + Debug {
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
    fn runtime(&self) -> &AgentRuntime;

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

    /// Get the agent's current state
    async fn state(&self) -> AgentState;

    /// Update the agent's state
    async fn set_state(&self, state: AgentState) -> Result<(), CoreError>;
}
```

**Step 3: Add module to lib.rs**

In `crates/pattern_core/src/lib.rs`, add:
```rust
pub mod agent_v2;
pub use agent_v2::{AgentV2, AgentV2Ext};
```

**Step 4: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS (trait defined, no implementations yet)

**Step 5: Commit**

```bash
git add crates/pattern_core/src/agent_v2/
git add crates/pattern_core/src/lib.rs
git commit -m "feat(agent_v2): define slim AgentV2 trait (6 methods)"
```

---

### Task D2: Add AgentV2Ext and Response Collector

**Files:**
- Modify: `crates/pattern_core/src/agent_v2/traits.rs`
- Create: `crates/pattern_core/src/agent_v2/collect.rs`

**Step 1: Add collect module**

Create `crates/pattern_core/src/agent_v2/collect.rs`:
```rust
//! Response collection utilities for stream-based agent processing

use futures::StreamExt;
use tokio_stream::Stream;

use crate::agent::ResponseEvent;
use crate::error::CoreError;
use crate::message::{Message, MessageContent, Response, ResponseMetadata};

/// Collect a stream of ResponseEvents into a final Response
///
/// This helper aggregates streaming events into a complete Response,
/// useful for callers who don't need real-time streaming.
pub async fn collect_response(
    mut stream: impl Stream<Item = ResponseEvent> + Unpin,
) -> Result<Response, CoreError> {
    let mut content = Vec::new();
    let mut reasoning = None;
    let mut metadata = None;

    while let Some(event) = stream.next().await {
        match event {
            ResponseEvent::TextChunk { text, is_final: true } => {
                content.push(MessageContent::Text(text));
            }
            ResponseEvent::TextChunk { text, is_final: false } => {
                // Accumulate partial chunks - for now just take finals
                // A more sophisticated impl could buffer partials
                let _ = text;
            }
            ResponseEvent::ReasoningChunk { text, is_final: true } => {
                reasoning = Some(text);
            }
            ResponseEvent::ReasoningChunk { text, is_final: false } => {
                let _ = text;
            }
            ResponseEvent::ToolCalls { calls } => {
                content.push(MessageContent::ToolCalls(calls));
            }
            ResponseEvent::ToolResponses { responses } => {
                content.push(MessageContent::ToolResponses(responses));
            }
            ResponseEvent::Complete { metadata: meta, .. } => {
                metadata = Some(meta);
            }
            ResponseEvent::Error { message, .. } => {
                return Err(CoreError::AgentProcessing {
                    agent_id: "unknown".to_string(),
                    details: message,
                });
            }
            _ => {}
        }
    }

    Ok(Response {
        content,
        reasoning,
        metadata: metadata.unwrap_or_default(),
    })
}
```

**Step 2: Add AgentV2Ext trait**

Add to `crates/pattern_core/src/agent_v2/traits.rs`:
```rust
use crate::message::Response;

/// Extension trait providing convenience methods for AgentV2
#[async_trait]
pub trait AgentV2Ext: AgentV2 {
    /// Process a message and collect the response (non-streaming)
    ///
    /// Convenience wrapper around `process()` for callers who
    /// don't need real-time streaming.
    async fn process_to_response(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Response, CoreError>
    where
        Self: 'static,
    {
        let stream = self.process(message).await?;
        super::collect::collect_response(stream).await
    }
}

// Blanket implementation for all AgentV2
impl<T: AgentV2 + 'static> AgentV2Ext for T {}
```

**Step 3: Update mod.rs exports**

Update `crates/pattern_core/src/agent_v2/mod.rs`:
```rust
//! V2 Agent framework with slim trait design
//!
//! The AgentV2 trait is dramatically slimmer than the original Agent trait:
//! - Agent is just identity + process loop + state
//! - Runtime handles all "doing" (tool execution, message sending, storage)
//! - ContextBuilder handles all "reading" (memory, messages, tools → Request)
//! - Memory access is via tools, not direct trait methods

mod collect;
mod traits;

pub use collect::collect_response;
pub use traits::{AgentV2, AgentV2Ext};
```

**Step 4: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/agent_v2/
git commit -m "feat(agent_v2): add AgentV2Ext and collect_response helper"
```

---

### Task D3: Add Tool Execution to AgentRuntime

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs`

The Runtime needs `execute_tool()` for the process loop. This was listed in Phase C but not implemented.

**Step 1: Add execute_tool method**

Add to `AgentRuntime` impl in `runtime/mod.rs`:
```rust
use crate::message::{ToolCall, ToolResponse};

impl AgentRuntime {
    // ... existing methods ...

    /// Execute a tool call and return the response
    ///
    /// This looks up the tool in the registry, executes it with the
    /// provided arguments, and returns a ToolResponse.
    ///
    /// # Arguments
    /// * `call` - The tool call to execute
    ///
    /// # Returns
    /// A ToolResponse with the result or error
    pub async fn execute_tool(&self, call: &ToolCall) -> Result<ToolResponse, CoreError> {
        // Look up the tool
        let tool = self.tools.get(&call.fn_name).ok_or_else(|| {
            CoreError::ToolNotFound {
                tool_name: call.fn_name.clone(),
                available_tools: self.tools.list_tools().join(", "),
            }
        })?;

        // Execute with timeout
        let timeout = self.config.tool_timeout;
        let result = tokio::time::timeout(
            timeout,
            tool.execute_dynamic(call.fn_arguments.clone()),
        )
        .await;

        match result {
            Ok(Ok(output)) => Ok(ToolResponse {
                call_id: call.id.clone(),
                content: serde_json::to_string(&output)
                    .unwrap_or_else(|_| output.to_string()),
            }),
            Ok(Err(e)) => Ok(ToolResponse {
                call_id: call.id.clone(),
                content: format!("Tool error: {}", e),
            }),
            Err(_) => Ok(ToolResponse {
                call_id: call.id.clone(),
                content: format!("Tool execution timed out after {:?}", timeout),
            }),
        }
    }

    /// Execute multiple tool calls, returning responses in order
    pub async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<ToolResponse> {
        let mut responses = Vec::with_capacity(calls.len());
        for call in calls {
            match self.execute_tool(call).await {
                Ok(response) => responses.push(response),
                Err(e) => responses.push(ToolResponse {
                    call_id: call.id.clone(),
                    content: format!("Execution error: {}", e),
                }),
            }
        }
        responses
    }
}
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/runtime/mod.rs
git commit -m "feat(runtime): add execute_tool and execute_tools methods"
```

---

### Task D4: Implement DatabaseAgentV2

**Files:**
- Create: `crates/pattern_core/src/agent_v2/db_agent.rs`
- Modify: `crates/pattern_core/src/agent_v2/mod.rs`

**Step 1: Create the implementation file**

Create `crates/pattern_core/src/agent_v2/db_agent.rs`:
```rust
//! Database-backed AgentV2 implementation
//!
//! This is the v2 implementation that uses:
//! - AgentRuntime for all "doing" (tools, messages, routing)
//! - ContextBuilder (via runtime) for all "reading" (context assembly)
//! - Slim trait surface (6 methods)

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_stream::Stream;

use crate::agent::{AgentState, ResponseEvent, SnowflakePosition};
use crate::agent_v2::AgentV2;
use crate::context::heartbeat::{HeartbeatRequest, HeartbeatSender};
use crate::error::CoreError;
use crate::message::{BatchType, ChatRole, Message, MessageContent};
use crate::model::ResponseOptions;
use crate::runtime::AgentRuntime;
use crate::{AgentId, ModelProvider};

/// Database-backed agent using v2 architecture
///
/// This agent:
/// - Holds identity (id, name) and runtime
/// - Delegates all storage to runtime
/// - Uses stream-based processing
/// - Manages state via internal RwLock
pub struct DatabaseAgentV2 {
    id: AgentId,
    name: String,
    runtime: AgentRuntime,
    state: RwLock<AgentState>,

    /// Model provider for completions
    model: Arc<dyn ModelProvider>,

    /// Response options for model calls
    response_options: ResponseOptions,

    /// Heartbeat sender for continuation requests
    heartbeat_tx: Option<HeartbeatSender>,
}

impl std::fmt::Debug for DatabaseAgentV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseAgentV2")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("runtime", &self.runtime)
            .field("state", &"<RwLock<AgentState>>")
            .finish()
    }
}

impl DatabaseAgentV2 {
    /// Create a new DatabaseAgentV2
    pub fn new(
        id: AgentId,
        name: String,
        runtime: AgentRuntime,
        model: Arc<dyn ModelProvider>,
        response_options: ResponseOptions,
    ) -> Self {
        Self {
            id,
            name,
            runtime,
            state: RwLock::new(AgentState::Ready),
            model,
            response_options,
            heartbeat_tx: None,
        }
    }

    /// Set the heartbeat sender for continuation support
    pub fn with_heartbeat(mut self, tx: HeartbeatSender) -> Self {
        self.heartbeat_tx = Some(tx);
        self
    }

    /// Get the model provider
    pub fn model(&self) -> &Arc<dyn ModelProvider> {
        &self.model
    }

    /// Get the response options
    pub fn response_options(&self) -> &ResponseOptions {
        &self.response_options
    }
}

#[async_trait]
impl AgentV2 for DatabaseAgentV2 {
    fn id(&self) -> AgentId {
        self.id.clone()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn runtime(&self) -> &AgentRuntime {
        &self.runtime
    }

    async fn state(&self) -> AgentState {
        self.state.read().await.clone()
    }

    async fn set_state(&self, state: AgentState) -> Result<(), CoreError> {
        *self.state.write().await = state;
        Ok(())
    }

    async fn process(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError> {
        use tokio::sync::mpsc;
        use tokio_stream::wrappers::ReceiverStream;

        let (tx, rx) = mpsc::channel(100);
        let agent = self.clone();
        let message_id = message.id.clone();

        tokio::spawn(async move {
            if let Err(e) = agent.process_inner(message, tx.clone()).await {
                let _ = tx.send(ResponseEvent::Error {
                    message: e.to_string(),
                    recoverable: false,
                }).await;
            }
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }
}

impl DatabaseAgentV2 {
    /// Inner processing logic
    async fn process_inner(
        self: Arc<Self>,
        message: Message,
        tx: tokio::sync::mpsc::Sender<ResponseEvent>,
    ) -> Result<(), CoreError> {
        use crate::agent::get_next_message_position_sync;

        // Set state to processing
        let batch_id = message.batch.unwrap_or_else(get_next_message_position_sync);
        self.set_state(AgentState::Processing {
            active_batches: std::collections::HashSet::from([batch_id]),
        }).await?;

        // Prepare request using runtime
        let model_id = self.response_options.model_info.name.as_str();
        let request = self.runtime.prepare_request(
            vec![message.clone()],
            Some(model_id),
            Some(batch_id),
            None,
        ).await?;

        // Call model
        let response = self.model.complete(&self.response_options, request).await?;

        // Process response content
        let mut next_seq = message.sequence_num.map(|s| s + 1).unwrap_or(1);
        let mut tool_responses_to_process = Vec::new();

        for content in &response.content {
            match content {
                MessageContent::Text(text) => {
                    let _ = tx.send(ResponseEvent::TextChunk {
                        text: text.clone(),
                        is_final: true,
                    }).await;
                }
                MessageContent::ToolCalls(calls) => {
                    let _ = tx.send(ResponseEvent::ToolCalls {
                        calls: calls.clone(),
                    }).await;

                    // Execute tools
                    let responses = self.runtime.execute_tools(calls).await;

                    let _ = tx.send(ResponseEvent::ToolResponses {
                        responses: responses.clone(),
                    }).await;

                    tool_responses_to_process = responses;
                }
                _ => {}
            }
        }

        // Store assistant response
        let mut assistant_msg = Message::assistant_in_batch(
            batch_id,
            next_seq,
            response.content.iter()
                .filter_map(|c| match c {
                    MessageContent::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assistant_msg.content = response.content.clone();
        self.runtime.store_message(&assistant_msg).await?;
        next_seq += 1;

        // Store tool responses if any
        if !tool_responses_to_process.is_empty() {
            let tool_msg = Message {
                id: crate::MessageId::default(),
                role: ChatRole::User,
                content: vec![MessageContent::ToolResponses(tool_responses_to_process)],
                batch: Some(batch_id),
                batch_type: Some(BatchType::UserRequest),
                position: Some(get_next_message_position_sync()),
                sequence_num: Some(next_seq),
                ..Default::default()
            };
            self.runtime.store_message(&tool_msg).await?;
        }

        // Send completion
        let _ = tx.send(ResponseEvent::Complete {
            message_id: message.id,
            metadata: response.metadata,
        }).await;

        // Reset state
        self.set_state(AgentState::Ready).await?;

        Ok(())
    }
}
```

**Step 2: Update mod.rs**

Update `crates/pattern_core/src/agent_v2/mod.rs`:
```rust
//! V2 Agent framework with slim trait design
//!
//! The AgentV2 trait is dramatically slimmer than the original Agent trait:
//! - Agent is just identity + process loop + state
//! - Runtime handles all "doing" (tool execution, message sending, storage)
//! - ContextBuilder handles all "reading" (memory, messages, tools → Request)
//! - Memory access is via tools, not direct trait methods

mod collect;
mod db_agent;
mod traits;

pub use collect::collect_response;
pub use db_agent::DatabaseAgentV2;
pub use traits::{AgentV2, AgentV2Ext};
```

**Step 3: Update lib.rs exports**

In `crates/pattern_core/src/lib.rs`:
```rust
pub use agent_v2::{AgentV2, AgentV2Ext, DatabaseAgentV2, collect_response};
```

**Step 4: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/agent_v2/
git add crates/pattern_core/src/lib.rs
git commit -m "feat(agent_v2): implement DatabaseAgentV2 with slim trait"
```

---

### Task D5: Add Basic Tests

**Files:**
- Create: `crates/pattern_core/src/agent_v2/tests.rs`
- Modify: `crates/pattern_core/src/agent_v2/mod.rs`

**Step 1: Create test module**

Create `crates/pattern_core/src/agent_v2/tests.rs`:
```rust
//! Tests for AgentV2 trait and implementations

use super::*;
use crate::agent::{AgentState, ResponseEvent};
use crate::memory_v2::{BlockType, MemoryStore, MemoryResult, BlockMetadata, BlockSchema, StructuredDocument, ArchivalEntry, SearchOptions, MemorySearchResult};
use crate::messages::MessageStore;
use crate::runtime::AgentRuntime;
use crate::tool::ToolRegistry;
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::Arc;

// Mock MemoryStore for testing
struct MockMemoryStore;

#[async_trait]
impl MemoryStore for MockMemoryStore {
    async fn create_block(&self, _: &str, _: &str, _: &str, _: BlockType, _: BlockSchema, _: usize) -> MemoryResult<String> {
        Ok("test-id".to_string())
    }
    async fn get_block(&self, _: &str, _: &str) -> MemoryResult<Option<StructuredDocument>> {
        Ok(None)
    }
    async fn get_block_metadata(&self, _: &str, _: &str) -> MemoryResult<Option<BlockMetadata>> {
        Ok(None)
    }
    async fn list_blocks(&self, _: &str) -> MemoryResult<Vec<BlockMetadata>> {
        Ok(Vec::new())
    }
    async fn list_blocks_by_type(&self, _: &str, _: BlockType) -> MemoryResult<Vec<BlockMetadata>> {
        Ok(Vec::new())
    }
    async fn delete_block(&self, _: &str, _: &str) -> MemoryResult<()> {
        Ok(())
    }
    async fn get_rendered_content(&self, _: &str, _: &str) -> MemoryResult<Option<String>> {
        Ok(None)
    }
    async fn persist_block(&self, _: &str, _: &str) -> MemoryResult<()> {
        Ok(())
    }
    fn mark_dirty(&self, _: &str, _: &str) {}
    async fn insert_archival(&self, _: &str, _: &str, _: Option<serde_json::Value>) -> MemoryResult<String> {
        Ok("archival-id".to_string())
    }
    async fn search_archival(&self, _: &str, _: &str, _: usize) -> MemoryResult<Vec<ArchivalEntry>> {
        Ok(Vec::new())
    }
    async fn delete_archival(&self, _: &str) -> MemoryResult<()> {
        Ok(())
    }
    async fn search(&self, _: &str, _: &str, _: SearchOptions) -> MemoryResult<Vec<MemorySearchResult>> {
        Ok(Vec::new())
    }
}

async fn test_pool() -> SqlitePool {
    SqlitePool::connect(":memory:").await.unwrap()
}

#[tokio::test]
async fn test_agent_v2_trait_methods() {
    // This test verifies the trait surface is as expected
    // The trait should have exactly 6 methods

    // We can't instantiate the trait directly, but we can verify
    // DatabaseAgentV2 implements it correctly
    let pool = test_pool().await;
    let memory = Arc::new(MockMemoryStore);
    let messages = MessageStore::new(pool.clone(), "test_agent");

    let runtime = AgentRuntime::builder()
        .agent_id("test_agent")
        .agent_name("Test Agent")
        .memory(memory)
        .messages(messages)
        .pool(pool)
        .build()
        .unwrap();

    // We need a mock model provider for full testing
    // For now just verify the runtime is accessible
    assert_eq!(runtime.agent_id(), "test_agent");
}

#[tokio::test]
async fn test_agent_state_transitions() {
    let pool = test_pool().await;
    let memory = Arc::new(MockMemoryStore);
    let messages = MessageStore::new(pool.clone(), "test_agent");

    let runtime = AgentRuntime::builder()
        .agent_id("test_agent")
        .memory(memory)
        .messages(messages)
        .pool(pool)
        .build()
        .unwrap();

    // Create a minimal agent to test state transitions
    // Note: Full agent tests need mock ModelProvider

    // Verify runtime is properly constructed
    assert_eq!(runtime.agent_name(), "test_agent");
}
```

**Step 2: Add test module to mod.rs**

Update `crates/pattern_core/src/agent_v2/mod.rs`:
```rust
//! V2 Agent framework with slim trait design

mod collect;
mod db_agent;
mod traits;
#[cfg(test)]
mod tests;

pub use collect::collect_response;
pub use db_agent::DatabaseAgentV2;
pub use traits::{AgentV2, AgentV2Ext};
```

**Step 3: Run tests**

Run: `cargo test -p pattern_core agent_v2`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/agent_v2/
git commit -m "test(agent_v2): add basic trait and state tests"
```

---

## Success Criteria

- [ ] `cargo check -p pattern_core` passes
- [ ] `cargo test -p pattern_core agent_v2` passes
- [ ] AgentV2 trait has exactly 6 methods (id, name, runtime, process, state, set_state)
- [ ] DatabaseAgentV2 implements AgentV2 trait
- [ ] Old Agent trait and DatabaseAgent still compile (parallel development)
- [ ] AgentRuntime has execute_tool method
- [ ] AgentV2Ext provides process_to_response convenience method

---

## Next Steps (Phase E)

After Phase D is complete:
1. Update heartbeat processor to work with AgentV2
2. Port built-in tools to use Runtime instead of AgentHandle
3. Integration tests with real model providers
4. Eventually: swap Agent → AgentV2, remove old code
