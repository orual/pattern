# Chunk 3: Agent Rework

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build new DatabaseAgent using SQLite backend (db_v2) and Loro-backed memory (memory_v2).

**Architecture:** New `db_agent_v2.rs` alongside existing agent, uses MemoryStore trait, ConstellationDb for persistence.

**Tech Stack:** db_v2, memory_v2, pattern_db

**Depends On:** Chunk 1 (db_v2), Chunk 2 (memory_v2)

---

## Philosophy

**DO:**
- Create `db_agent_v2.rs` alongside `db_agent.rs`
- Use dependency injection for MemoryStore
- Keep existing Agent trait
- Build incrementally, testing each piece

**DON'T:**
- Modify existing db_agent.rs
- Change the Agent trait interface
- Remove any existing code
- Try to make v1 and v2 agents interoperate

---

## As-Built Reconciliation (2025-12-23)

### Naming Difference: MemoryCache not SqliteMemoryStore

The plan references `SqliteMemoryStore` but the actual implementation is `MemoryCache` in `memory_v2/cache.rs`. Same functionality, different name.

**Action:** Replace all `SqliteMemoryStore` references with `MemoryCache`.

### API Pattern Difference: Document-Based Operations

The plan assumes direct methods on MemoryStore trait:
```rust
// Plan assumes:
store.set_block_content(agent_id, label, content, source)
store.append_block_content(agent_id, label, content, source)
```

But actual MemoryStore trait requires:
```rust
// Actual pattern:
let doc = store.get_block(agent_id, label).await?;
doc.set_text(content)?;
store.mark_dirty(agent_id, label);
store.persist_block(agent_id, label).await?;
```

**Action:** Task 3 (AgentHandleV2) convenience methods must follow the actual pattern.

### Task 4 (MessageManagerV2): REMOVE ENTIRELY

MessageManagerV2 is over-engineered. It's just a struct holding `(agent_id, db)` and wrapping query calls. That adds no value.

**Decision:** Delete Task 4. Agents should call `pattern_db::queries::` directly:
- `queries::get_messages(pool, agent_id, limit)`
- `queries::create_message(pool, &msg)`
- `queries::archive_messages(pool, agent_id, before_position)`
- etc.

### Use Snowflake IDs (NOT Timestamps)

The plan shows `generate_position()` using timestamp hex. That's wrong - the system already uses Snowflake IDs for message ordering.

**Action:** Use existing Snowflake ID generation for message positions.

### db_v2: What SHOULD Be There

The gap isn't query wrappers - it's the bridge between:
- `EmbeddingProvider` (pattern_core) - generates embeddings from text
- `pattern_db::vector` - stores and searches embeddings

**What db_v2 should provide:**
```rust
/// Embed content and store it
pub async fn embed_and_store<E: EmbeddingProvider>(
    db: &ConstellationDb,
    embedder: &E,
    content_type: ContentType,
    content_id: &str,
    text: &str,
) -> DbResult<i64>

/// Search with text query (embeds it first)
pub async fn semantic_search<E: EmbeddingProvider>(
    db: &ConstellationDb,
    embedder: &E,
    query: &str,
    content_type: Option<ContentType>,
    limit: i64,
) -> DbResult<Vec<VectorSearchResult>>
```

This is substantive - it combines two layers. Add this when needed, not preemptively.

### Task 6: Missing BlockSchema Parameter

The `create_block()` calls in Task 6 are missing the `schema: BlockSchema` parameter.

**Correct signature:**
```rust
async fn create_block(
    &self,
    agent_id: &str,
    label: &str,
    description: &str,
    block_type: BlockType,
    schema: BlockSchema,  // <- Add this!
    char_limit: usize,
) -> MemoryResult<String>
```

**Action:** Add `BlockSchema::Text` as the schema parameter in all create_block calls.

### ChangeSource Type

The plan references `ChangeSource::Agent(agent_id)` but this type doesn't exist in memory_v2.

**Decision:** Add ChangeSource to `memory_v2/types.rs`:
```rust
/// Source of a memory change (for audit trails)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeSource {
    /// Change made by an agent
    Agent(String),
    /// Change made by a human/partner
    Human(String),
    /// Change made by system (e.g., compression)
    System,
    /// Change from external integration
    Integration(String),
}
```

