# Chunk 1: Database Layer (SQLite Integration)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build new SQLite-based database integration in pattern_core alongside existing SurrealDB code.

**Architecture:** New `db_v2` module using pattern_db, compiles independently, no changes to existing db module.

**Tech Stack:** pattern_db, sqlx, SQLite

---

## Philosophy

**DO:**
- Add new modules that compile
- Import from pattern_db
- Write tests for new code
- Keep everything in source tree

**DON'T:**
- Remove existing db/ module
- Try to make old and new interoperate
- Change existing function signatures
- Break compilation

---

## Task 1: Add pattern_db Dependency

**Files:**
- Modify: `crates/pattern_core/Cargo.toml`

**Step 1: Add dependency**

```toml
[dependencies]
# ... existing deps ...
pattern_db = { path = "../pattern_db" }
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS (pattern_db is already a workspace member)

**Step 3: Commit**

```bash
git add crates/pattern_core/Cargo.toml
git commit -m "feat(pattern_core): add pattern_db dependency"
```

---

## Task 2: Create db_v2 Module Structure

**Files:**
- Create: `crates/pattern_core/src/db_v2/mod.rs`
- Create: `crates/pattern_core/src/db_v2/connection.rs`
- Modify: `crates/pattern_core/src/lib.rs`

**Step 1: Create module directory**

```bash
mkdir -p crates/pattern_core/src/db_v2
```

**Step 2: Create mod.rs with re-exports**

```rust
//! V2 Database layer using SQLite via pattern_db
//!
//! This module provides the new database integration that will
//! eventually replace the SurrealDB-based db module.

mod connection;

pub use connection::*;
pub use pattern_db::{ConstellationDb, DbError, DbResult};
pub use pattern_db::models;
pub use pattern_db::queries;
```

**Step 3: Create connection.rs skeleton**

```rust
//! Connection management for constellation databases

use pattern_db::ConstellationDb;
use std::path::Path;

/// Opens or creates a constellation database at the given path
pub async fn open_constellation_db(path: impl AsRef<Path>) -> pattern_db::DbResult<ConstellationDb> {
    ConstellationDb::open(path.as_ref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = open_constellation_db(&db_path).await.unwrap();
        assert!(db_path.exists());
    }
}
```

**Step 4: Add to lib.rs**

Add this line near other module declarations:
```rust
pub mod db_v2;
```

**Step 5: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 6: Run the test**

Run: `cargo test -p pattern_core db_v2::connection::tests::test_open_db`
Expected: PASS

**Step 7: Commit**

```bash
git add crates/pattern_core/src/db_v2/ crates/pattern_core/src/lib.rs
git commit -m "feat(pattern_core): add db_v2 module with connection management"
```

---

## Task 3: Add Agent Operations Wrapper

**Files:**
- Create: `crates/pattern_core/src/db_v2/agents.rs`
- Modify: `crates/pattern_core/src/db_v2/mod.rs`

**Step 1: Create agents.rs**

```rust
//! Agent database operations
//!
//! Thin wrapper around pattern_db::queries::agent providing
//! the interface pattern_core needs.

use pattern_db::{ConstellationDb, DbResult};
use pattern_db::models::{Agent, AgentGroup, GroupMember, AgentStatus};
use pattern_db::queries::agent as agent_queries;

/// Create a new agent
pub async fn create_agent(db: &ConstellationDb, agent: &Agent) -> DbResult<()> {
    agent_queries::create_agent(db.pool(), agent).await
}

/// Get agent by ID
pub async fn get_agent(db: &ConstellationDb, id: &str) -> DbResult<Option<Agent>> {
    agent_queries::get_agent(db.pool(), id).await
}

/// Get agent by name
pub async fn get_agent_by_name(db: &ConstellationDb, name: &str) -> DbResult<Option<Agent>> {
    agent_queries::get_agent_by_name(db.pool(), name).await
}

