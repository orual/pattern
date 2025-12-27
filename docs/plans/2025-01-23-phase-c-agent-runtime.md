# Phase C: AgentRuntime Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create AgentRuntime that holds all agent dependencies (stores, tools, permissions, router, model) and handles tool execution, message sending, and permission checks.

**Architecture:** Runtime is the "doing" layer - it executes tools, sends messages, stores messages, and manages permissions. Agent trait delegates all actions to Runtime.

**Tech Stack:** Rust async, pattern_db (SQLite), tokio channels for permissions

**Dependencies:** Phase A complete (MemoryStore, MessageStore), Phase B complete (ContextBuilder)

---

## Architecture Overview

```
                    ┌───────────────────┐
                    │   AgentRuntime    │
                    │                   │
                    │ • execute_tool()  │
                    │ • send_message()  │
                    │ • store_message() │
                    │ • build_context() │
                    └─────────┬─────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
        ▼                     ▼                     ▼
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│ MemoryStore  │     │ MessageStore │     │ ToolRegistry │
│ (trait obj)  │     │ (per-agent)  │     │   (shared)   │
└──────────────┘     └──────────────┘     └──────────────┘
        │                     │                     │
        └─────────────────────┴─────────────────────┘
                              │
                              ▼
                    ┌──────────────────┐
                    │   pattern_db     │
                    │   (SQLite)       │
                    └──────────────────┘
```

---

## Task C1: Create AgentRuntime Struct

**Files:**
- Create: `crates/pattern_core/src/runtime/mod.rs`
- Create: `crates/pattern_core/src/runtime/types.rs`
- Modify: `crates/pattern_core/src/lib.rs`

### Step 1: Write failing test for AgentRuntime construction

```rust
// In crates/pattern_core/src/runtime/mod.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runtime_construction() {
        let pool = test_pool().await;
        let memory = Arc::new(MockMemoryStore::new());
        let messages = MessageStore::new(pool.clone(), "test_agent");
        let tools = ToolRegistry::new();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .pool(pool)
            .build()
            .unwrap();

        assert_eq!(runtime.agent_id(), "test_agent");
    }
}
```

### Step 2: Run test to verify it fails

Run: `cargo test -p pattern-core runtime::tests::test_runtime_construction`
Expected: FAIL - module doesn't exist

### Step 3: Create AgentRuntime struct

