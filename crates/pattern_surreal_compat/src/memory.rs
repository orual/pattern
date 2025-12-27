//! Memory-related types for database compatibility
//!
//! This module contains memory-related types that are needed by the db module.

use chrono::Utc;
use compact_str::CompactString;
use dashmap::{DashMap, DashSet};
use pattern_macros::Entity;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use std::fmt::Debug;
use std::fmt::Display;
use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Arc;

use crate::id::{MemoryId, UserId};

// Re-export Result from db module to avoid circular dependency
type Result<T> = std::result::Result<T, crate::db::DatabaseError>;

/// Custom deserializer that handles both f32 and f64 values
/// This is needed because serde_ipld_dagcbor serializes f32 as f64
/// but doesn't deserialize f64 back to f32 automatically
pub fn deserialize_f32_vec_flexible<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<f32>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};

    struct F32VecVisitor;

    impl<'de> Visitor<'de> for F32VecVisitor {
        type Value = Option<Vec<f32>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("null or an array of numbers")
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_seq(self)
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut vec = Vec::new();

            while let Some(value) = seq.next_element::<serde_json::Value>()? {
                let f = if let Some(f) = value.as_f64() {
                    f as f32 // Convert f64 to f32
                } else if let Some(i) = value.as_i64() {
                    i as f32 // Handle integer values
                } else if let Some(u) = value.as_u64() {
                    u as f32 // Handle unsigned integer values
                } else {
                    return Err(de::Error::custom("Expected numeric value in embedding"));
                };
                vec.push(f);
            }

            Ok(Some(vec))
        }
    }

    deserializer.deserialize_option(F32VecVisitor)
}

/// Permission levels for memory operations (most to least restrictive)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPermission {
    /// Can only read, no modifications allowed
    ReadOnly,
    /// Requires permission from partner (owner)
    Partner,
    /// Requires permission from any human
    Human,
    /// Can append to existing content
    Append,
    /// Can modify content freely
    #[default]
    ReadWrite,
    /// Total control, can delete
    Admin,
}

impl Display for MemoryPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryPermission::ReadOnly => write!(f, "Read Only"),
            MemoryPermission::Partner => write!(f, "Requires Partner permission to write"),
            MemoryPermission::Human => write!(f, "Requires Human permission to write"),
            MemoryPermission::Append => write!(f, "Append Only"),
            MemoryPermission::ReadWrite => write!(f, "Read, Append, Write"),
            MemoryPermission::Admin => write!(f, "Read, Write, Delete"),
        }
    }
}

/// Type of memory storage
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// Always in context, cannot be swapped out
    #[default]
    Core,
    /// Active working memory, can be swapped
    Working,
    /// Long-term storage, searchable on demand
    Archival,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::Core => write!(f, "core"),
            MemoryType::Working => write!(f, "working"),
            MemoryType::Archival => write!(f, "recall"),
        }
    }
}

/// A memory block following the MemGPT pattern
#[derive(Debug, Clone, Entity, Serialize, Deserialize)]
#[entity(entity_type = "mem")]
pub struct MemoryBlock {
    /// Unique identifier for this memory block
    pub id: MemoryId,

    /// The user (human) who owns this memory block
    pub owner_id: UserId,

    /// Label identifying this memory block (e.g., "persona", "human", "context")
    pub label: CompactString,

    /// The actual value of the memory block
    pub value: String,

    /// Optional description of what this memory block contains
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Type of memory (core, working, archival)
    #[serde(default)]
    pub memory_type: MemoryType,

    /// Whether this block is pinned (can't be swapped out of core)
    #[serde(default)]
    pub pinned: bool,

    /// Inherent permission level for this block
    #[serde(default)]
    pub permission: MemoryPermission,

    /// Additional metadata for this block
    #[serde(default)]
    pub metadata: serde_json::Value,

    /// The embedding model used to generate embeddings for this block (if any)
    pub embedding_model: Option<String>,