/// List all agents
pub async fn list_agents(db: &ConstellationDb) -> DbResult<Vec<Agent>> {
    agent_queries::list_agents(db.pool()).await
}

/// List agents by status
pub async fn list_agents_by_status(db: &ConstellationDb, status: AgentStatus) -> DbResult<Vec<Agent>> {
    agent_queries::list_agents_by_status(db.pool(), status).await
}

/// Update agent status
pub async fn update_agent_status(db: &ConstellationDb, id: &str, status: AgentStatus) -> DbResult<bool> {
    agent_queries::update_agent_status(db.pool(), id, status).await
}

/// Delete agent
pub async fn delete_agent(db: &ConstellationDb, id: &str) -> DbResult<bool> {
    agent_queries::delete_agent(db.pool(), id).await
}

// Group operations

/// Create agent group
pub async fn create_group(db: &ConstellationDb, group: &AgentGroup) -> DbResult<()> {
    agent_queries::create_group(db.pool(), group).await
}

/// Get group by ID
pub async fn get_group(db: &ConstellationDb, id: &str) -> DbResult<Option<AgentGroup>> {
    agent_queries::get_group(db.pool(), id).await
}

/// Add agent to group
pub async fn add_group_member(db: &ConstellationDb, member: &GroupMember) -> DbResult<()> {
    agent_queries::add_group_member(db.pool(), member).await
}

