//! Helper utilities for implementing DataStream and DataBlock sources.
//!
//! This module provides fluent builders and utilities to simplify source implementations:
//!
//! - [`BlockBuilder`] - Create blocks in a memory store with fluent API
//! - [`NotificationBuilder`] - Build notifications for broadcast channels
//! - [`EphemeralBlockCache`] - Get-or-create cache for ephemeral blocks

use crate::AgentId;
use crate::SnowflakePosition;
use crate::memory::{BlockSchema, BlockType, MemoryResult, MemoryStore};
use crate::messages::Message;
use crate::utils::get_next_message_position_sync;

use super::{BlockRef, Notification};

/// Builder for creating blocks in a memory store.
///
/// Provides a fluent API for creating memory blocks with all necessary metadata.
///
/// # Example
///
/// ```ignore
/// let block_ref = BlockBuilder::new(memory, owner, "user_profile")
///     .description("User profile information")
///     .schema(BlockSchema::Text)
///     .block_type(BlockType::Working)
///     .pinned()
///     .content("Initial content")
///     .build()
///     .await?;
/// ```
pub struct BlockBuilder<'a> {
    memory: &'a dyn MemoryStore,
    owner: AgentId,
    label: String,
    description: Option<String>,
    schema: BlockSchema,
    block_type: BlockType,
    char_limit: usize,
    pinned: bool,
    initial_content: Option<String>,
}

impl<'a> BlockBuilder<'a> {
    /// Create a new block builder.
    ///
    /// # Arguments
    ///
    /// * `memory` - The memory store to create the block in
    /// * `owner` - The agent ID that will own this block
    /// * `label` - Human-readable label for the block
    pub fn new(memory: &'a dyn MemoryStore, owner: AgentId, label: impl Into<String>) -> Self {
        Self {
            memory,
            owner,
            label: label.into(),
            description: None,
            schema: BlockSchema::text(),
            block_type: BlockType::Working,
            char_limit: 4096,
            pinned: false,
            initial_content: None,
        }
    }

    /// Set block description.
    ///
    /// If not set, the label will be used as the description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set block schema.
    ///
    /// Defaults to `BlockSchema::Text`.
    pub fn schema(mut self, schema: BlockSchema) -> Self {
        self.schema = schema;
        self
    }

    /// Set block type.
    ///
    /// Defaults to `BlockType::Working`.
    pub fn block_type(mut self, block_type: BlockType) -> Self {
        self.block_type = block_type;
        self
    }

    /// Set character limit for the block.
    ///
    /// Defaults to 4096.
    pub fn char_limit(mut self, limit: usize) -> Self {
        self.char_limit = limit;
        self
    }

    /// Mark block as pinned (always in context).
    ///
    /// Pinned blocks are always loaded into agent context while subscribed.
    /// Unpinned (ephemeral) blocks only load when referenced by a notification.
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Set initial text content for the block.
    ///
    /// This content will be written after the block is created.
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.initial_content = Some(content.into());
        self
    }

    /// Build the block and return a BlockRef.
    ///
    /// This creates the block in the memory store, optionally sets initial content,
    /// and configures the pinned flag if requested.
    pub async fn build(self) -> MemoryResult<BlockRef> {
        let description = self.description.unwrap_or_else(|| self.label.clone());
        let owner_str = self.owner.to_string();

        let doc = self
            .memory
            .create_block(
                &owner_str,
                &self.label,
                &description,
                self.block_type,
                self.schema,
                self.char_limit,
            )
            .await?;
        let block_id = doc.id().to_string();

        // Set initial content if provided
        if let Some(content) = &self.initial_content {
            doc.set_text(content, true)?;
            self.memory.mark_dirty(&owner_str, &self.label);
            self.memory.persist_block(&owner_str, &self.label).await?;
        }

        // Set pinned flag if requested
        if self.pinned {
            self.memory
                .set_block_pinned(&owner_str, &self.label, true)
                .await?;
        }

        Ok(BlockRef::new(&self.label, block_id).owned_by(&owner_str))
    }
}

