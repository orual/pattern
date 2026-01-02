//! In-memory cache of StructuredDocument instances

use crate::db::ConstellationDatabases;
use crate::embeddings::EmbeddingProvider;
use crate::memory::{
    ArchivalEntry, BlockMetadata, BlockSchema, BlockType, CachedBlock, MemoryError, MemoryResult,
    MemorySearchResult, MemoryStore, SearchMode, SearchOptions, SharedBlockInfo,
    StructuredDocument,
};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::Value as JsonValue;
use sqlx::types::Json as SqlxJson;
use std::sync::Arc;
use uuid::Uuid;

/// Default character limit for memory blocks when not specified
pub const DEFAULT_MEMORY_CHAR_LIMIT: usize = 5000;

/// In-memory cache of LoroDoc instances with lazy loading
#[derive(Debug)]
pub struct MemoryCache {
    /// Combined database connections (constellation + auth)
    dbs: Arc<ConstellationDatabases>,

    /// Optional embedding provider for vector/hybrid search
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,

    /// Cached blocks: block_id -> CachedBlock
    blocks: DashMap<String, CachedBlock>,

    /// Default character limit for new memory blocks
    default_char_limit: usize,
}

impl MemoryCache {
    /// Create a new memory cache without embedding support
    pub fn new(dbs: Arc<ConstellationDatabases>) -> Self {
        Self {
            dbs,
            embedding_provider: None,
            blocks: DashMap::new(),
            default_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
        }
    }

