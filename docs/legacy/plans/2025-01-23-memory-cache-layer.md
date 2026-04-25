# Memory Cache Layer Implementation

> **For Claude:** Implement this plan task-by-task.

**Goal:** Build in-memory LoroDoc cache with lazy loading from DB and write-through persistence.

**Architecture:** DashMap cache of LoroDoc instances, load on access, persist deltas on write.

**Tech Stack:** Loro 1.10, pattern_db, DashMap

---

## Overview

```
DashMap<(AgentId, Label), CachedBlock>
         │
         ▼ on access to block X
    ┌────────────────────────────┐
    │ In cache?                  │
    │  NO  → load from DB        │
    │        (snapshot + deltas) │
    │        reconstruct LoroDoc │
    │  YES → check for new deltas│
    │        merge if needed     │
    └────────────────────────────┘
         │
         ▼ perform operation
    ┌────────────────────────────┐
    │ Mutate LoroDoc             │
    │ Track dirty state         │
    └────────────────────────────┘
         │
         ▼ persist (on write)
    ┌────────────────────────────┐
    │ Export delta since last    │
    │ Write to memory_block_updates│
    │ Update last_seq in cache   │
    └────────────────────────────┘
```

---

## Task 1: Add Loro Dependency

**Files:**
- Modify: `crates/pattern_core/Cargo.toml`

**Step 1: Add dependency**

Add under the Database section:
```toml
# Database
surrealdb = { workspace = true }
pattern_db = { path = "../pattern_db" }
loro = { version = "1.10", features = ["counter"] }
```

**Step 2: Verify compilation**

```bash
cargo check -p pattern_core
```

**Step 3: Commit**

```bash
git add crates/pattern_core/Cargo.toml
git commit -m "feat(pattern_core): add loro dependency for CRDT memory"
```

---

## Task 2: Create memory_v2 Module with Types

**Files:**
- Create: `crates/pattern_core/src/memory_v2/mod.rs`
- Create: `crates/pattern_core/src/memory_v2/types.rs`
- Modify: `crates/pattern_core/src/lib.rs`

**Step 1: Create types.rs**

```rust
//! Types for the v2 memory system

use chrono::{DateTime, Utc};
use loro::{LoroDoc, Frontiers};
use std::sync::Arc;
use parking_lot::RwLock;

/// A cached memory block with its LoroDoc
pub struct CachedBlock {
    /// Block metadata from DB
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub description: String,
    pub block_type: BlockType,
    pub char_limit: i64,
    pub read_only: bool,

    /// The Loro document (thread-safe)
    pub doc: Arc<RwLock<LoroDoc>>,

    /// Last sequence number we've seen from DB
    pub last_seq: i64,

    /// Frontier at last persist (for delta export)
    pub last_persisted_frontier: Option<Frontiers>,

    /// Whether we have unpersisted changes
    pub dirty: bool,

    /// When this was last accessed (for eviction)
    pub last_accessed: DateTime<Utc>,
}

/// Block types matching pattern_db
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Core,
    Working,
    Archival,
    Log,
}

impl From<pattern_db::models::MemoryBlockType> for BlockType {
    fn from(t: pattern_db::models::MemoryBlockType) -> Self {
        match t {
            pattern_db::models::MemoryBlockType::Core => BlockType::Core,
            pattern_db::models::MemoryBlockType::Working => BlockType::Working,
            pattern_db::models::MemoryBlockType::Archival => BlockType::Archival,
            pattern_db::models::MemoryBlockType::Log => BlockType::Log,
        }
    }
}

/// Error type for memory operations
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Block not found: {agent_id}/{label}")]
    NotFound { agent_id: String, label: String },

    #[error("Block is read-only: {0}")]
    ReadOnly(String),

    #[error("Database error: {0}")]
    Database(#[from] pattern_db::DbError),

    #[error("Loro error: {0}")]
    Loro(String),

    #[error("{0}")]
    Other(String),
}

pub type MemoryResult<T> = Result<T, MemoryError>;
```

**Step 2: Create mod.rs**

```rust
//! V2 Memory System
//!
//! In-memory LoroDoc cache with lazy loading and write-through persistence.

mod types;

pub use types::*;

// Future modules:
// mod cache;    // MemoryCache implementation
// mod ops;      // Operations on cached blocks
```

**Step 3: Add to lib.rs**

```rust
pub mod memory_v2;
```

**Step 4: Verify compilation**

```bash
cargo check -p pattern_core
```

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory_v2/ crates/pattern_core/src/lib.rs
git commit -m "feat(pattern_core): add memory_v2 module with types"
```

---

## Task 3: Implement MemoryCache Core

**Files:**
- Create: `crates/pattern_core/src/memory_v2/cache.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create cache.rs**