```rust
// crates/pattern_core/src/runtime/mod.rs

//! AgentRuntime: The "doing" layer for agents
//!
//! Holds all agent dependencies and handles:
//! - Tool execution with permission checks
//! - Message sending via router
//! - Message storage
//! - Context building (delegates to ContextBuilder)

use std::sync::Arc;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::error::CoreError;
use crate::id::AgentId;
use crate::memory_v2::MemoryStore;
use crate::messages::MessageStore;
use crate::message::Message;
use crate::tool::{ToolRegistry, ToolCall, ToolResponse, ExecutionMeta};
use crate::permission::{PermissionBroker, PermissionScope, PermissionGrant};
use crate::context_v2::{ContextBuilder, ContextConfig};
use crate::message::Request;

pub mod router;
pub mod types;

pub use router::{AgentMessageRouter, MessageEndpoint, MessageOrigin};
pub use types::RuntimeConfig;

/// AgentRuntime holds all agent dependencies and executes actions
pub struct AgentRuntime {
    agent_id: AgentId,
    agent_name: String,

    // Stores
    memory: Arc<dyn MemoryStore>,
    messages: MessageStore,

    // Execution
    tools: Arc<ToolRegistry>,
    permissions: PermissionBroker,
    router: AgentMessageRouter,

    // Model (for compression, summarization)
    model: Option<Arc<dyn crate::ModelProvider>>,

    // Database pool (for queries)
    pool: SqlitePool,

    // Configuration
    config: RuntimeConfig,
}

impl AgentRuntime {
    /// Create a new RuntimeBuilder
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    /// Get the agent ID
    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    /// Get the agent name
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// Get the tool registry
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Get the permission broker
    pub fn permissions(&self) -> &PermissionBroker {
        &self.permissions
    }

    /// Get the message router
    pub fn router(&self) -> &AgentMessageRouter {
        &self.router
    }

    /// Get the database pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Builder for AgentRuntime
#[derive(Default)]
pub struct RuntimeBuilder {
    agent_id: Option<AgentId>,
    agent_name: Option<String>,
    memory: Option<Arc<dyn MemoryStore>>,
    messages: Option<MessageStore>,
    tools: Option<Arc<ToolRegistry>>,
    permissions: Option<PermissionBroker>,
    model: Option<Arc<dyn crate::ModelProvider>>,
    pool: Option<SqlitePool>,
    config: RuntimeConfig,
}

impl RuntimeBuilder {
    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(AgentId::new(id.into()));
        self
    }

    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = Some(name.into());
        self
    }

    pub fn memory(mut self, memory: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn messages(mut self, messages: MessageStore) -> Self {
        self.messages = Some(messages);
        self
    }

    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = Some(Arc::new(tools));
        self
    }

    pub fn tools_shared(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn permissions(mut self, permissions: PermissionBroker) -> Self {
        self.permissions = Some(permissions);
        self
    }

    pub fn model(mut self, model: Arc<dyn crate::ModelProvider>) -> Self {
        self.model = Some(model);
        self
    }

    pub fn pool(mut self, pool: SqlitePool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn config(mut self, config: RuntimeConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> Result<AgentRuntime, CoreError> {
        let agent_id = self.agent_id.ok_or_else(|| CoreError::ConfigurationError {
            field: "agent_id".into(),
            message: "Agent ID is required".into(),
        })?;

        let agent_name = self.agent_name.unwrap_or_else(|| agent_id.to_string());

        let memory = self.memory.ok_or_else(|| CoreError::ConfigurationError {
            field: "memory".into(),
            message: "MemoryStore is required".into(),
        })?;

        let messages = self.messages.ok_or_else(|| CoreError::ConfigurationError {
            field: "messages".into(),
            message: "MessageStore is required".into(),
        })?;

        let pool = self.pool.ok_or_else(|| CoreError::ConfigurationError {
            field: "pool".into(),
            message: "SqlitePool is required".into(),
        })?;

        let tools = self.tools.unwrap_or_else(|| Arc::new(ToolRegistry::new()));
        let permissions = self.permissions.unwrap_or_else(PermissionBroker::new);

        // Create router (will be updated in C2 to use pattern_db)
        let router = AgentMessageRouter::new(
            agent_id.clone(),
            agent_name.clone(),
            pool.clone(),
        );

        Ok(AgentRuntime {
            agent_id,
            agent_name,
            memory,
            messages,
            tools,
            permissions,
            router,
            model: self.model,
            pool,
            config: self.config,
        })
    }
}
```

### Step 4: Create RuntimeConfig

```rust
// crates/pattern_core/src/runtime/types.rs

//! Runtime configuration types

use std::time::Duration;

/// Configuration for AgentRuntime
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Default timeout for tool execution
    pub tool_timeout: Duration,

    /// Whether to require permissions for sensitive tools
    pub require_permissions: bool,

    /// Cooldown between agent-to-agent messages (anti-loop)
    pub agent_message_cooldown: Duration,

    /// Context configuration
    pub context_config: crate::context_v2::ContextConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            tool_timeout: Duration::from_secs(30),
            require_permissions: true,
            agent_message_cooldown: Duration::from_secs(30),
            context_config: Default::default(),
        }
    }
}
```

### Step 5: Update lib.rs exports

```rust
// Add to crates/pattern_core/src/lib.rs

pub mod runtime;

pub use runtime::{AgentRuntime, RuntimeBuilder, RuntimeConfig};
```

### Step 6: Commit

```bash
git add crates/pattern_core/src/runtime/
git commit -m "feat(runtime): add AgentRuntime struct with builder pattern"
```

---