    /// Create a new memory cache with an embedding provider for vector/hybrid search
    pub fn with_embedding_provider(
        dbs: Arc<ConstellationDatabases>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            dbs,
            embedding_provider: Some(provider),
            blocks: DashMap::new(),
            default_char_limit: DEFAULT_MEMORY_CHAR_LIMIT,
        }
    }

    /// Set a custom default character limit for new memory blocks
    pub fn with_default_char_limit(mut self, limit: usize) -> Self {
        self.default_char_limit = limit;
        self
    }

    /// Get the default character limit
    pub fn default_char_limit(&self) -> usize {
        self.default_char_limit
    }

    /// Get or load a block owned by agent_id
    /// Returns a cloned StructuredDocument (cheap - LoroDoc internally Arc'd)
    /// For owned blocks, the effective permission is the block's inherent permission
    pub async fn get(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // 1. Check access FIRST (always) - DB is source of truth
        let access_result = pattern_db::queries::check_block_access(
            self.dbs.constellation.pool(),
            agent_id, // requester
            agent_id, // owner (same for owned blocks)
            label,
        )
        .await?;

        tracing::debug!(
            "Access Result: {:?}, agent: {}, label: {}",
            access_result,
            agent_id,
            label
        );
        let (block_id, permission) = match access_result {
            Some((id, perm)) => (id, perm),
            None => {
                return Err(MemoryError::NotFound {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                });
            } // Block doesn't exist or no access
        };

        // 2. Check cache using block_id
        if self.blocks.contains_key(&block_id) {
            // Extract data we need without holding the lock across async
            let last_seq = {
                let entry = self.blocks.get(&block_id).unwrap();
                entry.last_seq
            };

            // Check for new updates from DB since we last synced
            let updates = pattern_db::queries::get_updates_since(
                self.dbs.constellation.pool(),
                &block_id,
                last_seq,
            )
            .await?;

            // Re-acquire mutable lock to apply updates and update permission from DB
            {
                let mut entry = self.blocks.get_mut(&block_id).unwrap();
                if !updates.is_empty() {
                    for update in &updates {
                        entry.doc.apply_updates(&update.update_blob)?;
                    }
                    entry.last_seq = updates.last().unwrap().seq;
                }

                // DB permission overrides cached permission (in metadata)
                entry.doc.metadata_mut().permission = permission;
                entry.last_accessed = Utc::now();
            }

            // Get the document with updated permission
            let entry = self.blocks.get(&block_id).unwrap();
            let mut doc = entry.doc.clone();
            doc.set_permission(permission);
            return Ok(Some(doc));
        }

        // 3. Load from database with effective permission
        let block = self.load_from_db(agent_id, label, permission).await?;

        match block {
            Some(cached) => {
                let doc = cached.doc.clone();
                self.blocks.insert(block_id, cached);
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    /// Load a block from database, reconstructing StructuredDocument from snapshot + deltas.
    /// The permission parameter is the effective permission for this access (already calculated).
    async fn load_from_db(
        &self,
        agent_id: &str,
        label: &str,
        effective_permission: pattern_db::models::MemoryPermission,
    ) -> MemoryResult<Option<CachedBlock>> {
        // Get block from database
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;

        let block = match block {
            Some(b) if b.is_active => b,
            _ => {
                return Err(MemoryError::NotFound {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                });
            }
        };

        // Build BlockMetadata from DB block
        let mut metadata = db_block_to_metadata(&block);
        // Override with effective permission (may differ for shared blocks)
        metadata.permission = effective_permission;

        // Create StructuredDocument from snapshot with metadata
        let doc = if block.loro_snapshot.is_empty() {
            StructuredDocument::new_with_metadata(metadata.clone(), Some(agent_id.to_string()))
        } else {
            StructuredDocument::from_snapshot_with_metadata(
                &block.loro_snapshot,
                metadata.clone(),
                Some(agent_id.to_string()),
            )?
        };

        // Get and apply any updates since the snapshot
        let (_, updates) = pattern_db::queries::get_checkpoint_and_updates(
            self.dbs.constellation.pool(),
            &block.id,
        )
        .await?;

        for update in &updates {
            doc.apply_updates(&update.update_blob)?;
        }

        let last_seq = updates.last().map(|u| u.seq).unwrap_or(block.last_seq);
        let frontier = doc.current_version();

        Ok(Some(CachedBlock {
            doc,
            last_seq,
            last_persisted_frontier: Some(frontier),
            dirty: false,
            last_accessed: Utc::now(),
        }))
    }

    /// Persist changes for a block (export delta, write to DB)
    pub async fn persist(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Get block_id from DB first
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;
        let block_id = match block {
            Some(b) => b.id,
            None => {
                return Err(MemoryError::NotFound {
                    agent_id: agent_id.to_string(),
                    label: label.to_string(),
                });
            }
        };

        let entry = self
            .blocks
            .get(&block_id)
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        if !entry.dirty {
            return Ok(());
        }

        // Extract data we need before releasing the entry lock
        let doc = entry.doc.clone();
        let last_frontier = entry.last_persisted_frontier.clone();

        // Release the entry lock before doing async work
        drop(entry);

        // Now work with the doc (LoroDoc is already thread-safe, no need for read())
        let update_blob = match &last_frontier {
            Some(frontier) => doc.export_updates_since(frontier),
            None => doc.export_snapshot(),
        };

        let new_frontier = doc.current_version();
        let preview = doc.render();

        // Only persist if there's actual data
        let mut new_seq = None;
        if let Ok(blob) = update_blob {
            if !blob.is_empty() {
                let seq = pattern_db::queries::store_update(
                    self.dbs.constellation.pool(),
                    &block_id,
                    &blob,
                    Some("agent"),
                )
                .await?;

                new_seq = Some(seq);
            }
        }

        // Update the content preview in the main block
        let preview_str = if preview.is_empty() {
            None
        } else {
            Some(preview.as_str())
        };

        pattern_db::queries::update_block_content(
            self.dbs.constellation.pool(),
            &block_id,
            &[], // Don't update snapshot on every write
            preview_str,
        )
        .await?;

        // Now re-acquire the lock to update the cache entry
        let mut entry = self
            .blocks
            .get_mut(&block_id)
            .ok_or_else(|| MemoryError::NotFound {
                agent_id: agent_id.to_string(),
                label: label.to_string(),
            })?;

        if let Some(seq) = new_seq {
            entry.last_seq = seq;
        }
        entry.last_persisted_frontier = Some(new_frontier);
        entry.dirty = false;

        Ok(())
    }

    /// Helper to get block_id from agent_id and label
    async fn get_block_id(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;
        Ok(block.map(|b| b.id))
    }

    /// Mark a block as dirty (has unpersisted changes)
    pub fn mark_dirty(&self, agent_id: &str, label: &str) {
        // This is a synchronous method, so we can't query DB here
        // Instead, we'll iterate through cache to find the block
        let block_id = self
            .blocks
            .iter()
            .find(|entry| entry.doc.agent_id() == agent_id && entry.doc.label() == label)
            .map(|entry| entry.doc.id().to_string());

        if let Some(id) = block_id {
            if let Some(mut cached) = self.blocks.get_mut(&id) {
                cached.dirty = true;
            }
        }
    }

    /// Check if a block is cached
    pub async fn is_cached(&self, agent_id: &str, label: &str) -> bool {
        if let Ok(Some(block_id)) = self.get_block_id(agent_id, label).await {
            self.blocks.contains_key(&block_id)
        } else {
            false
        }
    }

    /// Evict a block from cache (persists first if dirty)
    pub async fn evict(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Persist first if dirty
        self.persist(agent_id, label).await?;

        if let Some(block_id) = self.get_block_id(agent_id, label).await? {
            self.blocks.remove(&block_id);
        }
        Ok(())
    }
}

/// Helper function to convert DB MemoryBlock to BlockMetadata
fn db_block_to_metadata(block: &pattern_db::models::MemoryBlock) -> BlockMetadata {
    let schema = block
        .metadata
        .as_ref()
        .and_then(|m| m.get("schema"))
        .and_then(|s| serde_json::from_value::<BlockSchema>(s.clone()).ok())
        .unwrap_or_default();

    BlockMetadata {
        id: block.id.clone(),
        agent_id: block.agent_id.clone(),
        label: block.label.clone(),
        description: block.description.clone(),
        block_type: block.block_type.into(),
        schema,
        char_limit: block.char_limit as usize,
        permission: block.permission,
        pinned: block.pinned,
        created_at: block.created_at,
        updated_at: block.updated_at,
    }
}

/// Helper function to convert DB ArchivalEntry to our ArchivalEntry
fn db_archival_to_archival(entry: &pattern_db::models::ArchivalEntry) -> ArchivalEntry {
    ArchivalEntry {
        id: entry.id.clone(),
        agent_id: entry.agent_id.clone(),
        content: entry.content.clone(),
        metadata: entry.metadata.as_ref().map(|j| j.0.clone()),
        created_at: entry.created_at,
    }
}

#[async_trait]
impl MemoryStore for MemoryCache {
    async fn create_block(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
        block_type: BlockType,
        schema: BlockSchema,
        char_limit: usize,
    ) -> MemoryResult<StructuredDocument> {
        // Use default char limit if 0 is passed
        let effective_char_limit = if char_limit == 0 {
            self.default_char_limit
        } else {
            char_limit
        };

        // Generate block ID
        let block_id = format!("mem_{}", Uuid::new_v4().simple());
        let now = Utc::now();

        // Build BlockMetadata
        let block_metadata = BlockMetadata {
            id: block_id.clone(),
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            block_type,
            schema: schema.clone(),
            char_limit: effective_char_limit,
            permission: pattern_db::models::MemoryPermission::ReadWrite,
            pinned: false,
            created_at: now,
            updated_at: now,
        };

        // Create new StructuredDocument with metadata
        let doc = StructuredDocument::new_with_metadata(
            block_metadata.clone(),
            Some(agent_id.to_string()),
        );

        // Store schema in DB metadata JSON
        let mut db_metadata = serde_json::Map::new();
        db_metadata.insert(
            "schema".to_string(),
            serde_json::to_value(&schema).map_err(|e| MemoryError::Other(e.to_string()))?,
        );
        let metadata_json = JsonValue::Object(db_metadata);

        // Create MemoryBlock for DB
        let db_block = pattern_db::models::MemoryBlock {
            id: block_id.clone(),
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            block_type: block_type.into(),
            char_limit: effective_char_limit as i64,
            permission: pattern_db::models::MemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: Some(SqlxJson(metadata_json)),
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: now,
            updated_at: now,
        };

        // Store in DB
        pattern_db::queries::create_block(self.dbs.constellation.pool(), &db_block).await?;

        // Add to cache (metadata is embedded in doc)
        let cached_block = CachedBlock {
            doc: doc.clone(),
            last_seq: 0,
            last_persisted_frontier: None,
            dirty: false,
            last_accessed: now,
        };

        self.blocks.insert(block_id, cached_block);

        Ok(doc)
    }

    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // Delegate to existing get method
        self.get(agent_id, label).await
    }

    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        // Query DB for block metadata without loading full document
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;

        Ok(block.as_ref().map(db_block_to_metadata))
    }

    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
        // Query DB for all blocks for agent
        let blocks =
            pattern_db::queries::list_blocks(self.dbs.constellation.pool(), agent_id).await?;

        Ok(blocks.iter().map(db_block_to_metadata).collect())
    }

    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        // Query DB filtered by type
        let blocks = pattern_db::queries::list_blocks_by_type(
            self.dbs.constellation.pool(),
            agent_id,
            block_type.into(),
        )
        .await?;

        Ok(blocks.iter().map(db_block_to_metadata).collect())
    }

    async fn list_all_blocks_by_label_prefix(
        &self,
        prefix: &str,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        // Query DB for all blocks with matching label prefix (across all agents)
        let blocks =
            pattern_db::queries::list_blocks_by_label_prefix(self.dbs.constellation.pool(), prefix)
                .await?;

        Ok(blocks.iter().map(db_block_to_metadata).collect())
    }

    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Get block ID first
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;

        if let Some(block) = block {
            // Evict from cache first (will persist if dirty)
            if self.blocks.contains_key(&block.id) {
                self.evict(agent_id, label).await?;
            }

            // Soft-delete in DB
            pattern_db::queries::deactivate_block(self.dbs.constellation.pool(), &block.id).await?;
        }

        Ok(())
    }

    async fn get_rendered_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>> {
        // Get doc, call doc.render()
        let doc = self.get(agent_id, label).await?;
        Ok(doc.map(|d| d.render()))
    }

    async fn persist_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        // Delegate to existing persist method
        self.persist(agent_id, label).await
    }

    fn mark_dirty(&self, agent_id: &str, label: &str) {
        // Delegate to existing method
        MemoryCache::mark_dirty(self, agent_id, label);
    }

    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<JsonValue>,
    ) -> MemoryResult<String> {
        // Generate archival entry ID
        let entry_id = format!("arch_{}", Uuid::new_v4().simple());

        // Create archival entry
        let entry = pattern_db::models::ArchivalEntry {
            id: entry_id.clone(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            metadata: metadata.map(sqlx::types::Json),
            chunk_index: 0,
            parent_entry_id: None,
            created_at: Utc::now(),
        };

        // Store in DB
        pattern_db::queries::create_archival_entry(self.dbs.constellation.pool(), &entry).await?;

        Ok(entry_id)
    }

    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        // Use rich search with FTS mode (no embedder available in MemoryCache yet)
        let results = pattern_db::search::search(self.dbs.constellation.pool())
            .text(query)
            .mode(pattern_db::search::SearchMode::FtsOnly)
            .limit(limit as i64)
            .filter(pattern_db::search::ContentFilter::archival(Some(agent_id)))
            .execute()
            .await?;

        // Convert search results to ArchivalEntry
        let mut entries = Vec::new();
        for result in results {
            // Get the full archival entry from DB by ID
            if let Some(entry) =
                pattern_db::queries::get_archival_entry(self.dbs.constellation.pool(), &result.id)
                    .await?
            {
                entries.push(db_archival_to_archival(&entry));
            }
        }

        Ok(entries)
    }

    async fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        // Delete from DB
        // NOTE fix to soft-delete
        pattern_db::queries::delete_archival_entry(self.dbs.constellation.pool(), id).await?;
        Ok(())
    }

    async fn search(
        &self,
        agent_id: &str,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        // Generate embedding if Vector/Hybrid mode is requested and provider is available
        let query_embedding = if options.mode.needs_embedding() {
            if let Some(provider) = &self.embedding_provider {
                match provider.embed_query(query).await {
                    Ok(embedding) => Some(embedding),
                    Err(e) => {
                        tracing::warn!(
                            "Failed to generate embedding for query, falling back to FTS: {}",
                            e
                        );
                        None
                    }
                }
            } else {
                tracing::warn!(
                    "Vector/Hybrid search requested but no embedding provider configured, falling back to FTS"
                );
                None
            }
        } else {
            None
        };

        // Determine effective mode based on what's available
        let effective_mode = match options.mode {
            SearchMode::Auto => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::Hybrid
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
            SearchMode::Fts => pattern_db::search::SearchMode::FtsOnly,
            SearchMode::Vector => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::VectorOnly
                } else {
                    // Fall back to FTS if embedding generation failed
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
            SearchMode::Hybrid => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::Hybrid
                } else {
                    // Fall back to FTS if embedding generation failed
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
        };

        // Build search with pattern_db
        let mut builder = pattern_db::search::search(self.dbs.constellation.pool())
            .text(query)
            .mode(effective_mode)
            .limit(options.limit as i64);

        // Add embedding if available
        if let Some(ref embedding) = query_embedding {
            builder = builder.embedding(embedding);
        }

        // If content types is empty, search all types
        if options.content_types.is_empty() {
            // No filter, search all types for this agent
            builder = builder.filter(pattern_db::search::ContentFilter {
                content_type: None,
                agent_id: Some(agent_id.to_string()),
            });
        } else if options.content_types.len() == 1 {
            // Single content type - use filter
            let db_content_type = options.content_types[0].to_db_content_type();
            builder = builder.filter(pattern_db::search::ContentFilter {
                content_type: Some(db_content_type),
                agent_id: Some(agent_id.to_string()),
            });
        } else {
            // Multiple content types - execute separate queries and combine results
            let mut all_results = Vec::new();

            for content_type in &options.content_types {
                let db_content_type = content_type.to_db_content_type();
                let mut type_builder = pattern_db::search::search(self.dbs.constellation.pool())
                    .text(query)
                    .mode(effective_mode)
                    .limit(options.limit as i64)
                    .filter(pattern_db::search::ContentFilter {
                        content_type: Some(db_content_type),
                        agent_id: Some(agent_id.to_string()),
                    });

                // Add embedding if available
                if let Some(ref embedding) = query_embedding {
                    type_builder = type_builder.embedding(embedding);
                }

                let results = type_builder.execute().await?;
                all_results.extend(results);
            }

            // Sort by score and limit
            all_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            all_results.truncate(options.limit);

            // Convert and return early
            return Ok(all_results
                .into_iter()
                .map(MemorySearchResult::from_db_result)
                .collect());
        }

        // Execute search
        let results = builder.execute().await?;

        // Convert to MemorySearchResult
        Ok(results
            .into_iter()
            .map(MemorySearchResult::from_db_result)
            .collect())
    }

    async fn search_all(
        &self,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>> {
        // Generate embedding if Vector/Hybrid mode is requested and provider is available
        let query_embedding = if options.mode.needs_embedding() {
            if let Some(provider) = &self.embedding_provider {
                match provider.embed_query(query).await {
                    Ok(embedding) => Some(embedding),
                    Err(e) => {
                        tracing::warn!(
                            "Failed to generate embedding for query, falling back to FTS: {}",
                            e
                        );
                        None
                    }
                }
            } else {
                tracing::warn!(
                    "Vector/Hybrid search requested but no embedding provider configured, falling back to FTS"
                );
                None
            }
        } else {
            None
        };

        // Determine effective mode based on what's available
        let effective_mode = match options.mode {
            SearchMode::Auto => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::Hybrid
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
            SearchMode::Fts => pattern_db::search::SearchMode::FtsOnly,
            SearchMode::Vector => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::VectorOnly
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
            SearchMode::Hybrid => {
                if query_embedding.is_some() {
                    pattern_db::search::SearchMode::Hybrid
                } else {
                    pattern_db::search::SearchMode::FtsOnly
                }
            }
        };

        // Build search with pattern_db (no agent_id filter for constellation-wide search)
        let mut builder = pattern_db::search::search(self.dbs.constellation.pool())
            .text(query)
            .mode(effective_mode)
            .limit(options.limit as i64);

        // Add embedding if available
        if let Some(ref embedding) = query_embedding {
            builder = builder.embedding(embedding);
        }

        // If content types is empty, search all types
        if options.content_types.is_empty() {
            // No filter, search all types across all agents
            builder = builder.filter(pattern_db::search::ContentFilter {
                content_type: None,
                agent_id: None, // No agent_id filter = constellation-wide
            });
        } else if options.content_types.len() == 1 {
            // Single content type - use filter
            let db_content_type = options.content_types[0].to_db_content_type();
            builder = builder.filter(pattern_db::search::ContentFilter {
                content_type: Some(db_content_type),
                agent_id: None, // No agent_id filter = constellation-wide
            });
        } else {
            // Multiple content types - execute separate queries and combine results
            let mut all_results = Vec::new();

            for content_type in &options.content_types {
                let db_content_type = content_type.to_db_content_type();
                let mut type_builder = pattern_db::search::search(self.dbs.constellation.pool())
                    .text(query)
                    .mode(effective_mode)
                    .limit(options.limit as i64)
                    .filter(pattern_db::search::ContentFilter {
                        content_type: Some(db_content_type),
                        agent_id: None, // No agent_id filter = constellation-wide
                    });

                // Add embedding if available
                if let Some(ref embedding) = query_embedding {
                    type_builder = type_builder.embedding(embedding);
                }

                let results = type_builder.execute().await?;
                all_results.extend(results);
            }

            // Sort by score and limit
            all_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            all_results.truncate(options.limit);

            // Convert and return early
            return Ok(all_results
                .into_iter()
                .map(MemorySearchResult::from_db_result)
                .collect());
        }

        // Execute search
        let results = builder.execute().await?;

        // Convert to MemorySearchResult
        Ok(results
            .into_iter()
            .map(MemorySearchResult::from_db_result)
            .collect())
    }

    async fn list_shared_blocks(&self, agent_id: &str) -> MemoryResult<Vec<SharedBlockInfo>> {
        let shared =
            pattern_db::queries::get_shared_blocks(self.dbs.constellation.pool(), agent_id).await?;

        Ok(shared
            .into_iter()
            .map(|(block, permission, owner_name)| SharedBlockInfo {
                block_id: block.id,
                owner_agent_id: block.agent_id,
                owner_agent_name: owner_name,
                label: block.label,
                description: block.description,
                block_type: block.block_type.into(),
                permission,
            })
            .collect())
    }

    async fn get_shared_block(
        &self,
        requester_agent_id: &str,
        owner_agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<StructuredDocument>> {
        // 1. Check access FIRST - DB is source of truth
        let access_result = pattern_db::queries::check_block_access(
            self.dbs.constellation.pool(),
            requester_agent_id,
            owner_agent_id,
            label,
        )
        .await?;

        let (block_id, shared_permission) = match access_result {
            Some((id, perm)) => (id, perm),
            None => return Ok(None), // No access
        };

        // 2. Check cache using block_id
        if self.blocks.contains_key(&block_id) {
            // Block is cached - get it and return with shared permission
            let last_seq = {
                let entry = self.blocks.get(&block_id).unwrap();
                entry.last_seq
            };

            // Check for new updates from DB since we last synced
            let updates = pattern_db::queries::get_updates_since(
                self.dbs.constellation.pool(),
                &block_id,
                last_seq,
            )
            .await?;

            // Re-acquire mutable lock to apply updates
            let mut entry = self.blocks.get_mut(&block_id).unwrap();
            if !updates.is_empty() {
                for update in &updates {
                    entry.doc.apply_updates(&update.update_blob)?;
                }
                entry.last_seq = updates.last().unwrap().seq;
            }
            entry.last_accessed = Utc::now();

            // Clone the doc but with the shared permission
            // LoroDoc is cheap to clone (shared internally), but permission is not shared
            let mut doc = entry.doc.clone();
            doc.set_permission(shared_permission);
            return Ok(Some(doc));
        }

        // 3. Load from DB with shared permission
        // Load from database with shared permission
        let block = self
            .load_from_db(owner_agent_id, label, shared_permission)
            .await?;

        match block {
            Some(cached) => {
                let doc = cached.doc.clone();
                self.blocks.insert(block_id, cached);
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    async fn set_block_pinned(
        &self,
        agent_id: &str,
        label: &str,
        pinned: bool,
    ) -> MemoryResult<()> {
        // Get block ID from DB
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;

        let block = block.ok_or_else(|| MemoryError::NotFound {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
        })?;

        // Update in database
        pattern_db::queries::update_block_pinned(self.dbs.constellation.pool(), &block.id, pinned)
            .await?;

        // Update in cache if loaded
        if let Some(mut cached) = self.blocks.get_mut(&block.id) {
            cached.doc.metadata_mut().pinned = pinned;
            cached.last_accessed = Utc::now();
        }

        Ok(())
    }

    async fn set_block_type(
        &self,
        agent_id: &str,
        label: &str,
        block_type: BlockType,
    ) -> MemoryResult<()> {
        // Get block ID from DB
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;

        let block = block.ok_or_else(|| MemoryError::NotFound {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
        })?;

        // Update in database
        pattern_db::queries::update_block_type(
            self.dbs.constellation.pool(),
            &block.id,
            block_type.into(),
        )
        .await?;

        // Update in cache if loaded
        if let Some(mut cached) = self.blocks.get_mut(&block.id) {
            cached.doc.metadata_mut().block_type = block_type;
            cached.last_accessed = Utc::now();
        }

        Ok(())
    }

    async fn update_block_schema(
        &self,
        agent_id: &str,
        label: &str,
        schema: BlockSchema,
    ) -> MemoryResult<()> {
        // Get block from DB
        let block =
            pattern_db::queries::get_block_by_label(self.dbs.constellation.pool(), agent_id, label)
                .await?;

        let block = block.ok_or_else(|| MemoryError::NotFound {
            agent_id: agent_id.to_string(),
            label: label.to_string(),
        })?;

        // Parse existing schema to validate compatibility
        let existing_schema = block
            .metadata
            .as_ref()
            .and_then(|m| m.get("schema"))
            .and_then(|s| serde_json::from_value::<BlockSchema>(s.clone()).ok())
            .unwrap_or_default();

        // Validate schema compatibility (same variant type)
        if std::mem::discriminant(&existing_schema) != std::mem::discriminant(&schema) {
            return Err(MemoryError::Other(format!(
                "Cannot change schema type from {:?} to {:?}",
                existing_schema, schema
            )));
        }

        // Build updated metadata
        let mut metadata = block
            .metadata
            .as_ref()
            .and_then(|m| m.as_object().cloned())
            .unwrap_or_default();
        metadata.insert(
            "schema".to_string(),
            serde_json::to_value(&schema).map_err(|e| MemoryError::Other(e.to_string()))?,
        );
        let metadata_json = serde_json::Value::Object(metadata);

        // Update in database
        pattern_db::queries::update_block_metadata(
            self.dbs.constellation.pool(),
            &block.id,
            &metadata_json,
        )
        .await?;

        // Update in cache if loaded - need to update the document's schema
        if let Some(mut cached) = self.blocks.get_mut(&block.id) {
            cached.doc.set_schema(schema);
            cached.last_accessed = Utc::now();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_db::models::{MemoryBlock, MemoryBlockType, MemoryPermission};

    async fn test_dbs() -> (tempfile::TempDir, Arc<ConstellationDatabases>) {
        let dir = tempfile::tempdir().unwrap();
        let dbs = Arc::new(ConstellationDatabases::open(dir.path()).await.unwrap());
        (dir, dbs)
    }

    /// Create a test agent in the database with sensible defaults.
    /// Returns the agent ID for use in tests.
    async fn create_test_agent(dbs: &ConstellationDatabases, agent_id: &str) -> String {
        let agent = pattern_db::models::Agent {
            id: agent_id.to_string(),
            name: format!("Test Agent {}", agent_id),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: Default::default(),
            enabled_tools: Default::default(),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(dbs.constellation.pool(), &agent)
            .await
            .expect("Failed to create test agent");
        agent_id.to_string()
    }

    /// Create test databases and a default test agent ("agent_1").
    /// Returns (TempDir, Arc<ConstellationDatabases>). The TempDir must be kept
    /// alive for the duration of the test.
    async fn test_dbs_with_agent() -> (tempfile::TempDir, Arc<ConstellationDatabases>) {
        let (dir, dbs) = test_dbs().await;
        create_test_agent(&dbs, "agent_1").await;
        (dir, dbs)
    }

    #[tokio::test]
    async fn test_cache_load_empty_block() {
        let (_dir, dbs) = test_dbs_with_agent().await;

        // Create a block in DB
        let block = MemoryBlock {
            id: "mem_1".to_string(),
            agent_id: "agent_1".to_string(),
            label: "persona".to_string(),
            description: "Agent personality".to_string(),
            block_type: MemoryBlockType::Core,
            char_limit: 5000,
            permission: MemoryPermission::ReadWrite,
            pinned: true,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_block(dbs.constellation.pool(), &block)
            .await
            .unwrap();

        // Create cache and load
        let cache = MemoryCache::new(dbs);
        let doc = cache.get("agent_1", "persona").await.unwrap();

        assert!(doc.is_some());
        assert!(cache.is_cached("agent_1", "persona").await);
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let (_dir, dbs) = test_dbs().await;
        let cache = MemoryCache::new(dbs);

        let doc = cache.get("agent_1", "nonexistent").await;
        assert!(doc.is_err());
    }

    #[tokio::test]
    async fn test_cache_persist() {
        let (_dir, dbs) = test_dbs_with_agent().await;

        // Create a block
        let block = MemoryBlock {
            id: "mem_2".to_string(),
            agent_id: "agent_1".to_string(),
            label: "scratch".to_string(),
            description: "Working memory".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 5000,
            permission: MemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        pattern_db::queries::create_block(dbs.constellation.pool(), &block)
            .await
            .unwrap();

        let cache = MemoryCache::new(dbs.clone());

        // Load and modify
        let doc = cache.get("agent_1", "scratch").await.unwrap().unwrap();
        // StructuredDocument methods are already thread-safe
        doc.set_text("Hello, world!", true).unwrap();

        cache.mark_dirty("agent_1", "scratch");

        // Persist
        cache.persist("agent_1", "scratch").await.unwrap();

        // Verify update was stored
        let (_, updates) =
            pattern_db::queries::get_checkpoint_and_updates(dbs.constellation.pool(), "mem_2")
                .await
                .unwrap();

        assert!(!updates.is_empty());
    }

    // ========== MemoryStore trait tests ==========

    #[tokio::test]
    async fn test_create_and_get_block() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block using MemoryStore trait
        let created_doc = cache
            .create_block(
                "agent_1",
                "test_block",
                "Test block description",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        assert!(created_doc.id().starts_with("mem_"));

        // Get the block back (should return same doc since it's cached)
        let doc = cache.get_block("agent_1", "test_block").await.unwrap();
        assert!(doc.is_some());

        // Verify content is initially empty
        let doc = doc.unwrap();
        assert_eq!(doc.render(), "");

        // Modify and verify
        doc.set_text("Test content", true).unwrap();
        assert_eq!(doc.render(), "Test content");
    }

    #[tokio::test]
    async fn test_list_blocks() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create multiple blocks
        cache
            .create_block(
                "agent_1",
                "block1",
                "First block",
                BlockType::Core,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        cache
            .create_block(
                "agent_1",
                "block2",
                "Second block",
                BlockType::Working,
                BlockSchema::text(),
                2000,
            )
            .await
            .unwrap();

        cache
            .create_block(
                "agent_1",
                "block3",
                "Third block",
                BlockType::Core,
                BlockSchema::text(),
                1500,
            )
            .await
            .unwrap();

        // List all blocks
        let all_blocks = cache.list_blocks("agent_1").await.unwrap();
        assert_eq!(all_blocks.len(), 3);

        // List blocks by type
        let core_blocks = cache
            .list_blocks_by_type("agent_1", BlockType::Core)
            .await
            .unwrap();
        assert_eq!(core_blocks.len(), 2);

        let working_blocks = cache
            .list_blocks_by_type("agent_1", BlockType::Working)
            .await
            .unwrap();
        assert_eq!(working_blocks.len(), 1);
        assert_eq!(working_blocks[0].label, "block2");
    }

    #[tokio::test]
    async fn test_delete_block() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block
        cache
            .create_block(
                "agent_1",
                "to_delete",
                "Will be deleted",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        // Verify it exists
        let doc = cache.get_block("agent_1", "to_delete").await.unwrap();
        assert!(doc.is_some());

        // Delete it
        cache.delete_block("agent_1", "to_delete").await.unwrap();

        // Verify it's gone (soft delete, so get_block returns None)
        let doc = cache.get_block("agent_1", "to_delete").await;
        assert!(doc.is_err());

        // List should not include deleted block
        let blocks = cache.list_blocks("agent_1").await.unwrap();
        assert_eq!(blocks.len(), 0);
    }

    #[tokio::test]
    async fn test_get_rendered_content() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block
        cache
            .create_block(
                "agent_1",
                "content_test",
                "Test content rendering",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        // Get and modify
        let doc = cache
            .get_block("agent_1", "content_test")
            .await
            .unwrap()
            .unwrap();
        doc.set_text("Hello, world!", true).unwrap();

        // Mark dirty and persist
        cache.mark_dirty("agent_1", "content_test");
        cache
            .persist_block("agent_1", "content_test")
            .await
            .unwrap();

        // Get rendered content
        let content = cache
            .get_rendered_content("agent_1", "content_test")
            .await
            .unwrap();
        assert_eq!(content, Some("Hello, world!".to_string()));
    }

    #[tokio::test]
    async fn test_archival_operations() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Insert archival entries
        let id1 = cache
            .insert_archival("agent_1", "First archival entry", None)
            .await
            .unwrap();
        assert!(id1.starts_with("arch_"));

        let metadata = serde_json::json!({"source": "test", "importance": "high"});
        let id2 = cache
            .insert_archival(
                "agent_1",
                "Second archival entry with metadata",
                Some(metadata),
            )
            .await
            .unwrap();
        assert!(id2.starts_with("arch_"));

        // Search archival (simple substring match)
        let results = cache
            .search_archival("agent_1", "archival", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = cache
            .search_archival("agent_1", "metadata", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].metadata.is_some());

        // Delete archival entry
        cache.delete_archival(&id1).await.unwrap();

        // Verify deletion
        let results = cache.search_archival("agent_1", "First", 10).await.unwrap();
        assert_eq!(results.len(), 0);

        // Second entry should still be there
        let results = cache
            .search_archival("agent_1", "Second", 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_get_block_metadata() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block
        cache
            .create_block(
                "agent_1",
                "metadata_test",
                "Test metadata retrieval",
                BlockType::Core,
                BlockSchema::text(),
                5000,
            )
            .await
            .unwrap();

        // Get metadata without loading full document
        let metadata = cache
            .get_block_metadata("agent_1", "metadata_test")
            .await
            .unwrap();

        assert!(metadata.is_some());
        let metadata = metadata.unwrap();
        assert_eq!(metadata.label, "metadata_test");
        assert_eq!(metadata.description, "Test metadata retrieval");
        assert_eq!(metadata.block_type, BlockType::Core);
        assert_eq!(metadata.char_limit, 5000);
        assert!(!metadata.pinned);
    }

    // ========== Search functionality tests ==========

    use crate::memory::{SearchContentType, SearchMode, SearchOptions};

    #[tokio::test]
    async fn test_search_memory_blocks_fts() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs.clone());

        // Create blocks with searchable content
        cache
            .create_block(
                "agent_1",
                "persona",
                "Agent personality",
                BlockType::Core,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let doc = cache
            .get_block("agent_1", "persona")
            .await
            .unwrap()
            .unwrap();
        doc.set_text(
            "I am a helpful assistant specializing in Rust programming",
            true,
        )
        .unwrap();
        cache.mark_dirty("agent_1", "persona");
        cache.persist_block("agent_1", "persona").await.unwrap();

        // Create another block
        cache
            .create_block(
                "agent_1",
                "notes",
                "Working notes",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let doc = cache.get_block("agent_1", "notes").await.unwrap().unwrap();
        doc.set_text(
            "Meeting scheduled for tomorrow about Python development",
            true,
        )
        .unwrap();
        cache.mark_dirty("agent_1", "notes");
        cache.persist_block("agent_1", "notes").await.unwrap();

        // Search for "Rust" - should find persona block
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache.search("agent_1", "Rust", opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("Rust programming")
        );

        // Search for "Python" - should find notes block
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache.search("agent_1", "Python", opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("Python development")
        );

        // Search for "development" - should find both
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks],
            limit: 10,
        };

        let results = cache.search("agent_1", "development", opts).await.unwrap();
        // Note: FTS might not match "development" in both if stemming is involved
        // But searching for a word that appears in both should work
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_search_archival_entries_fts() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Insert archival entries
        cache
            .insert_archival(
                "agent_1",
                "Discussed project requirements for the new authentication system",
                None,
            )
            .await
            .unwrap();

        cache
            .insert_archival(
                "agent_1",
                "Reviewed database schema design for user management",
                None,
            )
            .await
            .unwrap();

        cache
            .insert_archival(
                "agent_1",
                "Implemented token-based authentication with JWT",
                None,
            )
            .await
            .unwrap();

        // Search for "authentication" - should find relevant entries
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("agent_1", "authentication", opts)
            .await
            .unwrap();
        assert_eq!(results.len(), 2); // Should find entries 1 and 3

        // Verify content
        assert!(results.iter().any(|r| {
            r.content
                .as_ref()
                .unwrap()
                .contains("authentication system")
        }));
        assert!(results.iter().any(|r| {
            r.content
                .as_ref()
                .unwrap()
                .contains("token-based authentication")
        }));

        // Search for "database"
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache.search("agent_1", "database", opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("database schema")
        );
    }

    #[tokio::test]
    async fn test_search_multiple_content_types() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs.clone());

        // Create a memory block
        cache
            .create_block(
                "agent_1",
                "persona",
                "Agent personality",
                BlockType::Core,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let doc = cache
            .get_block("agent_1", "persona")
            .await
            .unwrap()
            .unwrap();
        doc.set_text("I specialize in Rust programming and system design", true)
            .unwrap();
        cache.mark_dirty("agent_1", "persona");
        cache.persist_block("agent_1", "persona").await.unwrap();

        // Create an archival entry
        cache
            .insert_archival(
                "agent_1",
                "Helped user debug a complex Rust lifetime issue",
                None,
            )
            .await
            .unwrap();

        // Search across both types
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Blocks, SearchContentType::Archival],
            limit: 10,
        };

        let results = cache.search("agent_1", "Rust", opts).await.unwrap();
        assert_eq!(results.len(), 2); // Should find both the block and archival entry

        // Verify we got results from both types
        let content_types: Vec<_> = results.iter().map(|r| r.content_type).collect();
        assert!(content_types.contains(&SearchContentType::Blocks));
        assert!(content_types.contains(&SearchContentType::Archival));
    }

    #[tokio::test]
    async fn test_search_respects_agent_id() {
        let (_dir, dbs) = test_dbs().await;

        // Create two agents
        create_test_agent(&dbs, "agent_1").await;
        create_test_agent(&dbs, "agent_2").await;

        let cache = MemoryCache::new(dbs);

        // Insert archival for agent_1
        cache
            .insert_archival("agent_1", "Agent 1 secret information", None)
            .await
            .unwrap();

        // Insert archival for agent_2
        cache
            .insert_archival("agent_2", "Agent 2 secret information", None)
            .await
            .unwrap();

        // Search for agent_1 should only return agent_1's data
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        let results = cache
            .search("agent_1", "secret", opts.clone())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.as_ref().unwrap().contains("Agent 1"));

        // Search for agent_2 should only return agent_2's data
        let results = cache.search("agent_2", "secret", opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.as_ref().unwrap().contains("Agent 2"));
    }

    #[tokio::test]
    async fn test_search_limit() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Insert many archival entries with same keyword
        for i in 0..10 {
            cache
                .insert_archival(
                    "agent_1",
                    &format!("Entry {} about testing functionality", i),
                    None,
                )
                .await
                .unwrap();
        }

        // Search with limit of 3
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![SearchContentType::Archival],
            limit: 3,
        };

        let results = cache.search("agent_1", "testing", opts).await.unwrap();
        assert_eq!(results.len(), 3); // Should respect limit
    }

    #[tokio::test]
    async fn test_search_empty_content_types() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs.clone());

        // Create data in both memory blocks and archival
        cache
            .create_block(
                "agent_1",
                "test_block",
                "Test",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let doc = cache
            .get_block("agent_1", "test_block")
            .await
            .unwrap()
            .unwrap();
        doc.set_text("Searchable block content", true).unwrap();
        cache.mark_dirty("agent_1", "test_block");
        cache.persist_block("agent_1", "test_block").await.unwrap();

        cache
            .insert_archival("agent_1", "Searchable archival content", None)
            .await
            .unwrap();

        // Search with empty content_types - should search all types
        let opts = SearchOptions {
            mode: SearchMode::Fts,
            content_types: vec![],
            limit: 10,
        };

        let results = cache.search("agent_1", "Searchable", opts).await.unwrap();
        assert_eq!(results.len(), 2); // Should find both
    }

    #[tokio::test]
    async fn test_search_hybrid_mode_fallback() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs.clone());

        // Insert archival entry
        cache
            .insert_archival("agent_1", "Test content for hybrid search", None)
            .await
            .unwrap();

        // Search with Hybrid mode (should gracefully fall back to FTS)
        let opts = SearchOptions {
            mode: SearchMode::Hybrid,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        // Should succeed (not error) and return results using FTS fallback
        let results = cache.search("agent_1", "hybrid", opts).await.unwrap();
        assert_eq!(results.len(), 1); // Should find the entry using FTS fallback
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("hybrid search")
        );
    }

    #[tokio::test]
    async fn test_search_vector_mode_fallback() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs.clone());

        // Insert archival entry
        cache
            .insert_archival("agent_1", "Test content for vector search", None)
            .await
            .unwrap();

        // Search with Vector mode (should gracefully fall back to FTS)
        let opts = SearchOptions {
            mode: SearchMode::Vector,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        // Should succeed (not error) and return results using FTS fallback
        let results = cache.search("agent_1", "vector", opts).await.unwrap();
        assert_eq!(results.len(), 1); // Should find the entry using FTS fallback
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("vector search")
        );
    }

    #[tokio::test]
    async fn test_search_all_hybrid_mode_fallback() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs.clone());

        // Insert archival entry
        cache
            .insert_archival("agent_1", "Constellation-wide searchable content", None)
            .await
            .unwrap();

        // Search across constellation with Hybrid mode (should gracefully fall back to FTS)
        let opts = SearchOptions {
            mode: SearchMode::Hybrid,
            content_types: vec![SearchContentType::Archival],
            limit: 10,
        };

        // Should succeed (not error) and return results using FTS fallback
        let results = cache.search_all("constellation", opts).await.unwrap();
        assert_eq!(results.len(), 1); // Should find the entry using FTS fallback
        assert!(
            results[0]
                .content
                .as_ref()
                .unwrap()
                .contains("Constellation-wide")
        );
    }

    #[tokio::test]
    async fn test_replace_text_crdt_aware() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block with some initial content.
        let doc = cache
            .create_block(
                "agent_1",
                "test_replace",
                "Test block for replacement",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        // Set initial content.
        doc.set_text("Hello world, this is a test.", true).unwrap();
        cache.mark_dirty("agent_1", "test_replace");
        cache.persist("agent_1", "test_replace").await.unwrap();

        // Get the version vector before replacement.
        let vv_before = doc.inner().oplog_vv();

        // Perform replacement using CRDT-aware method directly on doc.
        let replaced = doc.replace_text("world", "universe", true).unwrap();

        assert!(replaced, "Replacement should have occurred");

        // Persist the changes.
        cache.mark_dirty("agent_1", "test_replace");
        cache.persist("agent_1", "test_replace").await.unwrap();

        // Verify the content is correct.
        assert_eq!(doc.text_content(), "Hello universe, this is a test.");

        // Verify version vector advanced (CRDT operation was recorded).
        let vv_after = doc.inner().oplog_vv();
        assert_ne!(
            vv_before.encode().as_slice(),
            vv_after.encode().as_slice(),
            "Version vector should advance after CRDT operation"
        );
    }

    #[tokio::test]
    async fn test_replace_text_not_found() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block with some content.
        let doc = cache
            .create_block(
                "agent_1",
                "test_replace",
                "Test block for replacement",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        // Set initial content.
        doc.set_text("Hello world", true).unwrap();
        cache.mark_dirty("agent_1", "test_replace");
        cache.persist("agent_1", "test_replace").await.unwrap();

        // Try to replace something that doesn't exist.
        let replaced = doc
            .replace_text("nonexistent", "replacement", true)
            .unwrap();

        assert!(!replaced, "Replacement should not have occurred");

        // Verify content is unchanged.
        assert_eq!(doc.text_content(), "Hello world");
    }

    /// Test that replacement works correctly when content has multi-byte Unicode characters
    /// before/around the replacement target. This exercises the byte-to-Unicode position
    /// conversion in `replace_text` which uses Loro's `convert_pos` for correct splice().
    #[tokio::test]
    async fn test_replace_text_unicode() {
        let (_dir, dbs) = test_dbs_with_agent().await;
        let cache = MemoryCache::new(dbs);

        // Create a block for Unicode replacement testing.
        let doc = cache
            .create_block(
                "agent_1",
                "unicode_test",
                "Test block for Unicode replacement",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        // Test case 1: Emoji before target.
        // "Hello 🌍 world" - emoji is 4 bytes, but 1 Unicode scalar.
        doc.set_text("Hello 🌍 world", true).unwrap();

        let replaced = doc.replace_text("world", "universe", true).unwrap();

        assert!(
            replaced,
            "Replacement should have occurred with emoji before target"
        );
        assert_eq!(
            doc.text_content(),
            "Hello 🌍 universe",
            "Content should correctly replace 'world' with 'universe' after emoji"
        );

        // Test case 2: CJK characters (3 bytes each in UTF-8).
        doc.set_text("日本語 world and more", true).unwrap();

        let replaced = doc.replace_text("world", "世界", true).unwrap();

        assert!(
            replaced,
            "Replacement should have occurred with CJK characters before target"
        );
        assert_eq!(
            doc.text_content(),
            "日本語 世界 and more",
            "Content should correctly replace 'world' with '世界' after CJK chars"
        );

        // Test case 3: Multiple emoji and mixed content.
        doc.set_text("🎉🎊 Hello 🌍 beautiful world 🌈", true)
            .unwrap();

        let replaced = doc
            .replace_text("beautiful world", "amazing planet", true)
            .unwrap();

        assert!(
            replaced,
            "Replacement should work with multiple emoji surrounding target"
        );
        assert_eq!(
            doc.text_content(),
            "🎉🎊 Hello 🌍 amazing planet 🌈",
            "Content should correctly handle multiple emoji around replacement"
        );

        // Test case 4: Replace at very start after Unicode prefix.
        doc.set_text("🔥start middle end", true).unwrap();

        let replaced = doc.replace_text("start", "begin", true).unwrap();

        assert!(replaced, "Replacement should work immediately after emoji");
        assert_eq!(
            doc.text_content(),
            "🔥begin middle end",
            "Content should correctly replace 'start' with 'begin' right after emoji"
        );

        // Test case 5: Replace emoji itself.
        doc.set_text("Hello 🌍 world", true).unwrap();

        let replaced = doc.replace_text("🌍", "🌎", true).unwrap();

        assert!(
            replaced,
            "Replacement should work when replacing emoji with emoji"
        );
        assert_eq!(
            doc.text_content(),
            "Hello 🌎 world",
            "Content should correctly replace emoji with different emoji"
        );
    }
}