This enables tracking who/what modified memory blocks for debugging and audit purposes.

### Task Order

Recommended execution order:
1. **Pre-req:** ✅ ChangeSource added to memory_v2/types.rs
2. Task 1: Module structure (update SqliteMemoryStore → MemoryCache)
3. Task 2: AgentRecordV2 (verify against pattern_db models)
4. Task 6: DefaultBlocks (add BlockSchema parameter)
5. Task 3: AgentHandleV2 (rewrite with correct API pattern)
6. ~~Task 4: MessageManagerV2~~ **REMOVED** - use queries directly
7. Task 5: Full implementation (includes direct query calls for messages)

---

## Task 1: Create Agent V2 Module Structure

**Files:**
- Create: `crates/pattern_core/src/agent/impls/db_agent_v2.rs`
- Modify: `crates/pattern_core/src/agent/impls/mod.rs`

**Step 1: Create db_agent_v2.rs skeleton**

```rust
//! V2 DatabaseAgent using SQLite and Loro-backed memory
//!
//! Key differences from v1:
//! - Uses ConstellationDb (SQLite) instead of Surreal
//! - Uses MemoryStore trait for memory operations
//! - Agent-scoped memory (not user-scoped)
//! - Simplified structure

use std::sync::Arc;
use tokio::sync::RwLock;
use async_trait::async_trait;

use crate::db_v2::{ConstellationDb, Agent as DbAgent, AgentStatus};
use crate::memory_v2::{MemoryStore, SqliteMemoryStore, MemoryContextBuilder};
use crate::agent::Agent;
use crate::context::AgentContext;
use crate::tool::ToolRegistry;
use crate::error::CoreError;
use crate::id::AgentId;

/// V2 DatabaseAgent with SQLite backend
pub struct DatabaseAgent<M> {
    /// Agent ID (cached for sync access)
    id: AgentId,

    /// Agent name (cached)
    name: String,

    /// Database connection
    db: Arc<ConstellationDb>,

    /// Memory store
    memory: Arc<dyn MemoryStore>,

    /// Model provider
    model: Arc<RwLock<M>>,

    /// Agent context (runtime state)
    context: Arc<RwLock<AgentContext>>,

    /// Tool registry
    tools: Arc<ToolRegistry>,
}

impl<M> DatabaseAgent<M> {
    /// Create a new V2 agent from database record
    pub async fn from_db(
        db: Arc<ConstellationDb>,
        agent_id: &str,
        model: M,
        tools: Arc<ToolRegistry>,
    ) -> Result<Self, CoreError> {
        // Load agent record from database
        let record = crate::db_v2::get_agent(&db, agent_id)
            .await
            .map_err(|e| CoreError::database(format!("Failed to load agent: {}", e)))?
            .ok_or_else(|| CoreError::agent_not_found(agent_id))?;

        // Create memory store
        let memory = Arc::new(SqliteMemoryStore::new(db.clone()));

        // Build initial context
        let context = AgentContext::new(/* ... */);

        Ok(Self {
            id: AgentId::new(&record.id),
            name: record.name,
            db,
            memory,
            model: Arc::new(RwLock::new(model)),
            context: Arc::new(RwLock::new(context)),
            tools,
        })
    }

    /// Get agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// Get agent name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get memory store
    pub fn memory(&self) -> &Arc<dyn MemoryStore> {
        &self.memory
    }

    /// Get database connection
    pub fn db(&self) -> &Arc<ConstellationDb> {
        &self.db
    }
}

// Placeholder for Agent trait implementation
// Will be filled in subsequent tasks

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_agent_creation_placeholder() {
        // Placeholder - actual tests after more implementation
        assert!(true);
    }
}
```

**Step 2: Add to impls/mod.rs**

```rust
pub mod db_agent_v2;
pub use db_agent_v2::DatabaseAgent;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS (may need stub implementations)

**Step 4: Commit**

```bash
git add crates/pattern_core/src/agent/impls/
git commit -m "feat(pattern_core): add DatabaseAgent skeleton"
```

---

## Task 2: Add Agent Record Type Conversion

**Files:**
- Create: `crates/pattern_core/src/agent/entity_v2.rs`
- Modify: `crates/pattern_core/src/agent/mod.rs`

**Step 1: Create entity_v2.rs**

```rust
//! V2 Agent entity types
//!
//! Provides conversion between db_v2 models and pattern_core types.
//! No Entity macro - just plain Rust types.