```rust
//! In-memory cache of LoroDoc instances

use crate::memory_v2::{CachedBlock, BlockType, MemoryError, MemoryResult};
use dashmap::DashMap;
use loro::{LoroDoc, ExportMode};
use pattern_db::ConstellationDb;
use parking_lot::RwLock;
use std::sync::Arc;
use chrono::Utc;

/// Cache key: (agent_id, label)
type CacheKey = (String, String);

/// In-memory cache of LoroDoc instances with lazy loading
pub struct MemoryCache {
    /// The database connection
    db: Arc<ConstellationDb>,

    /// Cached blocks: (agent_id, label) -> CachedBlock
    blocks: DashMap<CacheKey, CachedBlock>,
}

impl MemoryCache {
    /// Create a new memory cache
    pub fn new(db: Arc<ConstellationDb>) -> Self {
        Self {
            db,
            blocks: DashMap::new(),
        }
    }

    /// Get or load a block, returns None if block doesn't exist in DB
    pub async fn get(&self, agent_id: &str, label: &str) -> MemoryResult<Option<Arc<RwLock<LoroDoc>>>> {
        let key = (agent_id.to_string(), label.to_string());

        // Check cache first
        if let Some(entry) = self.blocks.get(&key) {
            // Update last accessed
            // Note: We can't mutate through the ref, but that's ok for now
            return Ok(Some(entry.doc.clone()));
        }

        // Load from database
        let block = self.load_from_db(agent_id, label).await?;

        match block {
            Some(cached) => {
                let doc = cached.doc.clone();
                self.blocks.insert(key, cached);
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    /// Load a block from database, reconstructing LoroDoc from snapshot + deltas
    async fn load_from_db(&self, agent_id: &str, label: &str) -> MemoryResult<Option<CachedBlock>> {
        use pattern_db::queries::memory;

        // Get block metadata
        let block = memory::get_block_by_label(self.db.pool(), agent_id, label).await?;

        let block = match block {
            Some(b) => b,
            None => return Ok(None),
        };

        // Create LoroDoc and import snapshot
        let doc = LoroDoc::new();

        if !block.loro_snapshot.is_empty() {
            doc.import(&block.loro_snapshot)
                .map_err(|e| MemoryError::Loro(e.to_string()))?;
        }

        // Get and apply any updates since the snapshot
        let (_, updates) = memory::get_checkpoint_and_updates(self.db.pool(), &block.id).await?;

        for update in &updates {
            doc.import(&update.update_blob)
                .map_err(|e| MemoryError::Loro(e.to_string()))?;
        }

        let last_seq = updates.last().map(|u| u.seq).unwrap_or(block.last_seq);
        let frontier = doc.oplog_frontiers();

        Ok(Some(CachedBlock {
            id: block.id,
            agent_id: block.agent_id,
            label: block.label,
            description: block.description,
            block_type: block.block_type.into(),
            char_limit: block.char_limit,
            read_only: block.read_only,
            doc: Arc::new(RwLock::new(doc)),
            last_seq,
            last_persisted_frontier: Some(frontier),
            dirty: false,
            last_accessed: Utc::now(),
        }))
    }

    /// Persist changes for a block (export delta, write to DB)
    pub async fn persist(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        let key = (agent_id.to_string(), label.to_string());

        let mut entry = self.blocks.get_mut(&key)
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        if !entry.dirty {
            return Ok(());
        }

        let doc = entry.doc.read();

        // Export delta since last persist
        let update_blob = match &entry.last_persisted_frontier {
            Some(frontier) => doc.export(ExportMode::Updates { from: frontier.clone() }),
            None => doc.export(ExportMode::Snapshot),
        };

        // Only persist if there's actual data
        if let Ok(blob) = update_blob {
            if !blob.is_empty() {
                use pattern_db::queries::memory;
                use pattern_db::models::UpdateSource;

                let new_seq = memory::store_update(
                    self.db.pool(),
                    &entry.id,
                    &blob,
                    UpdateSource::Agent,
                ).await?;

                entry.last_seq = new_seq;
            }
        }

        // Update frontier and clear dirty flag
        entry.last_persisted_frontier = Some(doc.oplog_frontiers());
        entry.dirty = false;

        // Also update the content preview in the main block
        let preview = self.generate_preview(&doc);
        drop(doc); // Release read lock

        use pattern_db::queries::memory;
        memory::update_block_content(
            self.db.pool(),
            &entry.id,
            &[],  // Don't update snapshot on every write
            preview.as_deref(),
        ).await?;

        Ok(())
    }

    /// Mark a block as dirty (has unpersisted changes)
    pub fn mark_dirty(&self, agent_id: &str, label: &str) {
        let key = (agent_id.to_string(), label.to_string());
        if let Some(mut entry) = self.blocks.get_mut(&key) {
            entry.dirty = true;
        }
    }

    /// Check if a block is cached
    pub fn is_cached(&self, agent_id: &str, label: &str) -> bool {
        let key = (agent_id.to_string(), label.to_string());
        self.blocks.contains_key(&key)
    }

    /// Evict a block from cache (persists first if dirty)
    pub async fn evict(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Persist first if dirty
        self.persist(agent_id, label).await?;

        let key = (agent_id.to_string(), label.to_string());
        self.blocks.remove(&key);
        Ok(())
    }

    /// Generate a preview string from the doc (first ~100 chars of text content)
    fn generate_preview(&self, doc: &LoroDoc) -> Option<String> {
        // Try to get text from common container names
        for name in ["content", "text", "value"] {
            let text = doc.get_text(name);
            let content = text.to_string();
            if !content.is_empty() {
                let preview: String = content.chars().take(100).collect();
                return Some(preview);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::models::{MemoryBlock, MemoryBlockType, MemoryPermission};

    async fn test_db() -> Arc<ConstellationDb> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(ConstellationDb::open(dir.path().join("test.db")).await.unwrap())
    }

    #[tokio::test]
    async fn test_cache_load_empty_block() {
        let db = test_db().await;

        // Create a block in DB
        let block = MemoryBlock {
            id: "mem_1".to_string(),
            agent_id: "agent_1".to_string(),
            label: "persona".to_string(),
            description: "Agent personality".to_string(),
            block_type: MemoryBlockType::Core,
            char_limit: 5000,
            read_only: false,
            pinned: true,
            permission: MemoryPermission::ReadWrite,
            loro_snapshot: vec![],
            frontier: None,
            last_seq: 0,
            content_preview: None,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::memory::create_block(db.pool(), &block).await.unwrap();

        // Create cache and load
        let cache = MemoryCache::new(db);
        let doc = cache.get("agent_1", "persona").await.unwrap();

        assert!(doc.is_some());
        assert!(cache.is_cached("agent_1", "persona"));
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let db = test_db().await;
        let cache = MemoryCache::new(db);

        let doc = cache.get("agent_1", "nonexistent").await.unwrap();
        assert!(doc.is_none());
    }

    #[tokio::test]
    async fn test_cache_persist() {
        let db = test_db().await;

        // Create a block
        let block = MemoryBlock {
            id: "mem_2".to_string(),
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

        pattern_db::queries::memory::create_block(db.pool(), &block).await.unwrap();

        let cache = MemoryCache::new(db.clone());

        // Load and modify
        let doc = cache.get("agent_1", "scratch").await.unwrap().unwrap();
        {
            let doc = doc.write();
            let text = doc.get_text("content");
            text.insert(0, "Hello, world!").unwrap();
        }
        cache.mark_dirty("agent_1", "scratch");

        // Persist
        cache.persist("agent_1", "scratch").await.unwrap();

        // Verify update was stored
        let (_, updates) = pattern_db::queries::memory::get_checkpoint_and_updates(
            db.pool(),
            "mem_2"
        ).await.unwrap();

        assert!(!updates.is_empty());
    }
}
```