/// Builder for creating notifications.
///
/// Provides a fluent API for building [`Notification`] instances to send
/// through broadcast channels.
///
/// # Example
///
/// ```ignore
/// let notification = NotificationBuilder::new()
///     .text("New message received")
///     .block(user_block_ref)
///     .block(context_block_ref)
///     .build();
/// ```
pub struct NotificationBuilder {
    message: Option<Message>,
    block_refs: Vec<BlockRef>,
    batch_id: Option<SnowflakePosition>,
}

impl NotificationBuilder {
    /// Create a new notification builder.
    pub fn new() -> Self {
        Self {
            message: None,
            block_refs: Vec::new(),
            batch_id: None,
        }
    }

    /// Set the message content from text.
    ///
    /// Creates a user message with the given text content.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.message = Some(Message::user(text.into()));
        self
    }

    /// Set the message directly.
    ///
    /// Use this when you need more control over the message type or content.
    pub fn message(mut self, message: Message) -> Self {
        self.message = Some(message);
        self
    }

    /// Add a block reference to load with this notification.
    ///
    /// Blocks are loaded into agent context for the batch containing this notification.
    pub fn block(mut self, block_ref: BlockRef) -> Self {
        self.block_refs.push(block_ref);
        self
    }

    /// Add multiple block references.
    pub fn blocks(mut self, refs: impl IntoIterator<Item = BlockRef>) -> Self {
        self.block_refs.extend(refs);
        self
    }

    /// Set the batch ID for this notification.
    ///
    /// If not set, a new batch ID will be generated.
    pub fn batch_id(mut self, id: SnowflakePosition) -> Self {
        self.batch_id = Some(id);
        self
    }

    /// Build the notification.
    ///
    /// If no message was set, an empty user message is created.
    /// If no batch ID was set, a new one is generated.
    pub fn build(self) -> Notification {
        Notification {
            message: self.message.unwrap_or_else(|| Message::user(String::new())),
            block_refs: self.block_refs,
            batch_id: self.batch_id.unwrap_or_else(get_next_message_position_sync),
        }
    }
}

impl Default for NotificationBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Utility for managing ephemeral blocks with get-or-create semantics.
///
/// Caches block references by external ID (e.g., user DID, file path) to avoid
/// creating duplicate blocks for the same external entity.
///
/// # Example
///
/// ```ignore
/// let cache = EphemeralBlockCache::new();
///
/// // First call creates the block
/// let block_ref = cache.get_or_create(
///     "did:plc:abc123",
///     |id| format!("bluesky_user_{}", id),
///     |label| async move {
///         BlockBuilder::new(memory, owner, label)
///             .description("Bluesky user profile")
///             .build()
///             .await
///     },
/// ).await?;
///
/// // Second call returns cached reference
/// let same_ref = cache.get_or_create("did:plc:abc123", ...).await?;
/// ```
pub struct EphemeralBlockCache {
    /// Map of external ID to block info
    cache: dashmap::DashMap<String, CachedBlockInfo>,
}

#[derive(Clone)]
struct CachedBlockInfo {
    block_id: String,
    label: String,
    owner: String,
}