## Task C1.5: Add Tool Execution to Runtime

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs`

### Step 1: Write failing test for tool execution

```rust
#[tokio::test]
async fn test_runtime_execute_tool() {
    let runtime = create_test_runtime().await;

    // Register a simple echo tool
    runtime.tools().register(EchoTool);

    let call = ToolCall {
        id: "call_1".into(),
        name: "echo".into(),
        arguments: serde_json::json!({ "message": "hello" }),
    };

    let response = runtime.execute_tool(&call).await.unwrap();

    assert_eq!(response.content, "hello");
}
```

### Step 2: Implement execute_tool

```rust
impl AgentRuntime {
    /// Execute a tool call with permission checks
    pub async fn execute_tool(&self, call: &ToolCall) -> Result<ToolResponse, CoreError> {
        // Look up tool
        let tool = self.tools.get(&call.name).ok_or_else(|| {
            CoreError::tool_not_found(&call.name, self.tools.list_tools().iter().map(|s| s.as_str()))
        })?;

        // Check if permission required
        let permission_grant = if self.config.require_permissions && tool.requires_consent() {
            let scope = PermissionScope::ToolExecution {
                tool: call.name.clone(),
                args_digest: Some(crate::tool::hash_args(&call.arguments)),
            };

            self.permissions.request(
                self.agent_id.clone(),
                call.name.clone(),
                scope,
                Some(format!("Executing tool: {}", call.name)),
                Some(call.arguments.clone()),
                self.config.tool_timeout,
            ).await
        } else {
            None
        };

        // Build execution meta
        let meta = ExecutionMeta {
            permission_grant,
            request_heartbeat: false,
            caller_user: None,
            call_id: Some(call.id.clone().into()),
            route_metadata: None,
        };

        // Execute tool
        let result = tokio::time::timeout(
            self.config.tool_timeout,
            tool.execute(call.arguments.clone(), &meta)
        ).await.map_err(|_| CoreError::ToolTimeout {
            tool_name: call.name.clone(),
            timeout: self.config.tool_timeout,
        })??;

        Ok(ToolResponse {
            tool_call_id: call.id.clone(),
            content: result.to_string(),
        })
    }

    /// Execute multiple tool calls
    pub async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<Result<ToolResponse, CoreError>> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(self.execute_tool(call).await);
        }
        results
    }
}
```

### Step 3: Add ToolTimeout error variant

```rust
// In crates/pattern_core/src/error.rs

#[error("Tool '{tool_name}' timed out after {timeout:?}")]
#[diagnostic(code(pattern_core::tool_timeout))]
ToolTimeout {
    tool_name: String,
    timeout: std::time::Duration,
},
```

### Step 4: Commit

```bash
git add crates/pattern_core/src/runtime/mod.rs crates/pattern_core/src/error.rs
git commit -m "feat(runtime): add tool execution with permission checks"
```

---

## Task C2: Port MessageRouter to pattern_db

**Files:**
- Create: `crates/pattern_core/src/runtime/router.rs`
- Modify: `crates/pattern_db/src/queries/mod.rs` (add agent lookup)

### Step 1: Write failing test for router

```rust
#[tokio::test]
async fn test_router_send_to_user() {
    let runtime = create_test_runtime().await;

    // Register CLI endpoint
    let endpoint = Arc::new(MockEndpoint::new());
    runtime.router().register_endpoint("cli", endpoint.clone()).await;
    runtime.router().set_default_user_endpoint(endpoint.clone()).await;

    let result = runtime.send_message(
        MessageTarget::user(None),
        "Hello user!",
        None,
    ).await;

    assert!(result.is_ok());
    assert!(endpoint.received().contains(&"Hello user!".to_string()));
}
```

### Step 2: Create new router using pattern_db

```rust
// crates/pattern_core/src/runtime/router.rs

//! AgentMessageRouter: Routes messages from agents to destinations
//!
//! Ported from context/message_router.rs to use pattern_db instead of SurrealDB.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::error::{CoreError, Result};
use crate::id::{AgentId, GroupId, UserId};
use crate::message::Message;
use crate::tool::builtin::{MessageTarget, TargetType};

/// Message origin (kept from old implementation)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageOrigin {
    DataSource {
        source_id: String,
        source_type: String,
        item_id: Option<String>,
        cursor: Option<Value>,
    },
    Discord {
        server_id: String,
        channel_id: String,
        user_id: String,
        message_id: String,
    },
    Cli {
        session_id: String,
        command: Option<String>,
    },
    Api {
        client_id: String,
        request_id: String,
        endpoint: String,
    },
    Bluesky {
        handle: String,
        did: String,
        post_uri: Option<String>,
        is_mention: bool,
        is_reply: bool,
    },
    Agent {
        agent_id: AgentId,
        name: String,
        reason: String,
    },
    Other {
        origin_type: String,
        source_id: String,
        metadata: Value,
    },
}

impl MessageOrigin {
    pub fn description(&self) -> String {
        match self {
            Self::DataSource { source_id, source_type, .. } =>
                format!("Data from {} ({})", source_id, source_type),
            Self::Discord { user_id, server_id, channel_id, .. } =>
                format!("Discord message from {} in {}/{}", user_id, server_id, channel_id),
            Self::Cli { session_id, command } =>
                format!("CLI session {} - {}", session_id, command.as_deref().unwrap_or("interactive")),
            Self::Api { client_id, endpoint, .. } =>
                format!("API request from {} to {}", client_id, endpoint),
            Self::Bluesky { handle, is_mention, is_reply, .. } => {
                if *is_mention { format!("Mentioned by @{}", handle) }
                else if *is_reply { format!("Reply from @{}", handle) }
                else { format!("Post from @{}", handle) }
            }
            Self::Agent { name, reason, .. } =>
                format!("{} ({})", name, reason),
            Self::Other { origin_type, source_id, .. } =>
                format!("{} from {}", origin_type, source_id),
        }
    }