**Step 2: Update mod.rs**

```rust
//! V2 Memory System
//!
//! In-memory LoroDoc cache with lazy loading and write-through persistence.

mod types;
mod cache;

pub use types::*;
pub use cache::MemoryCache;
```

**Step 3: Verify and test**

```bash
cargo check -p pattern_core
cargo test -p pattern_core memory_v2::cache
```

**Step 4: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add MemoryCache with lazy loading and delta persistence"
```

---

## Task 4: Add Block Operations

**Files:**
- Create: `crates/pattern_core/src/memory_v2/ops.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create ops.rs with high-level operations**

```rust
//! High-level operations on cached memory blocks

use crate::memory_v2::{MemoryCache, MemoryError, MemoryResult};
use loro::LoroDoc;
use parking_lot::RwLock;
use std::sync::Arc;

impl MemoryCache {
    /// Get the text content of a block
    pub async fn get_text(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        let doc = self.get(agent_id, label).await?;

        match doc {
            Some(doc) => {
                let doc = doc.read();
                let text = doc.get_text("content");
                Ok(Some(text.to_string()))
            }
            None => Ok(None),
        }
    }

    /// Set the text content of a block (replaces existing)
    pub async fn set_text(&self, agent_id: &str, label: &str, content: &str) -> MemoryResult<()> {
        let doc = self.get(agent_id, label).await?
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        // Check read-only
        self.check_writable(agent_id, label)?;

        {
            let doc = doc.write();
            let text = doc.get_text("content");
            let len = text.len_unicode();
            if len > 0 {
                text.delete(0, len).map_err(|e| MemoryError::Loro(e.to_string()))?;
            }
            text.insert(0, content).map_err(|e| MemoryError::Loro(e.to_string()))?;
        }

        self.mark_dirty(agent_id, label);
        self.persist(agent_id, label).await?;

        Ok(())
    }

    /// Append text to a block
    pub async fn append_text(&self, agent_id: &str, label: &str, content: &str) -> MemoryResult<()> {
        let doc = self.get(agent_id, label).await?
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        self.check_writable(agent_id, label)?;

        {
            let doc = doc.write();
            let text = doc.get_text("content");
            let len = text.len_unicode();
            text.insert(len, content).map_err(|e| MemoryError::Loro(e.to_string()))?;
        }

        self.mark_dirty(agent_id, label);
        self.persist(agent_id, label).await?;

        Ok(())
    }

    /// Replace text in a block
    pub async fn replace_text(
        &self,
        agent_id: &str,
        label: &str,
        find: &str,
        replace: &str,
    ) -> MemoryResult<bool> {
        let content = self.get_text(agent_id, label).await?
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        if let Some(pos) = content.find(find) {
            let new_content = content.replacen(find, replace, 1);
            self.set_text(agent_id, label, &new_content).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Check if a block is writable
    fn check_writable(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        let key = (agent_id.to_string(), label.to_string());
        if let Some(entry) = self.blocks.get(&key) {
            if entry.read_only {
                return Err(MemoryError::ReadOnly(label.to_string()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::ConstellationDb;
    use pattern_db::models::{MemoryBlock, MemoryBlockType, MemoryPermission};
    use std::sync::Arc;

    async fn setup() -> (Arc<ConstellationDb>, MemoryCache) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(ConstellationDb::open(dir.path().join("test.db")).await.unwrap());

        // Create test block
        let block = MemoryBlock {
            id: "mem_ops_1".to_string(),
            agent_id: "agent_1".to_string(),
            label: "scratch".to_string(),
            description: "Test block".to_string(),
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

        pattern_db::queries::memory::create_block(db.pool(), &block).await.unwrap();

        let cache = MemoryCache::new(db.clone());
        (db, cache)
    }

    #[tokio::test]
    async fn test_set_and_get_text() {
        let (_db, cache) = setup().await;

        cache.set_text("agent_1", "scratch", "Hello, world!").await.unwrap();

        let content = cache.get_text("agent_1", "scratch").await.unwrap();
        assert_eq!(content, Some("Hello, world!".to_string()));
    }

    #[tokio::test]
    async fn test_append_text() {
        let (_db, cache) = setup().await;

        cache.set_text("agent_1", "scratch", "Hello").await.unwrap();
        cache.append_text("agent_1", "scratch", ", world!").await.unwrap();

        let content = cache.get_text("agent_1", "scratch").await.unwrap();
        assert_eq!(content, Some("Hello, world!".to_string()));
    }

    #[tokio::test]
    async fn test_replace_text() {
        let (_db, cache) = setup().await;

        cache.set_text("agent_1", "scratch", "Hello, world!").await.unwrap();
        let replaced = cache.replace_text("agent_1", "scratch", "world", "Loro").await.unwrap();

        assert!(replaced);
        let content = cache.get_text("agent_1", "scratch").await.unwrap();
        assert_eq!(content, Some("Hello, Loro!".to_string()));
    }
}
```

**Step 2: Update mod.rs**

```rust
mod ops;
```

**Step 3: Verify and test**

```bash
cargo check -p pattern_core
cargo test -p pattern_core memory_v2
```

**Step 4: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add high-level text operations on MemoryCache"
```

---

## Completion Checklist

- [ ] Loro dependency added
- [ ] memory_v2/types.rs with CachedBlock, BlockType, MemoryError
- [ ] memory_v2/cache.rs with MemoryCache, lazy loading, delta persistence
- [ ] memory_v2/ops.rs with get_text, set_text, append_text, replace_text
- [ ] All tests pass
- [ ] Old memory.rs untouched

---

## Notes

This is the foundation. Future work:
- Structured block schemas (Map, List, Log types)
- Block creation through cache
- Consolidation (snapshot updates when too many deltas)
- Eviction policies (LRU when cache gets too big)
- Sync with updates from other sources