impl EphemeralBlockCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self {
            cache: dashmap::DashMap::new(),
        }
    }

    /// Get or create an ephemeral block.
    ///
    /// Uses `external_id` as the cache key (e.g., "did:plc:abc123" for a user).
    /// If a block exists in the cache, returns its reference.
    /// Otherwise, calls `create_fn` to create a new block and caches the result.
    ///
    /// # Arguments
    ///
    /// * `external_id` - Unique identifier for the external entity
    /// * `label_fn` - Function to generate the block label from the external ID
    /// * `create_fn` - Async function to create the block, receives the generated label
    ///
    /// # Type Parameters
    ///
    /// * `E` - Error type that can be converted from `MemoryError`
    ///
    /// # Returns
    ///
    /// A [`BlockRef`] for the cached or newly created block.
    pub async fn get_or_create<F, Fut, E>(
        &self,
        external_id: &str,
        label_fn: impl FnOnce(&str) -> String,
        create_fn: F,
    ) -> Result<BlockRef, E>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<BlockRef, E>>,
    {
        // Check cache first
        if let Some(info) = self.cache.get(external_id) {
            return Ok(BlockRef {
                label: info.label.clone(),
                block_id: info.block_id.clone(),
                agent_id: info.owner.clone(),
            });
        }

        // Create new block
        let label = label_fn(external_id);
        let block_ref = create_fn(label.clone()).await?;

        // Cache it
        self.cache.insert(
            external_id.to_string(),
            CachedBlockInfo {
                block_id: block_ref.block_id.clone(),
                label: block_ref.label.clone(),
                owner: block_ref.agent_id.clone(),
            },
        );

        Ok(block_ref)
    }

    /// Remove a block from the cache.
    ///
    /// Does not delete the actual block from the memory store.
    pub fn invalidate(&self, external_id: &str) {
        self.cache.remove(external_id);
    }

    /// Clear all cached entries.
    ///
    /// Does not delete the actual blocks from the memory store.
    pub fn clear(&self) {
        self.cache.clear();
    }

    /// Get the number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

impl Default for EphemeralBlockCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_builder_default() {
        let notification = NotificationBuilder::new().build();

        // Should have empty message and no blocks
        assert!(notification.block_refs.is_empty());
        // batch_id should be generated (non-zero)
        // We can't easily test the exact value, but we can verify it exists
    }

    #[test]
    fn test_notification_builder_with_text() {
        let notification = NotificationBuilder::new().text("Hello, world!").build();

        // Message should be set (we can't easily inspect Message content in tests)
        assert!(notification.block_refs.is_empty());
    }

    #[test]
    fn test_notification_builder_with_blocks() {
        let block1 = BlockRef::new("label1", "id1");
        let block2 = BlockRef::new("label2", "id2");

        let notification = NotificationBuilder::new()
            .text("Test message")
            .block(block1)
            .block(block2)
            .build();

        assert_eq!(notification.block_refs.len(), 2);
        assert_eq!(notification.block_refs[0].label, "label1");
        assert_eq!(notification.block_refs[1].label, "label2");
    }

    #[test]
    fn test_notification_builder_with_batch_id() {
        let batch_id = get_next_message_position_sync();

        let notification = NotificationBuilder::new()
            .text("Test")
            .batch_id(batch_id)
            .build();

        assert_eq!(notification.batch_id, batch_id);
    }

    #[test]
    fn test_ephemeral_block_cache_new() {
        let cache = EphemeralBlockCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_ephemeral_block_cache_invalidate() {
        let cache = EphemeralBlockCache::new();

        // Manually insert an entry for testing
        cache.cache.insert(
            "test_id".to_string(),
            CachedBlockInfo {
                block_id: "block_123".to_string(),
                label: "test_label".to_string(),
                owner: "owner_456".to_string(),
            },
        );

        assert_eq!(cache.len(), 1);

        cache.invalidate("test_id");
        assert!(cache.is_empty());
    }

    #[test]
    fn test_ephemeral_block_cache_clear() {
        let cache = EphemeralBlockCache::new();

        // Manually insert entries for testing
        cache.cache.insert(
            "id1".to_string(),
            CachedBlockInfo {
                block_id: "block_1".to_string(),
                label: "label_1".to_string(),
                owner: "owner".to_string(),
            },
        );
        cache.cache.insert(
            "id2".to_string(),
            CachedBlockInfo {
                block_id: "block_2".to_string(),
                label: "label_2".to_string(),
                owner: "owner".to_string(),
            },
        );

        assert_eq!(cache.len(), 2);

        cache.clear();
        assert!(cache.is_empty());
    }
}