use crate::db_v2::{Agent as DbAgent, AgentStatus, AgentGroup as DbGroup};
use crate::id::{AgentId, GroupId};
use crate::agent::AgentType;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Agent configuration for v2
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigV2 {
    /// Model provider (anthropic, openai, google)
    pub model_provider: String,

    /// Model name (claude-3-5-sonnet, gpt-4o, etc.)
    pub model_name: String,

    /// System prompt / base instructions
    pub system_prompt: String,

    /// Maximum messages in context
    pub max_messages: usize,

    /// Compression threshold
    pub compression_threshold: usize,

    /// Memory character limit per block
    pub memory_char_limit: usize,

    /// Enable thinking/reasoning mode
    pub enable_thinking: bool,

    /// Temperature for generation
    pub temperature: Option<f32>,

    /// Max tokens for response
    pub max_tokens: Option<usize>,
}

impl Default for AgentConfigV2 {
    fn default() -> Self {
        Self {
            model_provider: "anthropic".into(),
            model_name: "claude-3-5-sonnet-20241022".into(),
            system_prompt: String::new(),
            max_messages: 50,
            compression_threshold: 30,
            memory_char_limit: 5000,
            enable_thinking: false,
            temperature: None,
            max_tokens: None,
        }
    }
}

/// Agent record for v2 (simplified from v1)
#[derive(Debug, Clone)]
pub struct AgentRecordV2 {
    pub id: AgentId,
    pub name: String,
    pub description: Option<String>,
    pub agent_type: AgentType,
    pub config: AgentConfigV2,
    pub enabled_tools: Vec<String>,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentRecordV2 {
    /// Create from database model
    pub fn from_db(db_agent: DbAgent) -> Self {
        let config: AgentConfigV2 = serde_json::from_value(db_agent.config.0.clone())
            .unwrap_or_default();

        Self {
            id: AgentId::new(&db_agent.id),
            name: db_agent.name,
            description: db_agent.description,
            agent_type: AgentType::General, // TODO: Store in DB
            config: AgentConfigV2 {
                model_provider: db_agent.model_provider,
                model_name: db_agent.model_name,
                system_prompt: db_agent.system_prompt,
                ..config
            },
            enabled_tools: db_agent.enabled_tools.0,
            status: db_agent.status,
            created_at: db_agent.created_at,
            updated_at: db_agent.updated_at,
        }
    }

    /// Convert to database model
    pub fn to_db(&self) -> DbAgent {
        DbAgent {
            id: self.id.to_string(),
            name: self.name.clone(),
            description: self.description.clone(),
            model_provider: self.config.model_provider.clone(),
            model_name: self.config.model_name.clone(),
            system_prompt: self.config.system_prompt.clone(),
            config: sqlx::types::Json(serde_json::to_value(&self.config).unwrap()),
            enabled_tools: sqlx::types::Json(self.enabled_tools.clone()),
            tool_rules: None,
            status: self.status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Create a new agent with default configuration
pub fn new_agent(name: &str, system_prompt: &str) -> AgentRecordV2 {
    AgentRecordV2 {
        id: AgentId::generate(),
        name: name.to_string(),
        description: None,
        agent_type: AgentType::General,
        config: AgentConfigV2 {
            system_prompt: system_prompt.to_string(),
            ..Default::default()
        },
        enabled_tools: vec![
            "send_message".into(),
            "context".into(),
            "recall".into(),
            "search".into(),
        ],
        status: AgentStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AgentConfigV2::default();
        assert_eq!(config.model_provider, "anthropic");
        assert_eq!(config.max_messages, 50);
    }

    #[test]
    fn test_new_agent() {
        let agent = new_agent("TestAgent", "You are a test agent.");
        assert_eq!(agent.name, "TestAgent");
        assert!(agent.enabled_tools.contains(&"send_message".to_string()));
    }
}
```

**Step 2: Add to agent/mod.rs**

```rust
pub mod entity_v2;
pub use entity_v2::*;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/agent/
git commit -m "feat(pattern_core): add AgentRecordV2 with db_v2 conversions"
```

---

## Task 3: Implement Agent Handle V2

**Files:**
- Create: `crates/pattern_core/src/agent/handle_v2.rs`
- Modify: `crates/pattern_core/src/agent/mod.rs`

**Step 1: Create handle_v2.rs**

```rust
//! V2 Agent Handle
//!
//! Cheap-to-clone handle for passing to tools.
//! Provides access to memory and message operations.

use std::sync::Arc;
use crate::db_v2::ConstellationDb;
use crate::memory_v2::{MemoryStore, BlockType, ChangeSource, MemoryError};
use crate::id::AgentId;

/// Handle to an agent for tool access
///
/// Designed to be cheaply cloneable (all Arc internally).
#[derive(Clone)]
pub struct AgentHandleV2 {
    agent_id: AgentId,
    agent_name: String,
    db: Arc<ConstellationDb>,
    memory: Arc<dyn MemoryStore>,
}

impl AgentHandleV2 {
    pub fn new(
        agent_id: AgentId,
        agent_name: String,
        db: Arc<ConstellationDb>,
        memory: Arc<dyn MemoryStore>,
    ) -> Self {
        Self {
            agent_id,
            agent_name,
            db,
            memory,
        }
    }

    /// Get agent ID
    pub fn id(&self) -> &AgentId {
        &self.agent_id
    }

    /// Get agent name
    pub fn name(&self) -> &str {
        &self.agent_name
    }

    /// Get memory store
    pub fn memory(&self) -> &Arc<dyn MemoryStore> {
        &self.memory
    }

    /// Get database connection
    pub fn db(&self) -> &Arc<ConstellationDb> {
        &self.db
    }

    // Convenience methods for tools

    /// Get block content by label
    pub async fn get_memory(&self, label: &str) -> Result<Option<String>, MemoryError> {
        self.memory.get_block_content(&self.agent_id.to_string(), label).await
    }

    /// Set block content
    pub async fn set_memory(&self, label: &str, content: &str) -> Result<(), MemoryError> {
        self.memory.set_block_content(
            &self.agent_id.to_string(),
            label,
            content,
            ChangeSource::Agent(self.agent_id.to_string()),
        ).await
    }

    /// Append to block content
    pub async fn append_memory(&self, label: &str, content: &str) -> Result<(), MemoryError> {
        self.memory.append_block_content(
            &self.agent_id.to_string(),
            label,
            content,
            ChangeSource::Agent(self.agent_id.to_string()),
        ).await
    }

    /// Insert archival memory
    pub async fn insert_archival(&self, content: &str) -> Result<String, MemoryError> {
        self.memory.insert_archival(&self.agent_id.to_string(), content, None).await
    }

    /// Search archival memory
    pub async fn search_archival(&self, query: &str, limit: usize) -> Result<Vec<String>, MemoryError> {
        let entries = self.memory.search_archival(&self.agent_id.to_string(), query, limit).await?;
        Ok(entries.into_iter().map(|e| e.content).collect())
    }

    /// List memory block labels
    pub async fn list_memory_labels(&self) -> Result<Vec<String>, MemoryError> {
        let blocks = self.memory.list_blocks(&self.agent_id.to_string()).await?;
        Ok(blocks.into_iter().map(|b| b.label).collect())
    }

    /// Check if block exists
    pub async fn has_memory(&self, label: &str) -> Result<bool, MemoryError> {
        let meta = self.memory.get_block_metadata(&self.agent_id.to_string(), label).await?;
        Ok(meta.is_some())
    }
}

impl std::fmt::Debug for AgentHandleV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHandleV2")
            .field("agent_id", &self.agent_id)
            .field("agent_name", &self.agent_name)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::SqliteMemoryStore;

    async fn test_handle() -> AgentHandleV2 {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(ConstellationDb::open(dir.path().join("test.db")).await.unwrap());
        let memory = Arc::new(SqliteMemoryStore::new(db.clone()));

        // Create a test block
        memory.create_block(
            "test_agent",
            "persona",
            "Test persona",
            BlockType::Core,
            "I am a test agent.",
            5000,
        ).await.unwrap();

        AgentHandleV2::new(
            AgentId::new("test_agent"),
            "TestAgent".into(),
            db,
            memory,
        )
    }

    #[tokio::test]
    async fn test_get_memory() {
        let handle = test_handle().await;
        let content = handle.get_memory("persona").await.unwrap();
        assert_eq!(content, Some("I am a test agent.".to_string()));
    }

    #[tokio::test]
    async fn test_append_memory() {
        let handle = test_handle().await;
        handle.append_memory("persona", " I help with testing.").await.unwrap();
        let content = handle.get_memory("persona").await.unwrap();
        assert_eq!(content, Some("I am a test agent. I help with testing.".to_string()));
    }

    #[tokio::test]
    async fn test_list_labels() {
        let handle = test_handle().await;
        let labels = handle.list_memory_labels().await.unwrap();
        assert!(labels.contains(&"persona".to_string()));
    }
}
```

**Step 2: Add to agent/mod.rs**

```rust
pub mod handle_v2;
pub use handle_v2::AgentHandleV2;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core agent::handle_v2`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/agent/
git commit -m "feat(pattern_core): add AgentHandleV2 for tool access"
```

---

## Task 4: Implement Message Persistence V2

**Files:**
- Create: `crates/pattern_core/src/agent/messages_v2.rs`
- Modify: `crates/pattern_core/src/agent/mod.rs`

**Step 1: Create messages_v2.rs**

```rust
//! V2 Message persistence layer
//!
//! Handles storing and loading message history using SQLite.

use crate::db_v2::{self, ConstellationDb, Message as DbMessage, MessageRole, ArchiveSummary};
use crate::message::{Message, MessageContent, ChatRole};
use crate::id::AgentId;
use crate::error::CoreError;
use chrono::Utc;
use std::sync::Arc;

/// Message manager for an agent
pub struct MessageManagerV2 {
    agent_id: String,
    db: Arc<ConstellationDb>,
}

impl MessageManagerV2 {
    pub fn new(agent_id: &AgentId, db: Arc<ConstellationDb>) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            db,
        }
    }

    /// Load recent messages (non-archived)
    pub async fn load_recent(&self, limit: usize) -> Result<Vec<Message>, CoreError> {
        let db_messages = db_v2::get_messages(&self.db, &self.agent_id, limit as i64)
            .await
            .map_err(|e| CoreError::database(format!("Failed to load messages: {}", e)))?;

        Ok(db_messages.into_iter().map(Self::from_db).collect())
    }

    /// Load all messages including archived
    pub async fn load_all(&self, limit: usize) -> Result<Vec<Message>, CoreError> {
        let db_messages = db_v2::get_messages_with_archived(&self.db, &self.agent_id, limit as i64)
            .await
            .map_err(|e| CoreError::database(format!("Failed to load messages: {}", e)))?;

        Ok(db_messages.into_iter().map(Self::from_db).collect())
    }

    /// Store a new message
    pub async fn store(&self, message: &Message, batch_id: Option<&str>, sequence: Option<i64>) -> Result<(), CoreError> {
        let position = Self::generate_position();
        let db_message = self.to_db(message, &position, batch_id, sequence);

        db_v2::create_message(&self.db, &db_message)
            .await
            .map_err(|e| CoreError::database(format!("Failed to store message: {}", e)))?;

        Ok(())
    }

    /// Archive old messages
    pub async fn archive_before(&self, position: &str) -> Result<u64, CoreError> {
        db_v2::archive_messages(&self.db, &self.agent_id, position)
            .await
            .map_err(|e| CoreError::database(format!("Failed to archive messages: {}", e)))
    }

    /// Store archive summary
    pub async fn store_summary(
        &self,
        summary: &str,
        start_position: &str,
        end_position: &str,
        message_count: usize,
    ) -> Result<(), CoreError> {
        let db_summary = ArchiveSummary {
            id: format!("sum_{}", uuid::Uuid::new_v4()),
            agent_id: self.agent_id.clone(),
            summary: summary.to_string(),
            start_position: start_position.to_string(),
            end_position: end_position.to_string(),
            message_count: message_count as i64,
            previous_summary_id: None,
            depth: 0,
            created_at: Utc::now(),
        };

        db_v2::create_archive_summary(&self.db, &db_summary)
            .await
            .map_err(|e| CoreError::database(format!("Failed to store summary: {}", e)))?;

        Ok(())
    }

    /// Get archive summaries
    pub async fn get_summaries(&self) -> Result<Vec<ArchiveSummary>, CoreError> {
        db_v2::get_archive_summaries(&self.db, &self.agent_id)
            .await
            .map_err(|e| CoreError::database(format!("Failed to load summaries: {}", e)))
    }

    // Conversion helpers

    fn from_db(db: DbMessage) -> Message {
        let role = match db.role {
            MessageRole::User => ChatRole::User,
            MessageRole::Assistant => ChatRole::Assistant,
            MessageRole::System => ChatRole::System,
            MessageRole::Tool => ChatRole::Tool,
        };

        let content = if let Some(text) = db.content {
            MessageContent::Text(text)
        } else if let Some(tool_result) = db.tool_result {
            // Handle tool response
            MessageContent::Text(serde_json::to_string(&tool_result.0).unwrap_or_default())
        } else {
            MessageContent::Text(String::new())
        };

        Message {
            id: Some(db.id.into()),
            role,
            content,
            position: Some(db.position.into()),
            batch: db.batch_id.map(Into::into),
            sequence_num: db.sequence_in_batch.map(|s| s as u32),
            batch_type: None, // TODO: Add to DB schema if needed
            metadata: Default::default(),
            embedding: None,
            created_at: db.created_at,
        }
    }

    fn to_db(&self, msg: &Message, position: &str, batch_id: Option<&str>, sequence: Option<i64>) -> DbMessage {
        let role = match msg.role {
            ChatRole::User => MessageRole::User,
            ChatRole::Assistant => MessageRole::Assistant,
            ChatRole::System => MessageRole::System,
            ChatRole::Tool => MessageRole::Tool,
        };

        let content = match &msg.content {
            MessageContent::Text(t) => Some(t.clone()),
            _ => None, // TODO: Handle other content types
        };

        DbMessage {
            id: msg.id.as_ref().map(|i| i.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            agent_id: self.agent_id.clone(),
            position: position.to_string(),
            batch_id: batch_id.map(String::from),
            sequence_in_batch: sequence,
            role,
            content,
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            source: None,
            source_metadata: None,
            is_archived: false,
            created_at: msg.created_at,
        }
    }

    fn generate_position() -> String {
        // Use Snowflake ID for ordering
        // For now, use timestamp-based ID
        format!("{:016x}", Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_manager() -> MessageManagerV2 {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(ConstellationDb::open(dir.path().join("test.db")).await.unwrap());
        MessageManagerV2::new(&AgentId::new("test_agent"), db)
    }

    #[tokio::test]
    async fn test_store_and_load() {
        let manager = test_manager().await;

        let msg = Message {
            id: None,
            role: ChatRole::User,
            content: MessageContent::Text("Hello!".into()),
            position: None,
            batch: None,
            sequence_num: None,
            batch_type: None,
            metadata: Default::default(),
            embedding: None,
            created_at: Utc::now(),
        };

        manager.store(&msg, None, None).await.unwrap();

        let loaded = manager.load_recent(10).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(matches!(&loaded[0].content, MessageContent::Text(t) if t == "Hello!"));
    }
}
```

**Step 2: Add to agent/mod.rs**

```rust
pub mod messages_v2;
pub use messages_v2::MessageManagerV2;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core agent::messages_v2`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/agent/
git commit -m "feat(pattern_core): add MessageManagerV2 for SQLite message persistence"
```

---

## Task 5: Complete DatabaseAgent Implementation

**Files:**
- Modify: `crates/pattern_core/src/agent/impls/db_agent_v2.rs`

**Step 1: Expand db_agent_v2.rs with full implementation**

This is a larger task - implement the Agent trait, context building, and LLM interaction.

Key methods to implement:
- `Agent::process_message()` - Main message handling
- `Agent::handle()` - Return AgentHandleV2
- Context building using MemoryContextBuilder
- Message persistence using MessageManagerV2

(Full implementation code would be lengthy - implement incrementally)

**Step 2: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/agent/
git commit -m "feat(pattern_core): implement DatabaseAgent Agent trait"
```

---

## Task 6: Add Default Memory Block Setup

**Files:**
- Create: `crates/pattern_core/src/agent/defaults_v2.rs`
- Modify: `crates/pattern_core/src/agent/mod.rs`

**Step 1: Create defaults_v2.rs**

```rust
//! Default memory block setup for new agents

use crate::memory_v2::{MemoryStore, BlockType, MemoryError};

/// Default memory block definitions
pub struct DefaultBlocks;

impl DefaultBlocks {
    /// Create default blocks for a new agent
    pub async fn create_for_agent<S: MemoryStore>(
        store: &S,
        agent_id: &str,
        agent_name: &str,
    ) -> Result<(), MemoryError> {
        // Persona block
        store.create_block(
            agent_id,
            "persona",
            "Stores details about your current persona, guiding how you behave and respond. \
             Update this as you learn about your role and preferences.",
            BlockType::Core,
            &format!("I am {}, a helpful AI assistant.", agent_name),
            5000,
        ).await?;

        // Human block
        store.create_block(
            agent_id,
            "human",
            "Stores key details about the person you are conversing with. \
             Update this as you learn about them.",
            BlockType::Core,
            "No information about the human yet.",
            5000,
        ).await?;

        // Scratchpad
        store.create_block(
            agent_id,
            "scratchpad",
            "Working notes and temporary information. \
             Use this for current task tracking and short-term memory.",
            BlockType::Working,
            "",
            5000,
        ).await?;

        Ok(())
    }

    /// Create ADHD-specific blocks
    pub async fn create_adhd_blocks<S: MemoryStore>(
        store: &S,
        agent_id: &str,
    ) -> Result<(), MemoryError> {
        // Task context
        store.create_block(
            agent_id,
            "current_task",
            "The task you're currently helping with. \
             Keep this updated to maintain focus.",
            BlockType::Working,
            "",
            3000,
        ).await?;

        // Energy/capacity tracking
        store.create_block(
            agent_id,
            "partner_state",
            "Current state of your partner (energy level, capacity, mood). \
             Update based on what they share.",
            BlockType::Working,
            "State unknown - check in with partner.",
            2000,
        ).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::SqliteMemoryStore;
    use crate::db_v2::ConstellationDb;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_create_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(ConstellationDb::open(dir.path().join("test.db")).await.unwrap());
        let store = SqliteMemoryStore::new(db);

        DefaultBlocks::create_for_agent(&store, "agent_1", "TestAgent").await.unwrap();

        let blocks = store.list_blocks("agent_1").await.unwrap();
        assert_eq!(blocks.len(), 3);

        let labels: Vec<_> = blocks.iter().map(|b| b.label.as_str()).collect();
        assert!(labels.contains(&"persona"));
        assert!(labels.contains(&"human"));
        assert!(labels.contains(&"scratchpad"));
    }
}
```

**Step 2: Add to mod.rs**

```rust
pub mod defaults_v2;
pub use defaults_v2::DefaultBlocks;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core agent::defaults_v2`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/agent/
git commit -m "feat(pattern_core): add default memory block setup for v2 agents"
```

---

## Chunk 3 Completion Checklist

- [ ] `db_agent_v2.rs` skeleton created
- [ ] `entity_v2.rs` with record conversions
- [ ] `handle_v2.rs` with AgentHandleV2
- [ ] `messages_v2.rs` with MessageManagerV2
- [ ] `defaults_v2.rs` with default blocks
- [ ] DatabaseAgent implements Agent trait
- [ ] All tests pass
- [ ] Old agent code untouched and compiles

---

## Next Steps After Chunk 3

With all three chunks complete:

1. **Tool Updates** - Update context, recall, search tools to use AgentHandleV2
2. **CLI Updates** - Add commands using v2 agents
3. **Integration Testing** - End-to-end tests with full stack
4. **Migration** - CAR import/export for v1 → v2 data
5. **Cleanup** - Remove old code once v2 is stable