    pub fn wrap_content(&self, content: String) -> String {
        match self {
            Self::Agent { name, reason, .. } => format!(
                "Message from agent: {}, reason: {}, content:\n\n{}\n\
                 You may opt to reply, if you haven't already replied to them recently.\n\
                 Only reply if you have something new to add.",
                name, reason, content
            ),
            _ => format!("{}\n\n{}", self.description(), content),
        }
    }
}

/// Trait for message delivery endpoints
#[async_trait::async_trait]
pub trait MessageEndpoint: Send + Sync {
    async fn send(
        &self,
        message: Message,
        metadata: Option<Value>,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>>;

    fn endpoint_type(&self) -> &'static str;
}

/// Routes messages from agents to their destinations
#[derive(Clone)]
pub struct AgentMessageRouter {
    agent_id: AgentId,
    name: String,
    pool: SqlitePool,
    endpoints: Arc<RwLock<HashMap<String, Arc<dyn MessageEndpoint>>>>,
    default_user_endpoint: Arc<RwLock<Option<Arc<dyn MessageEndpoint>>>>,
    recent_messages: Arc<RwLock<HashMap<String, Instant>>>,
    cooldown: std::time::Duration,
}

impl AgentMessageRouter {
    pub fn new(agent_id: AgentId, name: String, pool: SqlitePool) -> Self {
        Self {
            agent_id,
            name,
            pool,
            endpoints: Arc::new(RwLock::new(HashMap::new())),
            default_user_endpoint: Arc::new(RwLock::new(None)),
            recent_messages: Arc::new(RwLock::new(HashMap::new())),
            cooldown: std::time::Duration::from_secs(30),
        }
    }

    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    pub fn agent_name(&self) -> &str {
        &self.name
    }

    pub async fn register_endpoint(&self, name: impl Into<String>, endpoint: Arc<dyn MessageEndpoint>) {
        let mut endpoints = self.endpoints.write().await;
        endpoints.insert(name.into(), endpoint);
    }

    pub async fn set_default_user_endpoint(&self, endpoint: Arc<dyn MessageEndpoint>) {
        *self.default_user_endpoint.write().await = Some(endpoint);
    }

    /// Resolve agent name to ID using pattern_db
    pub async fn resolve_agent_name(&self, name: &str) -> Result<AgentId> {
        let agent = pattern_db::queries::get_agent_by_name(&self.pool, name)
            .await
            .map_err(|e| CoreError::SqliteError(e.into()))?
            .ok_or_else(|| CoreError::AgentNotFound {
                identifier: name.to_string(),
            })?;

        Ok(AgentId::new(agent.id))
    }

    /// Resolve group name to ID using pattern_db
    pub async fn resolve_group_name(&self, name: &str) -> Result<GroupId> {
        let group = pattern_db::queries::get_group_by_name(&self.pool, name)
            .await
            .map_err(|e| CoreError::SqliteError(e.into()))?
            .ok_or_else(|| CoreError::GroupNotFound {
                identifier: name.to_string(),
            })?;

        Ok(GroupId::new(group.id))
    }

    /// Send a message to the specified target
    pub async fn send_message(
        &self,
        target: MessageTarget,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        match target.target_type {
            TargetType::User => {
                self.send_to_user(content, metadata, origin).await
            }
            TargetType::Agent => {
                let agent_id = self.resolve_target_agent(&target).await?;
                self.send_to_agent(agent_id, content, metadata, origin).await
            }
            TargetType::Group => {
                let group_id = self.resolve_target_group(&target).await?;
                self.send_to_group(group_id, content, metadata, origin).await
            }
            TargetType::Channel => {
                self.send_to_channel(content, metadata, origin).await
            }
            TargetType::Bluesky => {
                self.send_to_bluesky(target.target_id, content, metadata, origin).await
            }
        }
    }