/// Get group members
pub async fn get_group_members(db: &ConstellationDb, group_id: &str) -> DbResult<Vec<GroupMember>> {
    agent_queries::get_group_members(db.pool(), group_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::models::AgentStatus;
    use sqlx::types::Json;

    async fn test_db() -> ConstellationDb {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        ConstellationDb::open(&db_path).await.unwrap()
    }

    #[tokio::test]
    async fn test_agent_crud() {
        let db = test_db().await;

        let agent = Agent {
            id: "agent_test_1".to_string(),
            name: "TestAgent".to_string(),
            description: Some("A test agent".to_string()),
            model_provider: "anthropic".to_string(),
            model_name: "claude-3-5-sonnet".to_string(),
            system_prompt: "You are a test agent.".to_string(),
            config: Json(serde_json::json!({})),
            enabled_tools: Json(vec!["send_message".to_string()]),
            tool_rules: None,
            status: AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Create
        create_agent(&db, &agent).await.unwrap();

        // Read
        let fetched = get_agent(&db, "agent_test_1").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "TestAgent");

        // Update status
        update_agent_status(&db, "agent_test_1", AgentStatus::Hibernated).await.unwrap();
        let updated = get_agent(&db, "agent_test_1").await.unwrap().unwrap();
        assert_eq!(updated.status, AgentStatus::Hibernated);

        // Delete
        delete_agent(&db, "agent_test_1").await.unwrap();
        let deleted = get_agent(&db, "agent_test_1").await.unwrap();
        assert!(deleted.is_none());
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod agents;
pub use agents::*;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 4: Run tests**

Run: `cargo test -p pattern_core db_v2::agents`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/db_v2/
git commit -m "feat(pattern_core): add db_v2 agent operations"
```

---

## Task 4: Add Memory Operations Wrapper

**Files:**
- Create: `crates/pattern_core/src/db_v2/memory.rs`
- Modify: `crates/pattern_core/src/db_v2/mod.rs`

**Step 1: Create memory.rs**

```rust
//! Memory block database operations
//!
//! Wrapper around pattern_db memory queries for the new
//! agent-scoped, Loro-backed memory system.

use pattern_db::{ConstellationDb, DbResult};
use pattern_db::models::{MemoryBlock, MemoryBlockType, MemoryBlockCheckpoint, MemoryBlockUpdate, UpdateSource, UpdateStats};
use pattern_db::queries::memory as memory_queries;

// Block CRUD

/// Create a new memory block
pub async fn create_block(db: &ConstellationDb, block: &MemoryBlock) -> DbResult<()> {
    memory_queries::create_block(db.pool(), block).await
}

/// Get block by ID
pub async fn get_block(db: &ConstellationDb, id: &str) -> DbResult<Option<MemoryBlock>> {
    memory_queries::get_block(db.pool(), id).await
}

/// Get block by agent and label
pub async fn get_block_by_label(db: &ConstellationDb, agent_id: &str, label: &str) -> DbResult<Option<MemoryBlock>> {
    memory_queries::get_block_by_label(db.pool(), agent_id, label).await
}

/// List all blocks for an agent
pub async fn list_blocks(db: &ConstellationDb, agent_id: &str) -> DbResult<Vec<MemoryBlock>> {
    memory_queries::list_blocks(db.pool(), agent_id).await
}

/// List blocks by type
pub async fn list_blocks_by_type(db: &ConstellationDb, agent_id: &str, block_type: MemoryBlockType) -> DbResult<Vec<MemoryBlock>> {
    memory_queries::list_blocks_by_type(db.pool(), agent_id, block_type).await
}

/// Update block content (snapshot + preview)
pub async fn update_block_content(
    db: &ConstellationDb,
    id: &str,
    loro_snapshot: &[u8],
    content_preview: Option<&str>,
) -> DbResult<bool> {
    memory_queries::update_block_content(db.pool(), id, loro_snapshot, content_preview).await
}

/// Soft delete (deactivate) block
pub async fn deactivate_block(db: &ConstellationDb, id: &str) -> DbResult<bool> {
    memory_queries::deactivate_block(db.pool(), id).await
}

// Delta storage for Loro updates

/// Store a Loro update delta
pub async fn store_update(
    db: &ConstellationDb,
    block_id: &str,
    update_blob: &[u8],
    source: UpdateSource,
) -> DbResult<i64> {
    memory_queries::store_update(db.pool(), block_id, update_blob, source).await
}

/// Get checkpoint and all updates for reconstruction
pub async fn get_checkpoint_and_updates(
    db: &ConstellationDb,
    block_id: &str,
) -> DbResult<(Option<MemoryBlockCheckpoint>, Vec<MemoryBlockUpdate>)> {
    memory_queries::get_checkpoint_and_updates(db.pool(), block_id).await
}

/// Get updates since a sequence number
pub async fn get_updates_since(
    db: &ConstellationDb,
    block_id: &str,
    after_seq: i64,
) -> DbResult<Vec<MemoryBlockUpdate>> {
    memory_queries::get_updates_since(db.pool(), block_id, after_seq).await
}

/// Check if updates exist since sequence
pub async fn has_updates_since(
    db: &ConstellationDb,
    block_id: &str,
    after_seq: i64,
) -> DbResult<bool> {
    memory_queries::has_updates_since(db.pool(), block_id, after_seq).await
}

/// Consolidate updates into checkpoint
pub async fn consolidate_checkpoint(
    db: &ConstellationDb,
    block_id: &str,
    new_snapshot: &[u8],
    new_frontier: Option<&[u8]>,
    up_to_seq: i64,
) -> DbResult<()> {
    memory_queries::consolidate_checkpoint(db.pool(), block_id, new_snapshot, new_frontier, up_to_seq).await
}

/// Get stats for consolidation decisions
pub async fn get_pending_update_stats(db: &ConstellationDb, block_id: &str) -> DbResult<UpdateStats> {
    memory_queries::get_pending_update_stats(db.pool(), block_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::models::MemoryPermission;

    async fn test_db() -> ConstellationDb {
        let dir = tempfile::tempdir().unwrap();
        ConstellationDb::open(dir.path().join("test.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_memory_block_crud() {
        let db = test_db().await;

        let block = MemoryBlock {
            id: "mem_test_1".to_string(),
            agent_id: "agent_1".to_string(),
            label: "persona".to_string(),
            description: "Agent personality".to_string(),
            block_type: MemoryBlockType::Core,
            char_limit: 5000,
            read_only: false,
            pinned: true,
            permission: MemoryPermission::ReadWrite,
            loro_snapshot: vec![0, 1, 2, 3], // Dummy snapshot
            frontier: None,
            last_seq: 0,
            content_preview: Some("I am a helpful assistant.".to_string()),
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        // Create
        create_block(&db, &block).await.unwrap();

        // Read by ID
        let fetched = get_block(&db, "mem_test_1").await.unwrap();
        assert!(fetched.is_some());

        // Read by label
        let by_label = get_block_by_label(&db, "agent_1", "persona").await.unwrap();
        assert!(by_label.is_some());
        assert_eq!(by_label.unwrap().label, "persona");

        // List
        let blocks = list_blocks(&db, "agent_1").await.unwrap();
        assert_eq!(blocks.len(), 1);

        // Deactivate
        deactivate_block(&db, "mem_test_1").await.unwrap();
        let deactivated = get_block(&db, "mem_test_1").await.unwrap();
        assert!(deactivated.is_none()); // Filtered by is_active
    }

    #[tokio::test]
    async fn test_delta_storage() {
        let db = test_db().await;

        // Create block first
        let block = MemoryBlock {
            id: "mem_delta_1".to_string(),
            agent_id: "agent_1".to_string(),
            label: "scratch".to_string(),
            description: "Working memory".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 5000,
            read_only: false,
            pinned: false,
            permission: MemoryPermission::ReadWrite,
            loro_snapshot: vec![],
            frontier: None,
            last_seq: 0,
            content_preview: None,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        create_block(&db, &block).await.unwrap();

        // Store updates
        let seq1 = store_update(&db, "mem_delta_1", b"update1", UpdateSource::Agent).await.unwrap();
        let seq2 = store_update(&db, "mem_delta_1", b"update2", UpdateSource::Agent).await.unwrap();

        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);

        // Check updates exist
        assert!(has_updates_since(&db, "mem_delta_1", 0).await.unwrap());

        // Get updates since
        let updates = get_updates_since(&db, "mem_delta_1", 1).await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].seq, 2);

        // Get stats
        let stats = get_pending_update_stats(&db, "mem_delta_1").await.unwrap();
        assert_eq!(stats.count, 2);
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod memory;
pub use memory::*;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core db_v2::memory`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/db_v2/
git commit -m "feat(pattern_core): add db_v2 memory operations with delta storage"
```

---

## Task 5: Add Message Operations Wrapper

**Files:**
- Create: `crates/pattern_core/src/db_v2/messages.rs`
- Modify: `crates/pattern_core/src/db_v2/mod.rs`

**Step 1: Create messages.rs**

```rust
//! Message database operations

use pattern_db::{ConstellationDb, DbResult};
use pattern_db::models::{Message, ArchiveSummary, MessageRole};
use pattern_db::queries::message as message_queries;

/// Create a message
pub async fn create_message(db: &ConstellationDb, message: &Message) -> DbResult<()> {
    message_queries::create_message(db.pool(), message).await
}

/// Get message by ID
pub async fn get_message(db: &ConstellationDb, id: &str) -> DbResult<Option<Message>> {
    message_queries::get_message(db.pool(), id).await
}

/// Get messages for agent (non-archived, ordered by position)
pub async fn get_messages(db: &ConstellationDb, agent_id: &str, limit: i64) -> DbResult<Vec<Message>> {
    message_queries::get_messages(db.pool(), agent_id, limit).await
}

/// Get messages including archived
pub async fn get_messages_with_archived(
    db: &ConstellationDb,
    agent_id: &str,
    limit: i64,
) -> DbResult<Vec<Message>> {
    message_queries::get_messages_with_archived(db.pool(), agent_id, limit).await
}

/// Get messages in a batch
pub async fn get_batch_messages(db: &ConstellationDb, batch_id: &str) -> DbResult<Vec<Message>> {
    message_queries::get_batch_messages(db.pool(), batch_id).await
}

/// Archive messages before position
pub async fn archive_messages(
    db: &ConstellationDb,
    agent_id: &str,
    before_position: &str,
) -> DbResult<u64> {
    message_queries::archive_messages(db.pool(), agent_id, before_position).await
}

/// Create archive summary
pub async fn create_archive_summary(db: &ConstellationDb, summary: &ArchiveSummary) -> DbResult<()> {
    message_queries::create_archive_summary(db.pool(), summary).await
}

/// Get archive summaries for agent
pub async fn get_archive_summaries(db: &ConstellationDb, agent_id: &str) -> DbResult<Vec<ArchiveSummary>> {
    message_queries::get_archive_summaries(db.pool(), agent_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> ConstellationDb {
        let dir = tempfile::tempdir().unwrap();
        ConstellationDb::open(dir.path().join("test.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_message_crud() {
        let db = test_db().await;

        let msg = Message {
            id: "msg_1".to_string(),
            agent_id: "agent_1".to_string(),
            position: "0001".to_string(),
            batch_id: Some("batch_1".to_string()),
            sequence_in_batch: Some(0),
            role: MessageRole::User,
            content: Some("Hello!".to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            source: Some("cli".to_string()),
            source_metadata: None,
            is_archived: false,
            created_at: chrono::Utc::now(),
        };

        create_message(&db, &msg).await.unwrap();

        let fetched = get_message(&db, "msg_1").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().content, Some("Hello!".to_string()));

        let messages = get_messages(&db, "agent_1", 10).await.unwrap();
        assert_eq!(messages.len(), 1);
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod messages;
pub use messages::*;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core db_v2::messages`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/db_v2/
git commit -m "feat(pattern_core): add db_v2 message operations"
```

---

## Task 6: Add Search Operations Wrapper

**Files:**
- Create: `crates/pattern_core/src/db_v2/search.rs`
- Modify: `crates/pattern_core/src/db_v2/mod.rs`

**Step 1: Create search.rs**

```rust
//! Search operations (FTS5 + vector)

use pattern_db::{ConstellationDb, DbResult};
use pattern_db::fts::{self, FtsMatch};
use pattern_db::vector::{self, VectorSearchResult, ContentType};
use pattern_db::search::{self, SearchResult, HybridSearchBuilder, SearchMode, ContentFilter};

// FTS5 searches

/// Search messages by text
pub async fn search_messages_fts(
    db: &ConstellationDb,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    fts::search_messages(db.pool(), query, agent_id, limit).await
}

/// Search memory blocks by text
pub async fn search_memory_fts(
    db: &ConstellationDb,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    fts::search_memory_blocks(db.pool(), query, agent_id, limit).await
}

/// Search archival entries by text
pub async fn search_archival_fts(
    db: &ConstellationDb,
    query: &str,
    agent_id: Option<&str>,
    limit: i64,
) -> DbResult<Vec<FtsMatch>> {
    fts::search_archival(db.pool(), query, agent_id, limit).await
}

// Vector searches

/// KNN search by embedding
pub async fn search_vector(
    db: &ConstellationDb,
    query_embedding: &[f32],
    limit: i64,
    content_type: Option<ContentType>,
) -> DbResult<Vec<VectorSearchResult>> {
    vector::knn_search(db.pool(), query_embedding, limit, content_type).await
}

/// Insert embedding for content
pub async fn insert_embedding(
    db: &ConstellationDb,
    content_type: ContentType,
    content_id: &str,
    embedding: &[f32],
    chunk_index: Option<i64>,
    content_hash: Option<&str>,
) -> DbResult<i64> {
    vector::insert_embedding(db.pool(), content_type, content_id, embedding, chunk_index, content_hash).await
}

// Hybrid search

/// Create hybrid search builder
pub fn hybrid_search(db: &ConstellationDb) -> HybridSearchBuilder<'_> {
    search::search(db.pool())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::models::{Message, MessageRole};
    use pattern_db::queries::message::create_message;

    async fn test_db() -> ConstellationDb {
        let dir = tempfile::tempdir().unwrap();
        ConstellationDb::open(dir.path().join("test.db")).await.unwrap()
    }

    #[tokio::test]
    async fn test_fts_search() {
        let db = test_db().await;

        // Create test message
        let msg = Message {
            id: "msg_search_1".to_string(),
            agent_id: "agent_1".to_string(),
            position: "0001".to_string(),
            batch_id: None,
            sequence_in_batch: None,
            role: MessageRole::User,
            content: Some("Hello world, this is a test message".to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            source: None,
            source_metadata: None,
            is_archived: false,
            created_at: chrono::Utc::now(),
        };
        create_message(db.pool(), &msg).await.unwrap();

        // Search
        let results = search_messages_fts(&db, "test", None, 10).await.unwrap();
        assert!(!results.is_empty());
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod search;
pub use search::*;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core db_v2::search`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/db_v2/
git commit -m "feat(pattern_core): add db_v2 search operations (FTS + vector)"
```

---

## Task 7: Final Module Organization

**Files:**
- Modify: `crates/pattern_core/src/db_v2/mod.rs`

**Step 1: Update mod.rs with complete structure**

```rust
//! V2 Database layer using SQLite via pattern_db
//!
//! This module provides the new database integration that will
//! eventually replace the SurrealDB-based db module.
//!
//! # Structure
//!
//! - `connection` - Database connection management
//! - `agents` - Agent CRUD operations
//! - `memory` - Memory block operations with Loro delta storage
//! - `messages` - Message operations with batch support
//! - `search` - FTS5 and vector search operations
//!
//! # Usage
//!
//! ```rust,ignore
//! use pattern_core::db_v2::{open_constellation_db, create_agent, get_agent};
//!
//! let db = open_constellation_db("path/to/constellation.db").await?;
//! let agent = get_agent(&db, "agent_123").await?;
//! ```

mod connection;
mod agents;
mod memory;
mod messages;
mod search;

// Re-export our wrappers
pub use connection::*;
pub use agents::*;
pub use memory::*;
pub use messages::*;
pub use search::*;

// Re-export key types from pattern_db
pub use pattern_db::{ConstellationDb, DbError, DbResult};
pub use pattern_db::models::{
    Agent, AgentStatus, AgentGroup, GroupMember,
    MemoryBlock, MemoryBlockType, MemoryPermission, MemoryBlockCheckpoint, MemoryBlockUpdate, UpdateSource,
    Message, MessageRole, ArchiveSummary,
};
pub use pattern_db::vector::ContentType;
pub use pattern_db::fts::FtsMatch;
pub use pattern_db::search::{SearchResult, SearchMode, ContentFilter};
```

**Step 2: Verify everything compiles and tests pass**

Run: `cargo check -p pattern_core && cargo test -p pattern_core db_v2`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/src/db_v2/
git commit -m "feat(pattern_core): complete db_v2 module organization"
```

---

## Chunk 1 Completion Checklist

- [ ] pattern_db added as dependency
- [ ] db_v2/mod.rs created with re-exports
- [ ] db_v2/connection.rs with open_constellation_db
- [ ] db_v2/agents.rs with CRUD operations
- [ ] db_v2/memory.rs with block + delta operations
- [ ] db_v2/messages.rs with message operations
- [ ] db_v2/search.rs with FTS + vector search
- [ ] All tests pass
- [ ] Old db/ module untouched and still compiles

---

## Notes for Chunk 2

With db_v2 in place, Chunk 2 (Memory Rework) can:
- Import from `db_v2` for persistence
- Create `memory_v2.rs` with Loro integration
- Use `db_v2::memory::*` for storage operations
- Build new MemoryStore trait on top