    #[serde(
        deserialize_with = "deserialize_f32_vec_flexible",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub embedding: Option<Vec<f32>>,
    /// When this memory block was created
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// When this memory block was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,

    /// Whether this memory block is active (false = soft deleted, hidden from agent)
    pub is_active: bool,
}

impl Default for MemoryBlock {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            id: MemoryId::generate(),
            owner_id: UserId::nil(),
            label: CompactString::new(""),
            value: String::new(),
            description: None,
            memory_type: MemoryType::Core,
            pinned: false,
            permission: MemoryPermission::ReadWrite,
            metadata: json!({}),
            embedding_model: None,
            embedding: None,
            created_at: now,
            updated_at: now,
            is_active: true,
        }
    }
}

/// Core memory system for agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Memory blocks by label
    #[serde(skip)]
    blocks: Arc<DashMap<CompactString, MemoryBlock>>,

    /// Set of newly created block IDs that need to be persisted
    #[serde(skip)]
    new_blocks: Arc<DashSet<MemoryId>>,

    /// Set of modified block IDs that need to be updated in the database
    #[serde(skip)]
    dirty_blocks: Arc<DashSet<MemoryId>>,

    /// Maximum characters per block (soft limit)
    char_limit: usize,
    /// The user (human) who owns this memory collection
    pub owner_id: UserId,
}

impl Memory {
    /// Create a new memory system
    pub fn new() -> Self {
        Self {
            blocks: Arc::new(DashMap::new()),
            new_blocks: Arc::new(DashSet::new()),
            dirty_blocks: Arc::new(DashSet::new()),
            char_limit: 5000,
            owner_id: UserId::generate(),
        }
    }

    /// Create a new memory system owned by a specific user
    pub fn with_owner(owner_id: &UserId) -> Self {
        Self {
            blocks: Arc::new(DashMap::new()),
            new_blocks: Arc::new(DashSet::new()),
            dirty_blocks: Arc::new(DashSet::new()),
            char_limit: 5000,
            owner_id: owner_id.clone(),
        }
    }

    /// Create with a specific character limit
    pub fn with_char_limit(mut self, limit: usize) -> Self {
        self.char_limit = limit;
        self
    }

    /// Create a new memory block
    pub fn create_block(
        &self,
        label: impl Into<CompactString>,
        value: impl Into<String>,
    ) -> Result<()> {
        let label = label.into();
        let value = value.into();

        let block = MemoryBlock {
            id: MemoryId::generate(),
            owner_id: self.owner_id.clone(),
            label: label.clone(),
            value,
            description: None,
            embedding_model: None,
            updated_at: Utc::now(),
            is_active: true,
            metadata: json!({}),
            created_at: Utc::now(),
            ..Default::default()
        };

        // Track this as a new block
        self.new_blocks.insert(block.id.clone());

        self.blocks.insert(label, block);
        Ok(())
    }

    /// Builder method to add a block
    pub fn with_block(
        self,
        label: impl Into<CompactString>,
        value: impl Into<String>,
    ) -> Result<Self> {
        self.create_block(label, value)?;
        Ok(self)
    }