    async fn resolve_target_agent(&self, target: &MessageTarget) -> Result<AgentId> {
        let target_str = target.target_id.as_ref().ok_or_else(|| {
            CoreError::InvalidFormat {
                data_type: "MessageTarget".into(),
                details: "Agent name or ID required".into(),
            }
        })?;

        // Try parsing as UUID first
        if let Ok(uuid) = uuid::Uuid::parse_str(target_str) {
            return Ok(AgentId::from_uuid(uuid));
        }

        // Fall back to name resolution
        self.resolve_agent_name(target_str).await
    }

    async fn resolve_target_group(&self, target: &MessageTarget) -> Result<GroupId> {
        let target_str = target.target_id.as_ref().ok_or_else(|| {
            CoreError::InvalidFormat {
                data_type: "MessageTarget".into(),
                details: "Group name or ID required".into(),
            }
        })?;

        // Try parsing as UUID first
        if let Ok(uuid) = uuid::Uuid::parse_str(target_str) {
            return Ok(GroupId::from_uuid(uuid));
        }

        // Fall back to name resolution
        self.resolve_group_name(target_str).await
    }

    async fn send_to_user(
        &self,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        let endpoint = self.default_user_endpoint.read().await;
        let endpoint = endpoint.as_ref().ok_or_else(|| CoreError::NoEndpointConfigured {
            target_type: "user".into(),
        })?;

        let message = Message::assistant(content);
        endpoint.send(message, metadata, origin.as_ref()).await
    }