    /// Get a memory block by label
    pub fn get_block(&self, label: &str) -> Option<impl Deref<Target = MemoryBlock> + use<'_>> {
        self.blocks.get(label)
    }

    /// Get a mutable reference to a memory block
    pub fn get_block_mut(&self, label: &str) -> Option<impl DerefMut<Target = MemoryBlock>> {
        self.blocks.get_mut(label)
    }

    /// Update the value of a memory block
    pub fn update_block_value(&self, label: &str, value: impl Into<String>) -> Result<()> {
        if let Some(mut block) = self.blocks.get_mut(label) {
            let block_id = block.id.clone();
            block.value = value.into();
            block.updated_at = Utc::now();

            self.dirty_blocks.insert(block_id);

            Ok(())
        } else {
            Err(crate::db::DatabaseError::Other(format!(
                "Memory block '{}' not found. Available blocks: {:?}",
                label,
                self.list_blocks()
            )))
        }
    }

    /// Get all memory blocks
    pub fn get_all_blocks(&self) -> Vec<MemoryBlock> {
        self.blocks.iter().map(|e| e.value().clone()).collect()
    }

    pub fn get_all_non_recall(&self) -> Vec<MemoryBlock> {
        self.blocks
            .iter()
            .filter_map(|e| {
                if e.value().memory_type != MemoryType::Archival {
                    Some(e.value().clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// List all block labels
    pub fn list_blocks(&self) -> Vec<CompactString> {
        self.blocks.iter().map(|e| e.key().clone()).collect()
    }

    /// Remove a memory block
    pub fn remove_block(&self, label: &str) -> Option<MemoryBlock> {
        self.blocks.remove(label).map(|e| e.1)
    }

    /// Check if a memory block exists
    pub fn contains_block(&self, label: &str) -> bool {
        self.blocks.contains_key(label)
    }

    /// Atomically update a memory block using a transformation function
    pub fn alter_block<F>(&self, label: &str, f: F)
    where
        F: FnOnce(&CompactString, MemoryBlock) -> MemoryBlock,
    {
        self.blocks.alter(label, |key, block| {
            let block_id = block.id.clone();
            let updated_block = f(key, block);

            self.dirty_blocks.insert(block_id);

            updated_block
        });
    }

    /// Add or update a complete memory block
    pub fn upsert_block(
        &self,
        label: impl Into<CompactString>,
        mut block: MemoryBlock,
    ) -> Result<()> {
        let label = label.into();

        // Ensure the block has the correct owner
        block.owner_id = self.owner_id.clone();
        block.label = label.clone();
        block.updated_at = Utc::now();

        if let Some(existing_block) = self.blocks.get(&label) {
            // Update existing block, preserving its ID
            let existing_id = existing_block.id.clone();
            drop(existing_block); // Drop the guard before inserting to avoid deadlock

            block.id = existing_id.clone(); // Preserve the existing ID
            self.blocks.insert(label, block);

            // Mark as dirty if not new
            if !self.new_blocks.contains(&existing_id) {
                self.dirty_blocks.insert(existing_id);
            }
        } else {
            // New block
            self.new_blocks.insert(block.id.clone());
            self.blocks.insert(label, block);
        }

        Ok(())
    }

    /// Get the set of newly created block IDs
    pub fn get_new_blocks(&self) -> Vec<MemoryId> {
        self.new_blocks.iter().map(|entry| entry.clone()).collect()
    }

    /// Get the set of modified block IDs
    pub fn get_dirty_blocks(&self) -> Vec<MemoryId> {
        self.dirty_blocks
            .iter()
            .map(|entry| entry.clone())
            .collect()
    }

    /// Clear the new blocks tracking (call after persisting)
    pub fn clear_new_blocks(&self) {
        self.new_blocks.clear();
    }

    /// Clear the dirty blocks tracking (call after persisting)
    pub fn clear_dirty_blocks(&self) {
        self.dirty_blocks.clear();
    }

    /// Mark a specific block as persisted (remove from new/dirty sets)
    pub fn mark_block_persisted(&self, id: &MemoryId) {
        self.new_blocks.remove(id);
        self.dirty_blocks.remove(id);
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBlock {
    /// Create a new memory block with auto-generated ID and owner
    pub fn new(label: impl Into<CompactString>, value: impl Into<String>) -> Self {
        Self {
            id: MemoryId::generate(),
            owner_id: UserId::generate(),
            label: label.into(),
            value: value.into(),
            description: None,

            embedding_model: None,
            updated_at: Utc::now(),
            metadata: json!({}),
            created_at: Utc::now(),
            is_active: true,
            ..Default::default()
        }
    }

    /// Create a new memory block with specific ID and owner
    pub fn owned_with_id(
        id: MemoryId,
        owner_id: UserId,
        label: impl Into<CompactString>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            id,
            owner_id,
            label: label.into(),
            value: value.into(),
            description: None,

            embedding_model: None,
            updated_at: Utc::now(),
            metadata: json!({}),
            created_at: Utc::now(),
            is_active: true,
            ..Default::default()
        }
    }

    /// Create a new memory block owned by a specific user
    pub fn owned(
        owner_id: UserId,
        label: impl Into<CompactString>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            id: MemoryId::generate(),
            owner_id,
            label: label.into(),
            value: value.into(),
            description: None,
            updated_at: Utc::now(),

            embedding_model: None,
            metadata: json!({}),
            created_at: Utc::now(),
            is_active: true,
            ..Default::default()
        }
    }

    /// Set the description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the embedding model name for this block
    pub fn with_embedding_model(mut self, embedding_model: impl Into<String>) -> Self {
        self.embedding_model = Some(embedding_model.into());
        self
    }

    /// Set the memory type
    pub fn with_memory_type(mut self, memory_type: MemoryType) -> Self {
        self.memory_type = memory_type;
        self
    }

    /// Set whether this block is pinned
    pub fn with_pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    /// Set the permission level
    pub fn with_permission(mut self, permission: MemoryPermission) -> Self {
        self.permission = permission;
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::db::DbEntity;

    use super::*;

    #[test]
    fn test_memory_creation() {
        let memory = Memory::new();
        assert_eq!(memory.list_blocks().len(), 0);

        memory.create_block("test", "test content").unwrap();
        assert_eq!(memory.list_blocks().len(), 1);

        let block = memory.get_block("test").unwrap();
        assert_eq!(block.value, "test content");
    }

    #[test]
    fn test_memory_block_versioning() {
        let memory = Memory::new();
        memory.create_block("persona", "I am a helpful AI").unwrap();
        memory
            .create_block("human", "The user's name is Alice")
            .unwrap();

        assert_eq!(memory.list_blocks().len(), 2);

        memory
            .update_block_value("persona", "I am a very helpful AI assistant")
            .unwrap();

        let persona_block = memory.get_block("persona").unwrap();
        assert_eq!(persona_block.value, "I am a very helpful AI assistant");
    }

    #[test]
    fn test_memory_block_with_description() {
        let block = MemoryBlock::new("test", "content").with_description("Test block");

        assert_eq!(block.label, "test");
        assert_eq!(block.value, "content");
        assert_eq!(block.description, Some("Test block".to_string()));
    }

    #[tokio::test]
    async fn test_memory_block_entity_operations() {
        use crate::db::client;

        // Initialize test database
        let db = client::create_test_db().await.unwrap();

        // Create a memory block
        let block = MemoryBlock::new("persona", "I am a helpful AI assistant")
            .with_description("Agent persona")
            .with_embedding_model("text-embedding-3-small");

        // Store it using entity system
        let stored = block.store_with_relations(&db).await.unwrap();
        assert_eq!(stored.label.as_str(), "persona");
        assert_eq!(stored.value, "I am a helpful AI assistant");
        assert_eq!(stored.description, Some("Agent persona".to_string()));
        assert_eq!(
            stored.embedding_model,
            Some("text-embedding-3-small".to_string())
        );

        // Load it back
        let loaded = MemoryBlock::load_with_relations(&db, stored.id())
            .await
            .unwrap()
            .expect("Memory block should exist");

        assert_eq!(loaded.label.as_str(), "persona");
        assert_eq!(loaded.value, "I am a helpful AI assistant");
        assert_eq!(loaded.description, Some("Agent persona".to_string()));

        // Test that CompactString conversion works correctly
        assert_eq!(loaded.label, stored.label);
    }
}