    async fn send_to_agent(
        &self,
        target_agent_id: AgentId,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        // Anti-loop check
        let pair_key = {
            let mut ids = [self.agent_id.to_string(), target_agent_id.to_string()];
            ids.sort();
            ids.join(":")
        };

        {
            let recent = self.recent_messages.read().await;
            if let Some(last_time) = recent.get(&pair_key) {
                if last_time.elapsed() < self.cooldown {
                    return Err(CoreError::RateLimited {
                        target: target_agent_id.to_string(),
                        cooldown: self.cooldown,
                    });
                }
            }
        }

        // Update recent messages
        {
            let mut recent = self.recent_messages.write().await;
            recent.insert(pair_key, Instant::now());
        }

        // Queue message for target agent using pattern_db
        let queued_msg = pattern_db::models::QueuedMessage {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: target_agent_id.to_string(),
            content,
            origin: origin.map(|o| serde_json::to_value(o).ok()).flatten(),
            metadata,
            priority: 0,
            created_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_queued_message(&self.pool, &queued_msg)
            .await
            .map_err(|e| CoreError::SqliteError(e.into()))?;

        Ok(Some(queued_msg.id))
    }

    async fn send_to_group(
        &self,
        _group_id: GroupId,
        _content: String,
        _metadata: Option<Value>,
        _origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        // TODO: Implement group messaging via pattern_db
        Err(CoreError::NotImplemented {
            feature: "Group messaging".into(),
        })
    }

    async fn send_to_channel(
        &self,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        // Look up channel endpoint from metadata
        let endpoints = self.endpoints.read().await;
        let endpoint = endpoints.get("discord")
            .or_else(|| endpoints.get("channel"))
            .ok_or_else(|| CoreError::NoEndpointConfigured {
                target_type: "channel".into(),
            })?;

        let message = Message::assistant(content);
        endpoint.send(message, metadata, origin.as_ref()).await
    }

    async fn send_to_bluesky(
        &self,
        _target_id: Option<String>,
        content: String,
        metadata: Option<Value>,
        origin: Option<MessageOrigin>,
    ) -> Result<Option<String>> {
        let endpoints = self.endpoints.read().await;
        let endpoint = endpoints.get("bluesky")
            .ok_or_else(|| CoreError::NoEndpointConfigured {
                target_type: "bluesky".into(),
            })?;

        let message = Message::assistant(content);
        endpoint.send(message, metadata, origin.as_ref()).await
    }
}
```

### Step 3: Add required error variants

```rust
// In crates/pattern_core/src/error.rs

#[error("Agent not found: {identifier}")]
#[diagnostic(code(pattern_core::agent_not_found))]
AgentNotFound { identifier: String },

#[error("Group not found: {identifier}")]
#[diagnostic(code(pattern_core::group_not_found))]
GroupNotFound { identifier: String },

#[error("No endpoint configured for target type: {target_type}")]
#[diagnostic(code(pattern_core::no_endpoint))]
NoEndpointConfigured { target_type: String },

#[error("Rate limited: message to {target} blocked (cooldown: {cooldown:?})")]
#[diagnostic(code(pattern_core::rate_limited))]
RateLimited {
    target: String,
    cooldown: std::time::Duration,
},

#[error("Feature not implemented: {feature}")]
#[diagnostic(code(pattern_core::not_implemented))]
NotImplemented { feature: String },
```

### Step 4: Add pattern_db queries for agent/group lookup

```rust
// In crates/pattern_db/src/queries/agent.rs (create if needed)

/// Get agent by name
pub async fn get_agent_by_name(pool: &SqlitePool, name: &str) -> DbResult<Option<Agent>> {
    let agent = sqlx::query_as!(
        Agent,
        r#"SELECT id, name, agent_type, status, created_at, updated_at
           FROM agents WHERE name = ?"#,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(agent)
}

/// Get group by name
pub async fn get_group_by_name(pool: &SqlitePool, name: &str) -> DbResult<Option<Group>> {
    let group = sqlx::query_as!(
        Group,
        r#"SELECT id, name, pattern, created_at FROM groups WHERE name = ?"#,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(group)
}
```

### Step 5: Commit

```bash
git add crates/pattern_core/src/runtime/router.rs crates/pattern_core/src/error.rs crates/pattern_db/src/queries/
git commit -m "feat(runtime): port MessageRouter to pattern_db"
```

---

## Task C3: Add Message Operations and Context Building

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs`

### Step 1: Write failing test for store_message

```rust
#[tokio::test]
async fn test_runtime_store_message() {
    let runtime = create_test_runtime().await;

    let message = Message::user("Hello, world!");
    runtime.store_message(&message).await.unwrap();

    let recent = runtime.messages().get_recent(1).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].content.to_string(), "Hello, world!");
}
```

### Step 2: Add message operations to Runtime

```rust
impl AgentRuntime {
    /// Store a message
    pub async fn store_message(&self, message: &Message) -> Result<(), CoreError> {
        self.messages.store(message).await
    }

    /// Get recent messages
    pub async fn get_recent_messages(&self, limit: usize) -> Result<Vec<Message>, CoreError> {
        self.messages.get_recent(limit).await
    }

    /// Get message store (for direct access)
    pub fn messages(&self) -> &MessageStore {
        &self.messages
    }

    /// Build context for model request
    pub async fn build_request(&self) -> Result<Request, CoreError> {
        let builder = ContextBuilder::new(self.memory.as_ref(), &self.config.context_config)
            .for_agent(&self.agent_id.to_string())
            .with_messages(&self.messages)
            .with_tools(&self.tools);

        builder.build().await
    }

    /// Send a message via router
    pub async fn send_message(
        &self,
        target: MessageTarget,
        content: impl Into<String>,
        metadata: Option<Value>,
    ) -> Result<Option<String>, CoreError> {
        let origin = MessageOrigin::Agent {
            agent_id: self.agent_id.clone(),
            name: self.agent_name.clone(),
            reason: "agent initiated".into(),
        };

        self.router.send_message(target, content.into(), metadata, Some(origin)).await
    }
}
```

### Step 3: Add search delegation

```rust
impl AgentRuntime {
    /// Search across memory and messages
    pub async fn search(
        &self,
        query: &str,
        options: crate::memory_v2::SearchOptions,
    ) -> Result<Vec<crate::memory_v2::MemorySearchResult>, CoreError> {
        self.memory.search(&self.agent_id.to_string(), query, options)
            .await
            .map_err(CoreError::MemoryError)
    }
}
```

### Step 4: Commit

```bash
git add crates/pattern_core/src/runtime/mod.rs
git commit -m "feat(runtime): add message operations and context building"
```

---

## Success Criteria

- [ ] `cargo check -p pattern-core` passes
- [ ] `cargo test -p pattern-core runtime` passes
- [ ] AgentRuntime builds with all required dependencies
- [ ] Tool execution works with permission checks
- [ ] MessageRouter uses pattern_db (not SurrealDB)
- [ ] Agent/group name resolution works
- [ ] Anti-loop protection for agent-to-agent messages
- [ ] Message storage delegates to MessageStore
- [ ] `build_request()` delegates to ContextBuilder, returns `Request`
- [ ] Search delegates to MemoryStore

---

## Notes

- **Router migration:** The key change is replacing SurrealDB queries with pattern_db queries
- **Permission broker:** Kept as-is (already uses tokio channels, not DB)
- **Endpoints:** Interface unchanged, implementations stay in consumers (discord, cli, etc.)
- **Cooldown:** 30 seconds between rapid agent-to-agent messages (anti-loop)
- **Error variants:** Added specific errors for better debugging
